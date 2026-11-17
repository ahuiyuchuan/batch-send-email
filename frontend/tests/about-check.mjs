/** 关于与更新1920×1080回归：所有GitHub/系统浏览器调用均为隔离合成IPC。@author ahui */
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

export async function checkAbout({
  browser,
  installFixture,
  artifacts,
  pageErrors,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const about = page.locator(".about-page");
  const navigate = (label) =>
    page
      .locator(".app-nav")
      .getByRole("button", { name: label, exact: true })
      .click();
  const result = (
    status,
    version = "1.1.0",
    notes = "",
    checkedAt = new Date().toISOString(),
  ) => ({
    status,
    currentVersion: "1.0.0",
    latestVersion: status === "no-release" ? null : version,
    releaseUrl: `https://github.com/ahuiyuchuan/batch-send-email/releases/tag/v${version}`,
    notes,
    checkedAt,
  });
  try {
    await page.goto("http://127.0.0.1:1420");
    await navigate("写邮件");
    await page.getByLabel("主题", { exact: true }).fill("关于页切换保留草稿");
    await navigate("关于");
    await about.getByRole("heading", { name: "关于", exact: true }).waitFor();
    assert.equal(
      await about.getByRole("button", { name: /前往下载/ }).count(),
      0,
    );
    const links = [
      ["GitHub 项目", ""],
      ["发布记录", "/releases"],
      ["问题反馈", "/issues"],
    ];
    for (const [label, suffix] of links) {
      await about.getByRole("button", { name: label, exact: true }).click();
      assert.equal(
        await page.evaluate(
          () =>
            window.__uiFixture.calls
              .filter((c) => c.command === "open_external_url")
              .at(-1).args.url,
        ),
        `https://github.com/ahuiyuchuan/batch-send-email${suffix}`,
      );
    }
    // 保留原有纵向卡片层级，只去掉两个许可按钮；三个项目入口及固定更新操作继续可用。
    for (const label of ["开源协议", "许可声明"]) {
      assert.equal(
        await about.getByRole("button", { name: label, exact: true }).count(),
        0,
      );
    }
    assert.equal(await about.locator(".about-links button").count(), 3);
    const identityCard = await about
      .getByRole("region", { name: "软件信息", exact: true })
      .boundingBox();
    const updateCard = await about
      .getByRole("region", { name: "版本更新", exact: true })
      .boundingBox();
    const projectCard = await about
      .getByRole("region", { name: "项目与反馈", exact: true })
      .boundingBox();
    assert.ok(updateCard.y >= identityCard.y + identityCard.height + 15);
    assert.ok(projectCard.y >= updateCard.y + updateCard.height + 15);
    assert.ok(Math.abs(updateCard.x - projectCard.x) < 1);
    assert.ok(Math.abs(updateCard.width - projectCard.width) < 1);
    assert.equal(
      await about
        .getByRole("region", { name: "本版功能", exact: true })
        .locator("li")
        .count(),
      6,
    );
    await page.screenshot({
      path: fileURLToPath(new URL("about-light.png", artifacts)),
      animations: "disabled",
    });
    const fixed = await about.locator(".about-footer").boundingBox();
    assert.ok(fixed.y + fixed.height <= 1080 && fixed.x >= 0);
    await page.evaluate(() => window.__uiFixture.holdNextUpdate());
    const check = about.getByRole("button", { name: "检查更新", exact: true });
    await check.dblclick({ delay: 30 });
    assert.equal(
      await about
        .getByRole("button", { name: "正在检查…", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter((c) => c.command === "check_updates")
            .length,
      ),
      1,
    );
    // 正在检查时可离开页面，返回仍保留状态；不会清空邮件内容。
    await navigate("写邮件");
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "关于页切换保留草稿",
    );
    await navigate("关于");
    await page.evaluate(() => window.__uiFixture.releaseUpdate());
    await about.getByText("当前已是最新正式版本。", { exact: true }).waitFor();
    await page.evaluate(() => window.__uiFixture.failNextUpdate());
    await check.click();
    await about
      .getByRole("alert")
      .filter({ hasText: "检查更新失败" })
      .waitFor();
    assert.equal(
      await about.getByText("当前已是最新正式版本。", { exact: true }).count(),
      0,
    );
    await check.click();
    await about.getByText("当前已是最新正式版本。", { exact: true }).waitFor();
    await page.evaluate(
      (value) => window.__uiFixture.setUpdateResult(value),
      result("no-release"),
    );
    await check.click();
    await about.getByText("项目尚无正式发布版本。", { exact: true }).waitFor();
    // 公开正文仅按纯文本展示，长文本不推走页脚，新版发现点同步到主导航。
    const available = result(
      "available",
      "1.1.0",
      "<script>window.__aboutInjected=true</script>\n" +
        "更新说明\n".repeat(180),
    );
    await page.evaluate(
      (value) => window.__uiFixture.publishUpdate(value),
      available,
    );
    await about.getByText("发现新版本 v1.1.0", { exact: true }).waitFor();
    assert.equal(await page.locator(".nav-update-dot").count(), 1);
    assert.equal(await page.evaluate(() => window.__aboutInjected), undefined);
    await about
      .getByRole("button", { name: "前往下载 v1.1.0", exact: true })
      .click();
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls
            .filter((c) => c.command === "open_external_url")
            .at(-1).args.url,
      ),
      available.releaseUrl,
    );
    const notes = await about.locator(".about-release-notes").boundingBox();
    assert.ok(notes.height <= 181);
    const footer = await about.locator(".about-footer").boundingBox();
    assert.ok(Math.abs(footer.y - fixed.y) < 1);
    // 背景事件比旧手动响应新时，响应不得回退新版本发现。
    await page.evaluate(() => window.__uiFixture.holdNextUpdate());
    await check.click();
    await page.evaluate(
      (value) => window.__uiFixture.publishUpdate(value),
      result(
        "available",
        "1.2.0",
        "后台获得的新版本",
        new Date(Date.now() + 1000).toISOString(),
      ),
    );
    await page.evaluate(
      (value) => window.__uiFixture.setUpdateResult(value),
      result("current", "1.0.0", "", new Date(Date.now() - 1000).toISOString()),
    );
    await page.evaluate(() => window.__uiFixture.releaseUpdate());
    await about.getByText("发现新版本 v1.2.0", { exact: true }).waitFor();
    await page.emulateMedia({ colorScheme: "dark" });
    await page.screenshot({
      path: fileURLToPath(new URL("about-dark.png", artifacts)),
      animations: "disabled",
    });
    await navigate("成员提取");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );
    await page.emulateMedia({ colorScheme: "light" });
    await page.screenshot({
      path: fileURLToPath(new URL("release-members-light.png", artifacts)),
      animations: "disabled",
    });
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    console.log(
      "1920×1080 About: three project links, removed license controls, preserved vertical-card layout, current/no-release/error/retry, duplicate lock, plain-text long notes, fixed footer, background/manual race, draft retention and light/dark passed (synthetic only)",
    );
  } finally {
    await context.close();
  }
}
