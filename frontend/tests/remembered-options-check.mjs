/** 确认前动态时间、设置重载和账号隔离回归；1920×1080，完全隔离QQ/SMTP。@author ahui */
import assert from "node:assert/strict";

export async function checkRememberedOptions({
  browser,
  installFixture,
  artifacts,
  pageErrors,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const selectTarget = () =>
    page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
  const writeMail = () =>
    page.getByRole("button", { name: "写邮件", exact: true }).click();
  try {
    await page.goto("http://127.0.0.1:1420");
    await selectTarget();
    await page.getByLabel("过滤已发送记录", { exact: true }).uncheck();
    await page.getByLabel("排除我的 QQ 好友", { exact: true }).check();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .check();
    await writeMail();
    await page.locator(".mail-delivery-settings > summary").click();
    await page.getByLabel("跳过已发送", { exact: true }).uncheck();
    await page.getByLabel("失败后继续发送", { exact: true }).check();
    await page.getByLabel("每封收件人数", { exact: true }).fill("23");
    await page.getByLabel("同账号发送间隔秒数", { exact: true }).fill("750");
    await page.getByLabel("账号切换间隔秒数", { exact: true }).fill("45");
    await page
      .getByLabel("发送方式", { exact: true })
      .selectOption("individual");
    // 重载模拟重新打开软件，发送方式隐藏每封人数时仍应保留该字段的上次值。
    await page.reload();
    await selectTarget();
    assert.equal(
      await page.getByLabel("过滤已发送记录", { exact: true }).isChecked(),
      false,
    );
    assert.equal(
      await page.getByLabel("排除我的 QQ 好友", { exact: true }).isChecked(),
      true,
    );
    assert.equal(
      await page
        .locator(".exclusion-options")
        .getByLabel(/排除测试群/)
        .isChecked(),
      true,
    );
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );
    const fetch = (await calls("fetch_members")).at(-1).args;
    assert.equal(fetch.excludeFriends, true);
    assert.equal(fetch.excludeSent, false);
    assert.deepEqual(fetch.excludeGroupIds, ["900002"]);
    // 旧快照恢复不能覆盖刚保存的筛选；新账号缺省、返回原账号恢复各自选择。
    await page.evaluate(() => window.__uiFixture.switchAccount("99887766"));
    await selectTarget();
    assert.equal(
      await page.getByLabel("排除我的 QQ 好友", { exact: true }).isChecked(),
      false,
    );
    assert.equal(
      await page.getByLabel("过滤已发送记录", { exact: true }).isChecked(),
      true,
    );
    const binding = await page.evaluate(() =>
      Object.keys(localStorage).find((key) =>
        key.startsWith("qq-group-exporter-list-filter-v1:"),
      ),
    );
    const originalQQ = decodeURIComponent(binding.split("-v1:")[1]).split(
      ":",
    )[1];
    await page.evaluate(
      (qq) => window.__uiFixture.switchAccount(qq),
      originalQQ,
    );
    await selectTarget();
    assert.equal(
      await page.getByLabel("排除我的 QQ 好友", { exact: true }).isChecked(),
      true,
    );
    assert.equal(
      await page
        .locator(".exclusion-options")
        .getByLabel(/排除测试群/)
        .isChecked(),
      true,
    );
    await writeMail();
    assert.equal(
      await page.locator(".mail-delivery-settings").getAttribute("open"),
      "",
    );
    assert.equal(
      await page.getByLabel("跳过已发送", { exact: true }).isChecked(),
      false,
    );
    assert.equal(
      await page.getByLabel("失败后继续发送", { exact: true }).isChecked(),
      true,
    );
    assert.equal(
      await page.getByLabel("发送方式", { exact: true }).inputValue(),
      "individual",
    );
    assert.equal(
      await page.getByLabel("同账号发送间隔秒数", { exact: true }).inputValue(),
      "750",
    );
    assert.equal(
      await page.getByLabel("账号切换间隔秒数", { exact: true }).inputValue(),
      "45",
    );
    await page.getByLabel("发送方式", { exact: true }).selectOption("bcc");
    assert.equal(
      await page.getByLabel("每封收件人数", { exact: true }).inputValue(),
      "23",
    );
    await page.getByRole("button", { name: "快捷发送", exact: true }).click();
    await page
      .locator("#mail-recipients")
      .fill(
        "preview-a@example.test;preview-b@example.test;preview-c@example.test",
      );
    await page.getByLabel("主题", { exact: true }).fill("确认前结束时间回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("本机隔离回归，不连接SMTP。");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "确认发送", exact: true });
    const time = dialog.locator(".mail-confirm-estimate time");
    await time.waitFor();
    const first = await time.getAttribute("datetime");
    assert.match(
      await time.innerText(),
      /^\d{4}年\d{2}月\d{2}日\d{2}:\d{2}:\d{2}$/,
    );
    await page.waitForFunction(
      (old) =>
        document
          .querySelector(".mail-confirm-estimate time")
          ?.getAttribute("datetime") !== old,
      first,
    );
    assert.equal((await calls("prepare_mail_recipients")).length, 1);
    assert.equal((await calls("start_mail_job")).length, 0);
    const footer = await dialog.locator(".dialog-footer").boundingBox();
    const deadlineBox = await time.boundingBox();
    assert.ok(footer.y + footer.height <= 1080 && deadlineBox.y < footer.y);
    await page.screenshot({
      animations: "disabled",
      path: new URL(
        "remember-options-confirm-light.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await dialog.getByRole("button", { name: "返回修改", exact: true }).click();
    const countAfterClose = (await calls("estimate_prepared_mail_completion"))
      .length;
    await page.waitForTimeout(1200);
    assert.equal(
      (await calls("estimate_prepared_mail_completion")).length,
      countAfterClose,
    );
    // 同一业务弹窗在深色主题下复核文字、列线与固定底部，无需额外屏幕尺寸。
    await page.emulateMedia({ colorScheme: "dark" });
    // 跳过五分钟只用于隔离浏览器时钟；确认到期必须关闭且不能产生真实发送命令。
    await page.clock.install();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await time.waitFor();
    await page.screenshot({
      animations: "disabled",
      path: new URL(
        "remember-options-confirm-dark.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await page.clock.fastForward(300_001);
    await dialog.waitFor({ state: "hidden" });
    assert.equal((await calls("start_mail_job")).length, 0);
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await time.waitFor();
    await dialog.getByRole("button", { name: "确认发送", exact: true }).click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const countAfterSend = (await calls("estimate_prepared_mail_completion"))
      .length;
    await page.waitForTimeout(1200);
    assert.equal(
      (await calls("estimate_prepared_mail_completion")).length,
      countAfterSend,
    );
    assert.equal((await calls("start_mail_job")).length, 1);
    console.log(
      "1920×1080 dynamic confirmation ETA, persisted send/filter options and QQ isolation passed",
    );
  } finally {
    await context.close();
  }
}
