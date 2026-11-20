/**
 * Floating feedback regression against the real Sonner DOM and native dialog top layer.
 * Runs only at 1920×1080 with synthetic IPC; never logs in or sends a message.
 * @author ahui
 */
import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";

/**
 * Verify stable work areas, centered cards, usable close targets and modal transfer.
 * @param tools Existing isolated UI harness and its shared pointer/footer assertions.
 * @returns Resolves after screenshots and geometry evidence are written; rejects on regression.
 */
export async function checkFloatingFeedback({
  browser,
  installFixture,
  artifacts,
  pageErrors,
  assertFixedDialogFooter,
  checkToastReceivesPointer,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const evidence = process.env.FEEDBACK_EVIDENCE || "after";
  const visible = () =>
    page.locator(
      '[data-sonner-toast][data-visible="true"]:not([data-removed="true"])',
    );
  const records = [];
  // Measure the actual workspace and modal regions, not just the toolbar that never moved.
  const geometry = () =>
    page.evaluate(() =>
      [
        ...document.querySelectorAll(
          ".app-header, .page-frame:not([hidden]), .workspace-header, .workbench-layout, .workbench-sidebar, .workbench-content, .mail-footer, dialog[open], dialog[open] > .dialog-body, dialog[open] > .dialog-footer, .login-dialog[open] .qr-stage",
        ),
      ]
        .filter((el) => el.getClientRects().length)
        .map((el) => {
          const r = el.getBoundingClientRect();
          return {
            element: el.className,
            rect: [r.x, r.y, r.width, r.height].map(
              (v) => Math.round(v * 100) / 100,
            ),
          };
        }),
    );
  const publish = (message, tone = "error") =>
    page.evaluate(
      async (value) => {
        const { notify } = await import("/frontend/lib/notify.ts");
        notify(value);
      },
      { message, tone },
    );
  const clear = async () => {
    while (await visible().count()) {
      const item = visible().first();
      const node = await item.elementHandle();
      await item.getByRole("button", { name: "关闭提示", exact: true }).click();
      // Track this node's removal; a positional/text locator can switch to the next/reopened item.
      await node.waitForElementState("hidden");
      await node.dispose();
    }
  };
  const capture = (name) =>
    page.screenshot({
      path: new URL(
        `floating-${evidence}-${name}.png`,
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
  const verify = async (name, before, modal = false) => {
    await visible().first().waitFor();
    await checkToastReceivesPointer(visible().first());
    await page.waitForFunction(() => {
      const first = document.querySelector(
        '[data-sonner-toast][data-visible="true"]:not([data-removed="true"])',
      );
      return (
        first?.dataset.mounted === "true" &&
        getComputedStyle(first).opacity === "1"
      );
    });
    const after = await geometry();
    const cards = await visible().evaluateAll((items) =>
      items.map((item) => {
        const r = item.getBoundingClientRect();
        const close = item.querySelector("[data-close-button]");
        const c = close.getBoundingClientRect();
        const title = item
          .querySelector("[data-title]")
          .getBoundingClientRect();
        const icon = item.querySelector("[data-icon]")?.getBoundingClientRect();
        return {
          center: r.x + r.width / 2,
          top: r.top,
          bottom: r.bottom,
          height: r.height,
          textTopSpace: title.top - r.top,
          textBottomSpace: r.bottom - title.bottom,
          iconCenterDelta: icon
            ? Math.abs(icon.y + icon.height / 2 - title.y - title.height / 2)
            : 0,
          closeInside:
            c.left >= r.left &&
            c.right <= r.right &&
            c.top >= r.top &&
            c.bottom <= r.bottom,
          closeSize: [c.width, c.height],
          closeHit: close.contains(
            document.elementFromPoint(c.x + c.width / 2, c.y + c.height / 2),
          ),
        };
      }),
    );
    // Save evidence before asserting: the old normal-flow implementation must visibly fail.
    records.push({ name, before, after, cards });
    await writeFile(
      new URL(`floating-feedback-${evidence}.json`, artifacts),
      JSON.stringify(records, null, 2) + "\n",
      "utf8",
    );
    await capture(name);
    assert.deepEqual(after, before, `${name}: feedback moved the workspace`);
    assert.equal(await page.locator(".feedback-layer").count(), 1);
    assert.equal(
      await page.locator(".feedback-layer").evaluate((el) => {
        const target =
          [...document.querySelectorAll("dialog[open]")].at(-1) ||
          document.querySelector(".app-shell");
        return (
          el.parentElement === target &&
          getComputedStyle(el).position === "fixed"
        );
      }),
      true,
      `${name}: host must float in the active top layer`,
    );
    for (const card of cards) {
      assert.ok(Math.abs(card.center - 960) <= 1, `${name}: off-center card`);
      assert.ok(
        card.top >= 12 && card.bottom < 600,
        `${name}: oversized stack`,
      );
      assert.ok(
        card.height <= 180,
        `${name}: long content must scroll within its card`,
      );
      if (card.height <= 60) {
        assert.ok(
          Math.abs(card.textTopSpace - card.textBottomSpace) <= 1,
          `${name}: asymmetric text padding`,
        );
        assert.ok(
          card.iconCenterDelta <= 1,
          `${name}: icon and text centers differ`,
        );
      }
      assert.equal(
        card.closeInside && card.closeHit,
        true,
        `${name}: misplaced close button`,
      );
      assert.ok(
        card.closeSize.every((v) => v >= 32),
        `${name}: small close target`,
      );
    }
    // The full-viewport host must not intercept navigation or bottom action buttons.
    const controls = modal
      ? page
          .locator("dialog[open]")
          .last()
          .locator(
            ".dialog-footer button, .dialog-head button, .mail-dialog-heading button",
          )
      : page.locator(
          ".app-nav button, .header-actions button, .mail-footer button",
        );
    assert.equal(
      await controls.evaluateAll((items) =>
        items.every((el) => {
          const r = el.getBoundingClientRect();
          return (
            !r.width ||
            !r.height ||
            el.contains(
              document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2),
            )
          );
        }),
      ),
      true,
      `${name}: floating host blocked an action`,
    );
    if (modal) await assertFixedDialogFooter(page);
  };
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByLabel("当前已登录账号", { exact: true }).waitFor();
    const members = await geometry();
    await publish("登录成功，请选择要获取成员的群。", "success");
    await verify("members-light", members);
    await clear();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
        // Reproduce the production cascade: Sonner can insert its stylesheet after app CSS.
        for (const style of document.querySelectorAll("style")) {
          if (style.textContent.includes("--toast-close-button-start"))
            document.head.append(style);
        }
      }, theme);
      const initial = await geometry();
      await publish("登录成功，请选择要获取成员的群。", "success");
      await verify(`single-${theme}`, initial);
      await clear();
      assert.deepEqual(
        await geometry(),
        initial,
        "Closing feedback moved the page",
      );
      for (let index = 0; index < 4; index++)
        await publish(
          `长提示${index}：${"长内容可以滚动阅读，关闭按钮始终可用。".repeat(20)}`,
        );
      await page.waitForFunction(
        () =>
          document.querySelectorAll(
            '[data-sonner-toast][data-visible="true"]:not([data-removed="true"])',
          ).length === 3,
      );
      await verify(`stack-${theme}`, initial);
      const content = visible().first().locator("[data-content]");
      assert.equal(
        await content.evaluate((el) => {
          el.scrollTop = el.scrollHeight;
          return el.scrollTop > 0;
        }),
        true,
        "Long feedback must remain readable by scrolling",
      );
      await clear();
      for (const [entry, key] of [
        ["发件设置", "settings"],
        ["查看日志", "logs"],
        ["重新扫码", "qr"],
      ]) {
        await page.getByRole("button", { name: entry, exact: true }).click();
        await page.locator("dialog[open]").last().waitFor();
        const dialogGeometry = await geometry();
        await publish(`${entry}中的悬浮提示`);
        await verify(`dialog-${key}-${theme}`, dialogGeometry, true);
        // Modal scrolling must not move a viewport-fixed notification.
        const top = await visible().first().boundingBox();
        await page
          .locator("dialog[open] > .dialog-body")
          .evaluateAll((items) => {
            for (const item of items) item.scrollTop = item.scrollHeight;
          });
        assert.equal((await visible().first().boundingBox()).y, top.y);
        await page.keyboard.press("Escape");
        await page.waitForFunction(
          () => !document.querySelector("dialog[open]"),
        );
        await verify(`return-${key}-${theme}`, initial);
        await clear();
      }
    }
    // Exercise a real nested application dialog; cancelling must restore the same library host.
    await page.evaluate(() => window.__uiFixture.seedTemplateTransfer());
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library.locator(".mail-template-item").first().click();
    await library
      .getByRole("button", { name: "重命名模板", exact: true })
      .click();
    await page
      .getByRole("dialog", { name: "重命名模板", exact: true })
      .waitFor();
    const nestedGeometry = await geometry();
    await publish("双层弹窗提示检查");
    await verify("nested-dialog", nestedGeometry, true);
    await page.keyboard.press("Escape");
    await page
      .getByRole("dialog", { name: "重命名模板", exact: true })
      .waitFor({ state: "hidden" });
    const libraryGeometry = await geometry();
    await verify("nested-return", libraryGeometry, true);
    await clear();
    await library
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    for (const [name, key] of [
      ["已发送记录", "history"],
      ["手动排除", "manual"],
    ]) {
      await page.getByRole("button", { name, exact: true }).click();
      const before = await geometry();
      await publish("操作已完成", "success");
      await verify(`page-${key}`, before);
      await clear();
    }
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    // Background completion must not steal input focus; duplicated events remain one notice.
    await page.getByLabel("主题", { exact: true }).focus();
    await publish("去重与键盘关闭检查");
    await publish("去重与键盘关闭检查");
    assert.equal(await visible().count(), 1);
    assert.equal(
      await page
        .getByLabel("主题", { exact: true })
        .evaluate((el) => document.activeElement === el),
      true,
    );
    const keyboardClose = visible()
      .first()
      .getByRole("button", { name: "关闭提示", exact: true });
    // Switch to keyboard modality; a pointer-origin programmatic focus need not match :focus-visible.
    await page.keyboard.press("Tab");
    await keyboardClose.focus();
    assert.equal(
      await keyboardClose.evaluate((el) => getComputedStyle(el).outlineStyle),
      "solid",
    );
    await page.keyboard.press("Enter");
    await page
      .getByText("去重与键盘关闭检查", { exact: true })
      .waitFor({ state: "hidden" });
    // Reopening identical feedback during the old card's exit must survive its delayed cleanup.
    await publish("关闭后立即重复的提示");
    await visible().first().waitFor();
    const exiting = await visible().first().elementHandle();
    await visible()
      .first()
      .getByRole("button", { name: "关闭提示", exact: true })
      .click();
    await publish("关闭后立即重复的提示");
    await exiting.waitForElementState("hidden");
    await exiting.dispose();
    assert.equal(
      await visible().count(),
      1,
      "Old exit removed the new same-message feedback",
    );
    await clear();
    await page.emulateMedia({ reducedMotion: "reduce" });
    const initial = await geometry();
    await publish("减少动态效果检查");
    assert.equal(
      await visible()
        .first()
        .evaluate((el) => getComputedStyle(el).transitionDuration),
      "0s",
    );
    await verify("reduced-motion", initial);
    await clear();
    await publish("自动收起检查", "success");
    await visible().first().waitFor();
    await page.mouse.move(10, 900);
    await visible().first().waitFor({ state: "hidden", timeout: 7000 });
    assert.deepEqual(await geometry(), initial, "Expiry moved the page");
  } finally {
    await context.close();
  }
  console.log(
    "1920x1080 floating feedback: zero workspace movement, centered cards, late Sonner CSS, light/dark, bounded scrollable stack, close hit targets, pointer passthrough, modal transfer/scroll, reduced motion and expiry passed (synthetic only)",
  );
}

/**
 * Verify the bundled production assets through actual application actions, without dev imports.
 * @param tools Isolated browser, synthetic IPC installer and screenshot destination.
 * @returns Resolves after light/dark and native modal checks; rejects on layout or pointer failure.
 */
export async function checkProductionFeedback({
  browser,
  installFixture,
  artifacts,
  pageErrors,
  checkToastReceivesPointer,
}) {
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    reducedMotion: "reduce",
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const geometry = () =>
    page
      .locator(
        ".app-header, .page-frame:not([hidden]), .workspace-header, .workbench-layout",
      )
      .evaluateAll((items) =>
        items.map((el) => {
          const r = el.getBoundingClientRect();
          return [r.x, r.y, r.width, r.height];
        }),
      );
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "手动排除", exact: true }).click();
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
      }, theme);
      const before = await geometry();
      await page
        .getByLabel("手动排除账号", { exact: true })
        .fill(`production-${theme}@example.test`);
      await page
        .getByRole("button", { name: "添加手动排除", exact: true })
        .click();
      const toast = page
        .locator('[data-sonner-toast]:not([data-removed="true"])')
        .filter({ hasText: "已添加 1 条" });
      await toast.waitFor({ timeout: 8000 }).catch(async (error) => {
        await page.screenshot({
          path: new URL(
            "floating-production-failure.png",
            artifacts,
          ).pathname.replace(/^\/(\w:)/, "$1"),
        });
        console.log(
          "Production feedback diagnostics:",
          await page.evaluate(() => ({
            messages: [...document.querySelectorAll("[data-sonner-toast]")].map(
              (el) => el.textContent,
            ),
            commands: window.__uiFixture.calls.map((entry) => entry.command),
          })),
        );
        throw error;
      });
      await checkToastReceivesPointer(toast);
      assert.deepEqual(
        await geometry(),
        before,
        "Production feedback moved the page",
      );
      const box = await toast.boundingBox();
      assert.equal(box.x + box.width / 2, 960);
      const title = await toast.locator("[data-title]").boundingBox();
      const icon = await toast.locator("[data-icon]").boundingBox();
      assert.ok(
        Math.abs(
          title.y - box.y - (box.y + box.height - title.y - title.height),
        ) <= 1,
        "Production text padding must be symmetric",
      );
      assert.ok(
        Math.abs(icon.y + icon.height / 2 - title.y - title.height / 2) <= 1,
        "Production text and icon must share a center line",
      );
      const close = toast.getByRole("button", {
        name: "关闭提示",
        exact: true,
      });
      const c = await close.boundingBox();
      assert.ok(
        c.x >= box.x &&
          c.x + c.width <= box.x + box.width &&
          c.y >= box.y &&
          c.y + c.height <= box.y + box.height,
      );
      await page.screenshot({
        path: new URL(
          `floating-production-${theme}.png`,
          artifacts,
        ).pathname.replace(/^\/(\w:)/, "$1"),
      });
      await page.getByRole("button", { name: "查看日志", exact: true }).click();
      await page
        .getByRole("dialog", { name: "查看日志", exact: true })
        .waitFor();
      await checkToastReceivesPointer(toast);
      assert.equal(
        await toast.evaluate((el) => !!el.closest("dialog[open]")),
        true,
      );
      await close.click();
      await toast.waitFor({ state: "hidden" });
      assert.equal(
        await page
          .getByRole("dialog", { name: "查看日志", exact: true })
          .isVisible(),
        true,
      );
      await page.keyboard.press("Escape");
    }
  } finally {
    await context.close();
  }
  console.log(
    "1920x1080 bundled production feedback: actual add action, light/dark, zero layout shift, centered card, close bounds and native modal pointer/close passed (synthetic IPC only)",
  );
}
