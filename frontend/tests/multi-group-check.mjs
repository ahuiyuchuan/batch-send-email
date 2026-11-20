/** 1920×1080多群端到端界面回归；IPC完全隔离，不访问QQ或SMTP。@author ahui */
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

export async function checkMultiGroup({
  browser,
  installFixture,
  artifacts,
  pageErrors,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, {
    multiGroupScenario: true,
    multiAccountScenario: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (e) => pageErrors.push(e.message));
  const target = (name) =>
    page.getByRole("checkbox", { name: `选择目标群 ${name}`, exact: true });
  const fetch = () =>
    page.getByRole("button", { name: "获取成员", exact: true }).click();
  const calls = (name) =>
    page.evaluate(
      (command) =>
        window.__uiFixture.calls.filter((c) => c.command === command),
      name,
    );
  const snapshot = () => page.evaluate(() => window.__uiFixture.snapshot());
  const complete = () =>
    page.waitForFunction(() => window.__uiFixture.taskStatus() === "completed");
  try {
    await page.goto("http://127.0.0.1:1420");
    await target("目标甲群").check();
    await page.getByLabel("过滤已发送记录", { exact: true }).uncheck();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await page.getByLabel("搜索群名或群号").fill("目标乙");
    await page.getByRole("button", { name: "全选当前群", exact: true }).click();
    assert.equal(await target("目标乙群").isChecked(), true);
    await page.getByLabel("搜索群名或群号").fill("");
    assert.equal(await target("目标甲群").isChecked(), true);
    assert.equal(
      await page
        .locator(".target-group-actions")
        .innerText()
        .then((t) => t.includes("已选 2 个")),
      true,
    );
    const exclusion = page.locator(".exclusion-options");
    assert.equal(await exclusion.getByLabel(/目标甲群|目标乙群/).count(), 0);
    await exclusion.getByLabel(/排除丙群/).check();
    await fetch();
    await complete();
    await page
      .getByRole("tab", { name: "筛选后成员 5", exact: true })
      .waitFor();
    const first = await snapshot();
    assert.equal(first.sourceCount, 6);
    assert.equal(first.duplicateCount, 2);
    assert.equal(new Set(first.members.map((m) => m.qqNumber)).size, 5);
    assert.equal(first.sourceGroups.length, 2);
    assert.deepEqual((await calls("fetch_members")).at(-1).args, {
      groupIds: ["900001", "900002"],
      excludeGroupIds: ["900003"],
      excludeSent: false,
      excludeFriends: false,
    });
    await page.locator(".multi-group-sources > summary").click();
    assert.match(
      await page.locator(".multi-group-sources").innerText(),
      /目标甲群/,
    );
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    assert.equal(
      (await calls("export_members")).at(-1).args.snapshotId,
      first.id,
    );
    // 发布截图展示工作区本身；收起已验证的导出反馈，避免浮层遮挡页首。
    await page.getByRole("button", { name: "关闭提示", exact: true }).click();
    await page.screenshot({
      path: fileURLToPath(new URL("multi-group-members-light.png", artifacts)),
      animations: "disabled",
    });
    // 错误和取消均保留上次结果；等待期间所有目标勾选入口锁定。
    await page.evaluate(() => window.__uiFixture.failNextMultiGroup());
    await fetch();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "failed",
    );
    assert.equal((await snapshot()).id, first.id);
    await page.evaluate(() => window.__uiFixture.holdNextFetch());
    await fetch();
    assert.equal(await target("目标甲群").isDisabled(), true);
    await page.getByRole("button", { name: "取消获取", exact: true }).click();
    await page.evaluate(() => window.__uiFixture.releaseFetch());
    assert.equal((await snapshot()).id, first.id);
    await page.reload();
    await target("目标乙群").check();
    await target("目标甲群").check();
    await page
      .getByRole("tab", { name: "筛选后成员 5", exact: true })
      .waitFor();
    assert.equal((await calls("fetch_members")).length, 0);
    await page
      .getByRole("button", { name: "给全部 5 人写信", exact: true })
      .click();
    await page.locator(".mail-audience-details summary").click();
    assert.match(
      await page.locator(".mail-audience-details").innerText(),
      /目标甲群.*900001/,
    );
    await page.getByLabel("主题", { exact: true }).fill("多群名单回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("只使用合成成员，不连接SMTP。");
    await page.locator("#mail-recipients").fill("10000001@qq.com");
    await page
      .locator(".mail-recipient-boundary")
      .filter({ hasText: /本群已过滤或未选成员/ })
      .waitFor();
    assert.equal((await calls("start_mail_job")).length, 0);
    await page.locator("#mail-recipients").fill("");
    await page.getByRole("button", { name: "调整群成员", exact: true }).click();
    assert.equal(await target("目标甲群").isChecked(), true);
    assert.equal(await target("目标乙群").isChecked(), true);
    await page
      .getByRole("button", { name: "给全部 5 人写信", exact: true })
      .click();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "确认发送", exact: true });
    await dialog.waitFor();
    assert.match(await dialog.innerText(), /目标甲群/);
    assert.match(await dialog.innerText(), /目标乙群/);
    const preparation = (await calls("prepare_mail_recipients")).at(-1).args
      .input;
    assert.equal(preparation.groupAudience.qqNumbers.length, 5);
    assert.equal(new Set(preparation.groupAudience.qqNumbers).size, 5);
    await page.screenshot({
      path: fileURLToPath(new URL("multi-group-confirm-light.png", artifacts)),
      animations: "disabled",
    });
    const footer = await dialog.locator(".dialog-footer").boundingBox();
    assert.ok(footer.y + footer.height <= 1080);
    await dialog.getByRole("button", { name: "返回修改", exact: true }).click();
    await page.emulateMedia({ colorScheme: "dark" });
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await dialog.waitFor();
    await page.screenshot({
      path: fileURLToPath(new URL("multi-group-confirm-dark.png", artifacts)),
      animations: "disabled",
    });
    await dialog.getByRole("button", { name: "确认发送", exact: true }).click();
    assert.equal((await calls("start_mail_job")).length, 1);
    assert.equal(
      (await calls("start_mail_job")).at(-1).args.draft.groupAudience
        .snapshotId,
      first.id,
    );
    console.log(
      "1920×1080 multi-group select/search/bulk, dedup/source summaries, exclusions, persisted combination, export, failed/cancelled old-result preservation and fixed mail scope passed (synthetic only)",
    );
  } finally {
    await context.close();
  }
  await checkLargeCombinedList({ browser, installFixture, pageErrors });
}

/** 超过单次发信上限仍可导出/查看；发送状态分块完整读取，发信明确要求选择部分成员。 */
async function checkLargeCombinedList({ browser, installFixture, pageErrors }) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
  });
  await context.addInitScript(installFixture, {
    multiGroupScenario: "large",
    initialPasswordSaved: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(20000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .getByRole("checkbox", { name: "选择目标群 目标甲群", exact: true })
      .check();
    await page
      .getByRole("checkbox", { name: "选择目标群 目标乙群", exact: true })
      .check();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    const compose = page.getByRole("button", {
      name: "给全部 10005 人写信",
      exact: true,
    });
    await compose.waitFor();
    await page.waitForFunction(
      () =>
        document.querySelector(".export-bar button") &&
        window.__uiFixture.calls.filter(
          (c) => c.command === "get_member_mail_status",
        ).length === 2,
    );
    assert.deepEqual(
      await page.evaluate(() =>
        window.__uiFixture.calls
          .filter((c) => c.command === "get_member_mail_status")
          .map((c) => c.args.qqNumbers.length),
      ),
      [10000, 5],
    );
    await compose.click();
    await page
      .getByText(
        "每次最多给10000人写信，请在成员列表勾选部分成员后再操作；完整名单仍可导出。",
        { exact: true },
      )
      .waitFor();
    assert.equal(await page.locator(".main-content").isVisible(), true);
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter((c) => c.command === "export_members")
            .length,
      ),
      1,
    );
    console.log(
      "1920×1080 combined 10005 members: complete status reads in 10000+5 chunks, clear send limit and full export passed",
    );
  } finally {
    await context.close();
  }
}
