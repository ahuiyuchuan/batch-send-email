/**
 * 邮件文本链接的本机校验和选区操作；不读取网络、不改变草稿资源登记。
 * 所有正文修改使用编辑器原生 insertHTML 命令，保留与加粗、图片一致的撤销历史。
 * @author ahui
 */

/** 弹窗打开时固定的正文选区；模板更换后必须重新取得，不能复用已经脱离编辑器的节点。 */
export interface MailLinkSelection {
  range: Range;
  anchor: HTMLAnchorElement | null;
  text: string;
  href: string;
}

/**
 * 只接受明确协议的完整链接，前后端分别校验；不猜测协议，也不访问目标。
 * @param value 用户输入或模板 HTML 中的 href；控制字符和 URL 凭据不允许进入邮件。
 * @returns 规范化的 http/https/mailto 地址；无效时返回 null，供调用方保留输入并就地纠错。
 */
export function normalizeMailLink(value: string): string | null {
  if (/[\u0000-\u001f\u007f-\u009f]/u.test(value) || value.includes("\\"))
    return null;
  const trimmed = value.trim();
  if (/\s/u.test(trimmed) || /%(?:0[0-9a-f]|1[0-9a-f]|7f)/iu.test(trimmed))
    return null;
  if (!/^(https?:\/\/|mailto:)/iu.test(trimmed)) return null;
  try {
    const url = new URL(trimmed);
    if (url.protocol === "http:" || url.protocol === "https:") {
      if (!url.hostname || url.username || url.password) return null;
    } else if (url.protocol === "mailto:") {
      if (!/^[^\s<>@,;]+@[^\s<>@,;]+$/u.test(url.pathname)) return null;
      // 邮件链接可含主题，但不允许编码后的 CR/LF 注入额外邮件头。
      if (/%0[ad]/iu.test(trimmed)) return null;
    } else return null;
    return url.href;
  } catch {
    return null;
  }
}

/** 获取节点所在链接，只认当前正文内的节点，避免采用主题/弹窗输入框的选区。 */
function enclosingLink(
  node: Node,
  editor: HTMLElement,
): HTMLAnchorElement | null {
  const element = node instanceof Element ? node : node.parentElement;
  const anchor = element?.closest("a");
  return anchor instanceof HTMLAnchorElement && editor.contains(anchor)
    ? anchor
    : null;
}

/**
 * 固定实时或记住的正文选区；光标在已有链接内时编辑整条链接。
 * 多段落、图片或多个链接不能被一次覆盖，抛出的业务错误由发起处显示，原正文不变。
 * @param editor 当前可编辑正文节点。
 * @param remembered 工具栏获得焦点前记住的选区；无有效选区时默认插入正文末尾。
 * @returns 独立克隆的 Range 和显示字段，不修改 DOM。
 */
export function captureMailLinkSelection(
  editor: HTMLElement,
  remembered: Range | null,
): MailLinkSelection {
  const selection = window.getSelection();
  const live = selection?.rangeCount ? selection.getRangeAt(0) : null;
  let range =
    live && editor.contains(live.commonAncestorContainer)
      ? live.cloneRange()
      : remembered && editor.contains(remembered.commonAncestorContainer)
        ? remembered.cloneRange()
        : null;
  if (!range) {
    range = document.createRange();
    range.selectNodeContents(editor);
    range.collapse(false);
  }
  const start = enclosingLink(range.startContainer, editor);
  const end = enclosingLink(range.endContainer, editor);
  let anchor = start && start === end ? start : null;
  if (!range.collapsed && !anchor) {
    const links = [...editor.querySelectorAll("a")].filter((item) =>
      range.intersectsNode(item),
    );
    // 完整选中单条链接也可编辑；包含相邻文字时不能悄悄改变未选中的内容。
    if (links.length === 1 && range.toString() === links[0].textContent) {
      anchor = links[0];
    } else if (links.length) {
      throw new Error("请单独选择一条链接，或将光标放入链接后再编辑。");
    }
  }
  if (anchor) range.selectNode(anchor);
  const fragment = range.cloneContents();
  if (fragment.querySelector("img,br,p,div,ul,ol,li,blockquote")) {
    throw new Error("请选择同一段落内的文字添加链接，不要包含换行或图片。");
  }
  return {
    range,
    anchor,
    text: anchor?.textContent ?? range.toString(),
    href: anchor?.getAttribute("href") ?? "",
  };
}

/**
 * 插入、替换或移除一条链接；显示文字未改时保留选区里的加粗/斜体等内联格式。
 * @param editor 取得选区时的正文节点；Range 失效时不修改内容。
 * @param context 弹窗打开时的选区快照。
 * @param text 显示文字，空白时使用 href；移除时忽略。
 * @param href 已经 normalizeMailLink 的地址；null 表示只移除链接并保留原有文字及格式。
 * @returns 原生编辑命令是否成功；成功后光标移至链接之外，继续输入不延长链接。
 */
export function applyMailLink(
  editor: HTMLElement,
  context: MailLinkSelection,
  text: string,
  href: string | null,
): boolean {
  if (!editor.contains(context.range.commonAncestorContainer)) return false;
  const container = document.createElement("div");
  const content = context.anchor
    ? (() => {
        const fragment = document.createDocumentFragment();
        fragment.append(
          ...[...context.anchor.childNodes].map((node) => node.cloneNode(true)),
        );
        return fragment;
      })()
    : context.range.cloneContents();
  // cloneContents 仅包含选区内部节点：完全选中 <strong> 中的文字时不会带上 strong。
  // 原生 insertHTML 会规范化周围标签，因此显式补上选区共同继承的内联格式，避免加链接丢加粗。
  let parent = context.anchor
    ? context.anchor.parentElement
    : context.range.commonAncestorContainer instanceof Element
      ? context.range.commonAncestorContainer
      : context.range.commonAncestorContainer.parentElement;
  while (
    parent &&
    parent !== editor &&
    !/^(P|DIV|LI|BLOCKQUOTE)$/u.test(parent.tagName)
  ) {
    if (/^(B|STRONG|I|EM|U)$/u.test(parent.tagName)) {
      const wrapper = document.createElement(parent.tagName.toLowerCase());
      wrapper.append(...Array.from(content.childNodes));
      content.append(wrapper);
    }
    parent = parent.parentElement;
  }
  if (href === null) container.append(content);
  else {
    const anchor = document.createElement("a");
    anchor.href = href;
    anchor.rel = "noopener noreferrer";
    if (text === context.text && text) anchor.append(content);
    else anchor.textContent = text.trim() ? text : href;
    container.append(anchor);
  }
  editor.focus();
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(context.range);
  if (!document.execCommand("insertHTML", false, container.innerHTML))
    return false;
  if (selection?.rangeCount) {
    const caret = selection.getRangeAt(0);
    const insertedAnchor = enclosingLink(caret.endContainer, editor);
    if (insertedAnchor) {
      caret.setStartAfter(insertedAnchor);
      caret.collapse(true);
      selection.removeAllRanges();
      selection.addRange(caret);
    }
    // 清除原生命令残留的链接输入状态，不额外插入空格或零宽字符。
    document.execCommand("unlink");
  }
  return true;
}
