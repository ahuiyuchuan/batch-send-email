/**
 * 文本链接的 1920×1080 渲染回归；只使用合成 IPC，不登录 QQ、不连接 SMTP 或链接目标。
 * 覆盖用户操作与最终 IPC HTML，Rust 测试另行负责真实 MIME 和后台清理边界。
 * @author ahui
 */
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

/**
 * 沿用隔离宿主验证插入、编辑、移除、撤销、模板往返及非导航预览。
 * @param options 既有 UI harness 提供的浏览器、夹具、截图目录和错误收集器。
 * @returns 所有断言完成后的 Promise；任何业务或布局断言失败均直接向 harness 抛出。
 */
export async function checkMailLinks({
  browser,
  installFixture,
  artifacts,
  pageErrors,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: "light",
  });
  // 恶意 HTML 样例会短暂进入编辑 DOM；在网络层阻断外站，不能让合成图片产生真实请求。
  await context.route(/^https?:/, (route) => {
    const url = new URL(route.request().url());
    return url.hostname === "127.0.0.1" ? route.continue() : route.abort();
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const editor = page.getByRole("textbox", { name: "邮件正文", exact: true });
  const linkDialog = page.getByRole("dialog", { name: /^(插入|编辑)链接$/ });
  const linkButton = page.getByRole("button", { name: /^插入链接/ });
  const address = () => linkDialog.getByLabel("链接地址", { exact: true });
  const displayText = () => linkDialog.getByLabel("显示文字", { exact: true });
  const confirmLink = () =>
    linkDialog.getByRole("button", { name: /^(插入链接|保存修改)$/ });
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((call) => call.command === name),
      command,
    );

  /** 设置受控测试原文；不走粘贴，避免把 HTML 粘贴策略与链接编辑混成同一项测试。 */
  const setBody = async (html) => {
    await editor.evaluate((element, value) => {
      element.innerHTML = value;
      element.dispatchEvent(new InputEvent("input", { bubbles: true }));
      element.focus();
    }, html);
  };

  /** 根据可读文字定位选区，不依赖富文本中间生成的标签结构或节点下标。 */
  const selectBodyText = async (text, collapsed = false) => {
    await editor.evaluate(
      (element, input) => {
        const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
        let node;
        let offset = 0;
        const nodes = [];
        while ((node = walker.nextNode())) {
          nodes.push({ node, start: offset, end: offset + node.length });
          offset += node.length;
        }
        const begin = element.textContent.indexOf(input.text);
        if (begin < 0) throw new Error("Synthetic selection text is missing");
        const end = begin + input.text.length;
        const first = nodes.find((item) => item.end > begin);
        const last = nodes.find((item) => item.end >= end);
        if (!first || !last) throw new Error("Synthetic selection is empty");
        const range = document.createRange();
        range.setStart(first.node, begin - first.start);
        range.setEnd(last.node, end - last.start);
        if (input.collapsed) range.collapse(true);
        element.focus();
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        element.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
      },
      { text, collapsed },
    );
  };
  const openLink = async (shortcut = false) => {
    if (shortcut) await editor.press("Control+k");
    else await linkButton.click();
    await linkDialog.waitFor();
  };
  const cancelLink = async (keyboard = false) => {
    const cancel = linkDialog.getByRole("button", {
      name: "取消",
      exact: true,
    });
    if (keyboard) await cancel.press("Enter");
    else await cancel.click();
    await linkDialog.waitFor({ state: "hidden" });
  };
  // Chromium 的原生编辑命令可把 strong 正规化成 b；检查可见格式，避免绑定实现标签。
  const assertBoldText = async () => {
    assert.equal(
      await editor.evaluate((element) => {
        const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
        let node;
        while ((node = walker.nextNode())) {
          if (node.textContent.includes("查看详情")) {
            return (
              Number(getComputedStyle(node.parentElement).fontWeight) >= 600
            );
          }
        }
        return false;
      }),
      true,
      `链接操作须保留选中文字的粗体；实际 HTML：${await editor.innerHTML()}`,
    );
  };
  const assertEditorFocus = async () => {
    // 组件在 requestAnimationFrame 中恢复选区，需等原生 dialog 完成自动焦点恢复后再断言。
    await page.waitForFunction(
      () => document.activeElement?.getAttribute("aria-label") === "邮件正文",
    );
    assert.equal(
      await editor.evaluate((element) => document.activeElement === element),
      true,
      "关闭链接对话框后应回到正文继续编辑",
    );
  };
  const assertDialogLayout = async (theme) => {
    const bounds = await linkDialog.boundingBox();
    assert.ok(bounds.x >= 0 && bounds.y >= 0);
    assert.ok(bounds.x + bounds.width <= 1920);
    assert.ok(bounds.y + bounds.height <= 1080);
    assert.equal(
      await linkDialog.evaluate(
        (element) => element.scrollWidth <= element.clientWidth + 1,
      ),
      true,
      "链接对话框不应出现横向滚动",
    );
    const footer = await linkDialog.locator(".dialog-footer").boundingBox();
    assert.ok(
      footer.y >= bounds.y &&
        footer.y + footer.height <= bounds.y + bounds.height + 1,
    );
    await page.screenshot({
      path: fileURLToPath(new URL(`mail-links-${theme}.png`, artifacts)),
      animations: "disabled",
    });
  };

  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    await page.getByRole("button", { name: "快捷发送", exact: true }).click();
    await page.locator("#mail-recipients").fill("link-recipient@example.test");
    const subject = page.getByLabel("主题", { exact: true });
    await subject.fill("文本链接回归");
    await editor.waitFor({ state: "visible" });

    // 主题输入的 Ctrl+K 不能借用上一次正文选区或误开正文链接操作。
    await subject.press("Control+k");
    assert.equal(await linkDialog.isVisible(), false);
    await setBody("<p>前文 <strong>查看详情</strong> 后文</p>");
    await selectBodyText("查看详情");
    await openLink();
    assert.equal(await displayText().inputValue(), "查看详情");
    const beforeInvalid = await editor.innerHTML();
    for (const invalid of [
      "javascript:alert(1)",
      "data:text/html,test",
      "file:///C:/private.txt",
      "cid:private",
      "/relative/path",
      "https://user:password@example.test",
      "https://example.test/white space",
      "https://example.test/path%0d%0aheader",
    ]) {
      await address().fill(invalid);
      await confirmLink().click();
      assert.equal(await linkDialog.isVisible(), true);
      assert.equal(await address().inputValue(), invalid);
      assert.equal(await address().getAttribute("aria-invalid"), "true");
      assert.equal(await editor.innerHTML(), beforeInvalid);
    }
    const target = "https://example.test/资料?from=邮件&mode=full#详情";
    const normalizedTarget = new URL(target).href;
    await address().fill(target);
    await address().dispatchEvent("keydown", {
      key: "Enter",
      code: "Enter",
      isComposing: true,
    });
    assert.equal(
      await linkDialog.isVisible(),
      true,
      "输入法确认不能误提交链接",
    );
    await assertDialogLayout("light");
    await confirmLink().click();
    await linkDialog.waitFor({ state: "hidden" });
    await assertEditorFocus();
    assert.equal(await editor.locator("a").innerText(), "查看详情");
    assert.equal(
      await editor.locator("a").getAttribute("href"),
      normalizedTarget,
    );
    await assertBoldText();
    await page.getByRole("button", { name: "撤销", exact: true }).click();
    assert.equal(
      await editor.locator("a").count(),
      0,
      "链接插入应进入原生撤销栈",
    );
    await assertBoldText();

    // 取消保留正文并恢复选区，下一次键入替换刚才选中的文字。
    await selectBodyText("查看详情");
    await openLink(true);
    await address().fill("https://cancel.example.test");
    const beforeCancel = await editor.innerHTML();
    await cancelLink(true);
    await assertEditorFocus();
    assert.equal(await editor.innerHTML(), beforeCancel);
    assert.equal(
      await page.evaluate(() => window.getSelection()?.toString()),
      "查看详情",
    );
    await openLink();
    await address().press("Escape");
    await linkDialog.waitFor({ state: "hidden" });
    await assertEditorFocus();
    assert.equal(await editor.innerHTML(), beforeCancel);

    // 光标插入时显示文字可留空；之后正常输入不会把新文字带入链接。
    await editor.fill("链接前文 ");
    await editor.press("Control+End");
    await openLink();
    await displayText().fill("");
    const bareTarget = "https://example.test/direct?a=1&b=2";
    await address().fill(bareTarget);
    await confirmLink().click();
    await linkDialog.waitFor({ state: "hidden" });
    assert.equal(await editor.locator("a").innerText(), bareTarget);
    await page.keyboard.type(" 普通后文");
    assert.equal(await editor.locator("a").innerText(), bareTarget);
    assert.match(await editor.innerText(), /普通后文/);

    // 光标位于已有链接内部即可编辑；移除只去链接，不删除显示文字和原有格式。
    await setBody(
      '<p>前文 <a href="https://example.test/old"><strong>查看详情</strong></a> 后文</p>',
    );
    await selectBodyText("查看详情", true);
    await openLink(true);
    assert.equal(await displayText().inputValue(), "查看详情");
    assert.equal(await address().inputValue(), "https://example.test/old");
    await address().fill(target);
    await page.emulateMedia({ colorScheme: "dark" });
    await assertDialogLayout("dark");
    await confirmLink().click();
    await linkDialog.waitFor({ state: "hidden" });
    assert.equal(
      await editor.locator("a").getAttribute("href"),
      normalizedTarget,
    );
    await assertBoldText();
    await selectBodyText("查看详情", true);
    await openLink();
    await linkDialog
      .getByRole("button", { name: "移除链接", exact: true })
      .press("Enter");
    await linkDialog.waitFor({ state: "hidden" });
    assert.equal(await editor.locator("a").count(), 0);
    await assertBoldText();
    await editor.press("Control+z");
    assert.equal(
      await editor.locator("a").getAttribute("href"),
      normalizedTarget,
    );

    // 跨段、图片及多链接混合选区不应该被静默折叠成一段或丢掉图片。
    for (const [html, selected] of [
      ["<p>第一段</p><p>第二段</p>", "第一段第二段"],
      [
        '<p>左<a href="https://a.example.test">甲</a>中<a href="https://b.example.test">乙</a>右</p>',
        "甲中乙",
      ],
      [
        '<p>左<img alt="合成图片" src="data:image/png;base64,iVBORw0KGgo=">右</p>',
        "左右",
      ],
    ]) {
      await setBody(html);
      await selectBodyText(selected);
      const before = await editor.innerHTML();
      await linkButton.click();
      // 可在打开前拒绝，或在弹窗中反馈；两种方式均需保留原正文且允许取消。
      if (await linkDialog.isVisible()) {
        await address().fill("https://example.test/mixed");
        await confirmLink().click();
        assert.equal(await linkDialog.isVisible(), true);
        await cancelLink();
      }
      assert.equal(await editor.innerHTML(), before);
    }
    // 前面的拒绝混选属于故意触发的校验提示；关闭后再拍预览，避免旧提示遮住验收界面。
    await page
      .locator("[data-sonner-toast] [data-close-button]")
      .evaluateAll((buttons) => buttons.forEach((button) => button.click()));

    // 清理器在预览、保存及发送入口统一保留安全链接，并继续丢弃脚本与任意外部图片。
    await setBody(
      '<p><a href="https://example.test/details?a=1&amp;b=2" onclick="alert(1)">查看详情</a> <a href="javascript:alert(1)">危险链接文字</a></p><script>window.__mailLinkInjected=true</script><img src="https://example.test/tracker.png">',
    );
    await page.getByRole("button", { name: "预览", exact: true }).click();
    const preview = page.getByRole("dialog", { name: "正文预览", exact: true });
    await preview.waitFor();
    const iframe = preview.locator("iframe");
    assert.equal(await iframe.getAttribute("sandbox"), "");
    const frame = preview.frameLocator("iframe");
    await frame.getByText("查看详情", { exact: true }).waitFor();
    assert.equal(await frame.locator("script,img").count(), 0);
    assert.equal(await frame.locator("[onclick]").count(), 0);
    assert.equal(
      await frame.locator("a[href]").count(),
      0,
      "安全预览不能通过链接导航 iframe",
    );
    assert.ok(
      (await frame.locator("body").innerText()).includes(
        "https://example.test/details?a=1&b=2",
      ),
    );
    assert.equal(
      await page.evaluate(() => window.__mailLinkInjected),
      undefined,
    );
    await page.screenshot({
      path: fileURLToPath(new URL("mail-links-preview.png", artifacts)),
      animations: "disabled",
    });
    await preview
      .getByRole("button", { name: "关闭正文预览", exact: true })
      .click();
    await page.getByRole("button", { name: "保存为模板", exact: true }).click();
    const save = page.getByRole("dialog", {
      name: "保存邮件模板",
      exact: true,
    });
    await save.getByLabel("模板名称", { exact: true }).fill("文本链接往返模板");
    await save.getByRole("button", { name: "保存模板", exact: true }).click();
    await save.waitFor({ state: "hidden" });
    const savedHtml = (await calls("mail_template_save")).at(-1).args.input
      .html;
    assert.ok(
      savedHtml.includes('href="https://example.test/details?a=1&amp;b=2"'),
    );
    assert.doesNotMatch(savedHtml, /javascript:|onclick|<script|<img/);
    await editor.fill("临时草稿");
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library
      .locator(".mail-template-item")
      .filter({ hasText: "文本链接往返模板" })
      .click();
    await library
      .getByRole("heading", { name: "文本链接往返模板", exact: true })
      .waitFor();
    await library
      .frameLocator("iframe")
      .getByText("查看详情", { exact: true })
      .waitFor();
    assert.equal(
      await library.frameLocator("iframe").locator("a[href]").count(),
      0,
    );
    await library
      .getByRole("button", { name: "导出当前模板", exact: true })
      .click();
    await page.waitForFunction(
      () => window.__uiFixture.exportedTemplatePackages().length === 1,
    );
    const exported = await page.evaluate(
      () => window.__uiFixture.exportedTemplatePackages()[0].templates[0],
    );
    assert.equal(exported.html, savedHtml, "模板导出不应丢失链接目标");
    await page.evaluate((template) => {
      window.__uiFixture.setTemplateImportPackage([
        { ...template, name: "文本链接导入模板" },
      ]);
    }, exported);
    await library
      .getByRole("button", { name: "导入模板", exact: true })
      .click();
    const importing = page.getByRole("dialog", {
      name: "导入邮件模板",
      exact: true,
    });
    await importing
      .getByRole("button", { name: "确认导入", exact: true })
      .click();
    await importing.waitFor({ state: "hidden" });
    const imported = await page.evaluate(() =>
      window.__uiFixture
        .savedTemplates()
        .find((template) => template.name === "文本链接导入模板"),
    );
    assert.equal(imported.html, savedHtml, "模板导入不应丢失链接目标");
    await library
      .locator(".mail-template-item")
      .filter({ hasText: "文本链接导入模板" })
      .click();
    await library
      .getByRole("heading", { name: "文本链接导入模板", exact: true })
      .waitFor();
    await library
      .getByRole("button", { name: "使用此模板", exact: true })
      .click();
    const replace = page.getByRole("dialog", {
      name: "替换邮件内容",
      exact: true,
    });
    await replace
      .getByRole("button", { name: "替换内容", exact: true })
      .click();
    await library.waitFor({ state: "hidden" });
    assert.equal(
      await editor.locator("a").getAttribute("href"),
      "https://example.test/details?a=1&b=2",
    );

    // 最后经过真实页面的确认流程，检查送入原生宿主的 HTML；fixture 绝不实际投递。
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const confirmation = page.getByRole("dialog", {
      name: "确认发送",
      exact: true,
    });
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page.waitForFunction(() =>
      window.__uiFixture.calls.some(
        (call) => call.command === "start_mail_job",
      ),
    );
    const sentHtml = (await calls("start_mail_job")).at(-1).args.draft.html;
    assert.ok(
      sentHtml.includes('href="https://example.test/details?a=1&amp;b=2"'),
    );
    assert.doesNotMatch(sentHtml, /javascript:|onclick|<script|<img/);
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    console.log(
      "1920×1080 links: selected/caret insert, URL validation, IME, cancel/focus, edit/remove/undo, mixed-selection rejection, non-navigation preview, safe template roundtrip and synthetic send HTML passed; light/dark captured",
    );
  } finally {
    await context.close();
  }
}
