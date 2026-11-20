/** 邮件链接编辑弹窗；只收集文本和地址，正文修改与选区恢复由写信页统一管理。@author ahui */
import { useLayoutEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import DialogLayout from "./DialogLayout";
import { normalizeMailLink, type MailLinkSelection } from "../lib/mail-links";

interface Props {
  context: MailLinkSelection;
  onApply: (text: string, href: string | null) => boolean;
  onCancel: () => void;
}

/**
 * 原生模态框隔离键盘焦点；Enter 提交、Esc 取消，中文输入法确认候选时不提交。
 * 无效地址保留原值并聚焦错误字段；错误不占据全局通知，也不修改正文。
 */
export default function MailLinkDialog({ context, onApply, onCancel }: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const address = useRef<HTMLInputElement>(null);
  const submitting = useRef(false);
  const [text, setText] = useState(context.text);
  const [href, setHref] = useState(context.href);
  const [error, setError] = useState("");
  const editing = Boolean(context.anchor);

  useLayoutEffect(() => {
    dialog.current?.showModal();
    address.current?.focus();
    address.current?.select();
  }, []);

  /** 同步门闩防止双击重复插入；父层失败时仍保留当前输入，供用户复制或取消。 */
  function submit(remove = false) {
    if (submitting.current) return;
    const normalized = remove ? null : normalizeMailLink(href);
    if (!remove && !normalized) {
      setError(
        "请输入完整的 http://、https:// 或 mailto: 链接，不能包含账号密码或控制字符。",
      );
      address.current?.focus();
      return;
    }
    submitting.current = true;
    // 模态框打开时正文为 inert，必须先同步关闭，原生编辑命令才能恢复正文选区和撤销栈。
    dialog.current?.close();
    if (!onApply(text, normalized)) {
      submitting.current = false;
      setError("正文位置已变化，请取消后重新选择文字。");
      dialog.current?.showModal();
      address.current?.focus();
    }
  }

  return (
    <dialog
      ref={dialog}
      className="mail-dialog mail-link-dialog dialog-shell"
      aria-labelledby="mail-link-heading"
      onCancel={(event) => {
        event.preventDefault();
        onCancel();
      }}
      onKeyDown={(event) => {
        if (
          event.key === "Enter" &&
          event.target instanceof HTMLInputElement &&
          !event.nativeEvent.isComposing &&
          event.keyCode !== 229
        ) {
          event.preventDefault();
          submit();
        }
      }}
    >
      <DialogLayout
        heading={
          <div className="mail-dialog-heading">
            <h2 id="mail-link-heading">{editing ? "编辑链接" : "插入链接"}</h2>
            <button
              type="button"
              className="icon-button"
              aria-label="关闭链接编辑"
              onClick={onCancel}
            >
              <X size={18} />
            </button>
          </div>
        }
        footer={
          <div className="mail-dialog-actions dialog-footer">
            {editing && (
              <button
                type="button"
                className="button mail-link-remove"
                onClick={() => submit(true)}
              >
                移除链接
              </button>
            )}
            <button type="button" className="button" onClick={onCancel}>
              取消
            </button>
            <button
              type="button"
              className="button primary"
              onClick={() => submit()}
            >
              {editing ? "保存修改" : "插入链接"}
            </button>
          </div>
        }
      >
        <div className="mail-link-fields">
          <label>
            显示文字
            <input
              value={text}
              maxLength={10000}
              placeholder="留空时显示链接地址"
              onChange={(event) => setText(event.target.value)}
            />
          </label>
          <label>
            链接地址
            <input
              ref={address}
              type="text"
              inputMode="url"
              spellCheck={false}
              autoComplete="off"
              value={href}
              aria-invalid={Boolean(error)}
              aria-describedby={error ? "mail-link-error" : undefined}
              placeholder="https://example.com"
              onChange={(event) => {
                setHref(event.target.value);
                setError("");
              }}
            />
          </label>
          {error && (
            <p id="mail-link-error" className="mail-field-error" role="alert">
              {error}
            </p>
          )}
        </div>
      </DialogLayout>
    </dialog>
  );
}
