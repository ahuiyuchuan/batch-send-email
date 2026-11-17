/**
 * Isolated renderer regression check. IPC fixtures exist only in this test and never in the app.
 * Uses installed Edge with a disposable profile; no QQ login, network member access, or email send.
 * Friend relations and read failures are synthetic UI contracts, not evidence for QQ response fields.
 * @author ahui
 */
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { createServer, preview } from "vite";
import { chromium } from "playwright";
import {
  checkFloatingFeedback,
  checkProductionFeedback,
} from "./floating-feedback-check.mjs";
import { checkRememberedOptions } from "./remembered-options-check.mjs";
import { checkMultiGroup } from "./multi-group-check.mjs";
import { checkAbout } from "./about-check.mjs";

// Production mode serves Vite's saved build and generates feedback through real UI actions.
// It must not import source notify.ts, which would create a separate Sonner state from the bundle.
const production = [
  "production-feedback",
  "production-options",
  "production-multi-group",
  "production-about",
].includes(process.env.UI_CHECK_ONLY);
const server = production
  ? await preview({
      preview: { host: "127.0.0.1", port: 1420, strictPort: true },
    })
  : await createServer({
      server: { host: "127.0.0.1", port: 1420, strictPort: true },
    });
if (!production) await server.listen();
const browser = await chromium.launch({ channel: "msedge", headless: true });
const artifacts = new URL("../../artifacts/frontend/", import.meta.url);
await mkdir(artifacts, { recursive: true });
const pageErrors = [];
const fullHdOnly = [
  "apple-clean",
  "terminal-design",
  "terminal-layout",
  "top-feedback",
  "detail-polish",
  "detail-layout",
  "production-feedback",
  "remember-options",
  "production-options",
  "filter-preferences",
  "multi-group",
  "production-multi-group",
  "production-about",
  "about",
].includes(process.env.UI_CHECK_ONLY);
/** 本轮重构仅验收1920×1080；保留其他历史专项入口原有的尺寸覆盖。 */
function testViewport(width, height) {
  return fullHdOnly ? { width: 1920, height: 1080 } : { width, height };
}
function testWidths(widths) {
  return fullHdOnly ? [1920] : widths;
}

/** Install synthetic Tauri command handling before the renderer imports the real IPC library. */
function installFixture({
  initializeMailRace = false,
  initializeSessionRace = false,
  initialSessionStatus = "active",
  historyFailures = 0,
  groupScenario = false,
  initialPasswordSaved = false,
  friendScenario = false,
  countdownScenario = false,
  bulkHistoryScenario = false,
  preflightScenario = false,
  manualScenario = false,
  workspaceUnavailable = false,
  mailStateUnavailable = false,
  multiAccountScenario = false,
  sortingScenario = false,
  multiGroupScenario = false,
} = {}) {
  // IPC 只属于应用主页面；沙箱邮件预览不能访问宿主或隔离测试的会话存储。
  if (window.top !== window) return;
  const callbacks = new Map();
  const listeners = new Map();
  let nextId = 1;
  let taskNumber = 0;
  let mailNumber = 0;
  let completeBeforeStartReturn = false;
  // 延迟跳过响应只属于合成测试，用于检查重复点击、停止和旧等待之间的竞争。
  let holdWaitSkip = false;
  let releaseWaitSkip = null;
  let historyFailureCount = historyFailures;
  let holdHistoryRead = false;
  let releaseHistoryRead = null;
  let inlineImageNumber = 0;
  let failInlineImage = false;
  let holdInlineImage = false;
  let releaseInlineImage = null;
  let holdFetch = false;
  let releaseFetch = null;
  let holdMemberStatus = false;
  let releaseMemberStatus = null;
  let failMemberStatus = false;
  let omitMemberStatus = false;
  let holdWorkspaceRead = false,
    releaseWorkspaceRead = null;
  let holdMailStateRead = false,
    releaseMailStateRead = null;
  let nextFriendListError = null;
  let qrStatus = "wait";
  // 扫码代次只模拟公开协议，不接触真实 QQ；取消后迟到响应不得发布新身份。
  let qrGeneration = 0;
  let qrPending = false;
  let nextQrAccount = null;
  let nextQrGroups = null;
  let holdQrPoll = false;
  let releaseQrPoll = null;
  let failQrPoll = false;
  let failTemplateOperation = "";
  let holdTemplateApply = false;
  let releaseTemplateApply = null;
  let templateApplyNumber = 0;
  // 模板交换夹具只模拟原生文件选择和确认令牌；真实文件格式、字节与事务由 Rust 测试验证。
  let nextTemplatePackage = null;
  let pendingTemplateImport = null;
  let templateImportNumber = 0;
  let templateTransferHold = "";
  let releaseTemplateTransfer = null;
  let cancelTemplateExport = false;
  const exportedTemplatePackages = [];
  let failBulkResolution = false;
  let holdBulkResolution = false;
  let releaseBulkResolution = null;
  let recipientPlanNumber = 0;
  let holdRecipientPreparation = false;
  let releaseRecipientPreparation = null;
  const recipientPlans = new Map();
  let failDiagnosticReports = false;
  let diagnosticUnavailable = false;
  let recentLogs = {
    content:
      '{"timestamp":"2026-11-05T12:00:00Z","event":"synthetic_test_only"}\n',
    truncated: false,
  };
  let logReadFailures = 0;
  let holdLogRead = false;
  let releaseLogRead = null;
  const diagnosticStatus = () => ({
    available: !diagnosticUnavailable,
    message: diagnosticUnavailable
      ? "诊断日志暂时无法写入，请检查本机数据目录权限后重启。"
      : "",
  });
  const inlineResources = new Map();
  const attachmentResources = new Map();
  // 仅在隔离浏览器中模拟数据库跨重载保存；只保留显式模板与合成资源，绝不保存账号或凭据。
  const templateStorageKey = "__ui_fixture_mail_templates";
  let templates = JSON.parse(
    sessionStorage.getItem(templateStorageKey) ?? "[]",
  );
  const snapshots = new Map(
    multiGroupScenario
      ? JSON.parse(sessionStorage.getItem("__ui_fixture_multi_groups") ?? "[]")
      : [],
  );
  const snapshotsById = new Map(
    [...snapshots.values()].map((snapshot) => [snapshot.id, snapshot]),
  );
  let nextMultiGroupFailure = false;
  // 关于页夹具只模拟公开发布契约，不访问真实GitHub或打开用户浏览器。
  let updateResult = null;
  let updateFailure = false;
  let releaseUpdate = null;
  let holdUpdate = false;
  // 仅合成排除记录模拟 SQLite 跨重载保留，绝不写入真实应用数据目录。
  const manualStorageKey = "__ui_fixture_manual_exclusions";
  let manualView = manualScenario
    ? JSON.parse(
        sessionStorage.getItem(manualStorageKey) ??
          '{"revision":0,"records":[]}',
      )
    : { revision: 0, records: [] };
  let manualReadFailures = 0;
  let holdManualRead = false;
  let releaseManualRead = null;
  let manualWriteFailure = false;
  let manualRefreshFailure = false;
  let holdManualWrite = false;
  let releaseManualWrite = null;
  let snapshotViewFailures = 0;
  let restorationFailures = 0;
  let holdRestoration = false;
  let releaseRestoration = null;
  let restorationNumber = 0;
  const overlayManual = (base) => {
    if (!base) return null;
    const output = copy(base);
    const removed = output.filteredMembers.filter(
      (entry) => entry.reason === "manual",
    );
    const members = [
      ...output.members,
      ...removed.map((entry) => entry.member),
    ];
    output.filteredMembers = output.filteredMembers.filter(
      (entry) => entry.reason !== "manual",
    );
    const blocked = new Set(manualView.records.map((entry) => entry.email));
    output.members = members.filter(
      (member) => !blocked.has(`${member.qqNumber}@qq.com`),
    );
    output.filteredMembers.push(
      ...members
        .filter((member) => blocked.has(`${member.qqNumber}@qq.com`))
        .map((member) => ({
          member,
          reason: "manual",
          detail: "手动排除",
        })),
    );
    output.manualFilteredCount = members.length - output.members.length;
    output.exclusionRevision = manualView.revision;
    return output;
  };
  // 本机绘制可视回归图，避免真实图片、外部下载或微小占位图让预览截图失去验收价值。
  const inlineCanvas = document.createElement("canvas");
  inlineCanvas.width = 320;
  inlineCanvas.height = 180;
  const drawing = inlineCanvas.getContext("2d");
  drawing.fillStyle = "#edf4ff";
  drawing.fillRect(0, 0, 320, 180);
  drawing.strokeStyle = "#cadbf4";
  for (let offset = 0; offset <= 320; offset += 20) {
    drawing.beginPath();
    drawing.moveTo(offset, 0);
    drawing.lineTo(offset, 180);
    drawing.stroke();
  }
  for (let offset = 0; offset <= 180; offset += 20) {
    drawing.beginPath();
    drawing.moveTo(0, offset);
    drawing.lineTo(320, offset);
    drawing.stroke();
  }
  drawing.fillStyle = "#214b83";
  drawing.font = "bold 25px sans-serif";
  drawing.fillText("MAIL IMAGE", 28, 78);
  drawing.font = "16px sans-serif";
  drawing.fillText("320 x 180 / local fixture", 28, 110);
  const inlinePngBase64 = inlineCanvas.toDataURL("image/png").split(",")[1];
  const copy = (value) => JSON.parse(JSON.stringify(value));
  const templateSummary = (template) => ({
    id: template.id,
    name: template.name,
    subject: template.subject,
    attachmentCount: template.attachments.length,
    inlineImageCount: template.inlineImages.length,
    updatedAt: template.updatedAt,
  });
  const persistTemplates = () =>
    sessionStorage.setItem(templateStorageKey, JSON.stringify(templates));
  const members = Array.from(
    {
      length:
        multiGroupScenario === "large"
          ? 10005
          : friendScenario
            ? 6
            : preflightScenario
              ? 490
              : 62,
    },
    (_, index) => ({
      qqNumber: `10000${String(index + 1).padStart(3, "0")}`,
      nickname: index === 0 ? "唯一测试成员" : `测试成员${index + 1}`,
      card: index % 2 ? "" : `群名片${index + 1}`,
      friendRelation:
        friendScenario && [0, 1, 3].includes(index) ? "friend" : "not_friend",
    }),
  );
  const groups = groupScenario
    ? Array.from({ length: 100 }, (_, index) => ({
        id: String(900001 + index),
        name:
          index === 0
            ? "未知人数目标群"
            : index === 1
              ? "独立测试群"
              : index === 2
                ? "未知总数测试群"
                : `布局测试群${index + 1}`,
        memberCount: index === 0 || index === 2 ? null : index === 1 ? 7 : 2,
        role: "member",
      }))
    : [
        {
          id: "900001",
          name: "目标测试群",
          memberCount: members.length,
          role: "member",
        },
        { id: "900002", name: "排除测试群", memberCount: 2, role: "member" },
      ];
  // 排序回归刻意打乱获取顺序；只生成合成账号，不能触碰用户 SQLite 或实际发送。
  if (multiGroupScenario)
    groups.splice(
      0,
      groups.length,
      ...["目标甲群", "目标乙群", "排除丙群"].map((name, index) => ({
        id: String(900001 + index),
        name,
        memberCount: index < 2 ? 4 : 1,
        role: "member",
      })),
    );
  if (sortingScenario) {
    members.reverse();
    groups.splice(
      0,
      groups.length,
      ...[
        "中群",
        "B群",
        "10群",
        "a2群",
        "2群",
        "a10群",
        "!群",
        "😀群",
        "中群",
        "阿群",
        "波群",
        "李群",
        "张群",
      ].map((name, index) => ({
        id: String(900001 + index),
        name,
        memberCount: members.length,
        role: "member",
      })),
    );
    manualView = {
      revision: 1,
      records: Array.from({ length: 26 }, (_, index) => ({
        id: index + 1,
        qqNumber: String(30000 + index),
        email: `${30000 + index}@qq.com`,
        createdAt: "2026-11-10T06:00:00Z",
        note: "合成排序记录",
      })).reverse(),
    };
    templates = [
      "中模板",
      "B模板",
      "10模板",
      "a2模板",
      "2模板",
      "a10模板",
      "!模板",
      "😀模板",
    ].map((name, index) => ({
      id: `sort-template-${index + 1}`,
      name,
      subject: `${name}主题`,
      html: `<p>${name}正文</p>`,
      inlineImages: [],
      attachments: [],
      updatedAt: "2026-11-10T06:00:00Z",
    }));
  }
  let mailSettings = {
    host: "smtp.qq.com",
    port: 465,
    tlsMode: "tls",
    username: initialPasswordSaved ? "sender@example.test" : "",
    fromAddress: initialPasswordSaved ? "sender@example.test" : "",
    fromName: "",
  };
  let passwordSaved = initialPasswordSaved;
  let passwordStatus = initialPasswordSaved ? "saved" : "missing";
  const mailSettingsView = () => ({
    ...mailSettings,
    passwordSaved,
    passwordStatus,
    passwordMessage: passwordSaved
      ? "已安全保存授权码，可直接发送；如需更换可输入新授权码。"
      : passwordStatus === "mismatch"
        ? "发件账号或服务器配置已变化，请输入并保存新授权码。"
        : "尚未保存授权码，请填写后保存。",
  });
  // 账号仅为隔离测试合成目录，保存公开字段和凭据状态，永不记录或持久化真实授权码。
  const accountStorageKey = "__ui_fixture_sender_accounts";
  let accountCatalog = multiAccountScenario
    ? JSON.parse(
        sessionStorage.getItem(accountStorageKey) ??
          JSON.stringify({
            revision: 1,
            accounts: Array.from({ length: 5 }, (_, index) => ({
              ...mailSettingsView(),
              id: `sender-${index + 1}`,
              name: `发件账号${index + 1}`,
              username: `sender${index + 1}@example.test`,
              fromAddress: `sender${index + 1}@example.test`,
              passwordSaved: true,
              passwordStatus: "saved",
            })),
          }),
      )
    : {
        revision: initialPasswordSaved ? 1 : 0,
        accounts: initialPasswordSaved
          ? [
              {
                ...mailSettingsView(),
                id: "legacy-default",
                name: "默认发件账号",
              },
            ]
          : [],
      };
  let accountNumber = 10;
  let accountReadFailures = 0;
  let accountMutationFailure = false;
  let holdAccountMutation = false;
  let releaseAccountMutation = null;
  let lastAccountAssignments = [];
  const persistAccountCatalog = () => {
    if (multiAccountScenario)
      sessionStorage.setItem(accountStorageKey, JSON.stringify(accountCatalog));
  };
  const orderedAccounts = (ids) => {
    if (!Array.isArray(ids) || !ids.length || new Set(ids).size !== ids.length)
      throw {
        code: "mail_accounts_selection",
        message: "请至少选择一个发件账号。",
      };
    const selected = accountCatalog.accounts.filter((account) =>
      ids.includes(account.id),
    );
    if (selected.length !== ids.length)
      throw {
        code: "mail_accounts_changed",
        message: "发件账号已变化，请重新确认。",
      };
    return selected;
  };
  const assignAccounts = (
    actual,
    ids,
    mode,
    batchSize,
    allocationMode = "ordered",
  ) => {
    // 随机夹具采用确定性反转，便于断言冻结契约；宿主随机性由Rust独立回归，不用概率断言。
    const accounts = orderedAccounts(ids);
    if (allocationMode === "random") accounts.reverse();
    const count = Math.floor(actual.length / accounts.length);
    const remainder = actual.length % accounts.length;
    return accounts.map((account, index) => {
      const recipientCount = count + (index < remainder ? 1 : 0);
      return {
        accountId: account.id,
        name: account.name,
        fromAddress: account.fromAddress,
        recipientCount,
        messageCount:
          mode === "individual"
            ? recipientCount
            : Math.ceil(recipientCount / batchSize),
      };
    });
  };
  const state = {
    version: "1.0.0",
    source: "web",
    account: { qqNumber: "123456789", nickname: "测试账号", source: "web" },
    groups,
    snapshot: null,
    task: { id: "", status: "idle", message: "" },
    runtime: "Fixture · no network",
    historyCount: 1,
    exclusionRevision: manualView.revision,
    databasePath: "Local fixture database",
    session: {
      status: initialSessionStatus,
      message:
        initialSessionStatus === "active"
          ? "当前账号已登录，关闭程序后需重新扫码。"
          : initialSessionStatus === "expired"
            ? "登录状态已失效，请重新扫码。"
            : "请扫码登录，关闭程序后需重新扫码。",
    },
  };
  if (initialSessionStatus !== "active") {
    state.source = null;
    state.account = null;
    state.groups = [];
  }
  let records = [
    {
      id: 1,
      qqNumber: members[0].qqNumber,
      email: `${members[0].qqNumber}@qq.com`,
      sentAt: "2026-11-05T12:00:00+08:00",
      origin: "imported",
      note: "用户确认的标记",
    },
  ];
  if (preflightScenario)
    records = members.slice(0, 130).map((member, index) => ({
      id: index + 1,
      qqNumber: member.qqNumber,
      email: `${member.qqNumber}@qq.com`,
      sentAt: "2026-11-05T12:00:00+08:00",
      origin: "imported",
      note: "发送前实际名单过滤的合成历史。",
    }));
  state.historyCount = records.length;
  let unresolved = [
    {
      id: 31,
      email: "10000333@qq.com",
      updatedAt: "2026-11-05T12:00:10+08:00",
      message: "模拟超时，仅用于界面回归。",
      senderAddress: "trace-original@example.test",
      senderName: "原始发件账号",
    },
    {
      id: 32,
      email: "person@example.test",
      updatedAt: "2026-11-05T12:00:11+08:00",
      message: "模拟进程中断，仅用于界面回归。",
    },
  ];
  if (bulkHistoryScenario)
    unresolved = Array.from({ length: 53 }, (_, index) => ({
      id: 1001 + index,
      email: `pending-${String(index + 1).padStart(2, "0")}@example.test`,
      updatedAt: "2026-11-05T12:00:10+08:00",
      message: "隔离批量核实夹具：尚未确认服务器是否接受。",
    }));
  // 大名单专项只验证分块和人数上限，待核实限制由原有收件范围回归独立覆盖。
  if (preflightScenario || multiGroupScenario === "large") unresolved = [];
  if (sortingScenario) {
    records = Array.from({ length: 56 }, (_, index) => ({
      id: index + 1,
      qqNumber: String(20000 + index),
      email: `${20000 + index}@qq.com`,
      sentAt: "2026-11-10T06:00:00Z",
      origin: "imported",
      note: "合成排序记录",
    })).reverse();
    unresolved = Array.from({ length: 28 }, (_, index) => ({
      id: 1001 + index,
      email: `pending-${index + 1}@example.test`,
      updatedAt: "2026-11-10T06:00:00Z",
      message: "合成排序待核实记录",
    })).reverse();
    state.historyCount = records.length;
  }
  let mail = {
    id: "",
    status: "idle",
    phase: "idle",
    nextBatchAt: null,
    stateRevision: 0,
    total: 0,
    sent: 0,
    failed: 0,
    skipped: 0,
    current: "",
    message: "",
    failures: [],
    groupAudience: null,
  };
  // 仅倒计时测试模拟宿主跨页面重载仍持有原绝对截止时间，不保存真实邮件或用户资料。
  const countdownStorageKey = "__ui_fixture_countdown_mail";
  if (countdownScenario && sessionStorage.getItem(countdownStorageKey))
    mail = JSON.parse(sessionStorage.getItem(countdownStorageKey));
  let lastDraft = null;
  let lastActualRecipients = [];
  // 公开归属在任务创建时冻结；历史展示不能跟随后来编辑的账号目录变化。
  let lastDeliverySenders = [];
  const emit = (event, payload) => {
    for (const listener of listeners.values())
      if (listener.event === event)
        callbacks.get(listener.handler)?.({
          event,
          id: 1,
          payload: copy(payload),
        });
  };
  window.isTauri = true;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event, id) => listeners.delete(id),
  };
  window.__uiFixture = {
    calls: [],
    publishUpdate: (value) => {
      updateResult = copy(value);
      emit("update-status-changed", value);
    },
    setUpdateResult: (value) => {
      updateResult = copy(value);
    },
    failNextUpdate: () => {
      updateFailure = true;
    },
    holdNextUpdate: () => {
      holdUpdate = true;
    },
    releaseUpdate: () => releaseUpdate?.(),
    /** 暴露原始顺序供只读断言；刷新反转源数据用于验证同名 ID 的稳定位置。 */
    sortingSources: () =>
      copy({
        groups,
        members,
        records,
        unresolved,
        manual: manualView.records,
        templates,
      }),
    reverseSortingGroups: () => {
      groups.reverse();
      emit("app-state", state);
    },
    senderAccounts: () => copy(accountCatalog),
    accountAssignments: () => copy(lastAccountAssignments),
    preparedMailPlans: () =>
      [...recipientPlans.values()].map((plan) => copy(plan.view)),
    failNextAccountRead: () => {
      accountReadFailures += 1;
    },
    failNextAccountMutation: () => {
      accountMutationFailure = true;
    },
    holdNextAccountMutation: () => {
      holdAccountMutation = true;
    },
    accountMutationPending: () => releaseAccountMutation !== null,
    releaseAccountMutation: () => releaseAccountMutation?.(),
    /** 只模拟另一受控操作使预检目录失效，不修改页面状态，不接触真实文件。 */
    changeAccountsWithoutPublishing: () => {
      accountCatalog.revision += 1;
      persistAccountCatalog();
    },
    /** 合成拒收名单用于证明所有发件账号先共用同一次全局排除，不写真实 SQLite。 */
    excludeWithoutPublishing: (emails) => {
      for (const email of emails) {
        if (!manualView.records.some((entry) => entry.email === email)) {
          manualView.records.push({
            id: 50000 + manualView.records.length,
            email,
            qqNumber: "",
            note: "多账号隔离回归",
            createdAt: "2026-11-09T12:00:00-05:00",
          });
        }
      }
      manualView.revision += 1;
      state.exclusionRevision = manualView.revision;
      emit("app-state", state);
    },
    manualExclusions: () => copy(manualView),
    /** 读请求先冻结返回内容再延迟，真实模拟旧IPC响应晚于新revision事件到达。 */
    holdNextManualRead: () => {
      holdManualRead = true;
    },
    manualReadPending: () => releaseManualRead !== null,
    releaseManualRead: () => releaseManualRead?.(),
    failNextManualRead: () => {
      manualReadFailures += 1;
    },
    failNextManualWrite: () => {
      manualWriteFailure = true;
    },
    failNextManualOverlayRefresh: () => {
      manualRefreshFailure = true;
    },
    failNextSnapshotView: () => {
      snapshotViewFailures += 1;
    },
    failNextRestoration: () => {
      restorationFailures += 1;
    },
    holdNextRestoration: () => {
      holdRestoration = true;
    },
    restorationPending: () => releaseRestoration !== null,
    releaseRestoration: () => releaseRestoration?.(),
    holdNextManualWrite: () => {
      holdManualWrite = true;
    },
    manualWritePending: () => releaseManualWrite !== null,
    releaseManualWrite: () => releaseManualWrite?.(),
    /** 日志查看器仅使用合成纯文本，故障和挂起用于覆盖只读刷新、重试与重复点击。 */
    setRecentLogs: (value) => {
      recentLogs = copy(value);
    },
    failNextLogRead: () => {
      logReadFailures += 1;
    },
    holdNextLogRead: () => {
      holdLogRead = true;
    },
    logReadPending: () => releaseLogRead !== null,
    releaseLogRead: () => {
      releaseLogRead?.();
      releaseLogRead = null;
    },
    mail: () => copy(mail),
    holdNextWaitSkip: () => {
      holdWaitSkip = true;
    },
    waitSkipPending: () => releaseWaitSkip !== null,
    releaseWaitSkip: () => {
      releaseWaitSkip?.();
      releaseWaitSkip = null;
    },
    /** 模拟宿主发布单调版本；原始迟到事件另走 emitMailState，不改后台最新状态。 */
    publishMailState: (value) => {
      mail = { ...mail, ...value, stateRevision: mail.stateRevision + 1 };
      if (countdownScenario)
        sessionStorage.setItem(countdownStorageKey, JSON.stringify(mail));
      emit("mail-state", mail);
      return copy(mail);
    },
    emitMailState: (value) => emit("mail-state", value),
    /** 日志夹具只记录安全协议，不写文件；故障注入不能影响被包裹业务命令。 */
    failDiagnosticReports: (value) => {
      failDiagnosticReports = value;
    },
    setDiagnosticUnavailable: (value) => {
      diagnosticUnavailable = value;
    },
    emits: () => emit("app-state", state),
    /** 初始化失败使用持续开关，避免开发模式重复挂载把单次故障意外吞掉。 */
    setWorkspaceUnavailable: (value) => {
      workspaceUnavailable = value;
    },
    setMailStateUnavailable: (value) => {
      mailStateUnavailable = value;
    },
    holdNextWorkspaceRead: () => {
      holdWorkspaceRead = true;
    },
    workspaceReadPending: () => releaseWorkspaceRead !== null,
    releaseWorkspaceRead: () => releaseWorkspaceRead?.(),
    holdNextMailStateRead: () => {
      holdMailStateRead = true;
    },
    mailStateReadPending: () => releaseMailStateRead !== null,
    releaseMailStateRead: () => releaseMailStateRead?.(),
    snapshot: () => copy(state.snapshot),
    snapshotFor: (groupId) => copy(snapshots.get(groupId) ?? null),
    failNextMultiGroup: () => {
      nextMultiGroupFailure = true;
    },
    /** 仅模拟公开扫码状态，用于证明同义等待文案稳定且真实状态仍能更新。 */
    setQrStatus: (status) => {
      qrStatus = status;
    },
    /** 下一轮扫码账号/群均为合成公开数据，可使用相同群号验证身份隔离。 */
    setNextQrIdentity: (account, selectedGroups) => {
      nextQrAccount = copy(account);
      nextQrGroups = copy(selectedGroups);
    },
    holdNextQrPoll: () => {
      holdQrPoll = true;
    },
    qrPollPending: () => releaseQrPoll !== null,
    releaseQrPoll: () => releaseQrPoll?.(),
    failNextQrPoll: () => {
      failQrPoll = true;
    },
    /** 仅模拟后台公开给页面的本群三态关系，不证明 get_friend_list 或旧 fr 的实际响应。 */
    setFriendRelation: (qqNumber, relation) => {
      const member = members.find((entry) => entry.qqNumber === qqNumber);
      if (!member) throw new Error("Synthetic member does not exist");
      member.friendRelation = relation;
    },
    setAllFriendRelations: (relation) => {
      members.forEach((member) => {
        member.friendRelation = relation;
      });
    },
    /** 模拟现有安全错误的任务消息；只有勾选好友排除的任务才会消费该故障。 */
    failNextFriendList: (code) => {
      const messages = {
        connection_failed: "无法连接 QQ 服务，请检查网络后重试。",
        friend_list_incomplete:
          "QQ 未返回可确认完整的好友列表，未更新名单。请重试，或取消“排除好友”后重新获取。",
      };
      if (!Object.hasOwn(messages, code))
        throw new Error("Unsupported synthetic friend failure");
      nextFriendListError = { code, message: messages[code] };
    },
    /** 换号不复用另一账号的快照，旧快照仍可作为邮件的固定来源保留。 */
    switchAccount: (qqNumber) => {
      state.account = { qqNumber, nickname: "切换后的测试账号", source: "web" };
      state.snapshot = null;
      emit("app-state", state);
    },
    /** 模拟升级前的历史快照：缺少好友字段不能被解释成已经完成好友核验。 */
    publishLegacySnapshot: () => {
      if (!state.snapshot) throw new Error("No snapshot is available");
      const legacy = copy(state.snapshot);
      legacy.id = `${legacy.id}-legacy`;
      for (const field of [
        "excludeFriends",
        "friendFilteredCount",
        "friendUnknownCount",
        "friendFilterStatus",
      ])
        delete legacy[field];
      [
        ...legacy.members,
        ...legacy.filteredMembers.map((entry) => entry.member),
      ].forEach((member) => delete member.friendRelation);
      state.snapshot = legacy;
      snapshots.set(legacy.group.id, copy(legacy));
      emit("app-state", state);
    },
    /** 模板失败与挂起夹具用于验证原草稿/库资源不被失败操作破坏及重复操作被锁定。 */
    failNextTemplate: (operation) => {
      failTemplateOperation = operation;
    },
    holdNextTemplateApply: () => {
      holdTemplateApply = true;
    },
    templateApplyPending: () => releaseTemplateApply !== null,
    releaseTemplateApply: () => releaseTemplateApply?.(),
    savedTemplates: () => copy(templates),
    /** 构造独立模板库，包含中文、正文图片和附件；不读写真实数据库或用户文件。 */
    seedTemplateTransfer: () => {
      templates = ["活动通知", "产品介绍", "内部资料", "活动提醒"].map(
        (name, index) => ({
          id: `transfer-template-${index + 1}`,
          name,
          subject: `${name}主题`,
          html:
            index === 0
              ? '<p>原模板</p><img src="cid:transfer-image@fixture.local">'
              : `<p>${name}正文</p>`,
          inlineImages:
            index === 0
              ? [
                  {
                    id: "transfer-image",
                    contentId: "transfer-image@fixture.local",
                    name: "正文图.png",
                    size: atob(inlinePngBase64).length,
                    mimeType: "image/png",
                    dataBase64: inlinePngBase64,
                  },
                ]
              : [],
          attachments:
            index === 0
              ? [{ id: "transfer-attachment", name: "中文附件.txt", size: 5 }]
              : [],
          updatedAt: "2026-11-06T03:00:00Z",
        }),
      );
      persistTemplates();
    },
    setTemplateImportPackage: (values) => {
      nextTemplatePackage = copy(values);
    },
    exportedTemplatePackages: () => copy(exportedTemplatePackages),
    cancelNextTemplateExport: () => {
      cancelTemplateExport = true;
    },
    holdNextTemplateTransfer: (operation) => {
      templateTransferHold = operation;
    },
    templateTransferPending: () => releaseTemplateTransfer !== null,
    releaseTemplateTransfer: () => releaseTemplateTransfer?.(),
    expireTemplateImport: () => {
      if (pendingTemplateImport) pendingTemplateImport.expired = true;
    },
    draftResourceIds: () => ({
      inlineImages: [...inlineResources.keys()],
      attachments: [...attachmentResources.keys()],
    }),
    /** 当前测试任务状态用于等待本轮获取完成，避免把仍显示的旧快照当作新结果。 */
    taskStatus: () => state.task.status,
    /** 显式控制完成时点，取消回归不依赖浏览器是否在250毫秒内点击到按钮。 */
    holdNextFetch: () => {
      holdFetch = true;
    },
    releaseFetch: () => {
      const complete = releaseFetch;
      releaseFetch = null;
      complete?.();
    },
    /** 记录读取可被测试显式暂停，以稳定覆盖页面加载和导航边界。 */
    holdNextHistoryRead: () => {
      holdHistoryRead = true;
    },
    historyReadPending: () => releaseHistoryRead !== null,
    releaseHistoryRead: () => releaseHistoryRead?.(),
    /** 批量核实先验证全批，再一次提交；挂起和失败不改动任何待核实或历史记录。 */
    failNextBulkResolution: () => {
      failBulkResolution = true;
    },
    holdNextBulkResolution: () => {
      holdBulkResolution = true;
    },
    bulkResolutionPending: () => releaseBulkResolution !== null,
    releaseBulkResolution: () => releaseBulkResolution?.(),
    unresolvedRecords: () => copy(unresolved),
    sentRecords: () => copy(records),
    /** 仅改变隔离目录，证明历史依赖投递快照；不会改写已有历史或访问真实账号。 */
    removeSenderAccountsWithoutPublishing: () => {
      accountCatalog.accounts = [];
      accountCatalog.revision += 1;
      persistAccountCatalog();
    },
    removeUnresolvedWithoutPublishing: (id) => {
      unresolved = unresolved.filter((entry) => entry.id !== id);
    },
    holdNextRecipientPreparation: () => {
      holdRecipientPreparation = true;
    },
    recipientPreparationPending: () => releaseRecipientPreparation !== null,
    releaseRecipientPreparation: () => releaseRecipientPreparation?.(),
    /** 只维护合成历史，用于比较两次准备的真实剩余人数，不会自动发送任何邮件。 */
    markSentWithoutPublishing: (emails) => {
      for (const email of emails)
        if (!records.some((entry) => entry.email === email))
          records.push({
            id: 20000 + records.length,
            qqNumber: email.endsWith("@qq.com") ? email.split("@")[0] : "",
            email,
            sentAt: "2026-11-05T12:00:00+08:00",
            origin: "imported",
            note: "隔离测试新增历史。",
          });
      state.historyCount = records.length;
    },
    actualMailRecipients: () => copy(lastActualRecipients),
    /** 模拟宿主登记失败只验证错误反馈，不触发本机文件访问或真实图片上传。 */
    failNextInlineImage: () => {
      failInlineImage = true;
    },
    /** 暂停图片登记可稳定验证跨页锁，避免导航导致资源归属与编辑器选区发生竞态。 */
    holdNextInlineImage: () => {
      holdInlineImage = true;
    },
    inlineImagePending: () => releaseInlineImage !== null,
    releaseInlineImage: () => releaseInlineImage?.(),
    /** 只发布公开会话状态；夹具不包含 Cookie、授权码或真实网络请求。 */
    publishSession: (session) => {
      state.session = copy(session);
      if (session.status === "active") {
        state.source = "web";
        state.account = {
          qqNumber: "123456789",
          nickname: "测试账号",
          source: "web",
        };
        state.groups = groups;
      } else {
        // 认证失效撤下当前名单；保留历史和邮件草稿不代表当前登录身份。
        state.snapshot = null;
        state.source = null;
        state.account = null;
        state.groups = [];
      }
      emit("app-state", state);
    },
    /** 发送状态读取故障只模拟页面门闩，不改变群名单、发送记录或手动排除数据。 */
    holdNextMemberStatus: () => {
      holdMemberStatus = true;
    },
    memberStatusPending: () => releaseMemberStatus !== null,
    releaseMemberStatus: () => releaseMemberStatus?.(),
    failNextMemberStatus: () => {
      failMemberStatus = true;
    },
    omitNextMemberStatus: () => {
      omitMemberStatus = true;
    },
    /** Inject IPC/event ordering without introducing real asynchronous delivery. */
    completeNextStartBeforeReturn: () => {
      completeBeforeStartReturn = true;
    },
    /** A transport ambiguity is distinct from successful history, even for the same address. */
    markUnresolved: (qqNumber) => {
      unresolved.push({
        id: 999,
        email: `${qqNumber}@qq.com`,
        updatedAt: "2026-11-05T12:11:00+08:00",
        message:
          "Synthetic ambiguous transport result; no SMTP connection was made.",
      });
    },
    /** Complete a synthetic job only when the test explicitly requests it. */
    finishMail: () => {
      if (!lastDraft || mail.status !== "running")
        throw new Error("No synthetic mail job is running");
      const recipients = lastActualRecipients;
      recipients.forEach((email, index) => {
        const qqNumber = email.endsWith("@qq.com") ? email.split("@")[0] : "";
        records.push({
          id: 100 + index,
          qqNumber,
          email,
          sentAt: "2026-11-05T12:10:00+08:00",
          origin: "smtp_accepted",
          ...lastDeliverySenders[index],
          note: "Synthetic acceptance; no SMTP connection was made.",
        });
      });
      state.historyCount = records.length;
      mail = {
        ...mail,
        status: "completed",
        phase: "idle",
        nextBatchAt: null,
        stateRevision: mail.stateRevision + 1,
        sent: recipients.length,
        current: "",
        message: "本机模拟任务已结束，不连接 SMTP。",
      };
      emit("app-state", state);
      emit("mail-state", mail);
    },
    /** 任务走完且含失败不能冒充全部成功；仅发布合成终态，不创建成功历史。 */
    finishMailWithFailures: (failedCount) => {
      if (!lastDraft || mail.status !== "running")
        throw new Error("No synthetic mail job is running");
      mail = {
        ...mail,
        status: "completed",
        phase: "idle",
        nextBatchAt: null,
        stateRevision: mail.stateRevision + 1,
        sent: mail.total - failedCount,
        failed: failedCount,
        current: "",
        message: "隔离任务已结束，部分投递未确认。",
      };
      emit("mail-state", mail);
    },
  };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" } },
    transformCallback: (callback) => {
      const id = nextId++;
      callbacks.set(id, callback);
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    invoke: async (command, args = {}) => {
      window.__uiFixture.calls.push({ command, args: copy(args) });
      if (command === "get_update_status") return copy(updateResult);
      if (command === "check_updates") {
        if (holdUpdate) {
          holdUpdate = false;
          await new Promise((resolve) => {
            releaseUpdate = resolve;
          });
          releaseUpdate = null;
        }
        if (updateFailure) {
          updateFailure = false;
          throw {
            code: "update_network",
            message: "检查更新失败，请检查网络后重试。",
          };
        }
        return copy(
          updateResult ?? {
            status: "current",
            currentVersion: "1.0.0",
            latestVersion: "1.0.0",
            releaseUrl:
              "https://github.com/ahuiyuchuan/batch-send-email/releases/tag/v1.0.0",
            notes: "",
            checkedAt: new Date().toISOString(),
          },
        );
      }
      if (command === "open_external_url") return null;
      if (
        command === "report_frontend_diagnostic" ||
        command === "report_frontend_fault"
      ) {
        if (failDiagnosticReports)
          throw {
            code: "fixture_log_write",
            message: "Synthetic diagnostic failure",
          };
        return diagnosticStatus();
      }
      if (command === "get_log_status") return diagnosticStatus();
      if (command === "get_recent_logs") {
        if (holdLogRead) {
          holdLogRead = false;
          await new Promise((resolve) => {
            releaseLogRead = resolve;
          });
        }
        if (logReadFailures > 0) {
          logReadFailures -= 1;
          throw {
            code: "log_read_error",
            message: "日志暂时无法读取，请检查访问权限后刷新重试。",
          };
        }
        return copy(recentLogs);
      }
      if (command === "open_log_directory") return;
      if (command === "get_manual_exclusions") {
        const snapshot = copy(manualView);
        if (holdManualRead) {
          holdManualRead = false;
          await new Promise((resolve) => {
            releaseManualRead = resolve;
          });
          releaseManualRead = null;
        }
        if (manualReadFailures > 0) {
          manualReadFailures -= 1;
          throw {
            code: "database_error",
            message: "手动排除名单暂时无法读取，请重试。",
          };
        }
        return snapshot;
      }
      if (
        command === "add_manual_exclusions" ||
        command === "remove_manual_exclusions"
      ) {
        if (state.task.status === "running" || mail.status === "running")
          throw { code: "mail_running", message: "请先停止当前任务。" };
        if (holdManualWrite) {
          holdManualWrite = false;
          await new Promise((resolve) => {
            releaseManualWrite = resolve;
          });
          releaseManualWrite = null;
        }
        if (manualWriteFailure) {
          manualWriteFailure = false;
          throw {
            code: "database_error",
            message: "排除名单保存失败，请重试；原记录未修改。",
          };
        }
        let added = 0,
          skipped = 0,
          invalid = 0,
          removed = 0;
        if (command === "add_manual_exclusions") {
          for (const value of args.text
            .split(/[\n\r,;，；]/)
            .map((item) => item.trim())
            .filter(Boolean)) {
            const address = /^[1-9]\d{0,19}$/.test(value)
              ? `${value}@qq.com`
              : value;
            const at = address.lastIndexOf("@");
            if (at <= 0 || !address.slice(at + 1).includes(".")) {
              invalid += 1;
              continue;
            }
            const email =
              address.slice(0, at) + "@" + address.slice(at + 1).toLowerCase();
            if (manualView.records.some((entry) => entry.email === email)) {
              skipped += 1;
              continue;
            }
            manualView.records.push({
              id:
                Math.max(0, ...manualView.records.map((entry) => entry.id)) + 1,
              email,
              qqNumber: /^\d+@qq\.com$/.test(email) ? email.split("@")[0] : "",
              note: args.note ?? "",
              createdAt: "2026-11-06T12:00:00+08:00",
            });
            added += 1;
          }
        } else {
          const ids = new Set(args.ids);
          removed = manualView.records.filter((entry) =>
            ids.has(entry.id),
          ).length;
          manualView.records = manualView.records.filter(
            (entry) => !ids.has(entry.id),
          );
        }
        if (added || removed) {
          manualView.revision += 1;
          state.exclusionRevision = manualView.revision;
          recipientPlans.clear();
          if (state.snapshot)
            state.snapshot = overlayManual(
              snapshotsById.get(state.snapshot.id) ?? state.snapshot,
            );
          if (manualScenario)
            sessionStorage.setItem(
              manualStorageKey,
              JSON.stringify(manualView),
            );
          emit("app-state", state);
        }
        if (manualRefreshFailure) {
          manualRefreshFailure = false;
          throw {
            code: "manual_exclusion_refresh",
            message: "手动排除已保存，但当前名单刷新失败，请刷新后重试。",
          };
        }
        return { view: copy(manualView), added, skipped, invalid, removed };
      }
      if (command === "restore_filtered_members") {
        if (holdRestoration) {
          holdRestoration = false;
          await new Promise((resolve) => {
            releaseRestoration = resolve;
          });
          releaseRestoration = null;
        }
        if (restorationFailures > 0) {
          restorationFailures -= 1;
          throw {
            code: "snapshot_restoration",
            message: "合成恢复失败，请重试。",
          };
        }
        const base = snapshotsById.get(args.snapshotId);
        const chosen = new Set(args.qqNumbers);
        const entries =
          base?.filteredMembers.filter((entry) =>
            chosen.has(entry.member.qqNumber),
          ) ?? [];
        if (
          !base ||
          args.expectedRevision !== manualView.revision ||
          entries.length !== chosen.size ||
          entries.some((entry) => entry.reason === "manual")
        )
          throw {
            code: "snapshot_restoration",
            message: "合成名单已变化，请重新确认。",
          };
        const next = copy(base);
        next.id = `restored-${++restorationNumber}`;
        next.members.push(...entries.map((entry) => entry.member));
        next.members.sort((a, b) => a.qqNumber.localeCompare(b.qqNumber));
        next.filteredMembers = next.filteredMembers.filter(
          (entry) => !chosen.has(entry.member.qqNumber),
        );
        next.excludedCount -= entries.filter(
          (entry) => entry.reason === "group",
        ).length;
        next.friendFilteredCount =
          (next.friendFilteredCount ?? 0) -
          entries.filter((entry) => entry.reason === "friend").length;
        next.sentFilteredCount -= entries.filter(
          (entry) => entry.reason === "sent",
        ).length;
        next.restoration = {
          parentSnapshotId: base.id,
          adjustedAt: new Date().toISOString(),
          members: [...(base.restoration?.members ?? []), ...entries],
        };
        snapshotsById.set(next.id, copy(next));
        if (snapshots.get(next.group.id)?.id === base.id)
          snapshots.set(next.group.id, copy(next));
        if (state.snapshot?.id === base.id)
          state.snapshot = overlayManual(next);
        recipientPlans.clear();
        emit("app-state", state);
        return overlayManual(next);
      }
      if (command === "get_snapshot_view") {
        if (snapshotViewFailures > 0) {
          snapshotViewFailures -= 1;
          throw {
            code: "snapshot_read",
            message: "本机名单暂时无法读取，请重试。",
          };
        }
        const base = snapshotsById.get(args.snapshotId);
        if (!base)
          throw {
            code: "snapshot_missing",
            message: "此版本名单不存在，请重新获取。",
          };
        return overlayManual(base);
      }
      // 原生标题栏主题由窗口插件控制；此夹具只核对命令参数，不伪称看到了系统标题栏。
      if (command === "plugin:window|set_theme") return;
      if (command === "plugin:event|listen") {
        if (args.event === "mail-state" && mailStateUnavailable)
          throw {
            code: "fixture_mail_listener",
            message: "合成状态通道暂不可用。",
          };
        const id = nextId++;
        listeners.set(id, args);
        return id;
      }
      if (command === "plugin:event|unlisten") {
        listeners.delete(args.eventId);
        return;
      }
      if (command === "get_app_state") {
        if (workspaceUnavailable)
          throw {
            code: "fixture_workspace_read",
            message: "合成工作区读取暂不可用。",
          };
        if (holdWorkspaceRead) {
          holdWorkspaceRead = false;
          await new Promise((resolve) => {
            releaseWorkspaceRead = resolve;
          });
          releaseWorkspaceRead = null;
        }
        const initial = copy(state);
        if (initializeSessionRace) {
          window.__uiFixture.publishSession({
            status: "active",
            message: "当前账号已登录，关闭程序后需重新扫码。",
          });
          // 后到的启动读取仍返回恢复中，页面必须保留先到的有效会话事件。
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        return initial;
      }
      if (command === "get_mail_state") {
        if (mailStateUnavailable)
          throw {
            code: "fixture_mail_read",
            message: "合成发送状态暂不可用。",
          };
        if (holdMailStateRead) {
          holdMailStateRead = false;
          await new Promise((resolve) => {
            releaseMailStateRead = resolve;
          });
          releaseMailStateRead = null;
        }
        const initial = copy(mail);
        if (initializeMailRace) {
          mail = {
            ...mail,
            id: "mail-initial-race",
            status: "completed",
            phase: "idle",
            nextBatchAt: null,
            stateRevision: mail.stateRevision + 1,
            total: 1,
            sent: 1,
            message: "初始化期间先收到的新任务状态。",
          };
          emit("mail-state", mail);
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        return initial;
      }
      if (command === "get_member_mail_status") {
        if (holdMemberStatus) {
          holdMemberStatus = false;
          await new Promise((resolve) => {
            releaseMemberStatus = resolve;
          });
          releaseMemberStatus = null;
        }
        if (failMemberStatus) {
          failMemberStatus = false;
          throw {
            code: "database_error",
            message: "发送状态暂时无法读取，请刷新重试。",
          };
        }
        const statuses = args.qqNumbers.map((qqNumber) => {
          const email = `${qqNumber}@qq.com`;
          const sent = records.find((record) => record.email === email);
          return {
            qqNumber,
            status: unresolved.some((record) => record.email === email)
              ? "unresolved"
              : sent
                ? "sent"
                : "available",
            origin: sent?.origin ?? "",
          };
        });
        if (omitMemberStatus) {
          omitMemberStatus = false;
          return statuses.slice(1);
        }
        return statuses;
      }
      if (command === "get_mail_settings") return copy(mailSettingsView());
      if (command === "mail_accounts") {
        if (accountReadFailures > 0) {
          accountReadFailures -= 1;
          throw {
            code: "database_error",
            message: "发件账号暂时无法读取，请重试。",
          };
        }
        return copy(accountCatalog);
      }
      if (
        [
          "mail_account_save",
          "mail_account_remove",
          "mail_accounts_reorder",
          "mail_account_clear_password",
        ].includes(command)
      ) {
        if (state.task.status === "running" || mail.status === "running")
          throw { code: "mail_running", message: "请先停止当前任务。" };
        if (holdAccountMutation) {
          holdAccountMutation = false;
          await new Promise((resolve) => {
            releaseAccountMutation = resolve;
          });
          releaseAccountMutation = null;
        }
        if (accountMutationFailure) {
          accountMutationFailure = false;
          throw {
            code: "database_error",
            message: "发件账号保存失败，原有账号保留。",
          };
        }
        const next = copy(accountCatalog);
        if (command === "mail_account_save") {
          const { id, name, settings } = args.input;
          const existing = next.accounts.find((account) => account.id === id);
          if (id && !existing)
            throw {
              code: "mail_accounts_changed",
              message: "账号已移除，请刷新后重试。",
            };
          const binding = (account) =>
            JSON.stringify([
              account.host.trim().toLowerCase(),
              account.port,
              account.tlsMode,
              account.username,
              account.fromAddress.trim(),
            ]);
          if (
            next.accounts.some(
              (account) =>
                account.id !== id && binding(account) === binding(settings),
            )
          )
            throw {
              code: "mail_account_duplicate",
              message: "该发件账号已存在，请编辑原账号。",
            };
          const { password, ...publicSettings } = settings;
          const passwordSaved =
            !!password ||
            !!(
              existing?.passwordSaved && binding(existing) === binding(settings)
            );
          const output = {
            ...publicSettings,
            id: id ?? `sender-new-${++accountNumber}`,
            name: name.trim(),
            passwordSaved,
            passwordStatus: passwordSaved
              ? "saved"
              : existing?.passwordSaved
                ? "mismatch"
                : "missing",
            passwordMessage: passwordSaved
              ? "授权码已在本机加密保存，留空可继续使用。"
              : "尚未保存匹配授权码。",
          };
          if (existing)
            next.accounts[
              next.accounts.findIndex((account) => account.id === id)
            ] = output;
          else {
            if (next.accounts.length >= 20)
              throw {
                code: "mail_account_limit",
                message: "最多添加20个发件账号。",
              };
            next.accounts.push(output);
          }
        } else if (command === "mail_accounts_reorder") {
          if (
            args.ids.length !== next.accounts.length ||
            new Set(args.ids).size !== next.accounts.length ||
            args.ids.some(
              (id) => !next.accounts.some((account) => account.id === id),
            )
          )
            throw {
              code: "mail_accounts_selection",
              message: "账号顺序已变化，请刷新后重试。",
            };
          next.accounts = args.ids.map((id) =>
            next.accounts.find((account) => account.id === id),
          );
        } else {
          const account = next.accounts.find((value) => value.id === args.id);
          if (!account)
            throw {
              code: "mail_accounts_changed",
              message: "账号已移除，请刷新后重试。",
            };
          if (command === "mail_account_remove")
            next.accounts = next.accounts.filter(
              (value) => value.id !== args.id,
            );
          else {
            account.passwordSaved = false;
            account.passwordStatus = "missing";
            account.passwordMessage = "尚未保存授权码。";
          }
        }
        next.revision += 1;
        accountCatalog = next;
        recipientPlans.clear();
        persistAccountCatalog();
        return copy(accountCatalog);
      }
      if (command === "get_sent_history") {
        if (holdHistoryRead) {
          holdHistoryRead = false;
          await new Promise((resolve) => {
            releaseHistoryRead = resolve;
          });
          releaseHistoryRead = null;
        }
        if (historyFailureCount > 0) {
          historyFailureCount -= 1;
          throw {
            code: "fixture_history_read",
            message: "测试记录暂时无法读取，请重试。",
          };
        }
        return {
          records: copy(records),
          total: records.length,
          unresolved: copy(unresolved),
        };
      }
      if (command === "resolve_mail_delivery") {
        const record = unresolved.find((entry) => entry.id === args.id);
        if (!record) throw { message: "该测试记录已经核实。" };
        if (args.accepted)
          records.push({
            id: 2,
            qqNumber: "10000333",
            email: record.email,
            sentAt: record.updatedAt,
            origin: "manual_verified",
            senderAddress: record.senderAddress ?? "",
            senderName: record.senderName ?? "",
            note: "用户人工核实已发送，不代表 SMTP 接受证明。",
          });
        unresolved = unresolved.filter((entry) => entry.id !== args.id);
        state.historyCount = records.length;
        return {
          records: copy(records),
          total: records.length,
          unresolved: copy(unresolved),
        };
      }
      if (command === "resolve_mail_deliveries") {
        if (holdBulkResolution) {
          holdBulkResolution = false;
          await new Promise((resolve) => {
            releaseBulkResolution = resolve;
          });
          releaseBulkResolution = null;
        }
        if (failBulkResolution) {
          failBulkResolution = false;
          throw {
            code: "fixture_batch_resolution",
            message: "批量核实暂时失败，请重试；本批记录均未修改。",
          };
        }
        // 夹具模拟宿主的全批校验/原子事务；不能逐条成功一半后再报告整批失败。
        const ids = [...new Set(args.ids)];
        if (!ids.length || ids.some((id) => !Number.isSafeInteger(id)))
          throw { message: "请先选择有效的待核实记录。" };
        const selected = ids.map((id) =>
          unresolved.find((entry) => entry.id === id),
        );
        if (selected.some((entry) => !entry))
          throw { message: "部分记录已经更新，请刷新后重新选择。" };
        if (args.accepted)
          records.push(
            ...selected.map((entry, index) => ({
              id: 10000 + records.length + index,
              qqNumber: entry.email.endsWith("@qq.com")
                ? entry.email.split("@")[0]
                : "",
              email: entry.email,
              sentAt: entry.updatedAt,
              origin: "manual_verified",
              senderAddress: entry.senderAddress ?? "",
              senderName: entry.senderName ?? "",
              note: "用户人工批量核实已发送，不代表 SMTP 接受证明。",
            })),
          );
        unresolved = unresolved.filter((entry) => !ids.includes(entry.id));
        state.historyCount = records.length;
        return {
          records: copy(records),
          total: records.length,
          unresolved: copy(unresolved),
        };
      }
      if (command === "remove_sent_record") {
        records = records.filter((record) => record.id !== args.id);
        state.historyCount = records.length;
        return;
      }
      if (command === "add_sent_records") {
        state.historyCount += 1;
        return { added: 1, skipped: 0, invalid: 0 };
      }
      if (command === "import_sent_records") return null;
      if (command === "refresh_groups") return copy(state);
      if (command === "get_group_snapshot") {
        const key =
          args.groupIds?.length > 1
            ? `multi:${[...args.groupIds].sort().join(",")}`
            : args.groupId;
        return overlayManual(
          snapshots.get(key)?.account.qqNumber === state.account?.qqNumber
            ? snapshots.get(key)
            : null,
        );
      }
      // 多群夹具只模拟公开契约；真实合并、完整性、持久化和保护规则由Rust测试覆盖。
      if (command === "fetch_members" && args.groupIds?.length > 1) {
        const taskId = `fixture-task-${++taskNumber}`;
        const ids = [...new Set(args.groupIds)].sort();
        const key = `multi:${ids.join(",")}`;
        const fail = nextMultiGroupFailure;
        nextMultiGroupFailure = false;
        state.task = {
          id: taskId,
          status: "running",
          message: "正在读取目标群 1/2…",
        };
        const complete = () => {
          if (state.task.status !== "running" || state.task.id !== taskId)
            return;
          if (fail) {
            state.task = {
              id: taskId,
              status: "failed",
              message: "第二个目标群读取失败，保留上次结果。",
            };
            emit("app-state", state);
            return;
          }
          const sources = ids.map((id) => ({
            group: copy(state.groups.find((g) => g.id === id)),
            qqNumbers: (id === "900001"
              ? members.slice(0, multiGroupScenario === "large" ? 6000 : 4)
              : id === "900002"
                ? multiGroupScenario === "large"
                  ? members.slice(5000)
                  : members.slice(2, 6)
                : members.slice(0, 1)
            ).map((m) => m.qqNumber),
            reportedCount:
              id === "900003"
                ? 1
                : multiGroupScenario === "large"
                  ? id === "900001"
                    ? 6000
                    : 5005
                  : 4,
            status: "matched",
          }));
          const numbers = new Set(sources.flatMap((s) => s.qqNumbers));
          const all = members.filter((m) => numbers.has(m.qqNumber));
          const filteredMembers = args.excludeGroupIds?.length
            ? [{ member: all[0], reason: "group", detail: "排除丙群" }]
            : [];
          const base = {
            id: `snapshot-${taskNumber}`,
            account: copy(state.account),
            group: {
              id: key,
              name: `${ids.length}个群聊`,
              memberCount: null,
              role: "",
            },
            sourceGroups: sources,
            startedAt: new Date().toISOString(),
            finishedAt: new Date().toISOString(),
            members: all.filter(
              (m) =>
                !filteredMembers.some((f) => f.member.qqNumber === m.qqNumber),
            ),
            sourceCount: all.length,
            excludedCount: filteredMembers.length,
            exclusions: filteredMembers.length
              ? [
                  {
                    group: copy(state.groups[2]),
                    acquiredCount: 1,
                    reportedCount: 1,
                    status: "matched",
                    removedCount: 1,
                  },
                ]
              : [],
            excludeSent: args.excludeSent,
            excludeFriends: args.excludeFriends,
            sentFilteredCount: 0,
            friendFilteredCount: 0,
            friendUnknownCount: 0,
            friendFilterStatus: args.excludeFriends
              ? "completed"
              : "not_requested",
            filteredMembers,
            reportedCount: null,
            status: "matched",
            message: `已获取${ids.length}个群，合并去重后${all.length}人。`,
            duplicateCount:
              sources.reduce((n, s) => n + s.qqNumbers.length, 0) - all.length,
            invalidCount: 0,
            warnings: [],
          };
          snapshots.set(key, copy(base));
          snapshotsById.set(base.id, copy(base));
          sessionStorage.setItem(
            "__ui_fixture_multi_groups",
            JSON.stringify([...snapshots]),
          );
          state.snapshot = overlayManual(base);
          state.task = {
            id: taskId,
            status: "completed",
            message: "获取完成。",
          };
          emit("app-state", state);
        };
        if (holdFetch) {
          holdFetch = false;
          releaseFetch = complete;
        } else setTimeout(complete, 250);
        return copy(state);
      }
      if (command === "fetch_members") {
        const taskId = `fixture-task-${++taskNumber}`;
        // 好友 HTTP 只属于宿主；此处仅模拟任务最终错误与名单原子保留的页面合约。
        const friendListError = args.excludeFriends
          ? nextFriendListError
          : null;
        if (args.excludeFriends) nextFriendListError = null;
        state.task = {
          id: taskId,
          status: "running",
          message: "正在获取目标群…",
        };
        const exclude = args.excludeGroupIds?.length > 0;
        const fetchedGroup = state.groups.find(
          (group) => group.id === args.groupId,
        );
        if (!fetchedGroup) throw { message: "测试群不存在。" };
        const secondGroup = groupScenario && args.groupId === "900002";
        const sourceMembers = secondGroup
          ? members.slice(0, 7).map((member, index) => ({
              ...member,
              qqNumber: `2000000${index + 1}`,
              nickname: `独立群成员${index + 1}`,
            }))
          : members.map((member) => ({ ...member }));
        const sourceCount = sourceMembers.length;
        const friendUnknownCount = sourceMembers.filter(
          (member) => !["friend", "not_friend"].includes(member.friendRelation),
        ).length;
        const reportedCount =
          groupScenario && args.groupId === "900003" ? null : sourceCount;
        const filtered = [];
        if (exclude)
          filtered.push({
            member: sourceMembers.shift(),
            reason: "group",
            detail: "排除测试群",
          });
        if (args.excludeFriends) {
          const friendMembers = sourceMembers.filter(
            (member) => member.friendRelation === "friend",
          );
          friendMembers.forEach((member) =>
            filtered.push({
              member,
              reason: "friend",
              detail: "获取账号的 QQ 好友",
            }),
          );
          for (let index = sourceMembers.length - 1; index >= 0; index--)
            if (sourceMembers[index].friendRelation === "friend")
              sourceMembers.splice(index, 1);
        }
        if (args.excludeSent && !exclude && !secondGroup)
          filtered.push({
            member: sourceMembers.shift(),
            reason: "sent",
            detail: "人工已发送标记",
          });
        const completeFetch = () => {
          if (state.task.status !== "running" || state.task.id !== taskId)
            return;
          if (friendListError) {
            state.task = {
              id: taskId,
              status: "failed",
              message: friendListError.message,
            };
            emit("app-state", state);
            return;
          }
          if (args.excludeFriends && friendUnknownCount > 0) {
            state.task = {
              id: taskId,
              status: "failed",
              message: `无法确认 ${friendUnknownCount} 位成员的好友关系，本次结果未更新。`,
            };
            emit("app-state", state);
            return;
          }
          state.snapshot = {
            id: `snapshot-${taskNumber}`,
            account: copy(state.account),
            group: copy(fetchedGroup),
            startedAt: "2026-11-05T12:01:00+08:00",
            finishedAt: "2026-11-05T12:01:02+08:00",
            members: sourceMembers,
            sourceCount,
            excludedCount: exclude ? 1 : 0,
            exclusions: exclude
              ? [
                  {
                    group: groups[1],
                    acquiredCount: 2,
                    reportedCount: 2,
                    status: "matched",
                    removedCount: 1,
                  },
                ]
              : [],
            sentFilteredCount:
              args.excludeSent && !exclude && !secondGroup ? 1 : 0,
            filteredMembers: filtered,
            excludeSent: args.excludeSent,
            excludeFriends: args.excludeFriends ?? false,
            friendFilteredCount: filtered.filter(
              (entry) => entry.reason === "friend",
            ).length,
            friendUnknownCount,
            friendFilterStatus: args.excludeFriends
              ? "completed"
              : "not_requested",
            reportedCount,
            status: reportedCount === null ? "unverified" : "matched",
            message:
              reportedCount === null
                ? "未返回群总人数，仅展示已获取名单。"
                : "目标群人数一致；名单以获取时为准。",
            duplicateCount: 0,
            invalidCount: 0,
            warnings: [],
          };
          snapshots.set(fetchedGroup.id, copy(state.snapshot));
          snapshotsById.set(state.snapshot.id, copy(state.snapshot));
          state.snapshot = overlayManual(state.snapshot);
          // 后台公开视图以接口报告人数回填群列表，过滤后的条数只属于该次名单结果。
          fetchedGroup.memberCount = reportedCount;
          state.task = {
            id: taskId,
            status: "completed",
            message: "获取完成。",
          };
          emit("app-state", state);
        };
        if (holdFetch) {
          holdFetch = false;
          releaseFetch = completeFetch;
        } else {
          setTimeout(completeFetch, 250);
        }
        return copy(state);
      }
      if (command === "cancel_fetch") {
        state.task.status = "cancelled";
        state.task.message = "已取消。";
        emit("app-state", state);
        return copy(state);
      }
      if (command === "export_members")
        return { saved: true, path: `C:\\Fixture\\members.${args.format}` };
      if (command === "start_web_login") {
        qrGeneration += 1;
        qrPending = true;
        qrStatus = "wait";
        return {
          imageDataUrl:
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==",
          expiresIn: 180,
        };
      }
      if (command === "poll_web_login") {
        const generation = qrGeneration;
        if (holdQrPoll) {
          holdQrPoll = false;
          await new Promise((resolve) => {
            releaseQrPoll = resolve;
          });
          releaseQrPoll = null;
        }
        if (!qrPending || generation !== qrGeneration)
          throw {
            code: "login_cancelled",
            message: "本次扫码已取消，请重新扫码。",
          };
        if (failQrPoll) {
          failQrPoll = false;
          throw {
            code: "connection_failed",
            message: "合成扫码验证暂时失败，请刷新二维码。",
          };
        }
        if (qrStatus === "success") {
          qrPending = false;
          state.source = "web";
          state.account = nextQrAccount ?? {
            qqNumber: "123456789",
            nickname: "测试账号",
            source: "web",
          };
          state.groups = nextQrGroups ?? groups;
          state.snapshot = null;
          state.task = { id: "", status: "idle", message: "" };
          state.session = {
            status: "active",
            message: "当前账号已登录，关闭程序后需重新扫码。",
          };
          emit("app-state", state);
        }
        return {
          status: qrStatus,
          message: "夹具同义提示不应替换当前状态文案",
        };
      }
      if (command === "cancel_web_login") {
        qrGeneration += 1;
        qrPending = false;
        return;
      }
      if (command === "logout") {
        state.account = null;
        state.source = null;
        state.groups = [];
        state.snapshot = null;
        state.session = {
          status: "signed_out",
          message: "请扫码登录，关闭程序后需重新扫码。",
        };
        return copy(state);
      }
      if (command === "mail_pick_attachments") {
        const attachment = {
          id: "attachment-fixture",
          name: "本机测试附件.txt",
          size: 30,
        };
        attachmentResources.set(attachment.id, copy(attachment));
        return [attachment];
      }
      if (command === "mail_remove_attachment") {
        attachmentResources.delete(args.id);
        return;
      }
      if (
        command === "mail_add_inline_image" ||
        command === "mail_pick_inline_images"
      ) {
        if (holdInlineImage) {
          holdInlineImage = false;
          await new Promise((resolve) => {
            releaseInlineImage = resolve;
          });
          releaseInlineImage = null;
        }
        if (failInlineImage) {
          failInlineImage = false;
          throw {
            code: "fixture_image_registration",
            message: "测试图片暂时无法登记，请重试。",
          };
        }
        const image = {
          id: `inline-fixture-${++inlineImageNumber}`,
          contentId: `inline-fixture-${inlineImageNumber}@fixture.local`,
          name:
            command === "mail_pick_inline_images"
              ? "本机正文图片.png"
              : "粘贴截图.png",
          size: atob(inlinePngBase64).length,
          mimeType: "image/png",
          dataBase64: inlinePngBase64,
        };
        inlineResources.set(image.id, copy(image));
        return command === "mail_pick_inline_images" ? [image] : image;
      }
      if (command === "mail_remove_inline_image") return;
      if (command === "mail_clear_draft_content") {
        // 新建空模板时清掉当前宿主资源；收件范围、配置和模板库均不属于正文资源仓库。
        inlineResources.clear();
        attachmentResources.clear();
        return;
      }
      if (command.startsWith("mail_template_")) {
        const operation = command.slice("mail_template_".length);
        if (failTemplateOperation === operation) {
          failTemplateOperation = "";
          throw {
            code: "fixture_template_failure",
            message: "模板操作暂时失败，请重试。",
          };
        }
        if (templateTransferHold === operation) {
          templateTransferHold = "";
          await new Promise((resolve) => {
            releaseTemplateTransfer = resolve;
          });
          releaseTemplateTransfer = null;
        }
        if (operation === "export") {
          if (cancelTemplateExport) {
            cancelTemplateExport = false;
            return { saved: false, count: 0 };
          }
          const ids = [...new Set(args.ids)];
          const values = ids.map((id) =>
            templates.find((template) => template.id === id),
          );
          if (!ids.length || values.some((value) => !value))
            throw { message: "请重新选择导出模板。" };
          exportedTemplatePackages.push({ ids, templates: copy(values) });
          return { saved: true, count: ids.length };
        }
        if (operation === "import_preview") {
          if (!nextTemplatePackage) return null;
          const values = nextTemplatePackage;
          nextTemplatePackage = null;
          const token = `transfer-token-${++templateImportNumber}`;
          const names = values.map((value) => value.name);
          const entries = values.map((value) => ({
            name: value.name,
            subject: value.subject,
            inlineImageCount: value.inlineImages.length,
            attachmentCount: value.attachments.length,
            // 与宿主一致：统计仅指本机同名；源包重名另外限制覆盖策略。
            conflict: templates.some(
              (template) => template.name === value.name,
            ),
          }));
          pendingTemplateImport = {
            token,
            values: copy(values),
            catalog: JSON.stringify(templates),
            expired: false,
          };
          return {
            token,
            templates: entries,
            conflictCount: entries.filter((entry) => entry.conflict).length,
            overwriteAllowed: names.every(
              (name) =>
                names.filter((entry) => entry === name).length === 1 &&
                templates.filter((template) => template.name === name).length <=
                  1,
            ),
            totalBytes: 2048,
          };
        }
        if (operation === "import_cancel") {
          if (pendingTemplateImport?.token === args.token)
            pendingTemplateImport = null;
          return;
        }
        if (operation === "import_commit") {
          const prepared = pendingTemplateImport;
          if (!prepared || prepared.token !== args.token || prepared.expired)
            throw {
              code: "template_import_expired",
              message: "导入预览已过期，请重新选择文件。",
            };
          if (prepared.catalog !== JSON.stringify(templates))
            throw {
              code: "template_import_stale",
              message: "模板库已变化，请重新选择文件并确认。",
            };
          let added = 0,
            replaced = 0,
            skipped = 0;
          const output = copy(templates);
          const affected = [];
          for (const incoming of prepared.values) {
            const matches = output.filter(
              (value) => value.name === incoming.name,
            );
            if (matches.length && args.policy === "skip") {
              skipped += 1;
              continue;
            }
            if (matches.length && args.policy === "overwrite") {
              if (matches.length > 1)
                throw { message: "同名模板不唯一，请另存为副本。" };
              Object.assign(matches[0], copy(incoming), {
                id: matches[0].id,
                updatedAt: new Date().toISOString(),
              });
              affected.push(matches[0]);
              replaced += 1;
            } else {
              let name = incoming.name,
                number = 2;
              while (output.some((value) => value.name === name))
                name = `${incoming.name}（导入 ${number++}）`;
              const created = {
                ...copy(incoming),
                id: `imported-template-${templateImportNumber}-${++added}`,
                name,
                updatedAt: new Date().toISOString(),
              };
              output.push(created);
              affected.push(created);
            }
          }
          templates = output;
          pendingTemplateImport = null;
          persistTemplates();
          return {
            added,
            replaced,
            skipped,
            templates: affected.map(templateSummary),
          };
        }
        if (operation === "list") {
          const query = (args.query ?? "").trim().toLocaleLowerCase();
          return templates
            .filter((template) =>
              `${template.name} ${template.subject}`
                .toLocaleLowerCase()
                .includes(query),
            )
            .map(templateSummary);
        }
        if (operation === "save") {
          const input = args.input;
          const previous = input.updateId
            ? templates.find((template) => template.id === input.updateId)
            : null;
          if (input.updateId && !previous)
            throw { message: "模板已经不存在，请刷新列表。" };
          const template = {
            id:
              previous?.id ??
              `template-fixture-${Math.max(0, ...templates.map((entry) => Number(entry.id.split("-").at(-1)))) + 1}`,
            name: input.name.trim(),
            subject: input.subject,
            html: input.html,
            inlineImages: input.inlineImageIds.map((id) => {
              if (!inlineResources.has(id))
                throw new Error(
                  "Template save referenced a missing inline image",
                );
              return copy(inlineResources.get(id));
            }),
            attachments: input.includeAttachments
              ? input.attachmentIds.map((id) => {
                  if (!attachmentResources.has(id))
                    throw new Error(
                      "Template save referenced a missing attachment",
                    );
                  return copy(attachmentResources.get(id));
                })
              : [],
            updatedAt: new Date().toISOString(),
          };
          templates = [
            ...templates.filter((entry) => entry.id !== template.id),
            template,
          ];
          persistTemplates();
          return templateSummary(template);
        }
        const template = templates.find((entry) => entry.id === args.id);
        if (!template) throw { message: "模板已经不存在，请刷新列表。" };
        if (operation === "preview") return copy(template);
        if (operation === "apply") {
          if (holdTemplateApply) {
            holdTemplateApply = false;
            await new Promise((resolve) => {
              releaseTemplateApply = resolve;
            });
            releaseTemplateApply = null;
          }
          const output = copy(template);
          let html = output.html;
          const generation = ++templateApplyNumber;
          const newImages = output.inlineImages.map((image, index) => {
            const id = `applied-image-${generation}-${index}`;
            const contentId = `${id}@fixture.local`;
            html = html
              .replaceAll(`cid:${image.contentId}`, `cid:${contentId}`)
              .replaceAll(image.id, id);
            return { ...image, id, contentId };
          });
          const newAttachments = output.attachments.map(
            (attachment, index) => ({
              ...attachment,
              id: `applied-attachment-${generation}-${index}`,
            }),
          );
          // 所有资源准备完成后一起替换，与宿主原子应用边界对应；失败不清空原库。
          inlineResources.clear();
          attachmentResources.clear();
          newImages.forEach((image) =>
            inlineResources.set(image.id, copy(image)),
          );
          newAttachments.forEach((attachment) =>
            attachmentResources.set(attachment.id, copy(attachment)),
          );
          return {
            ...output,
            html,
            inlineImages: newImages,
            attachments: newAttachments,
          };
        }
        if (operation === "rename") {
          template.name = args.name.trim();
          template.updatedAt = new Date().toISOString();
          persistTemplates();
          return templateSummary(template);
        }
        if (operation === "duplicate") {
          const duplicate = {
            ...copy(template),
            id: `template-fixture-${Math.max(0, ...templates.map((entry) => Number(entry.id.split("-").at(-1)))) + 1}`,
            name: args.name.trim(),
            updatedAt: new Date().toISOString(),
          };
          templates.push(duplicate);
          persistTemplates();
          return templateSummary(duplicate);
        }
        if (operation === "delete") {
          templates = templates.filter((entry) => entry.id !== args.id);
          persistTemplates();
          return;
        }
      }
      if (command === "save_mail_settings") {
        const previousBinding = [
          mailSettings.host,
          mailSettings.port,
          mailSettings.tlsMode,
          mailSettings.username,
          mailSettings.fromAddress,
        ].join(":");
        mailSettings = copy(args.settings);
        const nextBinding = [
          mailSettings.host,
          mailSettings.port,
          mailSettings.tlsMode,
          mailSettings.username,
          mailSettings.fromAddress,
        ].join(":");
        if (args.password?.trim()) {
          passwordSaved = true;
          passwordStatus = "saved";
        } else if (previousBinding !== nextBinding && passwordSaved) {
          passwordSaved = false;
          passwordStatus = "mismatch";
        }
        const firstAccount = accountCatalog.accounts[0];
        const account = {
          ...mailSettingsView(),
          id: firstAccount?.id ?? "legacy-default",
          name: firstAccount?.name ?? "默认发件账号",
        };
        if (firstAccount) accountCatalog.accounts[0] = account;
        else accountCatalog.accounts.push(account);
        accountCatalog.revision += 1;
        recipientPlans.clear();
        persistAccountCatalog();
        return copy(mailSettingsView());
      }
      if (command === "clear_mail_password") {
        passwordSaved = false;
        passwordStatus = "missing";
        if (accountCatalog.accounts[0]) {
          accountCatalog.accounts[0].passwordSaved = false;
          accountCatalog.accounts[0].passwordStatus = "missing";
          accountCatalog.revision += 1;
          recipientPlans.clear();
          persistAccountCatalog();
        }
        return copy(mailSettingsView());
      }
      if (command === "prepare_mail_recipients") {
        if (holdRecipientPreparation) {
          holdRecipientPreparation = false;
          await new Promise((resolve) => {
            releaseRecipientPreparation = resolve;
          });
          releaseRecipientPreparation = null;
        }
        const input = args.input;
        const candidates = [
          ...new Set(
            input.recipients
              .split(/[;,\n\s]+/)
              .map((email) => email.trim().toLocaleLowerCase())
              .filter(Boolean),
          ),
        ];
        const excludedRecipients = candidates.filter((email) =>
          manualView.records.some((entry) => entry.email === email),
        );
        const allowed = candidates.filter(
          (email) => !excludedRecipients.includes(email),
        );
        if (
          allowed.some((email) =>
            unresolved.some((entry) => entry.email === email),
          )
        )
          throw {
            code: "fixture_unresolved_recipient",
            message: "收件人中有待核实投递，请先核实后再发送。",
          };
        const actual = input.skipSent
          ? allowed.filter(
              (email) =>
                !records.some(
                  (entry) => entry.email.toLocaleLowerCase() === email,
                ),
            )
          : allowed;
        const groupEmails = new Set(
          (input.groupAudience?.qqNumbers ?? []).map((qq) => `${qq}@qq.com`),
        );
        const groupMemberCount = actual.filter((email) =>
          groupEmails.has(email),
        ).length;
        const plan = {
          id: `recipient-plan-${++recipientPlanNumber}`,
          total: actual.length,
          skipped: allowed.length - actual.length,
          manualExcluded: excludedRecipients.length,
          excludedRecipients,
          exclusionRevision: manualView.revision,
          groupMemberCount,
          extraCount: actual.length - groupMemberCount,
          accountRevision:
            input.accountIds === undefined ? null : accountCatalog.revision,
          accountAssignments:
            input.accountIds === undefined
              ? []
              : assignAccounts(
                  actual,
                  input.accountIds,
                  input.sendMode ?? "bcc",
                  input.batchSize ?? 20,
                  input.allocationMode ?? "ordered",
                ),
        };
        if (
          input.accountIds !== undefined &&
          input.accountRevision !== accountCatalog.revision
        )
          throw {
            code: "mail_accounts_changed",
            message: "发件账号已变化，请重新确认。",
          };
        plan.totalMessageCount = plan.accountAssignments.length
          ? plan.accountAssignments.reduce(
              (sum, item) => sum + item.messageCount,
              0,
            )
          : input.sendMode === "individual"
            ? plan.total
            : Math.ceil(plan.total / (input.batchSize ?? 20));
        recipientPlans.set(plan.id, {
          view: copy(plan),
          actual: [...actual],
          accountIds:
            input.accountIds === undefined ? undefined : copy(input.accountIds),
          sendMode: input.sendMode,
          batchSize: input.batchSize,
          allocationMode: input.allocationMode ?? "ordered",
        });
        return copy(plan);
      }
      // 固定合成耗时只验证实时展示/生命周期，真实轮次与冷却公式由Rust回归覆盖。
      if (command === "estimate_prepared_mail_completion") {
        if (!recipientPlans.has(args.input.planId))
          throw { message: "合成计划已过期" };
        return Date.now() + 1_234_000;
      }
      if (command === "start_mail_job") {
        lastDraft = copy(args.draft);
        const plan = recipientPlans.get(args.draft.recipientPlanId);
        if (!plan) throw { message: "请重新确认发送名单。" };
        if (plan.view.exclusionRevision !== manualView.revision)
          throw {
            code: "mail_preparation_expired",
            message: "排除名单已变化，请重新确认。",
          };
        if (
          plan.accountIds !== undefined &&
          (args.draft.accountRevision !== accountCatalog.revision ||
            plan.view.accountRevision !== accountCatalog.revision ||
            JSON.stringify(
              orderedAccounts(args.draft.accountIds).map(
                (account) => account.id,
              ),
            ) !==
              JSON.stringify(
                orderedAccounts(plan.accountIds).map((account) => account.id),
              ) ||
            plan.sendMode !== args.draft.sendMode ||
            plan.batchSize !== args.draft.batchSize ||
            plan.allocationMode !== (args.draft.allocationMode ?? "ordered"))
        )
          throw {
            code: "mail_accounts_changed",
            message: "发件账号或分配已变化，请重新确认。",
          };
        lastAccountAssignments = copy(plan.view.accountAssignments ?? []);
        // 只模拟宿主已冻结的公开快照；夹具不读取真实凭据、不实现 SMTP 或后台随机算法。
        lastDeliverySenders = lastAccountAssignments.flatMap((account) =>
          Array.from({ length: account.recipientCount }, () => ({
            senderAddress: account.fromAddress,
            senderName: account.name,
          })),
        );
        // 确认时的固定集合直接供隔离发送任务使用；确认后历史变化不能偷偷重筛另一份名单。
        lastActualRecipients = [...plan.actual];
        recipientPlans.delete(args.draft.recipientPlanId);
        mail = {
          id: `mail-fixture-${++mailNumber}`,
          status: "running",
          phase: "sending",
          nextBatchAt: null,
          stateRevision: mail.stateRevision + 1,
          total: lastActualRecipients.length,
          sent: 0,
          failed: 0,
          skipped: 0,
          current: "fixture",
          message: "测试发送状态，不连接SMTP。",
          failures: [],
          groupAudience: args.draft.groupAudience
            ? {
                snapshotId: args.draft.groupAudience.snapshotId,
                groupId: groups[0].id,
                groupName: groups[0].name,
                memberCount: plan.view.groupMemberCount,
              }
            : null,
        };
        emit("mail-state", mail);
        const commandResponse = copy(mail);
        if (completeBeforeStartReturn) {
          completeBeforeStartReturn = false;
          window.__uiFixture.finishMail();
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        return commandResponse;
      }
      if (command === "skip_mail_wait") {
        if (holdWaitSkip) {
          holdWaitSkip = false;
          await new Promise((resolve) => {
            releaseWaitSkip = resolve;
          });
        }
        if (
          mail.id !== args.jobId ||
          mail.waitId !== args.waitId ||
          mail.status !== "running" ||
          mail.phase !== "waiting"
        ) {
          throw {
            code: "mail_wait_changed",
            message: "本次等待已结束或任务已变化，请查看最新发送状态。",
          };
        }
        return window.__uiFixture.publishMailState({
          phase: "sending",
          waitId: null,
          waitReason: null,
          nextBatchAt: null,
          estimatedCompletionAt: null,
          message: "已跳过本次等待，后续仍按原间隔发送。",
        });
      }
      if (command === "cancel_mail_job") {
        const staleRunningResponse = copy(mail);
        mail = {
          ...mail,
          status: "cancelled",
          phase: "idle",
          nextBatchAt: null,
          stateRevision: mail.stateRevision + 1,
          current: "",
          message: "测试取消。",
          waitId: null,
          waitReason: null,
          failures: [],
        };
        emit("mail-state", mail);
        // Deliberately return the pre-cancel state after the authoritative event.
        await new Promise((resolve) => setTimeout(resolve, 50));
        return staleRunningResponse;
      }
      throw {
        code: "unexpected_fixture_command",
        message: `测试未定义命令 ${command}`,
      };
    },
  };
}

/** 向真实 contentEditable 派发含图片 File 的粘贴事件，保持测试与用户粘贴入口一致。 */
async function pasteSyntheticImage(
  page,
  { mimeType = "image/png", size = 0 } = {},
) {
  await page.getByRole("textbox", { name: "邮件正文", exact: true }).evaluate(
    (element, input) => {
      element.focus();
      const range = document.createRange();
      range.selectNodeContents(element);
      range.collapse(false);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      const png = Uint8Array.from(
        atob(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==",
        ),
        (value) => value.charCodeAt(0),
      );
      const bytes = input.size ? new Uint8Array(input.size) : png;
      if (input.size) bytes.set(png);
      const transfer = new DataTransfer();
      transfer.items.add(
        new File([bytes], "粘贴截图.png", { type: input.mimeType }),
      );
      element.dispatchEvent(
        new ClipboardEvent("paste", {
          bubbles: true,
          cancelable: true,
          clipboardData: transfer,
        }),
      );
    },
    { mimeType, size },
  );
}

/** 使用浏览器真实右键输入，验证宿主页面取消菜单默认动作且不影响后续编辑、粘贴。 */
async function checkContextMenuBlocked(page, target) {
  await page.evaluate(() => {
    window.__uiFixture.lastContextMenu = null;
    document.addEventListener(
      "contextmenu",
      (event) => {
        window.__uiFixture.lastContextMenu = {
          prevented: event.defaultPrevented,
          trusted: event.isTrusted,
        };
      },
      { once: true },
    );
  });
  await target.click({ button: "right", position: { x: 12, y: 12 } });
  await page.waitForFunction(() => window.__uiFixture.lastContextMenu !== null);
  assert.deepEqual(
    await page.evaluate(() => window.__uiFixture.lastContextMenu),
    { prevented: true, trusted: true },
  );
}

/** 反馈通过Sonner的公开DOM呈现；只读定位可见内容，不读取组件内部私有状态。 */
function toastMessage(page, text) {
  return page.locator("[data-sonner-toast]").filter({ hasText: text }).last();
}

/** 浏览器可见不代表未被模态遮罩盖住；命中测试与真实指针操作共同验证浮层层级。 */
async function checkToastReceivesPointer(toast) {
  await toast.hover();
  assert.equal(
    await toast.evaluate((element) => {
      const bounds = element.getBoundingClientRect();
      const target = document.elementFromPoint(
        bounds.x + bounds.width / 2,
        bounds.y + bounds.height / 2,
      );
      return target !== null && element.contains(target);
    }),
    true,
    "Toast must receive pointer input above the native dialog scrim",
  );
}

/** 点击行、键盘和勾选框共用同一选择行为；真实拖选文本时不能顺便改变收件人。 */
async function checkMemberRowSelection(page, qqNumber) {
  const checkbox = page.getByLabel(`选择成员 ${qqNumber}`, { exact: true });
  await page.waitForFunction((number) => {
    const element = document.querySelector(
      `input[aria-label="选择成员 ${number}"]`,
    );
    return element && !element.disabled;
  }, qqNumber);
  const row = checkbox.locator("xpath=ancestor::tr");
  await row.locator(".qq-number").click();
  assert.equal(
    await checkbox.isChecked(),
    true,
    "Clicking a member row selects it",
  );
  await checkbox.click();
  assert.equal(
    await checkbox.isChecked(),
    false,
    "Checkbox clicks must toggle exactly once",
  );
  await row.locator(".qq-number").click();
  assert.equal(await checkbox.isChecked(), true);
  await row.focus();
  await page.keyboard.press("Space");
  assert.equal(
    await checkbox.isChecked(),
    false,
    "Focused rows support Space selection",
  );
  await page.keyboard.press("Space");
  assert.equal(await checkbox.isChecked(), true);

  const nameBounds = await row
    .locator("td")
    .nth(3)
    .evaluate((cell) => {
      const range = document.createRange();
      range.selectNodeContents(cell);
      const bounds = range.getBoundingClientRect();
      return {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
      };
    });
  await page.mouse.move(nameBounds.x + 1, nameBounds.y + nameBounds.height / 2);
  await page.mouse.down();
  await page.mouse.move(
    nameBounds.x + nameBounds.width - 1,
    nameBounds.y + nameBounds.height / 2,
    { steps: 12 },
  );
  await page.mouse.up();
  assert.equal(
    await page.evaluate(() => !!window.getSelection()?.toString()),
    true,
  );
  assert.equal(
    await checkbox.isChecked(),
    true,
    "Dragging member text must preserve its selection",
  );
  await page.evaluate(() => window.getSelection()?.removeAllRanges());
}

/** Verify that member selection, draft context, and delivery feedback form one workflow. */
/** 覆盖截图中的无解除入口：定位、逐项纠错、确认取消/失败重试/防重复、版本交接与草稿保留。 */
async function checkFilteredMemberRecovery() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { manualScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page
      .getByRole("button", { name: "排除其他群的成员", exact: false })
      .click();
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .check();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    const original = await page.evaluate(() => window.__uiFixture.snapshot());
    const blocked = original.filteredMembers[0].member.qqNumber;
    const chosen = original.members[1].qqNumber;
    const unselected = original.members[2].qqNumber;
    await page.getByLabel(`选择成员 ${chosen}`, { exact: true }).check();
    await page
      .getByRole("button", { name: "给已选 1 人写信", exact: true })
      .click();
    await page.getByLabel("主题", { exact: true }).fill("过滤恢复合成回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("恢复和导航均应保留此正文。");
    const input = page.locator("#mail-recipients");
    await input.fill(
      `${blocked}@QQ.COM；${blocked}@qq.com\n${unselected}@qq.com,valid@Example.test;bad-address`,
    );
    await page
      .getByRole("button", { name: "移除无效项 bad-address", exact: true })
      .click();
    assert.match(await input.inputValue(), /valid@Example.test/);
    assert.doesNotMatch(await input.inputValue(), /bad-address/);
    await page
      .getByRole("button", {
        name: `移除补充地址 ${blocked}@qq.com`,
        exact: true,
      })
      .click();
    assert.doesNotMatch(
      await input.inputValue(),
      new RegExp(`${blocked}@`, "i"),
    );
    assert.match(await input.inputValue(), /valid@Example.test/);
    await input.fill(
      `${blocked}@QQ.COM；${blocked}@qq.com\n${unselected}@qq.com,valid@Example.test`,
    );
    await page
      .getByRole("button", {
        name: `移除补充地址 ${unselected}@qq.com`,
        exact: true,
      })
      .click();
    assert.match(await input.inputValue(), new RegExp(`${blocked}@QQ.COM`));
    assert.doesNotMatch(await input.inputValue(), new RegExp(`${unselected}@`));
    await page
      .getByRole("button", { name: `调整成员 ${blocked}@qq.com`, exact: true })
      .click();
    await page.getByRole("tab", { name: "已过滤 1", exact: true }).waitFor();
    await page
      .getByRole("button", { name: `恢复成员 ${blocked}`, exact: true })
      .waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: `恢复成员 ${blocked}`, exact: true })
        .evaluate((button) => {
          const cell = button.closest("td").getBoundingClientRect();
          const bounds = button.getBoundingClientRect();
          return bounds.left >= cell.left && bounds.right <= cell.right;
        }),
      true,
      "Restoration action must fit completely inside its table cell",
    );
    assert.equal(
      await page.getByPlaceholder("搜索 QQ 号、昵称或群名片").inputValue(),
      blocked,
    );
    await page.screenshot({
      path: new URL(
        "filter-recovery-member-action.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await page
      .getByRole("button", { name: `恢复成员 ${blocked}`, exact: true })
      .click();
    const dialog = page.getByRole("dialog", {
      name: "恢复此成员到名单？",
      exact: true,
    });
    await dialog.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal((await calls("restore_filtered_members")).length, 0);
    await page
      .getByRole("button", { name: `恢复成员 ${blocked}`, exact: true })
      .click();
    await page.evaluate(() => window.__uiFixture.failNextRestoration());
    await dialog.getByRole("button", { name: "确认恢复", exact: true }).click();
    await dialog
      .getByRole("alert")
      .filter({ hasText: "合成恢复失败" })
      .waitFor();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.snapshot())).id,
      original.id,
    );
    await page.evaluate(() => window.__uiFixture.holdNextRestoration());
    await dialog.getByRole("button", { name: "确认恢复", exact: true }).click();
    await page.waitForFunction(() => window.__uiFixture.restorationPending());
    assert.equal(
      await dialog
        .getByRole("button", { name: "正在恢复…", exact: true })
        .isDisabled(),
      true,
    );
    await page.keyboard.press("Escape");
    assert.equal(await dialog.isVisible(), true);
    await page.evaluate(() => window.__uiFixture.releaseRestoration());
    await page.getByLabel(`选择成员 ${blocked}`, { exact: true }).waitFor();
    assert.equal((await calls("restore_filtered_members")).length, 2);
    await page.getByLabel(`选择成员 ${blocked}`, { exact: true }).check();
    await page
      .getByRole("button", { name: "给已选 2 人写信", exact: true })
      .click();
    await page
      .getByRole("dialog")
      .getByRole("button", { name: /确认|替换/ })
      .click();
    await page.getByRole("heading", { name: "写邮件", exact: true }).waitFor();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "过滤恢复合成回归",
    );
    assert.match(
      await page
        .getByRole("textbox", { name: "邮件正文", exact: true })
        .innerText(),
      /保留此正文/,
    );
    assert.equal(await page.locator("#mail-recipient-blocked").count(), 0);
    assert.match(await input.inputValue(), /valid@Example.test/);
    // 未勾选也有明确定位路径；移除全部受阻项不删除合法补充地址。
    await input.fill(`${unselected}@qq.com;valid@Example.test;bad1;bad2`);
    await page
      .getByRole("button", { name: "移除全部无效项", exact: true })
      .click();
    await page
      .getByRole("button", { name: "移除全部受阻地址", exact: true })
      .click();
    assert.match(await input.inputValue(), /valid@Example.test/);
    assert.equal(await input.getAttribute("aria-invalid"), "false");
    for (const width of testWidths([1920])) {
      await page.setViewportSize(testViewport(width, 1080));
      await input.fill(
        `${unselected}@qq.com;another-address-with-a-long-local-name@example.test;bad-address`,
      );
      await page.locator("#mail-recipient-blocked").scrollIntoViewIfNeeded();
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
        true,
      );
      await page.screenshot({
        path: new URL(
          `filter-recovery-${width}.png`,
          artifacts,
        ).pathname.replace(/^\/(\w:)/, "$1"),
      });
    }
    // 手动排除不能使用名单例外绕过；入口直接定位对应完整邮箱，普通重返保留后续搜索。
    await page.setViewportSize(testViewport(1920, 1080));
    await page
      .getByRole("button", {
        name: `调整成员 ${unselected}@qq.com`,
        exact: true,
      })
      .click();
    await page
      .getByRole("button", { name: `排除成员 ${unselected}`, exact: true })
      .click();
    await page
      .getByRole("dialog", { name: "确认手动排除", exact: true })
      .getByRole("button", { name: "确认排除", exact: true })
      .click();
    await page.getByRole("tab", { name: "已过滤 1", exact: true }).click();
    await page
      .getByRole("button", { name: `移除手动排除 ${unselected}`, exact: true })
      .click();
    const manualSearch = page.getByRole("textbox", {
      name: "搜索手动排除",
      exact: true,
    });
    assert.equal(await manualSearch.inputValue(), `${unselected}@qq.com`);
    const remove = page.getByRole("button", { name: /^移除排除记录 / });
    await remove.click();
    await page
      .getByRole("group", { name: "确认移除手动排除", exact: true })
      .getByRole("button", { name: "确认移除", exact: true })
      .click();
    await toastMessage(page, /已移除 1 条手动排除/).waitFor();
    await manualSearch.fill("用户后来输入的搜索词");
    await page.getByRole("button", { name: "成员提取", exact: true }).click();
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    assert.equal(await manualSearch.inputValue(), "用户后来输入的搜索词");
    assert.equal((await calls("start_mail_job")).length, 0);
    console.log(
      "Filtered member recovery: address correction/targeting, immutable version handoff, cancel/failure/retry/duplicate lock, manual rule targeting/removal/query retention, draft preservation and 1920x1080 layout checked (synthetic only)",
    );
  } finally {
    await context.close();
  }
}

async function checkGroupMailFlow(page) {
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const snapshotBeforeInitialFetch = await page.evaluate(
    () => window.__uiFixture.snapshot()?.id ?? null,
  );
  await page.getByRole("button", { name: "获取成员", exact: true }).click();
  await page.waitForFunction((previousId) => {
    const snapshot = window.__uiFixture.snapshot();
    return (
      window.__uiFixture.taskStatus() === "completed" &&
      snapshot?.id !== previousId
    );
  }, snapshotBeforeInitialFetch);
  await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
  const firstSnapshot = await page.evaluate(() =>
    window.__uiFixture.snapshot(),
  );
  const firstQq = firstSnapshot.members[0].qqNumber;
  const nextPageQq = firstSnapshot.members[50].qqNumber;
  await checkMemberRowSelection(page, firstQq);
  await page.getByRole("button", { name: "下一页成员", exact: true }).click();
  await page.getByLabel(`选择成员 ${nextPageQq}`, { exact: true }).check();
  // 同一个主导航保留各工作区状态，查看记录不能丢失跨页的成员勾选。
  await page
    .locator(".app-nav")
    .getByRole("button", { name: /已发送记录/ })
    .click();
  await page
    .getByRole("heading", { name: "已发送记录", exact: true })
    .waitFor();
  assert.match(
    await page.locator('.app-nav [aria-current="page"]').innerText(),
    /^已发送记录\s*1$/,
  );
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.getByRole("button", { name: "返回成员提取", exact: true }).click();
  await page
    .getByRole("button", { name: "给已选 2 人写信", exact: true })
    .waitFor();
  assert.equal(
    await page
      .getByLabel(`选择成员 ${nextPageQq}`, { exact: true })
      .isChecked(),
    true,
  );
  // 成员表在独立内容区内滚动；交接后写信页应从顶部开始，先露出来源和范围。
  await page.locator(".main-content").evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  assert.equal(
    await page
      .locator(".main-content")
      .evaluate((element) => element.scrollTop > 0),
    true,
    "The handoff regression must start from a scrolled member page",
  );
  await page
    .getByRole("button", { name: "给已选 2 人写信", exact: true })
    .click();
  await page.getByRole("heading", { name: "写邮件", exact: true }).waitFor();
  await page.waitForFunction(() => {
    const activePage = [...document.querySelectorAll(".page-scroll")].find(
      (element) => element.getClientRects().length > 0,
    );
    return window.scrollY === 0 && activePage?.scrollTop === 0;
  });
  assert.equal(
    await page.locator("#mail-audience-heading").evaluate((heading) => {
      const bounds = heading.getBoundingClientRect();
      return bounds.top >= 0 && bounds.bottom <= window.innerHeight;
    }),
    true,
    "The source group heading must be visible after handing members to the composer",
  );
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /2/,
  );
  assert.match(await page.locator(".mail-audience").innerText(), /目标测试群/);
  await page
    .getByRole("button", { name: "查看群收件人（2）", exact: true })
    .click();
  assert.equal(await page.locator(".mail-audience-roster tbody tr").count(), 2);
  // 预览搜索只缩小展示，清除后归还焦点并保留固定收件范围。
  const audienceSearch = page.getByLabel("搜索本封群收件人", { exact: true });
  await audienceSearch.fill("没有这个收件人");
  await page
    .getByRole("button", { name: "清除群收件人搜索", exact: true })
    .click();
  assert.equal(
    await audienceSearch.evaluate(
      (element) => document.activeElement === element,
    ),
    true,
  );
  assert.equal(await page.locator(".mail-audience-roster tbody tr").count(), 2);
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /2/,
  );
  assert.match(
    await page.locator(".mail-audience-roster").innerText(),
    new RegExp(nextPageQq),
  );
  const draftSubject = "群成员交互回归，不发送真实邮件";
  const draftBody = "返回成员页调整范围后，这段正文和附件应保留。";
  await page.getByLabel("主题", { exact: true }).fill(draftSubject);
  await page
    .getByRole("textbox", { name: "邮件正文", exact: true })
    .fill(draftBody);
  await checkContextMenuBlocked(
    page,
    page.getByRole("textbox", { name: "邮件正文", exact: true }),
  );
  await page.evaluate(() => window.__uiFixture.holdNextInlineImage());
  await pasteSyntheticImage(page);
  await page.waitForFunction(() => window.__uiFixture.inlineImagePending());
  for (const button of await page.locator(".app-nav > button").all()) {
    assert.equal(
      await button.isDisabled(),
      true,
      "Navigation must wait until inline image registration completes",
    );
  }
  assert.equal(
    await page.getByRole("button", { name: "发送", exact: true }).isDisabled(),
    true,
  );
  await page.evaluate(() => window.__uiFixture.releaseInlineImage());
  const inlineEditor = page.getByRole("textbox", {
    name: "邮件正文",
    exact: true,
  });
  await inlineEditor.locator("img[data-inline-image-id]").waitFor();
  const pastedInlineId = await inlineEditor
    .locator("img[data-inline-image-id]")
    .getAttribute("data-inline-image-id");
  assert.equal(pastedInlineId, "inline-fixture-1");
  assert.match(
    await inlineEditor.locator("img[data-inline-image-id]").getAttribute("src"),
    /^blob:/,
  );
  assert.equal((await inlineEditor.innerText()).trim(), draftBody);
  await page.getByRole("button", { name: "插入图片", exact: true }).click();
  await page
    .getByRole("button", { name: "移除正文图片 本机正文图片.png", exact: true })
    .waitFor();
  assert.equal(
    await inlineEditor.locator("img[data-inline-image-id]").count(),
    2,
  );
  await page
    .getByRole("button", { name: "移除正文图片 本机正文图片.png", exact: true })
    .click();
  assert.equal(
    await inlineEditor.locator("img[data-inline-image-id]").count(),
    1,
  );

  // 大图及不受支持的类型需要清楚反馈，同时保持此前正文和已插入图片。
  const beforeInvalidImages = (await calls("mail_add_inline_image")).length;
  await pasteSyntheticImage(page, { size: 5 * 1024 * 1024 + 1 });
  await toastMessage(page, /5|过大|大小/).waitFor();
  await pasteSyntheticImage(page, { mimeType: "image/svg+xml" });
  await toastMessage(page, /格式|类型|PNG|JPEG/).waitFor();
  assert.equal(await page.locator(".mail-composer > .notice").count(), 0);
  assert.equal(
    (await calls("mail_add_inline_image")).length,
    beforeInvalidImages,
  );
  await page.evaluate(() => window.__uiFixture.failNextInlineImage());
  await pasteSyntheticImage(page);
  await page
    .getByText("测试图片暂时无法登记，请重试。", { exact: true })
    .waitFor();
  assert.equal(
    await inlineEditor.locator("img[data-inline-image-id]").count(),
    1,
  );
  await page.getByRole("button", { name: "添加附件", exact: true }).click();
  await page
    .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
    .waitFor();
  assert.equal(
    await inlineEditor.locator("img[data-inline-image-id]").count(),
    1,
  );
  assert.equal(
    (await calls("start_mail_job")).length,
    0,
    "Choosing members must never send mail",
  );

  // 从写信进入记录后可明确返回，主题、正文、附件和冻结收件范围都必须保留。
  await page.getByRole("button", { name: /已发送记录/ }).click();
  await page
    .getByRole("heading", { name: "已发送记录", exact: true })
    .waitFor();
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.screenshot({
    path: new URL("history-page-dark.png", artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
    fullPage: false,
  });
  await page.getByRole("button", { name: "返回写邮件", exact: true }).click();
  assert.equal(
    await page.getByLabel("主题", { exact: true }).inputValue(),
    draftSubject,
  );
  assert.equal((await inlineEditor.innerText()).trim(), draftBody);
  await page
    .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
    .waitFor();
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /2/,
  );
  assert.equal(
    await page
      .locator('.app-nav [aria-current="page"]')
      .getAttribute("aria-label"),
    "写邮件",
  );

  // Header selection affects only the visible page; the second page keeps its own choice.
  await page.getByRole("button", { name: "调整群成员", exact: true }).click();
  await page.getByLabel("全选本页成员", { exact: true }).check();
  await page
    .getByRole("button", { name: "给已选 51 人写信", exact: true })
    .waitFor();
  await page.getByLabel("取消全选本页成员", { exact: true }).uncheck();
  await page
    .getByRole("button", { name: "给已选 1 人写信", exact: true })
    .waitFor();
  const memberSearch = page.getByRole("textbox", {
    name: "搜索成员QQ号、昵称或群名片",
  });
  await memberSearch.fill("测试成员2");
  await page
    .getByRole("button", { name: "全选搜索结果 11 人", exact: true })
    .click();
  await page
    .getByRole("button", { name: "给已选 12 人写信", exact: true })
    .waitFor();
  await page.getByRole("button", { name: "清空成员选择", exact: true }).click();
  await page
    .getByRole("button", { name: "全选搜索结果 11 人", exact: true })
    .click();
  await page
    .getByRole("button", { name: "给已选 11 人写信", exact: true })
    .click();
  await page
    .getByRole("dialog", { name: "替换这封邮件的群成员？", exact: true })
    .waitFor();
  await page
    .getByRole("button", { name: "保留原收件范围", exact: true })
    .click();
  await page.getByRole("button", { name: "写邮件", exact: true }).click();
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /2/,
  );
  assert.equal(
    await page.getByLabel("主题", { exact: true }).inputValue(),
    draftSubject,
  );
  assert.equal((await inlineEditor.innerText()).trim(), draftBody);
  await page
    .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
    .waitFor();

  // Confirming a new range replaces recipients, while the composed message stays intact.
  await page.getByRole("button", { name: "调整群成员", exact: true }).click();
  await page.getByRole("button", { name: "清空成员选择", exact: true }).click();
  await page
    .getByLabel(`选择成员 ${firstSnapshot.members[1].qqNumber}`, {
      exact: true,
    })
    .check();
  await page
    .getByRole("button", { name: "给已选 1 人写信", exact: true })
    .click();
  await page
    .getByRole("button", { name: "替换群成员并写信", exact: true })
    .click();
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /1/,
  );
  assert.equal(
    await page.getByLabel("主题", { exact: true }).inputValue(),
    draftSubject,
  );
  assert.equal((await inlineEditor.innerText()).trim(), draftBody);

  await page.getByRole("button", { name: "调整群成员", exact: true }).click();
  const snapshotBeforeRefresh = await page.evaluate(
    () => window.__uiFixture.snapshot()?.id ?? null,
  );
  await page.getByRole("button", { name: "获取成员", exact: true }).click();
  await page.waitForFunction((previousId) => {
    const snapshot = window.__uiFixture.snapshot();
    return (
      window.__uiFixture.taskStatus() === "completed" &&
      snapshot?.id !== previousId
    );
  }, snapshotBeforeRefresh);
  await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
  const nextSnapshot = await page.evaluate(() => window.__uiFixture.snapshot());
  assert.notEqual(nextSnapshot.id, firstSnapshot.id);
  await page.getByRole("button", { name: "写邮件", exact: true }).click();
  await page.getByText(/该群已有更新的名单，本封仍使用已确认版本/).waitFor();
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /1/,
  );
  await page.getByRole("button", { name: "调整群成员", exact: true }).click();
  await page
    .locator(".saved-audience-note")
    .filter({ hasText: "正在调整邮件中的已保存名单" })
    .waitFor();
  // 调整入口恢复的是邮件原版本；显式重新选择同一群才切换到该群最新获取结果。
  await page
    .locator(".group-list")
    .getByRole("button", { name: /目标测试群/ })
    .click();
  await memberSearch.fill("测试成员2");
  await page
    .getByRole("button", { name: "给全部 61 人写信", exact: true })
    .click();
  await page
    .getByRole("button", { name: "替换群成员并写信", exact: true })
    .click();
  assert.match(
    await page.locator(".mail-audience-counts strong").innerText(),
    /61/,
  );
  assert.equal(
    await page.getByLabel("主题", { exact: true }).inputValue(),
    draftSubject,
  );
  assert.equal((await inlineEditor.innerText()).trim(), draftBody);

  // 补充栏使用完整邮箱；多品牌、多行和常见分隔符兼容，重复地址只计算一次。
  const supplemental = page.locator("#mail-recipients");
  assert.equal(
    await supplemental.getAttribute("placeholder"),
    "someone@qq.com\nsomeone@gmail.com",
  );
  await supplemental.fill(
    "person@gmail.com\nadditional@qq.com\ncontact@example.test;person@gmail.com,contact@example.test",
  );
  assert.match(
    await page.locator(".mail-recipient-footer").innerText(),
    /已补充 3 个邮箱/,
  );
  assert.equal(await supplemental.getAttribute("aria-invalid"), "false");
  await supplemental.fill("123456789");
  await page
    .locator(".mail-recipient-boundary")
    .filter({ hasText: "地址不完整" })
    .waitFor();
  assert.equal(await supplemental.getAttribute("aria-invalid"), "true");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await toastMessage(page, /地址不完整.*完整邮箱/).waitFor();
  assert.equal(
    await page
      .getByRole("dialog", { name: "确认发送", exact: true })
      .isVisible(),
    false,
  );

  // A supplemental QQ address must not reintroduce someone removed by group/history filtering.
  await page.locator("#mail-recipients").fill("10000001@qq.com");
  await page.getByText(/有 1 位补充地址是本群已过滤或未选成员/).waitFor();
  await page.getByRole("button", { name: "发送", exact: true }).click();
  assert.equal(
    await page
      .getByRole("dialog", { name: "确认发送", exact: true })
      .isVisible(),
    false,
  );
  assert.equal((await calls("start_mail_job")).length, 0);
  await page
    .locator("#mail-recipients")
    .fill(`${nextSnapshot.members[0].qqNumber}@qq.com\nother@example.test`);
  assert.match(
    await page.locator(".mail-recipient-footer").innerText(),
    /已补充 1 个邮箱/,
  );
  await page.getByRole("button", { name: "预览", exact: true }).click();
  assert.equal(
    await page
      .locator("iframe[title='安全邮件正文预览']")
      .getAttribute("sandbox"),
    "",
  );
  const previewImage = page
    .frameLocator("iframe[title='安全邮件正文预览']")
    .locator("img");
  await previewImage.waitFor();
  assert.match(
    await previewImage.getAttribute("src"),
    /^data:image\/png;base64,/,
  );
  assert.equal(
    await previewImage.evaluate(
      (image) => image.complete && image.naturalWidth > 0,
    ),
    true,
  );
  await page.screenshot({
    path: new URL("mail-inline-preview.png", artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
    fullPage: false,
  });
  await page.getByRole("button", { name: "关闭正文预览", exact: true }).click();
  await page.getByRole("button", { name: "发件设置", exact: true }).click();
  await page
    .getByLabel("SMTP 登录账号", { exact: true })
    .fill("sender@example.test");
  await page
    .getByLabel("密码或授权码", { exact: true })
    .fill("fixture-only-not-a-real-secret");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await page.getByRole("button", { name: "发送", exact: true }).click();
  const confirmation = page.getByRole("dialog", {
    name: "确认发送",
    exact: true,
  });
  await confirmation.waitFor();
  assert.match(await confirmation.innerText(), /目标测试群/);
  assert.match(
    await confirmation.locator(".mail-confirm-summary strong").innerText(),
    /^62\s*人$/,
  );
  assert.match(
    await confirmation.locator(".mail-confirm-summary p").innerText(),
    /群成员 61 人 · 补充邮箱 1 个/,
  );
  assert.match(await confirmation.innerText(), /预计 4 封/);
  assert.doesNotMatch(
    await confirmation.innerText(),
    /名单范围|排除群|获取时间|目标 62 人/,
  );
  await page.screenshot({
    path: new URL("mail-confirm-compact.png", artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
    fullPage: false,
  });
  assert.equal(
    (await calls("start_mail_job")).length,
    0,
    "Confirmation is required after audience handoff",
  );
  await page.getByRole("button", { name: "确认发送", exact: true }).click();
  await page
    .getByRole("button", { name: "停止后续发送", exact: true })
    .waitFor();
  const sendCall = (await calls("start_mail_job"))[0];
  assert.deepEqual(sendCall.args.draft.groupAudience, {
    snapshotId: nextSnapshot.id,
    qqNumbers: nextSnapshot.members.map((member) => member.qqNumber),
  });
  assert.equal(
    sendCall.args.draft.recipients.split(";").length,
    62,
    "All uses the full final snapshot despite an active search",
  );
  assert.equal(
    sendCall.args.draft.recipients.includes("10000001@qq.com"),
    false,
  );
  assert.deepEqual(sendCall.args.draft.attachmentIds, ["attachment-fixture"]);
  assert.deepEqual(sendCall.args.draft.inlineImageIds, [pastedInlineId]);
  assert.match(
    sendCall.args.draft.html,
    /src="cid:inline-fixture-1@fixture\.local"/,
  );
  assert.doesNotMatch(
    sendCall.args.draft.html,
    /blob:|data:image|inline-fixture-2/,
  );
  assert.equal(sendCall.args.draft.subject, draftSubject);
  assert.match(sendCall.args.draft.html, new RegExp(draftBody));
  assert.equal(sendCall.args.draft.sendMode, "bcc");
  assert.equal(sendCall.args.draft.batchSize, 20);
  assert.equal(sendCall.args.draft.intervalSeconds, 900);
  assert.equal(sendCall.args.draft.continueOnFailure, false);
  assert.match(sendCall.args.draft.recipientPlanId, /^recipient-plan-/);
  await page.getByRole("button", { name: "停止后续发送", exact: true }).click();
  await page.getByRole("button", { name: "发送", exact: true }).waitFor();
  await page.waitForFunction(
    () => document.querySelector(".mail-footer .primary")?.disabled === false,
  );
  await page.locator(".mail-progress-cancelled").waitFor();
  assert.equal(
    await page
      .getByRole("button", { name: "调整群成员", exact: true })
      .isDisabled(),
    false,
    "A stale cancel response must not restore the running lock",
  );

  // A later explicit send can complete; feedback refreshes rows without rewriting the frozen result.
  await page.getByRole("button", { name: "发件设置", exact: true }).click();
  await page
    .getByLabel("密码或授权码", { exact: true })
    .fill("fixture-only-not-a-real-secret");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await page.evaluate(() => window.__uiFixture.completeNextStartBeforeReturn());
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await page.getByRole("button", { name: "确认发送", exact: true }).click();
  await page.locator(".mail-progress-completed").waitFor();
  await page
    .getByRole("button", { name: "返回成员查看发送状态", exact: true })
    .click();
  await page.locator(".member-mail-status-sent").first().waitFor();
  await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
  await page.evaluate(
    (qqNumber) => window.__uiFixture.markUnresolved(qqNumber),
    nextSnapshot.members[0].qqNumber,
  );
  await page.getByRole("button", { name: "刷新发送状态", exact: true }).click();
  await page.locator(".member-mail-status-unresolved").first().waitFor();
  const unresolvedCheckbox = page.getByLabel(
    `选择成员 ${nextSnapshot.members[0].qqNumber}`,
    { exact: true },
  );
  assert.equal(
    await page
      .getByLabel(`选择成员 ${nextSnapshot.members[0].qqNumber}`, {
        exact: true,
      })
      .isDisabled(),
    true,
  );
  const unresolvedRow = unresolvedCheckbox.locator("xpath=ancestor::tr");
  await unresolvedRow.locator(".qq-number").click();
  assert.equal(
    await unresolvedCheckbox.isChecked(),
    false,
    "Disabled rows must not bypass unresolved delivery restrictions",
  );
  await unresolvedRow
    .getByRole("button", { name: "去核实", exact: true })
    .click();
  await page
    .getByRole("heading", { name: "已发送记录", exact: true })
    .waitFor();
  await page.getByRole("button", { name: "返回成员提取", exact: true }).click();
  assert.equal(
    await unresolvedCheckbox.isChecked(),
    false,
    "Row actions must not change its recipient selection",
  );
  assert.equal(
    await page
      .getByRole("button", { name: "给全部 61 人写信", exact: true })
      .isDisabled(),
    true,
  );
  assert.match(await page.locator(".export-bar").innerText(), /全部 61 人/);
  console.log(
    "Group selection, frozen audience, replacement, draft preservation, payload and delivery feedback checked",
  );
  await page.screenshot({
    path: new URL("members-mail-feedback.png", artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
    fullPage: false,
  });
  await page.getByRole("button", { name: "写邮件", exact: true }).click();
  await page.getByRole("button", { name: "收起群收件人", exact: true }).click();
  await page.screenshot({
    path: new URL("group-mail-desktop.png", artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
    fullPage: false,
  });
  for (const width of testWidths([780, 700, 375])) {
    await page.setViewportSize(testViewport(width, 900));
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth + 1,
      ),
      true,
      `group mail overflow at ${width}`,
    );
    await page.screenshot({
      path: new URL(`group-mail-${width}.png`, artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
  }
}

/** 加载和错误留在记录工作区内，重试成功前不显示可误认为完整的空名单。 */
async function checkHistoryLoadingAndRetry() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { historyFailures: 1 });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.evaluate(() => window.__uiFixture.holdNextHistoryRead());
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await page.waitForFunction(() => window.__uiFixture.historyReadPending());
    await page.locator(".history-loading").waitFor();
    assert.notEqual(
      await page.locator(".history-entry-card").getAttribute("open"),
      null,
    );
    assert.equal(await page.getByRole("dialog").count(), 0);
    assert.equal(
      await page
        .getByRole("button", { name: "添加记录", exact: true })
        .isDisabled(),
      true,
      "History mutations must remain disabled while its authoritative data is loading",
    );
    // 读取可以继续后台完成，用户离开后仍可再次进入；同一轮载入不重复请求。
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.getByRole("heading", { name: "写邮件", exact: true }).waitFor();
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await page.locator(".history-loading").waitFor();
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "get_sent_history",
          ).length,
      ),
      1,
      "Reopening a loading history page must reuse the pending read",
    );
    await page.evaluate(() => window.__uiFixture.releaseHistoryRead());
    await page.locator(".history-error").waitFor();
    assert.match(
      await page.locator(".history-error").innerText(),
      /测试记录暂时无法读取/,
    );
    assert.equal(
      await page.getByText("还没有已发送记录，可以在上方添加或导入。").count(),
      0,
    );
    await page
      .getByRole("button", { name: "重新加载记录", exact: true })
      .click();
    await page
      .locator(".history-table-wrap td")
      .filter({ hasText: "导入记录" })
      .waitFor();
    assert.equal(await page.locator(".history-error").count(), 0);
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "get_sent_history",
          ).length,
      ),
      2,
      "A failed read needs exactly one explicit retry",
    );
  } finally {
    await context.close();
  }
}

/**
 * 手动排除读取期间允许离页，新revision不能被旧响应覆盖后永久丢失。
 * 延迟响应及失败均使用隔离IPC，补读必须合并为一次且未完成前不能维护旧记录。
 */
async function checkManualExclusionRefreshRace() {
  for (const failOldRead of [false, true]) {
    const context = await browser.newContext({
      viewport: testViewport(1200, 900),
    });
    await context.addInitScript(installFixture, { manualScenario: true });
    const page = await context.newPage();
    page.on("pageerror", (error) => pageErrors.push(error.message));
    const manual = page.locator(".manual-exclusions-page");
    const readCount = () =>
      page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "get_manual_exclusions",
          ).length,
      );
    try {
      await page.goto("http://127.0.0.1:1420");
      await page.evaluate(() =>
        window.__uiFixture.excludeWithoutPublishing(["retained@example.test"]),
      );
      await page.getByRole("button", { name: "手动排除", exact: true }).click();
      await manual
        .getByText("retained@example.test", { exact: true })
        .waitFor();
      const input = manual.getByLabel("手动排除账号", { exact: true });
      await input.fill("unfinished@example.test");
      const baseline = await readCount();
      await page.evaluate((shouldFail) => {
        window.__uiFixture.holdNextManualRead();
        if (shouldFail) window.__uiFixture.failNextManualRead();
      }, failOldRead);
      await manual.getByRole("button", { name: "刷新", exact: true }).click();
      await page.waitForFunction(() => window.__uiFixture.manualReadPending());
      await page.getByRole("button", { name: "写邮件", exact: true }).click();
      await page.evaluate(() => {
        window.__uiFixture.excludeWithoutPublishing([
          "latest-one@example.test",
        ]);
        window.__uiFixture.excludeWithoutPublishing([
          "latest-two@example.test",
        ]);
        window.__uiFixture.holdNextManualRead();
      });
      await page.getByRole("button", { name: "手动排除", exact: true }).click();
      assert.equal(await input.isDisabled(), true);
      await page.evaluate(() => window.__uiFixture.releaseManualRead());
      await page.waitForFunction(
        (count) =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "get_manual_exclusions",
          ).length === count && window.__uiFixture.manualReadPending(),
        baseline + 2,
      );
      assert.equal(
        await input.isDisabled(),
        true,
        "An obsolete response must not unlock stale maintenance",
      );
      await page.evaluate(() => window.__uiFixture.releaseManualRead());
      await manual
        .getByText("latest-two@example.test", { exact: true })
        .waitFor();
      await page.waitForFunction(
        () => !document.querySelector('[aria-label="手动排除账号"]')?.disabled,
      );
      assert.equal(await input.inputValue(), "unfinished@example.test");
      assert.equal(
        await readCount(),
        baseline + 2,
        "Revision bursts merge into exactly one follow-up read",
      );
      assert.equal(await manual.getByRole("alert").count(), 0);
    } finally {
      await context.close();
    }
  }
  console.log(
    "Manual exclusion refresh race: frozen stale responses/failures across navigation and revision bursts queue one follow-up without unlocking old data; draft retained (synthetic only)",
  );
}

/**
 * 发件归属只展示宿主保存的公开快照，旧记录不能用当前账号补猜。
 * 合成流程覆盖搜索、人工核实、账号删除及多账号结果；SQLite真实迁移/事务由Rust验证。
 */
async function checkHistorySenderAttribution() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const rows = page.locator(".history-table-wrap tbody tr");
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await page
      .getByRole("columnheader", { name: "发件账号", exact: true })
      .waitFor();
    assert.equal(
      (
        await rows.first().locator(".history-sender-account").innerText()
      ).trim(),
      "未记录",
    );
    const pending = page.locator(".unresolved-row");
    assert.match(
      await pending
        .filter({ hasText: "10000333@qq.com" })
        .locator(".history-sender-account")
        .innerText(),
      /原始发件账号[\s\S]*trace-original@example\.test/,
    );
    assert.equal(
      (
        await pending
          .filter({ hasText: "person@example.test" })
          .locator(".history-sender-account")
          .innerText()
      ).trim(),
      "未记录",
    );
    const pendingSearch = page.getByLabel("搜索待核实投递", { exact: true });
    for (const query of ["原始发件账号", "TRACE-ORIGINAL@EXAMPLE.TEST"]) {
      await pendingSearch.fill(query);
      assert.equal(await pending.count(), 1);
    }
    await page
      .getByRole("button", {
        name: "核实为已发送 10000333@qq.com",
        exact: true,
      })
      .click();
    await page.getByRole("button", { name: "确认已发送", exact: true }).click();
    await page
      .getByLabel("搜索已发送记录", { exact: true })
      .fill("trace-original");
    await rows.filter({ hasText: "人工核实已发送" }).waitFor();
    assert.equal(await rows.count(), 1);
    assert.match(
      await rows.first().locator(".history-sender-account").innerText(),
      /原始发件账号/,
    );
    await page.getByLabel("搜索已发送记录", { exact: true }).fill("");
    await pendingSearch.fill("");

    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page
      .locator("#mail-recipients")
      .fill("history-one@example.test\nhistory-two@example.test");
    await page.getByLabel("主题", { exact: true }).fill("发件账号追溯回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("合成投递，只验证公开归属，不连接SMTP。");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.getByRole("button", { name: "确认发送", exact: true }).click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const expected = await page.evaluate(() =>
      window.__uiFixture
        .accountAssignments()
        .filter((entry) => entry.recipientCount > 0),
    );
    assert.equal(expected.length, 2);
    await page.evaluate(() => {
      window.__uiFixture.finishMail();
      window.__uiFixture.removeSenderAccountsWithoutPublishing();
    });
    await page.locator(".mail-progress-completed").waitFor();
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await rows.filter({ hasText: "history-two@example.test" }).waitFor();
    for (const account of expected) {
      const search = page.getByLabel("搜索已发送记录", { exact: true });
      for (const query of [account.name, account.fromAddress.toUpperCase()]) {
        await search.fill(query);
        assert.equal(await rows.count(), 1);
        assert.match(await rows.first().innerText(), /SMTP 已接受/);
        assert.equal(
          (
            await rows.first().locator(".history-sender-account").innerText()
          ).includes(account.fromAddress),
          true,
        );
      }
    }
    await page.getByLabel("搜索已发送记录", { exact: true }).fill("");
    // 成功列表增加列仍应在卡片内滚动，长邮箱不能推宽整窗；明暗主题使用相同交互。
    for (const width of testWidths([1200, 780, 375])) {
      for (const theme of ["light", "dark"]) {
        await page.setViewportSize(testViewport(width, 900));
        await page.evaluate((value) => {
          document.documentElement.dataset.theme = value;
        }, theme);
        await page.locator(".history-records-card").scrollIntoViewIfNeeded();
        assert.equal(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= window.innerWidth + 1,
          ),
          true,
        );
        await page.screenshot({
          path: new URL(
            `history-sender-${theme}-${width}.png`,
            artifacts,
          ).pathname.replace(/^\/(\w:)/, "$1"),
          fullPage: false,
        });
      }
    }
    console.log(
      "History sender snapshot: legacy fallback, unknown attribution/search/manual resolution, two-account acceptance/search and deleted-directory independence, light/dark layouts checked (synthetic only)",
    );
  } finally {
    await context.close();
  }
}

/**
 * 批量核实使用 53 条合成记录覆盖跨页选择和事务边界；不会访问真实发送历史或连接 SMTP。
 * 取消/离开只撤销确认，失败保留选择，只有确认成功后才从待核实集合中移除整批记录。
 */
async function checkBulkHistoryResolution() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, { bulkHistoryScenario: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = () =>
    page.evaluate(() =>
      window.__uiFixture.calls.filter(
        (entry) => entry.command === "resolve_mail_deliveries",
      ),
    );
  const selection = (number) =>
    page.getByRole("checkbox", {
      name: `选择待核实 pending-${String(number).padStart(2, "0")}@example.test`,
      exact: true,
    });
  const selectedCount = (count) =>
    page.getByText(`已选 ${count} 条`, { exact: true });
  const bulkButton = (accepted) =>
    page.getByRole("button", {
      name: accepted ? "批量核实为已发送" : "批量核实为未发送",
      exact: true,
    });
  const confirmation = () =>
    page.getByRole("group", { name: "批量核实投递", exact: true });
  const confirmButton = (accepted) =>
    confirmation().getByRole("button", {
      name: accepted ? "确认已发送" : "确认未发送",
      exact: true,
    });
  const openHistory = async () => {
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await page.getByRole("heading", { name: /待核实投递 \d+ 条/ }).waitFor();
  };
  const nextPage = () =>
    page.getByRole("button", { name: "下一页待核实", exact: true }).click();
  const previousPage = () =>
    page.getByRole("button", { name: "上一页待核实", exact: true }).click();
  try {
    await page.goto("http://127.0.0.1:1420");
    await openHistory();
    await page
      .getByRole("heading", { name: "待核实投递 53 条", exact: true })
      .waitFor();
    assert.equal(await page.locator(".unresolved-row").count(), 25);
    const selectPage = page.getByRole("checkbox", {
      name: "本页全选待核实",
      exact: true,
    });
    // 搜索只改变可见范围；范围内全选/取消必须保留隐藏选择，搜索变化撤销尚未提交的核实。
    const pendingSearch = page.getByLabel("搜索待核实投递", { exact: true });
    await selection(1).check();
    await pendingSearch.fill("pending-2");
    assert.equal(await page.locator(".unresolved-row").count(), 10);
    await page
      .getByRole("button", { name: "全选待核实搜索结果", exact: true })
      .click();
    assert.match(
      await page.locator(".unresolved-selected-count").innerText(),
      /已选 11 条.*当前匹配 10 条，已选 10 条/,
    );
    await page.screenshot({
      path: new URL(
        "history-search-hidden-selection.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await page
      .getByRole("button", { name: "取消全选（待核实搜索结果）", exact: true })
      .click();
    assert.match(
      await page.locator(".unresolved-selected-count").innerText(),
      /已选 1 条.*已选 0 条/,
    );
    await bulkButton(false).click();
    await confirmation().waitFor();
    await pendingSearch.fill("pending-3");
    assert.equal(await confirmation().count(), 0);
    assert.equal((await calls()).length, 0);
    await page
      .getByRole("button", { name: "清除待核实搜索", exact: true })
      .click();
    assert.equal(
      await pendingSearch.evaluate(
        (element) => document.activeElement === element,
      ),
      true,
    );
    assert.equal(await selection(1).isChecked(), true);
    await selection(1).uncheck();
    await selectPage.check();
    await selectedCount(25).waitFor();
    await nextPage();
    assert.equal(await selectPage.isChecked(), false);
    await selection(26).check();
    await selectedCount(26).waitFor();
    assert.equal(
      await selectPage.evaluate((element) => element.indeterminate),
      true,
      "A partially selected page exposes the indeterminate state",
    );
    await previousPage();
    assert.equal(await selection(1).isChecked(), true);
    assert.equal(await selectPage.isChecked(), true);
    await selectPage.uncheck();
    await selectedCount(1).waitFor();
    await nextPage();
    assert.equal(await selection(26).isChecked(), true);
    await page
      .getByRole("button", { name: "全选全部待核实", exact: true })
      .click();
    await selectedCount(53).waitFor();
    await nextPage();
    assert.equal(await page.locator(".unresolved-row").count(), 3);
    assert.equal(await selection(53).isChecked(), true);
    assert.equal(
      await page
        .getByRole("button", { name: "全选全部待核实", exact: true })
        .isDisabled(),
      true,
    );
    await page
      .getByRole("button", { name: "取消全选（全部待核实）", exact: true })
      .click();
    assert.equal(await selection(53).isChecked(), false);
    assert.equal(
      await page
        .getByRole("button", { name: "取消全选（全部待核实）", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(await bulkButton(true).isDisabled(), true);
    assert.equal(await bulkButton(false).isDisabled(), true);
    await previousPage();
    await previousPage();
    await selection(1).check();
    await nextPage();
    await selection(26).check();
    await bulkButton(true).click();
    await confirmation().waitFor();
    assert.match(await confirmation().innerText(), /2 条|2.*记录/);
    assert.match(await confirmation().innerText(), /人工|SMTP/);
    await confirmation()
      .getByRole("button", { name: "取消核实", exact: true })
      .click();
    assert.equal((await calls()).length, 0);
    await selectedCount(2).waitFor();
    await bulkButton(false).click();
    await confirmation().waitFor();
    assert.match(await confirmation().innerText(), /解除|重发|重复/);
    await previousPage();
    assert.equal(
      await confirmation().count(),
      0,
      "Changing page cancels the unsubmitted confirmation",
    );
    await bulkButton(true).click();
    await page
      .getByRole("button", { name: "返回成员提取", exact: true })
      .click();
    await openHistory();
    assert.equal(
      await confirmation().count(),
      0,
      "Leaving history cancels the unsubmitted confirmation",
    );
    await selectedCount(2).waitFor();
    assert.equal((await calls()).length, 0);

    // 全批失败后确认和选择仍在，重试提交同一固定集合；同步门闩拒绝连续点击重复写入。
    await bulkButton(true).click();
    await page.evaluate(() => window.__uiFixture.failNextBulkResolution());
    await confirmButton(true).click();
    await toastMessage(
      page,
      "批量核实暂时失败，请重试；本批记录均未修改。",
    ).waitFor();
    await confirmation().waitFor();
    await selectedCount(2).waitFor();
    assert.equal(await page.locator(".history-error").count(), 0);
    assert.equal(await confirmButton(true).isDisabled(), false);
    assert.equal(
      await page.evaluate(() => window.__uiFixture.unresolvedRecords().length),
      53,
    );
    await page.evaluate(() => window.__uiFixture.holdNextBulkResolution());
    await confirmButton(true).evaluate((element) => {
      element.click();
      element.click();
    });
    await page.waitForFunction(() =>
      window.__uiFixture.bulkResolutionPending(),
    );
    assert.equal((await calls()).length, 2);
    for (const button of await page.locator(".app-nav > button").all())
      assert.equal(await button.isDisabled(), true);
    assert.equal(
      await page
        .getByRole("button", { name: "返回成员提取", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(await confirmButton(true).isDisabled(), true);
    assert.equal(await selectPage.isDisabled(), true);
    assert.equal(
      await page
        .getByRole("button", { name: "刷新记录", exact: true })
        .isDisabled(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.releaseBulkResolution());
    await page
      .getByRole("heading", { name: "待核实投递 51 条", exact: true })
      .waitFor();
    assert.equal(await confirmation().count(), 0);
    const acceptedCalls = await calls();
    for (const entry of acceptedCalls) {
      assert.equal(entry.args.accepted, true);
      assert.deepEqual(
        [...entry.args.ids].sort((a, b) => a - b),
        [1001, 1026],
      );
      assert.equal(new Set(entry.args.ids).size, entry.args.ids.length);
    }
    assert.equal(
      await page.evaluate(() => window.__uiFixture.unresolvedRecords().length),
      51,
    );
    await page
      .locator(".history-table-wrap td")
      .filter({ hasText: "人工核实已发送" })
      .first()
      .waitFor();

    // 刷新后剔除另一处已处理的选择，避免将消失的 id 带入下一次批量确认。
    await selection(2).check();
    await selection(3).check();
    await selectedCount(2).waitFor();
    await bulkButton(false).click();
    await confirmation().waitFor();
    await page.evaluate(() =>
      window.__uiFixture.removeUnresolvedWithoutPublishing(1002),
    );
    await page.getByRole("button", { name: "刷新记录", exact: true }).click();
    await page
      .getByRole("heading", { name: "待核实投递 50 条", exact: true })
      .waitFor();
    await selectedCount(1).waitFor();
    assert.equal(
      await confirmation().count(),
      0,
      "A disappeared id cancels the whole fixed confirmation",
    );
    assert.equal(await selection(2).count(), 0);
    assert.equal(await selection(3).isChecked(), true);
    await bulkButton(false).click();
    await confirmButton(false).click();
    await page
      .getByRole("heading", { name: "待核实投递 49 条", exact: true })
      .waitFor();
    assert.deepEqual((await calls()).at(-1).args, {
      ids: [1003],
      accepted: false,
    });
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "start_mail_job",
          ).length,
      ),
      0,
    );

    // 同组操作按钮使用统一的尺寸和字号；响应式可换行，不能靠缩小单个按钮挤进一行。
    await selection(4).check();
    const checkButtonMetrics = async () => {
      const groups = await page
        .locator(".unresolved-actions")
        .evaluateAll((elements) =>
          elements.map((element) =>
            [...element.querySelectorAll("button")].map((button) => {
              const bounds = button.getBoundingClientRect();
              const style = getComputedStyle(button);
              return {
                height: bounds.height,
                width: bounds.width,
                font: style.fontSize,
              };
            }),
          ),
        );
      for (const group of groups.filter((entries) => entries.length > 1)) {
        assert.ok(
          Math.max(...group.map((entry) => entry.height)) -
            Math.min(...group.map((entry) => entry.height)) <=
            1,
        );
        assert.ok(
          Math.max(...group.map((entry) => entry.width)) -
            Math.min(...group.map((entry) => entry.width)) <=
            1,
        );
        assert.equal(new Set(group.map((entry) => entry.font)).size, 1);
      }
      const bulk = await Promise.all(
        [bulkButton(true), bulkButton(false)].map((button) =>
          button.evaluate((element) => {
            const bounds = element.getBoundingClientRect();
            return {
              height: bounds.height,
              width: bounds.width,
              font: getComputedStyle(element).fontSize,
            };
          }),
        ),
      );
      assert.ok(Math.abs(bulk[0].height - bulk[1].height) <= 1);
      assert.ok(Math.abs(bulk[0].width - bulk[1].width) <= 1);
      assert.equal(bulk[0].font, bulk[1].font);
    };
    for (const theme of ["light", "dark"]) {
      if (theme === "dark") {
        await page.getByRole("button", { name: /外观：跟随系统/ }).click();
        await page.getByRole("button", { name: /外观：浅色/ }).click();
      }
      for (const width of testWidths([1200, 780, 375])) {
        await page.setViewportSize(testViewport(width, 900));
        assert.equal(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth + 1,
          ),
          true,
        );
        await checkButtonMetrics();
        // 证据截图显式定位批量卡片顶部；不能依赖上一条点击留下的滚动位置而裁掉标题。
        await page.locator(".unresolved-section").evaluate((element) => {
          const container = element.closest(".page-scroll");
          container.scrollTop +=
            element.getBoundingClientRect().top -
            container.getBoundingClientRect().top -
            12;
        });
        for (const close of await page
          .locator("[data-sonner-toast] [data-close-button]")
          .all())
          if (await close.isVisible()) await close.click();
        await page.screenshot({
          path: new URL(
            `history-bulk-${theme}-${width}.png`,
            artifacts,
          ).pathname.replace(/^\/(\w:)/, "$1"),
          fullPage: false,
        });
        await bulkButton(false).click();
        await checkButtonMetrics();
        await page.screenshot({
          path: new URL(
            `history-bulk-confirm-${theme}-${width}.png`,
            artifacts,
          ).pathname.replace(/^\/(\w:)/, "$1"),
          fullPage: false,
        });
        await confirmation()
          .getByRole("button", { name: "取消核实", exact: true })
          .click();
      }
      await page.setViewportSize(testViewport(1200, 900));
    }
    console.log(
      "Bulk delivery resolution page/all/cross-page/clear selection, cancel/leave/refresh, atomic failure/retry/duplicate lock and light/dark responsive button metrics checked",
    );
  } finally {
    await context.close();
  }
}

/**
 * 发送前名单以宿主预筛结果为准；490 位候选不会冒充实际待发送人数。
 * 准备仅生成固定名单令牌，确认后由隔离任务使用原集合，不连接 SMTP。
 */
async function checkRecipientPreparationAndFailurePolicy() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture, {
    preflightScenario: true,
    initialPasswordSaved: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const confirmation = () =>
    page.getByRole("dialog", { name: "确认发送", exact: true });
  const actualCount = (total) =>
    page.waitForFunction(
      (expected) =>
        document
          .querySelector(".mail-confirm-summary strong")
          ?.textContent?.replace(/\s/g, "") === `${expected}人`,
      total,
    );
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByLabel("过滤已发送记录").uncheck();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 490", exact: true })
      .waitFor();
    await page
      .getByRole("button", { name: "给全部 490 人写信", exact: true })
      .click();
    await page
      .getByLabel("主题", { exact: true })
      .fill("实际名单与失败策略回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("只使用隔离fixture，不进行真实发送。");
    await page.locator(".mail-delivery-settings > summary").click();
    const failurePolicy = page.getByRole("checkbox", {
      name: "失败后继续发送",
      exact: true,
    });
    assert.equal(await failurePolicy.isChecked(), false);
    await page
      .locator("#mail-recipients")
      .fill(
        "10000131@qq.com\nextra-one@example.test\nextra-two@example.test\nextra-one@example.test",
      );
    assert.match(
      await page.locator(".mail-batch-summary").innerText(),
      /候选收件人 492/,
    );

    // 准备期间主导航和发送入口锁定；同一事件循环双击只调用一次预筛命令。
    await page.evaluate(() =>
      window.__uiFixture.holdNextRecipientPreparation(),
    );
    await page
      .getByRole("button", { name: "发送", exact: true })
      .evaluate((element) => {
        element.click();
        element.click();
      });
    await page.waitForFunction(() =>
      window.__uiFixture.recipientPreparationPending(),
    );
    assert.equal((await calls("prepare_mail_recipients")).length, 1);
    for (const button of await page.locator(".app-nav > button").all())
      assert.equal(await button.isDisabled(), true);
    assert.equal(await failurePolicy.isDisabled(), true);
    await page.evaluate(() => window.__uiFixture.releaseRecipientPreparation());
    await confirmation().waitFor();
    await actualCount(362);
    assert.match(
      await confirmation().innerText(),
      /群成员 360 人 · 补充邮箱 2 个/,
    );
    assert.match(await confirmation().innerText(), /已跳过 130|跳过.*130/);
    assert.match(await confirmation().innerText(), /预计 19 封/);
    assert.match(await confirmation().innerText(), /失败后停止后续发送/);
    assert.equal((await calls("start_mail_job")).length, 0);
    await confirmation()
      .getByRole("button", { name: "返回修改", exact: true })
      .click();

    // 历史新增后再次准备必须重新读取人数，不沿用刚刚确认过的候选总数或令牌。
    await page.evaluate(() =>
      window.__uiFixture.markSentWithoutPublishing([
        "10000131@qq.com",
        "extra-one@example.test",
      ]),
    );
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirmation().waitFor();
    await actualCount(360);
    assert.match(
      await confirmation().innerText(),
      /群成员 359 人 · 补充邮箱 1 个/,
    );
    assert.match(await confirmation().innerText(), /预计 18 封/);
    assert.match(await confirmation().innerText(), /已跳过 132|跳过.*132/);
    await confirmation()
      .getByRole("button", { name: "返回修改", exact: true })
      .click();
    await failurePolicy.check();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirmation().waitFor();
    await actualCount(360);
    assert.match(
      await confirmation().innerText(),
      /失败后继续后续批次，不自动重试/,
    );
    const prepared = (await calls("prepare_mail_recipients")).at(-1);
    assert.equal(prepared.args.input.recipients.split(";").length, 492);
    assert.equal(prepared.args.input.groupAudience.qqNumbers.length, 490);
    assert.equal(prepared.args.input.skipSent, true);
    // 确认后历史再变化：启动命令只交固定令牌，夹具实际集合不重新过滤。
    await page.evaluate(() =>
      window.__uiFixture.markSentWithoutPublishing(["10000132@qq.com"]),
    );
    await confirmation()
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const draft = (await calls("start_mail_job")).at(-1).args.draft;
    assert.equal(draft.continueOnFailure, true);
    assert.match(draft.recipientPlanId, /^recipient-plan-/);
    assert.equal(draft.recipients.split(";").length, 492);
    const actual = await page.evaluate(() =>
      window.__uiFixture.actualMailRecipients(),
    );
    assert.equal(actual.length, 360);
    assert.equal(actual.includes("10000132@qq.com"), true);
    assert.equal(actual.includes("10000131@qq.com"), false);
    assert.equal(actual.includes("extra-one@example.test"), false);
    await page.evaluate(() => window.__uiFixture.finishMailWithFailures(20));
    await page.locator(".mail-progress-completed").waitFor();
    assert.match(
      await page.locator(".mail-progress-completed").innerText(),
      /部分|未确认/,
    );
    assert.doesNotMatch(
      await page.locator(".mail-progress-completed").innerText(),
      /全部成功/,
    );
    assert.equal(
      await page
        .locator(".mail-progress-completed.mail-progress-failed")
        .count(),
      1,
    );
    await page.waitForFunction(
      () => !document.querySelector(".mail-footer .primary")?.disabled,
    );

    // 全部跳过时不能出现发送确认，更不能再次启动发送；仍可显示实际为零的预筛结果。
    await page.evaluate(
      (emails) => window.__uiFixture.markSentWithoutPublishing(emails),
      actual,
    );
    const starts = (await calls("start_mail_job")).length;
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await toastMessage(page, /没有|全部|0/).waitFor();
    assert.equal(await confirmation().isVisible(), false);
    assert.equal((await calls("start_mail_job")).length, starts);
    console.log(
      "Recipient preparation actual counts/skip/dedup/group-extra/batches/reprepare/zero/lock/fixed plan and failure-policy default/opt-in checked",
    );
  } finally {
    await context.close();
  }
}

/**
 * 1920×1080 验证最新扫码账号、同群号的数据隔离及取消/失败不破坏当前工作。
 * 合成 IPC 只验证页面协议；真实代次、磁盘清理和内存会话由 Rust 回归覆盖。
 */
async function checkScanAccountSwitch() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const dialog = page.getByRole("dialog", { name: "扫码登录", exact: true });
  const editor = page.getByRole("textbox", { name: "邮件正文", exact: true });
  const openScan = async () => {
    await page
      .locator(".header-actions")
      .getByRole("button", { name: "重新扫码", exact: true })
      .click();
    await dialog.getByAltText("手机 QQ 登录二维码").waitFor();
  };
  const closeScan = async () => {
    await page
      .getByRole("button", { name: "关闭扫码登录", exact: true })
      .click();
    await dialog.waitFor({ state: "hidden" });
  };
  const assertDraft = async () => {
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "切换账号保留草稿",
    );
    assert.match(await editor.innerText(), /不自动发送/);
    assert.equal(await editor.locator("img[data-inline-image-id]").count(), 1);
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      "retained-extra@example.test",
    );
    assert.match(
      await page.locator(".mail-audience-counts strong").innerText(),
      /61/,
    );
    assert.match(
      await page.locator(".mail-audience").innerText(),
      /目标测试群/,
    );
  };
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    const originalSnapshot = await page.evaluate(() =>
      window.__uiFixture.snapshot(),
    );
    await page
      .getByRole("button", { name: "给全部 61 人写信", exact: true })
      .click();
    await page.getByLabel("主题", { exact: true }).fill("切换账号保留草稿");
    await editor.fill("切换登录不自动发送，保留图片和名单版本。");
    await pasteSyntheticImage(page);
    await editor.locator("img[data-inline-image-id]").waitFor();
    await page.locator("#mail-recipients").fill("retained-extra@example.test");
    const resources = await page.evaluate(() =>
      window.__uiFixture.draftResourceIds(),
    );
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "成员提取", exact: true })
      .click();

    await openScan();
    assert.match(
      await dialog.innerText(),
      /以本次扫码确认的账号登录，关闭程序后需重新扫码/,
    );
    assert.doesNotMatch(await dialog.innerText(), /7 天|七天|同一账号|先退出/);
    await page.evaluate(() => window.__uiFixture.setQrStatus("scanned"));
    await dialog
      .locator(".login-hint")
      .filter({ hasText: "已扫码，请在手机上确认登录" })
      .waitFor();
    await page.evaluate(() => window.__uiFixture.setQrStatus("expired"));
    await dialog
      .locator(".login-hint")
      .filter({ hasText: "二维码已过期，请刷新" })
      .waitFor();
    await dialog
      .getByRole("button", { name: "刷新二维码", exact: true })
      .click();
    await dialog
      .locator(".login-hint")
      .filter({ hasText: "请使用手机 QQ 扫码" })
      .waitFor();
    await page.screenshot({
      path: new URL("qr-session-dialog-1920.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    // B 使用相同群号但不同名称，旧账号的缓存名单不能成为 B 的当前名单。
    await page.evaluate(() => {
      window.__uiFixture.setNextQrIdentity(
        { qqNumber: "987654321", nickname: "最新扫码账号", source: "web" },
        [
          {
            id: "900001",
            name: "新账号同号群",
            memberCount: 62,
            role: "member",
          },
        ],
      );
      window.__uiFixture.setQrStatus("success");
    });
    await dialog.waitFor({ state: "hidden" });
    assert.match(await page.locator(".account").innerText(), /最新扫码账号/);
    assert.equal(await page.locator(".group-item").count(), 1);
    assert.equal(await page.locator(".group-item.selected").count(), 0);
    assert.equal(
      await page.evaluate(() => window.__uiFixture.snapshot()),
      null,
    );
    await page
      .locator(".group-list")
      .getByRole("button", { name: /新账号同号群/ })
      .click();
    await page.getByRole("button", { name: "获取成员", exact: true }).waitFor();
    assert.equal(
      await page
        .getByRole("tab", { name: "筛选后成员 61", exact: true })
        .count(),
      0,
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.snapshotFor("900001"))).id,
      originalSnapshot.id,
    );
    await assertDraft();
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.draftResourceIds()),
      resources,
    );

    // 关闭弹窗保留 B；迟到成功响应也不能切换身份或重新打开弹窗。
    await page.evaluate(() => window.__uiFixture.holdNextQrPoll());
    await openScan();
    await page.waitForFunction(() => window.__uiFixture.qrPollPending());
    await closeScan();
    await page.evaluate(() => {
      window.__uiFixture.setQrStatus("success");
      window.__uiFixture.releaseQrPoll();
    });
    await page.waitForFunction(() => !window.__uiFixture.qrPollPending());
    assert.match(await page.locator(".account").innerText(), /最新扫码账号/);
    await page.evaluate(() => window.__uiFixture.failNextQrPoll());
    await openScan();
    await dialog
      .getByText("合成扫码验证暂时失败，请刷新二维码。", { exact: true })
      .waitFor();
    assert.match(await page.locator(".account").innerText(), /最新扫码账号/);
    await closeScan();
    await assertDraft();

    // 同账号重复扫码可刷新目录，无需先退出，也不会合并上一次的群。
    await openScan();
    await page.evaluate(() => {
      window.__uiFixture.setNextQrIdentity(
        { qqNumber: "987654321", nickname: "最新扫码账号", source: "web" },
        [
          {
            id: "900002",
            name: "重新扫码后的群",
            memberCount: 7,
            role: "member",
          },
        ],
      );
      window.__uiFixture.setQrStatus("success");
    });
    await dialog.waitFor({ state: "hidden" });
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "成员提取", exact: true })
      .click();
    await page
      .locator(".group-list")
      .getByRole("button", { name: /重新扫码后的群/ })
      .waitFor();
    assert.equal(
      await page.getByRole("button", { name: /新账号同号群/ }).count(),
      0,
    );
    // 失效撤下账号和目录，但保留独立草稿；只提供重新扫码入口。
    await page.evaluate(() =>
      window.__uiFixture.publishSession({
        status: "expired",
        message: "QQ 登录已失效，请重新扫码登录。",
      }),
    );
    await page.locator(".session-expired").waitFor();
    assert.equal(await page.locator(".account").count(), 0);
    assert.equal(await page.locator(".group-item").count(), 0);
    await assertDraft();
    await page.locator(".session-expired").waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: /重试恢复登录|清除保存的登录/ })
        .count(),
      0,
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) =>
              entry.command === "restore_web_session" ||
              entry.command === "start_mail_job",
          ).length,
      ),
      0,
    );
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
      true,
    );
    await page.screenshot({
      path: new URL("qr-session-expired-1920.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
  } finally {
    await context.close();
  }
  // 新宿主启动状态为 signed_out；页面刷新本身不代表进程重启。
  const freshContext = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await freshContext.addInitScript(installFixture, {
    initialSessionStatus: "signed_out",
  });
  const freshPage = await freshContext.newPage();
  freshPage.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await freshPage.goto("http://127.0.0.1:1420");
    await freshPage
      .locator(".header-actions")
      .getByRole("button", { name: "扫码登录", exact: true })
      .waitFor();
    assert.equal(await freshPage.locator(".account").count(), 0);
    assert.doesNotMatch(
      await freshPage.locator("body").innerText(),
      /7 天|七天|重试恢复登录|清除保存的登录/,
    );
    await freshPage.screenshot({
      path: new URL(
        "qr-session-signed-out-1920.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
  } finally {
    await freshContext.close();
  }
  // 扫码事件先于初始读取完成时保留新事件，只模拟公开消息到达顺序。
  const raceContext = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await raceContext.addInitScript(installFixture, {
    initialSessionStatus: "signed_out",
    initializeSessionRace: true,
  });
  const racePage = await raceContext.newPage();
  racePage.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await racePage.goto("http://127.0.0.1:1420");
    await racePage
      .locator(".header-actions")
      .getByRole("button", { name: "重新扫码", exact: true })
      .waitFor();
    assert.match(await racePage.locator(".account").innerText(), /测试账号/);
    await racePage
      .getByRole("button", { name: "退出当前账号", exact: true })
      .click();
    await racePage
      .locator(".header-actions")
      .getByRole("button", { name: "扫码登录", exact: true })
      .waitFor();
    assert.equal(await racePage.locator(".account").count(), 0);
  } finally {
    await raceContext.close();
  }
  console.log(
    "1920x1080 QR session: account replacement/same-account scan, old-group isolation, cancel/stale/failure preservation, draft/image/audience retention, expired/signed-out UI, logout and initialization race passed (synthetic only)",
  );
}

/** 工作区与发件状态初始化失败分别提供明确重试入口，不影响独立模板与草稿。 */
async function checkInitializationRetryEntrypoints() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture, {
    initialPasswordSaved: true,
    workspaceUnavailable: true,
    mailStateUnavailable: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  try {
    await page.goto("http://127.0.0.1:1420");
    const workspaceRetry = page.getByRole("button", {
      name: "重新加载工作区",
      exact: true,
    });
    await workspaceRetry.waitFor();
    const workspaceReads = (await calls("get_app_state")).length;
    await page.evaluate(() => {
      window.__uiFixture.setWorkspaceUnavailable(false);
      window.__uiFixture.holdNextWorkspaceRead();
    });
    await workspaceRetry.evaluate((button) => {
      button.click();
      button.click();
    });
    await page.waitForFunction(() => window.__uiFixture.workspaceReadPending());
    assert.equal((await calls("get_app_state")).length, workspaceReads + 1);
    await page.evaluate(() => window.__uiFixture.releaseWorkspaceRead());
    await page.locator(".account").waitFor();
    assert.equal(await workspaceRetry.count(), 0);
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    const mailRetry = page.getByRole("button", {
      name: "重新加载发送状态",
      exact: true,
    });
    await mailRetry.waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: "发送", exact: true })
        .isDisabled(),
      true,
    );
    const settingsReads = (await calls("mail_accounts")).length;
    const mailReads = (await calls("get_mail_state")).length;
    await page.evaluate(() => {
      window.__uiFixture.setMailStateUnavailable(false);
      window.__uiFixture.holdNextMailStateRead();
    });
    await mailRetry.evaluate((button) => {
      button.click();
      button.click();
    });
    await page.waitForFunction(() => window.__uiFixture.mailStateReadPending());
    assert.equal((await calls("get_mail_state")).length, mailReads + 1);
    await page.evaluate(() => window.__uiFixture.releaseMailStateRead());
    await page.waitForFunction(
      () => !document.querySelector(".mail-footer .primary")?.disabled,
    );
    assert.equal((await calls("mail_accounts")).length, settingsReads);
    assert.equal(await mailRetry.count(), 0);
    assert.equal(
      await page
        .getByRole("checkbox", {
          name: "本次使用 sender@example.test",
          exact: true,
        })
        .isChecked(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.failNextTemplate("list"));
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library.getByText("模板暂不可用", { exact: true }).waitFor();
    assert.equal(
      await library.getByText("还没有模板", { exact: true }).count(),
      0,
    );
    await library.getByRole("button", { name: "重试", exact: true }).click();
    await library.getByText("还没有模板", { exact: true }).waitFor();
    await library
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    assert.equal((await calls("start_mail_job")).length, 0);
    console.log(
      "Initialization retry: workspace read and mail-state subscription persistent failures, explicit retry/double-click lock/settings preservation and distinct template error/empty states checked",
    );
  } finally {
    await context.close();
  }
}

/**
 * 全选只影响用户当前看到的搜索范围；跨页执行时保留隐藏选择，未决与加载状态不得进入收件范围。
 * 排除群、群成员和独立手动列表使用同一套范围语义，所有数据为隔离浏览器合成记录。
 */
async function checkSelectionScopeInteractions() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture, {
    groupScenario: true,
    manualScenario: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /未知人数目标群/ })
      .click();
    const sidebarSearch = page.getByLabel("搜索群名或群号", { exact: true });
    await sidebarSearch.fill("没有这个群");
    await page.getByRole("button", { name: "清除群搜索", exact: true }).click();
    assert.equal(
      await sidebarSearch.evaluate(
        (element) => document.activeElement === element,
      ),
      true,
    );
    assert.equal(await page.locator(".group-item.selected").count(), 1);
    await page.locator(".filter-expand").click();
    const picker = page.locator(".exclusion-picker");
    const groupSearch = picker.getByLabel("搜索排除群", { exact: true });
    await picker.getByRole("checkbox", { name: /独立测试群/ }).check();
    await groupSearch.fill("布局测试群1");
    const matched = await picker.getByRole("checkbox").count();
    assert.equal(matched, 11);
    await picker
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    for (const checkbox of await picker.getByRole("checkbox").all())
      assert.equal(await checkbox.isChecked(), true);
    await page.screenshot({
      path: new URL(
        "exclusion-group-search-selection.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await picker
      .getByRole("button", { name: "取消全选（当前结果）", exact: true })
      .click();
    for (const checkbox of await picker.getByRole("checkbox").all())
      assert.equal(await checkbox.isChecked(), false);
    await picker
      .getByRole("button", { name: "清除排除群搜索", exact: true })
      .click();
    assert.equal(
      await groupSearch.evaluate(
        (element) => document.activeElement === element,
      ),
      true,
    );
    assert.equal(
      await picker.getByRole("checkbox", { name: /独立测试群/ }).isChecked(),
      true,
    );
    assert.equal(await picker.getByRole("checkbox").count(), 99);
    await picker
      .getByRole("button", { name: "清空全部选择", exact: true })
      .click();
    assert.equal(await picker.locator("input:checked").count(), 0);
    await groupSearch.fill("没有这个排除群");
    assert.equal(
      await picker
        .getByRole("button", { name: "全选当前结果", exact: true })
        .isDisabled(),
      true,
    );
    await groupSearch.fill("");
    await page.evaluate(() => window.__uiFixture.holdNextFetch());
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    assert.equal(
      await picker
        .getByRole("button", { name: "全选当前结果", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await picker.getByRole("checkbox", { name: /独立测试群/ }).isDisabled(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.releaseFetch());
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    await page.evaluate(() => window.__uiFixture.markUnresolved("10000003"));
    await page
      .getByRole("button", { name: "刷新发送状态", exact: true })
      .click();
    await page
      .getByRole("button", { name: "全选成员 60 人", exact: true })
      .waitFor();
    await page.getByRole("button", { name: "下一页成员", exact: true }).click();
    await page.getByLabel("选择成员 10000062", { exact: true }).check();
    const memberSearch = page.getByLabel("搜索成员QQ号、昵称或群名片", {
      exact: true,
    });
    await memberSearch.fill("测试成员2");
    await page
      .getByRole("button", { name: "全选搜索结果 11 人", exact: true })
      .click();
    await page
      .getByRole("button", { name: "给已选 12 人写信", exact: true })
      .waitFor();
    await page
      .getByRole("button", { name: "取消全选搜索结果 11 人", exact: true })
      .click();
    await page
      .getByRole("button", { name: "给已选 1 人写信", exact: true })
      .waitFor();
    await memberSearch.fill("没有这个成员");
    assert.equal(
      await page
        .getByRole("button", { name: "全选搜索结果 0 人", exact: true })
        .isDisabled(),
      true,
    );
    await page
      .getByRole("button", { name: "清除成员搜索", exact: true })
      .click();
    assert.equal(
      await memberSearch.evaluate(
        (element) => document.activeElement === element,
      ),
      true,
    );
    await page
      .getByRole("button", { name: "给已选 1 人写信", exact: true })
      .waitFor();
    await memberSearch.fill("   ");
    await page
      .getByRole("button", { name: "全选成员 60 人", exact: true })
      .click();
    await page
      .getByRole("button", { name: "给已选 60 人写信", exact: true })
      .waitFor();
    assert.equal(
      await page.getByLabel("选择成员 10000003", { exact: true }).isChecked(),
      false,
    );
    assert.equal(
      await page.getByLabel("选择成员 10000003", { exact: true }).isDisabled(),
      true,
    );
    await page.getByRole("button", { name: "下一页成员", exact: true }).click();
    assert.equal(
      await page.getByLabel("选择成员 10000062", { exact: true }).isChecked(),
      true,
    );
    await page
      .getByRole("button", { name: "取消全选成员 60 人", exact: true })
      .click();
    await page
      .getByRole("button", { name: "给已选 0 人写信", exact: true })
      .waitFor();
    await page.evaluate(() => {
      window.__uiFixture.holdNextMemberStatus();
      window.__uiFixture.failNextMemberStatus();
    });
    await page
      .getByRole("button", { name: "刷新发送状态", exact: true })
      .click();
    await page.waitForFunction(() => window.__uiFixture.memberStatusPending());
    assert.equal(
      await page
        .getByRole("button", { name: /^全选成员 \d+ 人$/ })
        .isDisabled(),
      true,
    );
    assert.equal(
      await page
        .getByRole("checkbox", { name: "全选本页成员", exact: true })
        .isDisabled(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.releaseMemberStatus());
    await page
      .getByRole("alert")
      .filter({ hasText: "发送状态暂时无法读取" })
      .waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: /^全选成员 \d+ 人$/ })
        .isDisabled(),
      true,
    );
    await page
      .getByRole("button", { name: "刷新发送状态", exact: true })
      .click();
    await page.waitForFunction(
      () =>
        document.querySelector(".member-mail-selection button")?.disabled ===
        false,
    );
    await page.evaluate(() => window.__uiFixture.omitNextMemberStatus());
    await page
      .getByRole("button", { name: "刷新发送状态", exact: true })
      .click();
    await page
      .getByRole("alert")
      .filter({ hasText: "发送状态返回不完整" })
      .waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: /^全选成员 \d+ 人$/ })
        .isDisabled(),
      true,
    );
    await page
      .getByRole("button", { name: "刷新发送状态", exact: true })
      .click();
    await page.waitForFunction(
      () =>
        document.querySelector(".member-mail-selection button")?.disabled ===
        false,
    );
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    const manual = page.locator(".manual-exclusions-page");
    await manual
      .getByLabel("手动排除账号", { exact: true })
      .fill(
        Array.from(
          { length: 31 },
          (_, index) =>
            `batch-${String(index + 1).padStart(2, "0")}@example.test`,
        ).join("\n"),
      );
    await manual
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /已添加 31 条/).waitFor();
    const tools = manual.locator(".manual-exclusions-tools");
    const selected = (count) =>
      tools
        .getByRole("status")
        .filter({ hasText: new RegExp(`已选 ${count} 条(?:（|$)`) })
        .waitFor();
    const pageSelect = manual.getByRole("checkbox", {
      name: "选择本页手动排除",
      exact: true,
    });
    await pageSelect.check();
    await selected(25);
    await manual.getByRole("button", { name: "下一页", exact: true }).click();
    await manual.getByLabel("选择排除记录 26", { exact: true }).check();
    await selected(26);
    await manual.getByRole("button", { name: "上一页", exact: true }).click();
    await pageSelect.uncheck();
    await selected(1);
    const manualSearch = manual.getByLabel("搜索手动排除", { exact: true });
    await manualSearch.fill("batch-0");
    await tools
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await selected(10);
    await tools
      .getByRole("button", { name: "取消全选（当前结果）", exact: true })
      .click();
    await selected(1);
    await manualSearch.fill("没有这条记录");
    assert.equal(
      await tools
        .getByRole("button", { name: "全选当前结果", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(await pageSelect.isDisabled(), true);
    await tools
      .getByRole("button", { name: "清空全部选择", exact: true })
      .click();
    await selected(0);
    await manual
      .getByRole("button", { name: "清除手动排除搜索", exact: true })
      .click();
    assert.equal(
      await manualSearch.evaluate(
        (element) => document.activeElement === element,
      ),
      true,
    );
    await tools
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await selected(31);
    await page.setViewportSize(testViewport(375, 900));
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
      true,
    );
    await page.screenshot({
      path: new URL(
        "selection-controls-narrow.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await page.setViewportSize(testViewport(1200, 900));
    await tools.getByRole("button", { name: "移除所选", exact: true }).click();
    const confirmation = manual.getByRole("group", {
      name: "确认移除手动排除",
      exact: true,
    });
    // 翻页仅撤销旧删除确认，保留跨页勾选；真正提交期间翻页必须锁住。
    await confirmation.waitFor();
    await manual.getByRole("button", { name: "下一页", exact: true }).click();
    assert.equal(await confirmation.count(), 0);
    await selected(31);
    await tools.getByRole("button", { name: "移除所选", exact: true }).click();
    await page.evaluate(() => window.__uiFixture.holdNextManualWrite());
    await confirmation
      .getByRole("button", { name: "确认移除", exact: true })
      .evaluate((button) => {
        button.click();
        button.click();
      });
    await page.waitForFunction(() => window.__uiFixture.manualWritePending());
    assert.equal(
      await manual
        .getByRole("button", { name: "上一页", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await manual
        .getByRole("button", { name: "下一页", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await tools
        .getByRole("button", { name: "取消全选（当前结果）", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "remove_manual_exclusions",
          ).length,
      ),
      1,
    );
    await page.evaluate(() => window.__uiFixture.releaseManualWrite());
    await toastMessage(page, /已移除 31 条/).waitFor();
    assert.equal(
      await tools
        .getByRole("button", { name: "全选当前结果", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await tools
        .getByRole("button", { name: "清空全部选择", exact: true })
        .count(),
      0,
    );
    console.log(
      "Selection scopes: group target exclusion/current-search toggle/hidden retention, cross-page member toggle/unresolved/loading/error lock, manual cross-page/search/empty/remove double-submit and 375px layout checked",
    );
  } finally {
    await context.close();
  }
}

/** 大群列表和每群名单分别维护，导出及邮件调整始终绑定用户当前看到的那份结果。 */
async function checkGroupIsolationAndLayout() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 820),
    timezoneId: "Asia/Shanghai",
  });
  await context.addInitScript(installFixture, { groupScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await page.goto("http://127.0.0.1:1420");
    const groups = page.locator(".group-list");
    await page.getByRole("button", { name: /未知人数目标群/ }).waitFor();
    assert.equal(await groups.locator(".group-item").count(), 100);
    const originalMainBounds = await page
      .locator(".main-content")
      .boundingBox();
    await groups.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    assert.equal(
      await groups.evaluate(
        (element) =>
          element.scrollHeight > element.clientHeight && element.scrollTop > 0,
      ),
      true,
      "A long group list must scroll inside its own pane",
    );
    assert.deepEqual(
      await page.locator(".main-content").boundingBox(),
      originalMainBounds,
    );
    assert.equal(await page.evaluate(() => window.scrollY), 0);
    await page.screenshot({
      path: new URL("group-list-scroll.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await groups.evaluate((element) => {
      element.scrollTop = 0;
    });
    await page.getByRole("button", { name: /未知人数目标群/ }).click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );
    await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
    const first = await page.evaluate(() =>
      window.__uiFixture.snapshotFor("900001"),
    );
    assert.equal(first.reportedCount, 62);
    assert.match(
      await page.locator(".group-item.selected").innerText(),
      /62 人/,
    );
    assert.doesNotMatch(
      await page.locator(".group-item.selected").innerText(),
      /61 人|人数未知/,
    );
    assert.match(await page.locator(".group-summary").innerText(), /62 人/);
    assert.match(
      await page.locator(".result-meta").innerText(),
      /2026[年/\-]11[月/\-]05.*\d{2}:\d{2}:\d{2}/,
    );
    assert.doesNotMatch(
      await page.locator(".app-shell").innerText(),
      /来源：[ \t]*(网页方式|历史来源)|Tauri|WebView2|Fixture · no network/,
    );

    const firstQq = first.members[0].qqNumber;
    await page.getByLabel(`选择成员 ${firstQq}`, { exact: true }).check();
    await page
      .getByRole("button", { name: "给已选 1 人写信", exact: true })
      .click();
    await page.getByRole("button", { name: "成员提取", exact: true }).click();
    await page.getByRole("button", { name: /独立测试群/ }).click();
    await page
      .getByRole("heading", { name: "该群还没有已保存名单", exact: true })
      .waitFor();
    assert.equal(await page.locator(".result-summary:visible").count(), 0);
    assert.equal(
      await page
        .getByRole("button", { name: "导出 Excel", exact: true })
        .count(),
      0,
    );
    await page.screenshot({
      path: new URL("group-empty-state.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );
    await page.getByRole("tab", { name: "筛选后成员 7" }).waitFor();
    const second = await page.evaluate(() =>
      window.__uiFixture.snapshotFor("900002"),
    );
    assert.notEqual(second.id, first.id);
    assert.equal(
      await page.getByLabel(`选择成员 ${firstQq}`, { exact: true }).count(),
      0,
    );
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    await toastMessage(page, "文件已保存").waitFor();
    let exports = await page.evaluate(() =>
      window.__uiFixture.calls.filter(
        (entry) => entry.command === "export_members",
      ),
    );
    assert.deepEqual(exports.at(-1).args, {
      format: "xlsx",
      snapshotId: second.id,
      exclusionRevision: 0,
    });
    await page.screenshot({
      path: new URL("group-second-snapshot.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });

    await page.getByRole("button", { name: /未知人数目标群/ }).click();
    await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
    assert.equal(
      await page.getByLabel(`选择成员 ${firstQq}`, { exact: true }).isChecked(),
      true,
    );
    assert.match(
      await page.locator(".result-meta").innerText(),
      /未知人数目标群/,
    );
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    await toastMessage(page, "文件已保存").waitFor();
    exports = await page.evaluate(() =>
      window.__uiFixture.calls.filter(
        (entry) => entry.command === "export_members",
      ),
    );
    assert.deepEqual(exports.at(-1).args, {
      format: "xlsx",
      snapshotId: first.id,
      exclusionRevision: 0,
    });

    await page.getByRole("button", { name: /独立测试群/ }).click();
    await page.getByRole("tab", { name: "筛选后成员 7" }).waitFor();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    assert.match(
      await page.locator(".mail-audience").innerText(),
      /未知人数目标群/,
    );
    await page.getByRole("button", { name: "调整群成员", exact: true }).click();
    await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
    assert.match(
      await page.locator(".group-item.selected").innerText(),
      /未知人数目标群/,
    );
    assert.equal(
      await page.getByLabel(`选择成员 ${firstQq}`, { exact: true }).isChecked(),
      true,
    );
    await page.screenshot({
      path: new URL("group-isolation-desktop.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    // 未返回参考总人数时，只能表述“已获取”，仍按未过滤的 sourceCount 展示已知范围。
    await page
      .locator(".group-list")
      .getByRole("button", { name: /未知总数测试群/ })
      .click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );
    await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
    assert.match(
      await page.locator(".group-item.selected").innerText(),
      /已获取 62 人/,
    );
    assert.doesNotMatch(
      await page.locator(".group-item.selected").innerText(),
      /已获取 61 人/,
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.snapshotFor("900003")))
        .reportedCount,
      null,
    );
  } finally {
    await context.close();
  }
}

/**
 * 使用后台绝对时间与合成状态验证等待界面；虚拟时钟推进不连接 SMTP，也不等待真实十五分钟。
 * 同时覆盖命令/事件乱序、重载恢复、停止和终态，避免本机倒计时伪造任务进度。
 */
async function checkMailCountdown() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, {
    initialPasswordSaved: true,
    countdownScenario: true,
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const progress = page.locator(".mail-progress");
  const countdown = page.locator(".mail-countdown");
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  try {
    await page.clock.install({ time: new Date("2026-11-05T12:00:00Z") });
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.locator(".mail-delivery-settings > summary").click();
    const interval = page.getByLabel("同账号发送间隔秒数", { exact: true });
    assert.equal(await interval.inputValue(), "900");
    await page
      .locator("#mail-recipients")
      .fill("countdown-a@example.test\ncountdown-b@example.test");
    await page.getByLabel("主题", { exact: true }).fill("等待状态回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("合成邮件，仅检查批次倒计时，不连接 SMTP。");
    await interval.fill("1.5");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await toastMessage(page, "同账号间隔请填写 1 到 3600 的整数秒。").waitFor();
    assert.equal((await calls("prepare_mail_recipients")).length, 0);
    await interval.fill("900");
    const switchInterval = page.getByLabel("账号切换间隔秒数", { exact: true });
    assert.equal(await switchInterval.inputValue(), "60");
    for (const invalid of ["1.5", "-1", "3601", ""]) {
      await switchInterval.fill(invalid);
      await page.getByRole("button", { name: "发送", exact: true }).click();
      await toastMessage(
        page,
        "账号切换间隔请填写 0 到 3600 的整数秒。",
      ).waitFor();
      assert.equal((await calls("prepare_mail_recipients")).length, 0);
    }
    // 0关闭和3600边界均可确认；返回修改后按新值重新预检，不能沿用旧草稿。
    for (const valid of ["0", "3600"]) {
      await switchInterval.fill(valid);
      await page.getByRole("button", { name: "发送", exact: true }).click();
      const dialog = page.getByRole("dialog", {
        name: "确认发送",
        exact: true,
      });
      await dialog.waitFor();
      const row = dialog
        .locator("dl > div")
        .filter({ has: page.getByText("账号切换间隔", { exact: true }) });
      assert.match(await row.innerText(), valid === "0" ? /已关闭/ : /3600/);
      await dialog
        .getByRole("button", { name: "返回修改", exact: true })
        .click();
    }
    await switchInterval.fill("60");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.getByRole("button", { name: "确认发送", exact: true }).click();
    assert.equal(
      (await calls("start_mail_job")).at(-1).args.draft
        .accountSwitchIntervalSeconds,
      60,
    );
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    assert.equal(
      (await calls("start_mail_job")).at(-1).args.draft.intervalSeconds,
      900,
    );
    assert.match(await progress.innerText(), /正在发送/);
    assert.equal(await countdown.count(), 0);

    await page.clock.pauseAt(
      new Date((await page.evaluate(() => Date.now())) + 1000),
    );
    const waiting = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "waiting",
        nextBatchAt: Date.now() + 900_000,
        sent: 1,
        current: "本批收件人 1 人",
        message: "本批已结束，等待下一批。",
      }),
    );
    await page.getByText("等待发送间隔", { exact: true }).waitFor();
    await countdown.getByText("15:00", { exact: true }).waitFor();
    assert.match(await page.locator(".mail-current").innerText(), /^待发送：/);
    assert.equal(await countdown.getAttribute("aria-live"), "off");
    assert.equal(await progress.getAttribute("aria-live"), null);
    // 邮件运行时保留原有跨页查看能力，等待阶段仍锁住会改变任务内容和设置的控件。
    assert.equal(await interval.isDisabled(), true);
    assert.equal(await switchInterval.isDisabled(), true);
    assert.equal(
      await page.getByLabel("主题", { exact: true }).isDisabled(),
      true,
    );
    assert.equal(
      await page
        .getByRole("button", { name: "发件设置", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(await page.locator(".nav-task-dot").count(), 1);
    const preparationCount = (await calls("prepare_mail_recipients")).length;
    const historyReadCount = (await calls("get_sent_history")).length;
    await page.clock.runFor(2000);
    await countdown.getByText("14:58", { exact: true }).waitFor();
    assert.equal(
      (await calls("prepare_mail_recipients")).length,
      preparationCount,
    );
    assert.equal((await calls("get_sent_history")).length, historyReadCount);

    // 相同截止时间的刷新不重新开始900秒；新渲染初态也从宿主原截止时间恢复。
    await page.evaluate(() => window.__uiFixture.publishMailState({}));
    assert.match(await countdown.innerText(), /14:58/);
    await page.reload();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    await countdown.getByText("14:58", { exact: true }).waitFor();
    assert.equal((await calls("get_mail_state")).length, 1);
    const refreshed = await page.evaluate(() => window.__uiFixture.mail());
    assert.equal(refreshed.nextBatchAt, waiting.nextBatchAt);

    for (const [width, appearance, filename] of fullHdOnly
      ? [
          [1920, "light", "mail-countdown-light.png"],
          [1920, "dark", "mail-countdown-dark.png"],
        ]
      : [
          [1200, "light", "mail-countdown-light.png"],
          [1200, "dark", "mail-countdown-dark.png"],
          [375, "dark", "mail-countdown-narrow.png"],
        ]) {
      await page.setViewportSize(testViewport(width, 900));
      await page.evaluate((theme) => {
        document.documentElement.dataset.theme = theme;
      }, appearance);
      await progress.scrollIntoViewIfNeeded();
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth + 1,
        ),
        true,
      );
      await page.screenshot({
        path: new URL(filename, artifacts).pathname.replace(/^\/(\w:)/, "$1"),
        fullPage: false,
      });
    }
    await page.setViewportSize(testViewport(1200, 900));
    await page.clock.runFor(898_000);
    await countdown.getByText("正在准备下一批", { exact: true }).waitFor();
    assert.equal((await calls("start_mail_job")).length, 0);
    assert.match(await progress.innerText(), /服务器接受 1/);

    // 同计数的新 sending 立即卸载时钟，较旧/重复版本和旧无版本事件都不能恢复 waiting。
    const sending = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        current: "本批收件人 1 人",
        message: "正在发送下一批。",
      }),
    );
    await page.getByText("正在发送", { exact: true }).waitFor();
    assert.equal(await countdown.count(), 0);
    for (const late of [
      refreshed,
      { ...refreshed, stateRevision: sending.stateRevision },
      { ...refreshed, stateRevision: undefined },
    ]) {
      await page.evaluate(
        (value) => window.__uiFixture.emitMailState(value),
        late,
      );
      assert.equal(await countdown.count(), 0);
      assert.match(await progress.innerText(), /正在发送下一批/);
    }
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "stopping",
        // 故意保留陈旧截止时间：界面必须按phase清掉时钟，不能只检查非空deadline。
        nextBatchAt: Date.now() + 900_000,
        message: "正在结束当前发送。",
      }),
    );
    await page.getByText("正在停止发送", { exact: true }).waitFor();
    assert.equal(await countdown.count(), 0);
    assert.equal(
      await page
        .getByRole("button", { name: "停止后续发送", exact: true })
        .isDisabled(),
      true,
    );
    const terminal = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        status: "cancelled",
        phase: "idle",
        nextBatchAt: null,
        current: "",
        message: "已取消后续批次。",
      }),
    );
    await page.locator(".mail-progress-cancelled").waitFor();
    assert.equal(await countdown.count(), 0);
    await page.evaluate(
      (value) => window.__uiFixture.emitMailState(value),
      refreshed,
    );
    assert.equal(await countdown.count(), 0);
    assert.match(await progress.innerText(), /已停止发送/);

    // 新任务仍沿用全局版本号，上一任务终态迟到不能替换正在等待的新任务。
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        id: "mail-fixture-countdown-new-job",
        status: "running",
        phase: "waiting",
        nextBatchAt: Date.now() + 120_000,
        message: "新任务等待下一批。",
      }),
    );
    await countdown.getByText("02:00", { exact: true }).waitFor();
    await page.evaluate(
      (value) => window.__uiFixture.emitMailState(value),
      terminal,
    );
    assert.match(await progress.innerText(), /新任务等待下一批/);
    // 系统时间跳变或窗口从后台返回时，应立即以同一绝对截止时间校准。
    await page.clock.setSystemTime(
      new Date((await page.evaluate(() => Date.now())) + 5000),
    );
    await page.evaluate(() => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await countdown.getByText("01:55", { exact: true }).waitFor();
    // 等待期间取消沿用真实停止按钮，终态清理时钟；迟到的命令 running 响应仍被挡住。
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.clock.runFor(100);
    await page.locator(".mail-progress-cancelled").waitFor();
    assert.equal(await countdown.count(), 0);
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        id: "mail-fixture-completed-last-batch",
        status: "completed",
        phase: "waiting",
        nextBatchAt: Date.now() + 900_000,
        sent: 2,
        message: "最后一批已结束。",
      }),
    );
    await page.locator(".mail-progress-completed").waitFor();
    assert.equal(await countdown.count(), 0);
    console.log(
      "Mail countdown default900, integer boundary, absolute-time ticks/reload, no per-tick IPC, phase/revision races, stop and terminal cleanup checked (no SMTP)",
    );
  } finally {
    await context.close();
  }
}

/**
 * 验证按轮发送的公开展示契约，不在浏览器夹具内重复实现后台调度算法。
 * 合成事件表示同轮账号切换、轮间冷却及末轮完成；真实轮次/时钟正确性由 Rust 回归证明。
 */
async function checkMultiAccountRoundProgress() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const progress = page.locator(".mail-progress");
  const countdown = page.locator(".mail-countdown");
  const calls = (name) =>
    page.evaluate(
      (command) =>
        window.__uiFixture.calls.filter((item) => item.command === command),
      name,
    );
  try {
    await page.clock.install({ time: new Date("2026-11-09T12:00:00Z") });
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.locator(".mail-delivery-settings > summary").click();
    const interval = page.getByLabel("同账号发送间隔秒数", { exact: true });
    assert.equal(await interval.inputValue(), "900");
    assert.match(await interval.getAttribute("title"), /取较晚时间/);
    // 仅选择两账号，逐人发4位实际收件人；确认页应解释2轮且无需规则选择。
    await page
      .getByRole("button", { name: "取消全选发件账号", exact: true })
      .click();
    await page
      .getByRole("checkbox", {
        name: "本次使用 sender1@example.test",
        exact: true,
      })
      .check();
    await page
      .getByRole("checkbox", {
        name: "本次使用 sender2@example.test",
        exact: true,
      })
      .check();
    await page
      .locator("#mail-recipients")
      .fill(
        [1, 2, 3, 4].map((index) => `round-${index}@example.test`).join("\n"),
      );
    await page.getByLabel("主题", { exact: true }).fill("两账号轮次显示回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("仅使用合成状态，不连接 SMTP。");
    await page
      .getByLabel("发送方式", { exact: true })
      .selectOption("individual");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const confirmation = page.getByRole("dialog", {
      name: "确认发送",
      exact: true,
    });
    await confirmation.waitFor();
    assert.match(await confirmation.innerText(), /同账号间隔/);
    assert.match(await confirmation.innerText(), /900/);
    assert.match(await confirmation.innerText(), /预计 4 封/);
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const draft = (await calls("start_mail_job")).at(-1).args.draft;
    assert.equal(draft.intervalSeconds, 900);
    assert.equal(draft.accountSwitchIntervalSeconds, 60);
    assert.equal(draft.allocationMode, "random");
    assert.deepEqual([...draft.accountIds].sort(), ["sender-1", "sender-2"]);
    const initial = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        status: "running",
        phase: "sending",
        nextBatchAt: null,
        total: 4,
        sent: 0,
        failed: 0,
        skipped: 0,
        current: "第 1/2 轮 · 发件账号2 · 第 1/4 批，1 位收件人",
        message: "正在等待邮件服务器确认…",
      }),
    );
    await page
      .locator(".mail-current")
      .filter({ hasText: /第 1\/2 轮.*发件账号2/ })
      .waitFor();
    assert.equal(await countdown.count(), 0);
    // 默认同轮切换先展示60秒截止时间；宿主发布sending时才卸载倒计时。
    // 计时结束不由渲染器发IPC，具体取max和轮次调度由Rust回归验证。
    await page.clock.pauseAt(
      new Date((await page.evaluate(() => Date.now())) + 1000),
    );
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "waiting",
        waitReason: "accountSwitch",
        nextBatchAt: Date.now() + 60_000,
        sent: 1,
        current: "第 1/2 轮 · 发件账号1 · 第 2/4 批，1 位收件人",
        message: "等待账号切换间隔。",
      }),
    );
    await page.getByText("等待发送间隔", { exact: true }).waitFor();
    await countdown.getByText("01:00", { exact: true }).waitFor();
    assert.equal(await countdown.getAttribute("aria-label"), "发送间隔倒计时");
    const switchStartCount = (await calls("start_mail_job")).length;
    await page.clock.runFor(60_000);
    await countdown.getByText("正在准备下一批", { exact: true }).waitFor();
    assert.equal((await calls("start_mail_job")).length, switchStartCount);
    // 人数按实际结果更新，不能用等待阶段或轮次推测服务器接受人数。
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        sent: 1,
        current: "第 1/2 轮 · 发件账号1 · 第 2/4 批，1 位收件人",
      }),
    );
    await page
      .locator(".mail-current")
      .filter({ hasText: /第 1\/2 轮.*发件账号1/ })
      .waitFor();
    assert.equal(await countdown.count(), 0);
    await page.clock.pauseAt(
      new Date((await page.evaluate(() => Date.now())) + 1000),
    );
    const waiting = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "waiting",
        nextBatchAt: Date.now() + 900_000,
        sent: 2,
        current: "第 2/2 轮 · 发件账号2 · 第 3/4 批，1 位收件人",
        waitReason: "combined",
        message: "本轮待发账号仍在冷却，等待最近到期账号。",
      }),
    );
    await page.getByText("等待发送间隔", { exact: true }).waitFor();
    await countdown.getByText("15:00", { exact: true }).waitFor();
    assert.match(await countdown.innerText(), /距离下一批可发送还有/);
    assert.match(
      await page.locator(".mail-current").innerText(),
      /^待发送：.*第 2\/2 轮/,
    );
    const startCount = (await calls("start_mail_job")).length;
    const prepareCount = (await calls("prepare_mail_recipients")).length;
    for (const [width, theme] of [
      [1200, "light"],
      [1200, "dark"],
      [780, "light"],
      [780, "dark"],
      [375, "light"],
      [375, "dark"],
    ]) {
      await page.setViewportSize(testViewport(width, 900));
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
      }, theme);
      await progress.scrollIntoViewIfNeeded();
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth + 1,
        ),
        true,
      );
      await page
        .getByRole("button", { name: "停止后续发送", exact: true })
        .scrollIntoViewIfNeeded();
      await page.screenshot({
        path: new URL(
          `account-round-${theme}-${width}.png`,
          artifacts,
        ).pathname.replace(/^\/(\w:)/, "$1"),
        fullPage: false,
      });
    }
    await page.clock.runFor(2000);
    await countdown.getByText("14:58", { exact: true }).waitFor();
    assert.equal((await calls("start_mail_job")).length, startCount);
    assert.equal((await calls("prepare_mail_recipients")).length, prepareCount);
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        current: "第 2/2 轮 · 发件账号2 · 第 3/4 批，1 位收件人",
        message: "正在等待邮件服务器确认…",
      }),
    );
    await page.getByText("正在发送", { exact: true }).waitFor();
    assert.equal(await countdown.count(), 0);
    await page.evaluate(
      (state) => window.__uiFixture.emitMailState(state),
      waiting,
    );
    assert.equal(await countdown.count(), 0);
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        sent: 3,
        current: "第 2/2 轮 · 发件账号1 · 第 4/4 批，1 位收件人",
      }),
    );
    await page
      .locator(".mail-current")
      .filter({ hasText: /第 4\/4 批/ })
      .waitFor();
    assert.equal(await countdown.count(), 0);
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        status: "completed",
        phase: "idle",
        nextBatchAt: null,
        sent: 4,
        current: "",
        message: "所有轮次发送结束。",
      }),
    );
    await page.locator(".mail-progress-completed").waitFor();
    assert.equal(await countdown.count(), 0);
    // 上一轮或初始事件迟到不能恢复已结束任务的冷却。
    for (const state of [initial, waiting]) {
      await page.evaluate(
        (value) => window.__uiFixture.emitMailState(value),
        state,
      );
      assert.equal(await countdown.count(), 0);
    }
    // 模拟宿主保留的冷却期限：实际 stop/start 使用真实页面，池的时间计算由 Rust 证明。
    // 本次模拟 A 已完成一批；取消等待50秒后再发，必须显示剩余850秒而非重新900秒。
    const readyAt = await page.evaluate(() => {
      const deadline = Date.now() + 900_000;
      window.__uiFixture.publishMailState({
        id: "cooldown-before-cancel",
        status: "running",
        phase: "waiting",
        nextBatchAt: deadline,
        total: 1,
        sent: 0,
        failed: 0,
        skipped: 0,
        current: "第 1/1 轮 · 发件账号1 · 第 1/1 批，1 位收件人",
        message: "该账号仍在冷却，等待剩余时间。",
      });
      return deadline;
    });
    await countdown.getByText("15:00", { exact: true }).waitFor();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.clock.runFor(100);
    await page.locator(".mail-progress-cancelled").waitFor();
    assert.equal(await countdown.count(), 0);
    await page.clock.runFor(49_900);
    await page
      .locator("#mail-recipients")
      .fill("resume-after-cancel@example.test");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    await page.evaluate(
      (deadline) =>
        window.__uiFixture.publishMailState({
          phase: "waiting",
          nextBatchAt: deadline,
          current: "第 1/1 轮 · 发件账号1 · 第 1/1 批，1 位收件人",
          message: "继续等待账号原有冷却。",
        }),
      readyAt,
    );
    await page.getByText("等待发送间隔", { exact: true }).waitFor();
    await countdown.getByText("14:10", { exact: true }).waitFor();
    assert.ok(
      (await page.locator(".mail-current").innerText()).includes("第 1/1 轮"),
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.mail())).nextBatchAt,
      readyAt,
    );
    // 再取消一次并等待10秒，展示继续减少，重建任务不能给页面一个新的满900秒。
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.clock.runFor(100);
    await page.locator(".mail-progress-cancelled").waitFor();
    await page.clock.runFor(9_900);
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    await page.evaluate(
      (deadline) =>
        window.__uiFixture.publishMailState({
          phase: "waiting",
          nextBatchAt: deadline,
          current: "第 1/1 轮 · 发件账号1 · 第 1/1 批，1 位收件人",
          message: "继续等待账号原有冷却。",
        }),
      readyAt,
    );
    await countdown.getByText("14:00", { exact: true }).waitFor();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.mail())).nextBatchAt,
      readyAt,
    );
    // B 未冷却时宿主可以转为sending；渲染端必须立刻撤销A的等待，而不是自行计满。
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        current: "第 1/1 轮 · 发件账号2 · 第 1/1 批，1 位收件人",
        message: "优先发送已就绪账号。",
      }),
    );
    await page.getByText("正在发送", { exact: true }).waitFor();
    assert.equal(await countdown.count(), 0);
    console.log(
      "Account rounds UI: default60 switch countdown and generic wait reasons, per-account deadline/cancel-resume unchanged/retry remaining900, interval guidance/confirmation, counts, stale events, terminal cleanup and light/dark layouts checked (synthetic only)",
    );
  } finally {
    await context.close();
  }
}

/**
 * 多发件账号的目录与分配使用合成 IPC；验证真实页面展示和操作边界，绝不连接 SMTP。
 * Rust 回归另行验证实际队列、SQLite 与 DPAPI，不能把本夹具当作真实投递证明。
 */
async function checkMultiSenderAccounts() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const confirmation = page.getByRole("dialog", {
    name: "确认发送",
    exact: true,
  });
  const settings = page.getByRole("dialog", { name: "发件设置", exact: true });
  const calls = (name) =>
    page.evaluate(
      (command) =>
        window.__uiFixture.calls.filter((call) => call.command === command),
      name,
    );
  const candidates = (count) =>
    Array.from(
      { length: count },
      (_, index) => `multi${index + 1}@example.test`,
    );
  const prepare = async (addresses, expected) => {
    await page.locator("#mail-recipients").fill(addresses.join("\n"));
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirmation.waitFor();
    await confirmation.getByLabel("本次账号分配").waitFor();
    const plan = await page.evaluate(() =>
      window.__uiFixture.preparedMailPlans().at(-1),
    );
    assert.deepEqual(
      plan.accountAssignments
        .map((entry) => entry.recipientCount)
        .sort((left, right) => right - left),
      [...expected].sort((left, right) => right - left),
    );
    assert.equal(
      plan.total,
      expected.reduce((sum, count) => sum + count, 0),
    );
    assert.equal(
      (await calls("prepare_mail_recipients")).at(-1).args.input.allocationMode,
      "random",
    );
    assert.equal(
      await page
        .getByRole("combobox", { name: "分配方式", exact: true })
        .count(),
      0,
    );
    const rows = confirmation.getByLabel("本次账号分配").locator("li");
    assert.equal(await rows.count(), expected.length);
    for (let index = 0; index < expected.length; index += 1) {
      assert.match(
        await rows.nth(index).innerText(),
        new RegExp(
          plan.accountAssignments[index].fromAddress.replaceAll(".", "\\."),
        ),
      );
      assert.match(
        await rows.nth(index).innerText(),
        new RegExp(`${plan.accountAssignments[index].recipientCount}\\s*人`),
      );
    }
    return plan;
  };
  const back = () =>
    confirmation.getByRole("button", { name: "返回修改", exact: true }).click();
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.locator(".mail-delivery-settings > summary").click();
    for (let index = 1; index <= 5; index += 1)
      await page
        .getByRole("checkbox", {
          name: `本次使用 sender${index}@example.test`,
          exact: true,
        })
        .waitFor();
    await page.getByLabel("主题", { exact: true }).fill("多账号平均分配回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("正文、图片与收件人保持独立；仅模拟，不发送邮件。");
    for (const [count, expected] of [
      [1, [1, 0, 0, 0, 0]],
      [2, [1, 1, 0, 0, 0]],
      [3, [1, 1, 1, 0, 0]],
      [5, [1, 1, 1, 1, 1]],
      [8, [2, 2, 2, 1, 1]],
      [10, [2, 2, 2, 2, 2]],
    ]) {
      const plan = await prepare(candidates(count), expected);
      assert.equal(plan.totalMessageCount, Math.min(count, 5));
      await back();
    }
    assert.equal((await calls("start_mail_job")).length, 0, "预览分配不应发信");
    await page
      .getByLabel("发送方式", { exact: true })
      .selectOption("individual");
    assert.equal(
      (await prepare(candidates(8), [2, 2, 2, 1, 1])).totalMessageCount,
      8,
    );
    await back();
    await page.getByLabel("发送方式", { exact: true }).selectOption("bcc");

    // 所有账号只使用同一份全局历史和排除；重复地址也必须先去重，再均分实际人数。
    await page.evaluate(() => {
      window.__uiFixture.markSentWithoutPublishing(["multi10@example.test"]);
      window.__uiFixture.excludeWithoutPublishing(["multi11@example.test"]);
    });
    const plan = await prepare(
      [...candidates(11), "multi1@example.test"],
      [2, 2, 2, 2, 1],
    );
    assert.equal(plan.skipped, 1);
    assert.equal(plan.manualExcluded, 1);
    assert.equal(plan.total, 9);
    await back();

    // 取消全部后不允许静默回退首账号；选择次序不改变目录的先后规则。
    await page
      .getByRole("button", { name: "取消全选发件账号", exact: true })
      .click();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    assert.equal(await confirmation.isVisible(), false);
    if (await settings.isVisible())
      await settings.getByRole("button", { name: "取消", exact: true }).click();
    await page
      .getByRole("checkbox", {
        name: "本次使用 sender5@example.test",
        exact: true,
      })
      .check();
    await page
      .getByRole("checkbox", {
        name: "本次使用 sender2@example.test",
        exact: true,
      })
      .check();
    const selectedPlan = await prepare(candidates(8), [4, 4]);
    assert.deepEqual(
      selectedPlan.accountAssignments.map((entry) => entry.accountId).sort(),
      ["sender-2", "sender-5"],
    );
    await back();

    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    await settings
      .getByRole("button", {
        name: "上移账号 sender5@example.test",
        exact: true,
      })
      .click();
    await page.waitForFunction(
      () => window.__uiFixture.senderAccounts().accounts[3]?.id === "sender-5",
    );
    assert.equal((await calls("mail_accounts_reorder")).length, 1);
    await settings
      .getByRole("button", { name: "添加账号", exact: true })
      .click();
    await settings
      .getByLabel("账号名称（可选）", { exact: true })
      .fill("新增客服邮箱");
    await settings
      .getByLabel("SMTP 登录账号", { exact: true })
      .fill("service@example.test");
    await settings
      .getByLabel("密码或授权码", { exact: true })
      .fill("synthetic-account-code");
    await page.evaluate(() => window.__uiFixture.failNextAccountMutation());
    await settings
      .getByRole("button", { name: "保存配置", exact: true })
      .click();
    await toastMessage(page, "发件账号保存失败，原有账号保留。").waitFor();
    assert.equal(
      await settings.getByLabel("SMTP 登录账号", { exact: true }).inputValue(),
      "service@example.test",
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.senderAccounts())).accounts
        .length,
      5,
    );
    await page.evaluate(() => window.__uiFixture.holdNextAccountMutation());
    await settings
      .getByRole("button", { name: "保存配置", exact: true })
      .click();
    await page.waitForFunction(() =>
      window.__uiFixture.accountMutationPending(),
    );
    await settings
      .getByRole("button", { name: "保存配置", exact: true })
      .evaluate((button) => button.click());
    await page.evaluate(() => window.__uiFixture.releaseAccountMutation());
    await page.waitForFunction(
      () => window.__uiFixture.senderAccounts().accounts.length === 6,
    );
    assert.equal(
      (await calls("mail_account_save")).length,
      2,
      "失败一次及重试一次，连点不重复提交",
    );
    if (!(await settings.isVisible()))
      await page.getByRole("button", { name: "发件设置", exact: true }).click();
    await settings
      .getByRole("button", {
        name: "删除账号 service@example.test",
        exact: true,
      })
      .click();
    const deletion = page.getByRole("dialog", {
      name: "删除发件账号",
      exact: true,
    });
    await deletion.waitFor();
    await deletion.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal((await calls("mail_account_remove")).length, 0);
    await settings
      .getByRole("button", {
        name: "删除账号 service@example.test",
        exact: true,
      })
      .click();
    await deletion
      .getByRole("button", { name: "确认删除账号", exact: true })
      .click();
    await page.waitForFunction(
      () => window.__uiFixture.senderAccounts().accounts.length === 5,
    );
    await settings.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "多账号平均分配回归",
    );
    assert.match(
      await page
        .getByRole("textbox", { name: "邮件正文", exact: true })
        .innerText(),
      /正文、图片/,
    );

    await page
      .getByRole("button", { name: "全选发件账号", exact: true })
      .click();
    await prepare(candidates(8), [2, 2, 2, 1, 1]);
    await page.evaluate(() =>
      window.__uiFixture.changeAccountsWithoutPublishing(),
    );
    const accountReadsBeforeStale = (await calls("mail_accounts")).length;
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await toastMessage(page, /发件账号.*变化.*重新确认/).waitFor();
    await page.waitForFunction(
      (previous) =>
        window.__uiFixture.calls.filter(
          (call) => call.command === "mail_accounts",
        ).length > previous,
      accountReadsBeforeStale,
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.accountAssignments()))
        .length,
      0,
      "过期确认不得开始任何账号投递",
    );
    await page.reload();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.senderAccounts()))
        .accounts[3].id,
      "sender-5",
    );
    await page.getByLabel("主题", { exact: true }).fill("发送边界回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("重新载入后仅验证固定分配与取消，不连接 SMTP。");
    const finalPlan = await prepare(candidates(8), [2, 2, 2, 1, 1]);
    assert.deepEqual(
      finalPlan.accountAssignments.map((entry) => entry.accountId).sort(),
      ["sender-1", "sender-2", "sender-3", "sender-4", "sender-5"],
    );
    await confirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.accountAssignments()),
      finalPlan.accountAssignments,
      "确认后必须使用同一份随机分配，发送时不能重新打乱",
    );
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.locator(".mail-progress-cancelled").waitFor();
    assert.deepEqual(pageErrors, []);
    console.log(
      "Multi sender accounts: five-account allocation 1/2/3/5/8/10, actual shared-history/exclusion counts, ordered subsets, no-selection gate, CRUD/reorder/cancel/failure/retry/duplicate lock/reload and stale confirmation checked (synthetic only)",
    );
  } finally {
    await context.close();
  }
}

/** 授权码不回显；保存、复用、身份变更和主动清除均通过明确交互验证。 */
async function checkSavedMailPassword() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const settingsDialog = page.getByRole("dialog", {
    name: "发件设置",
    exact: true,
  });
  const password = settingsDialog.getByLabel("密码或授权码", { exact: true });
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    assert.equal(await password.inputValue(), "");
    assert.equal(
      await password.getAttribute("placeholder"),
      "已保存授权码，留空继续使用",
    );
    assert.equal(
      await password.getAttribute("placeholder"),
      "已保存授权码，留空继续使用",
    );
    await page.screenshot({
      path: new URL("smtp-credential-saved.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    // 显示名称不是凭据身份的一部分，修改名称后仍允许复用此前保存的授权码。
    await settingsDialog
      .getByLabel("发件人名称（可选）", { exact: true })
      .fill("本机界面回归");
    await settingsDialog
      .getByRole("button", { name: "保存配置", exact: true })
      .click();
    const rename = (await calls("mail_account_save")).at(-1);
    assert.equal(rename.args.input.settings.password, "");
    assert.equal(rename.args.input.id, "legacy-default");
    await page.locator("#mail-recipients").fill("recipient@example.test");
    await page.getByLabel("主题", { exact: true }).fill("授权码复用回归");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("仅模拟发送参数，不连接 SMTP。");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.getByRole("button", { name: "确认发送", exact: true }).click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const reused = (await calls("start_mail_job")).at(-1);
    assert.equal(reused.args.settings.password, "");
    assert.equal(reused.args.settings.username, "sender@example.test");
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.locator(".mail-progress-cancelled").waitFor();

    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    await settingsDialog
      .getByRole("button", { name: "清除已保存授权码", exact: true })
      .click();
    await toastMessage(page, /授权码.*清除|已清除.*授权码/).waitFor();
    await checkToastReceivesPointer(
      toastMessage(page, /授权码.*清除|已清除.*授权码/),
    );
    assert.equal(
      await settingsDialog
        .locator("[data-sonner-toast]")
        .filter({ hasText: /授权码.*清除|已清除.*授权码/ })
        .count(),
      1,
      "Toasts for an open native dialog must join that dialog's top layer",
    );
    await settingsDialog
      .getByRole("button", { name: "取消", exact: true })
      .click();
    await toastMessage(page, /授权码.*清除|已清除.*授权码/).waitFor();
    assert.equal(
      await settingsDialog.locator("[data-sonner-toast]").count(),
      0,
    );
    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    await page.waitForFunction(
      () =>
        document.querySelector('input[type="password"]')?.placeholder ===
        "输入邮箱授权码",
    );
    assert.equal(await password.inputValue(), "");
    assert.equal((await calls("mail_account_clear_password")).length, 1);
    assert.equal(
      await settingsDialog
        .getByText("授权码已保存，下次自动使用", { exact: true })
        .count(),
      0,
    );
    await password.fill("fixture-new-code-not-a-real-secret");
    await settingsDialog
      .getByRole("button", { name: "保存配置", exact: true })
      .click();
    const saved = (await calls("mail_account_save")).at(-1);
    assert.equal(
      saved.args.input.settings.password,
      "fixture-new-code-not-a-real-secret",
    );
    assert.deepEqual(Object.keys(saved.args.input.settings).sort(), [
      "fromAddress",
      "fromName",
      "host",
      "password",
      "port",
      "tlsMode",
      "username",
    ]);
    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    assert.equal(await password.inputValue(), "");
    assert.equal(
      await password.getAttribute("placeholder"),
      "已保存授权码，留空继续使用",
    );

    // 改动账号及自动跟随的发件地址，旧凭据立即失去可复用标识，不能静默用于另一身份。
    await settingsDialog
      .getByLabel("SMTP 登录账号", { exact: true })
      .fill("replacement@example.test");
    assert.equal(await password.getAttribute("placeholder"), "输入邮箱授权码");
    assert.equal(
      await settingsDialog
        .getByText("授权码已保存，下次自动使用", { exact: true })
        .count(),
      0,
    );
    await settingsDialog
      .getByRole("button", { name: "保存配置", exact: true })
      .click();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await settingsDialog.waitFor();
    await toastMessage(
      page,
      "本次参与发送的账号 replacement@example.test 授权码不可用，请在发件设置中补充后保存。",
    ).waitFor();
    assert.equal(
      await settingsDialog
        .locator("[data-sonner-toast]")
        .filter({
          hasText:
            "本次参与发送的账号 replacement@example.test 授权码不可用，请在发件设置中补充后保存。",
        })
        .count(),
      1,
      "A missing credential error must stay visible inside its open settings dialog",
    );
    const credentialError = toastMessage(
      page,
      "本次参与发送的账号 replacement@example.test 授权码不可用，请在发件设置中补充后保存。",
    );
    await checkToastReceivesPointer(credentialError);
    await credentialError
      .getByRole("button", { name: "关闭提示", exact: true })
      .click();
    await credentialError.waitFor({ state: "hidden" });
    assert.equal(
      await settingsDialog.isVisible(),
      true,
      "Closing feedback must leave the settings dialog open",
    );
    assert.equal((await calls("start_mail_job")).length, 1);
    assert.equal(
      await page
        .getByRole("dialog", { name: "确认发送", exact: true })
        .isVisible(),
      false,
    );
    await page.screenshot({
      path: new URL("smtp-credential-binding.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
  } finally {
    await context.close();
  }
}

/** 外观偏好跨重新载入恢复，同时向宿主提交显式主题或系统 null，不把系统颜色固化。 */
async function checkThemePersistence() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 820),
    colorScheme: "light",
  });
  await context.addInitScript(installFixture);
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const waitNativeTheme = (theme) =>
    page.waitForFunction(
      (expected) =>
        window.__uiFixture.calls.some(
          (entry) =>
            entry.command === "plugin:window|set_theme" &&
            entry.args.value === expected,
        ),
      theme,
    );
  try {
    await page.goto("http://127.0.0.1:1420");
    await waitNativeTheme(null);
    assert.equal(
      await page.locator("html").getAttribute("data-theme"),
      "system",
    );
    assert.equal(await page.locator(".app-header .version").count(), 0);
    assert.match(
      await page.locator(".brand").getAttribute("title"),
      /batch-send-email/,
    );
    await page.getByRole("button", { name: /外观：跟随系统/ }).click();
    await waitNativeTheme("light");
    await page.getByRole("button", { name: /外观：浅色/ }).click();
    await waitNativeTheme("dark");
    assert.equal(
      await page.evaluate(() =>
        localStorage.getItem("qq-group-exporter-theme"),
      ),
      "dark",
    );
    await page.reload();
    await waitNativeTheme("dark");
    assert.equal(await page.locator("html").getAttribute("data-theme"), "dark");
    await page.getByRole("button", { name: /外观：深色/ }).click();
    await waitNativeTheme(null);
    await page.emulateMedia({ colorScheme: "dark" });
    assert.match(
      await page
        .locator("html")
        .evaluate((element) => getComputedStyle(element).colorScheme),
      /dark/,
    );
    await page.reload();
    await waitNativeTheme(null);
    assert.equal(
      await page.locator("html").getAttribute("data-theme"),
      "system",
    );
    // 没有提示时 Sonner 不渲染列表；通过真实表单校验生成反馈后再核对其系统主题。
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.waitForFunction(
      () =>
        document
          .querySelector("[data-sonner-toaster]")
          ?.getAttribute("data-sonner-theme") === "dark",
    );
    await page.getByRole("button", { name: /外观：跟随系统/ }).click();
    await waitNativeTheme("light");
    await page.emulateMedia({ colorScheme: "light" });
    await page.emulateMedia({ colorScheme: "dark" });
    await page.waitForFunction(
      () =>
        document
          .querySelector("[data-sonner-toaster]")
          ?.getAttribute("data-sonner-theme") === "light",
    );
  } finally {
    await context.close();
  }
}

/**
 * 模拟宿主公开的好友关系与读取失败，验证页面保留名单、账号边界和邮件交接。
 * 本测试不访问好友 HTTP 接口，不能验证 get_friend_list 完成性或旧 fr 字段的真实含义。
 */
async function checkFriendFiltering() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
    timezoneId: "Asia/Shanghai",
  });
  await context.addInitScript(installFixture, { friendScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const friendOption = page.getByLabel("排除我的 QQ 好友", { exact: true });
  // 好友开关按当前账号的操作偏好恢复；等待DOM达到预期，再断言，避免读取切号过渡帧。
  const waitForFriendOption = async (expected) => {
    const input = await friendOption.elementHandle();
    await page.waitForFunction(
      ({ element, checked }) => element?.checked === checked,
      { element: input, checked: expected },
    );
    await input?.dispose();
  };
  const currentSnapshot = () =>
    page.evaluate(() => window.__uiFixture.snapshot());
  const fetchCompleted = async () => {
    const previous = await currentSnapshot();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      (previousId) =>
        window.__uiFixture.taskStatus() === "completed" &&
        window.__uiFixture.snapshot()?.id !== previousId,
      previous?.id ?? null,
    );
    return currentSnapshot();
  };
  const chooseTarget = () =>
    page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
  try {
    await page.goto("http://127.0.0.1:1420");
    assert.equal(
      await page.locator(".brand .app-mark img").getAttribute("alt"),
      "",
    );
    assert.equal(
      await page
        .locator(".brand .app-mark img")
        .evaluate((image) => image.complete && image.naturalWidth > 0),
      true,
    );
    await chooseTarget();
    await waitForFriendOption(false);
    assert.equal(await friendOption.isChecked(), false);
    await page.getByLabel("过滤已发送记录").uncheck();
    const unfiltered = await fetchCompleted();
    assert.equal(unfiltered.members.length, 6);
    assert.equal(unfiltered.excludeFriends, false);
    assert.equal(unfiltered.friendFilterStatus, "not_requested");
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls
            .filter((entry) => entry.command === "fetch_members")
            .at(-1).args.excludeFriends,
      ),
      false,
    );

    // 升级前快照没有关系字段，只可继续查看/导出；勾选选项不能就地把unknown假装过滤完成。
    await page.evaluate(() => window.__uiFixture.publishLegacySnapshot());
    await page
      .getByRole("tab", { name: "筛选后成员 6", exact: true })
      .waitFor();
    await waitForFriendOption(false);
    assert.equal(await friendOption.isChecked(), false);
    const legacy = await currentSnapshot();
    await friendOption.check();
    assert.equal((await currentSnapshot()).id, legacy.id);
    assert.equal((await currentSnapshot()).members.length, 6);
    await page
      .getByRole("button", { name: "排除其他群的成员", exact: false })
      .click();
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .check();
    const filtered = await fetchCompleted();
    assert.equal(filtered.sourceCount, 6);
    assert.equal(filtered.members.length, 3);
    assert.equal(filtered.excludedCount, 1);
    assert.equal(filtered.friendFilteredCount, 2);
    assert.equal(filtered.friendUnknownCount, 0);
    assert.equal(filtered.friendFilterStatus, "completed");
    assert.equal(
      filtered.sourceCount,
      filtered.members.length + filtered.filteredMembers.length,
    );
    assert.deepEqual(
      filtered.filteredMembers.map((entry) => entry.reason),
      ["group", "friend", "friend"],
    );
    assert.equal(
      new Set(filtered.filteredMembers.map((entry) => entry.member.qqNumber))
        .size,
      3,
    );
    await page.getByRole("tab", { name: "已过滤 3", exact: true }).click();
    assert.equal(
      await page
        .locator(".filter-reason")
        .filter({ hasText: "QQ 好友" })
        .count(),
      2,
    );
    assert.equal(
      await page
        .locator(".filter-reason")
        .filter({ hasText: "排除群重合" })
        .count(),
      1,
    );
    await page.screenshot({
      path: new URL("friends-filtered-summary.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await page
      .locator(".filter-reason")
      .filter({ hasText: "QQ 好友" })
      .first()
      .scrollIntoViewIfNeeded();
    await page.screenshot({
      path: new URL("friends-filtered-details.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await page.getByRole("tab", { name: "筛选后成员 3", exact: true }).click();
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    await toastMessage(page, "文件已保存").waitFor();
    const exportCall = await page.evaluate(() =>
      window.__uiFixture.calls
        .filter((entry) => entry.command === "export_members")
        .at(-1),
    );
    assert.equal(exportCall.args.snapshotId, filtered.id);
    await page
      .getByRole("button", { name: "给全部 3 人写信", exact: true })
      .click();
    await page.locator(".mail-audience-details summary").click();
    assert.match(
      await page.locator(".mail-audience-details").innerText(),
      /已排除获取账号的 QQ 好友 2 人/,
    );
    const friendQq = filtered.filteredMembers.find(
      (entry) => entry.reason === "friend",
    ).member.qqNumber;
    await page.locator("#mail-recipients").fill(`${friendQq}@qq.com`);
    await page
      .locator(".mail-recipient-boundary")
      .filter({ hasText: /本群已过滤或未选成员/ })
      .waitFor();
    await page.getByRole("button", { name: "发送", exact: true }).click();
    assert.equal(
      await page
        .getByRole("dialog", { name: "确认发送", exact: true })
        .isVisible(),
      false,
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "start_mail_job",
          ).length,
      ),
      0,
    );

    // 调整原名单和切群沿用同账号上次选择；只有新QQ账号使用自己的默认值/已保存偏好。
    await page.getByRole("button", { name: "调整群成员", exact: true }).click();
    await waitForFriendOption(true);
    assert.equal(await friendOption.isChecked(), true);
    await page
      .locator(".group-list")
      .getByRole("button", { name: /排除测试群/ })
      .click();
    await waitForFriendOption(true);
    assert.equal(await friendOption.isChecked(), true);
    await chooseTarget();
    await waitForFriendOption(true);
    assert.equal(await friendOption.isChecked(), true);
    assert.equal((await currentSnapshot()).id, filtered.id);
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    assert.match(
      await page.locator(".mail-audience-counts strong").innerText(),
      /3/,
    );
    await page.getByRole("button", { name: "调整群成员", exact: true }).click();
    await waitForFriendOption(true);
    assert.equal(await friendOption.isChecked(), true);
    await chooseTarget();
    await friendOption.check();
    await page.getByLabel("过滤已发送记录").uncheck();
    const exclusionToggle = page.getByRole("button", {
      name: /排除其他群的成员/,
    });
    if ((await exclusionToggle.getAttribute("aria-expanded")) === "false")
      await exclusionToggle.click();
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .check();
    // 好友请求失败沿用既有任务浮层；不得清空旧结果或发布“未排除好友”的替代名单。
    const beforeFriendReadFailure = await currentSnapshot();
    for (const [code, message] of [
      ["connection_failed", "无法连接 QQ 服务，请检查网络后重试。"],
      [
        "friend_list_incomplete",
        "QQ 未返回可确认完整的好友列表，未更新名单。请重试，或取消“排除好友”后重新获取。",
      ],
    ]) {
      await page.evaluate(
        (failureCode) => window.__uiFixture.failNextFriendList(failureCode),
        code,
      );
      await page.getByRole("button", { name: "获取成员", exact: true }).click();
      await page.waitForFunction(
        () => window.__uiFixture.taskStatus() === "failed",
      );
      await toastMessage(page, message).waitFor();
      assert.deepEqual(await currentSnapshot(), beforeFriendReadFailure);
      assert.equal(
        await page
          .getByRole("tab", { name: "筛选后成员 3", exact: true })
          .isVisible(),
        true,
      );
    }
    // 即使未知成员与排除群重合，仍无法证明“已排除全部好友”，必须失败并保留旧结果。
    await page.evaluate(() =>
      window.__uiFixture.setFriendRelation("10000001", "unknown"),
    );
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "failed",
    );
    await toastMessage(page, /无法确认 1 位成员的好友关系/).waitFor();
    assert.equal((await currentSnapshot()).id, filtered.id);
    assert.equal(
      await page
        .getByRole("tab", { name: "筛选后成员 3", exact: true })
        .isVisible(),
      true,
    );
    await friendOption.uncheck();
    const unknownAllowed = await fetchCompleted();
    assert.equal(unknownAllowed.excludeFriends, false);
    assert.equal(unknownAllowed.friendUnknownCount, 1);
    assert.equal(unknownAllowed.friendFilterStatus, "not_requested");

    await page.evaluate(() =>
      window.__uiFixture.setAllFriendRelations("friend"),
    );
    await friendOption.check();
    const allExcluded = await fetchCompleted();
    assert.equal(allExcluded.members.length, 0);
    assert.equal(allExcluded.excludedCount, 1);
    assert.equal(allExcluded.friendFilteredCount, 5);
    await page
      .getByRole("tab", { name: "筛选后成员 0", exact: true })
      .waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: "给全部 0 人写信", exact: true })
        .isDisabled(),
      true,
    );
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    await toastMessage(page, "文件已保存").waitFor();
    assert.equal((await currentSnapshot()).id, allExcluded.id);
    await page.evaluate(() => window.__uiFixture.switchAccount("987654321"));
    await page
      .locator(".group-list .group-item.selected")
      .waitFor({ state: "hidden" });
    await chooseTarget();
    await waitForFriendOption(false);
    assert.equal(await friendOption.isChecked(), false);
    await page
      .getByRole("heading", { name: "该群还没有已保存名单", exact: true })
      .waitFor();
    assert.equal(
      await page
        .getByRole("tab", { name: "筛选后成员 0", exact: true })
        .count(),
      0,
    );
    console.log(
      "Synthetic friend UI contracts: read-failure toast/old-result preservation, identity, unknown boundaries, overlap, all-excluded, legacy and mail/export flow checked (no QQ HTTP verification)",
    );
  } finally {
    await context.close();
  }
}

/**
 * 模板沿用真实写信入口和隔离 IPC：验证显式保存、资源重新登记、取消/失败原子性，
 * 以及模板管理不会改动被冻结的群收件范围、补充邮箱或发件凭据。
 */
async function checkMailTemplates() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const savedTemplates = () =>
    page.evaluate(() => window.__uiFixture.savedTemplates());
  const editor = () =>
    page.getByRole("textbox", { name: "邮件正文", exact: true });
  const library = () =>
    page.getByRole("dialog", { name: "邮件模板", exact: true });
  const selectTemplate = async (name) => {
    await library()
      .locator(".mail-template-item")
      .filter({
        has: page.getByText(name, { exact: true }),
      })
      .click();
    await library().getByRole("heading", { name, exact: true }).waitFor();
  };
  const openLibrary = async () => {
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    await library().waitFor();
    await page.waitForFunction(
      () =>
        document
          .querySelector(".mail-template-list")
          ?.getAttribute("aria-busy") === "false",
    );
  };
  const saveAs = async (name, includeAttachments = false) => {
    await page.getByRole("button", { name: "保存为模板", exact: true }).click();
    const dialog = page.getByRole("dialog", {
      name: "保存邮件模板",
      exact: true,
    });
    await dialog.waitFor();
    assert.equal(await dialog.getByRole("checkbox").isChecked(), false);
    await dialog.getByLabel("模板名称", { exact: true }).fill(name);
    if (includeAttachments) await dialog.getByRole("checkbox").check();
    const nameInput = dialog.getByLabel("模板名称", { exact: true });
    const savesBefore = (await calls("mail_template_save")).length;
    await nameInput.dispatchEvent("keydown", {
      key: "Enter",
      code: "Enter",
      isComposing: true,
    });
    assert.equal((await calls("mail_template_save")).length, savesBefore);
    assert.equal(await dialog.isVisible(), true);
    await nameInput.press("Enter");
    await dialog.waitFor({ state: "hidden" });
  };
  const replacement = () =>
    page.getByRole("dialog", { name: "替换邮件内容", exact: true });
  try {
    await page.goto("http://127.0.0.1:1420");
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    await page
      .getByRole("button", { name: "给全部 61 人写信", exact: true })
      .click();
    await page.locator("#mail-recipients").fill("template-extra@example.test");
    await page.getByLabel("主题", { exact: true }).fill("模板主题与图片");
    await editor().fill("模板正文保留图片和格式。");
    await pasteSyntheticImage(page);
    await editor().locator("img[data-inline-image-id]").waitFor();
    await page.getByRole("button", { name: "添加附件", exact: true }).click();
    await page
      .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
      .waitFor();
    await saveAs("测试模板-仅正文");
    await saveAs("测试模板-含附件", true);
    const initialTemplates = await savedTemplates();
    assert.equal(initialTemplates.length, 2);
    const bodyTemplate = initialTemplates.find(
      (entry) => entry.name === "测试模板-仅正文",
    );
    const fileTemplate = initialTemplates.find(
      (entry) => entry.name === "测试模板-含附件",
    );
    assert.equal(bodyTemplate.inlineImages.length, 1);
    assert.equal(bodyTemplate.attachments.length, 0);
    assert.equal(fileTemplate.attachments.length, 1);
    assert.match(bodyTemplate.html, /cid:inline-fixture-1@fixture\.local/);
    assert.doesNotMatch(bodyTemplate.html, /blob:|data:image/);
    for (const value of initialTemplates) {
      assert.deepEqual(Object.keys(value).sort(), [
        "attachments",
        "html",
        "id",
        "inlineImages",
        "name",
        "subject",
        "updatedAt",
      ]);
      assert.doesNotMatch(
        JSON.stringify(value),
        /template-extra@example\.test|sender@example\.test|qqNumbers|groupAudience|password/,
      );
    }
    const inputs = (await calls("mail_template_save")).map(
      (entry) => entry.args.input,
    );
    assert.equal(inputs[0].includeAttachments, false);
    assert.deepEqual(inputs[0].attachmentIds, []);
    assert.equal(inputs[1].includeAttachments, true);
    assert.deepEqual(inputs[1].attachmentIds, ["attachment-fixture"]);
    assert.equal(
      inputs.some((input) => "updateId" in input),
      false,
    );

    await page.getByLabel("主题", { exact: true }).fill("必须保留的未保存主题");
    const beforeResources = await page.evaluate(() =>
      window.__uiFixture.draftResourceIds(),
    );
    const beforeHtml = await editor().innerHTML();
    await openLibrary();
    assert.equal(await library().locator(".mail-template-item").count(), 2);
    await library()
      .getByLabel("搜索模板名称或主题", { exact: true })
      .fill("含附件");
    assert.equal(await library().locator(".mail-template-item").count(), 1);
    await library()
      .getByLabel("搜索模板名称或主题", { exact: true })
      .fill("不匹配的名称");
    await library().getByText("没有匹配的模板", { exact: true }).waitFor();
    await library()
      .getByRole("button", { name: "清除模板搜索", exact: true })
      .click();
    assert.equal(
      await library()
        .getByLabel("搜索模板名称或主题", { exact: true })
        .evaluate((element) => document.activeElement === element),
      true,
    );
    assert.equal(await library().locator(".mail-template-item").count(), 2);
    await library()
      .getByLabel("搜索模板名称或主题", { exact: true })
      .fill("模板主题");
    assert.equal(await library().locator(".mail-template-item").count(), 2);
    await selectTemplate(bodyTemplate.name);
    assert.equal(await library().locator("iframe").getAttribute("sandbox"), "");
    await library().frameLocator("iframe").locator("img").waitFor();
    assert.match(
      await library().frameLocator("iframe").locator("img").getAttribute("src"),
      /^data:image\/png;base64,/,
    );
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.draftResourceIds()),
      beforeResources,
    );
    assert.equal((await calls("mail_template_apply")).length, 0);
    await page.screenshot({
      path: new URL("mail-templates-preview.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    for (const theme of ["light", "dark"]) {
      if (theme === "dark") {
        await library()
          .getByRole("button", { name: "关闭邮件模板", exact: true })
          .click();
        await page.getByRole("button", { name: /外观：跟随系统/ }).click();
        await page.getByRole("button", { name: /外观：浅色/ }).click();
        assert.equal(
          await page.locator("html").getAttribute("data-theme"),
          "dark",
        );
        await openLibrary();
        await selectTemplate(bodyTemplate.name);
      }
      for (const width of testWidths([1200, 780, 375])) {
        await page.setViewportSize(testViewport(width, 900));
        assert.equal(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth + 1,
          ),
          true,
          `The template page must fit ${width}px in ${theme} mode`,
        );
        const bounds = await library().boundingBox();
        assert.equal(
          bounds.x >= -1 && bounds.x + bounds.width <= width + 1,
          true,
          `The template dialog must stay inside ${width}px in ${theme} mode`,
        );
        assert.equal(
          await library().evaluate(
            (element) => element.scrollWidth <= element.clientWidth + 1,
          ),
          true,
          `The template dialog must not scroll horizontally at ${width}px in ${theme} mode`,
        );
        assert.equal(
          await library()
            .locator(".mail-template-layout")
            .evaluate(
              (element) => element.scrollWidth <= element.clientWidth + 1,
            ),
          true,
          `The template list and preview must fit at ${width}px in ${theme} mode`,
        );
        await page.screenshot({
          path: new URL(
            `mail-templates-${theme}-${width}.png`,
            artifacts,
          ).pathname.replace(/^\/(\w:)/, "$1"),
          fullPage: false,
        });
      }
      await page.setViewportSize(testViewport(1200, 900));
    }
    await page.setViewportSize(testViewport(1200, 900));
    await library()
      .getByRole("button", { name: "使用此模板", exact: true })
      .click();
    await replacement().waitFor();
    await replacement()
      .getByRole("button", { name: "取消", exact: true })
      .click();
    assert.equal((await calls("mail_template_apply")).length, 0);
    await library()
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "必须保留的未保存主题",
    );
    assert.equal(await editor().innerHTML(), beforeHtml);

    // 应用失败保留整份原草稿；重试期间同步门闩阻止关闭、导航和重复应用。
    await openLibrary();
    await selectTemplate(bodyTemplate.name);
    await library()
      .getByRole("button", { name: "使用此模板", exact: true })
      .click();
    await page.evaluate(() => window.__uiFixture.failNextTemplate("apply"));
    await replacement()
      .getByRole("button", { name: "替换内容", exact: true })
      .click();
    await toastMessage(page, "模板操作暂时失败，请重试。").waitFor();
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.draftResourceIds()),
      beforeResources,
    );
    await page.evaluate(() => window.__uiFixture.holdNextTemplateApply());
    await replacement()
      .getByRole("button", { name: "替换内容", exact: true })
      .click();
    await page.waitForFunction(() => window.__uiFixture.templateApplyPending());
    for (const button of await page.locator(".app-nav > button").all())
      assert.equal(await button.isDisabled(), true);
    assert.equal(
      await replacement()
        .getByRole("button", { name: "替换内容", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await replacement()
        .getByRole("button", { name: "取消", exact: true })
        .isDisabled(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.releaseTemplateApply());
    await replacement().waitFor({ state: "hidden" });
    await library().waitFor({ state: "hidden" });
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      bodyTemplate.subject,
    );
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      "template-extra@example.test",
    );
    assert.match(
      await page.locator(".mail-audience-counts strong").innerText(),
      /61/,
    );
    assert.equal(
      await page
        .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
        .count(),
      0,
    );
    const appliedImage = editor().locator("img[data-inline-image-id]");
    assert.match(
      await appliedImage.getAttribute("data-inline-image-id"),
      /^applied-image-/,
    );
    assert.match(await appliedImage.getAttribute("src"), /^blob:/);
    assert.equal((await calls("mail_template_apply")).length, 2);
    assert.equal((await calls("start_mail_job")).length, 0);

    await page.getByLabel("主题", { exact: true }).fill("更新后的模板主题");
    await page.getByRole("button", { name: "更新模板", exact: true }).click();
    const updateDialog = page.getByRole("dialog", {
      name: "更新邮件模板",
      exact: true,
    });
    await updateDialog
      .getByRole("button", { name: "更新模板", exact: true })
      .click();
    await updateDialog.waitFor({ state: "hidden" });
    const afterUpdate = await savedTemplates();
    assert.equal(afterUpdate.length, 2);
    assert.equal(
      afterUpdate.find((entry) => entry.id === bodyTemplate.id).subject,
      "更新后的模板主题",
    );
    assert.equal(
      (await calls("mail_template_save")).at(-1).args.input.updateId,
      bodyTemplate.id,
    );
    await saveAs("测试模板-另存");
    assert.equal((await savedTemplates()).length, 3);
    assert.equal(
      "updateId" in (await calls("mail_template_save")).at(-1).args.input,
      false,
    );

    // 名称管理和删除只修改模板库，不覆盖当前草稿，也不能跳过显式删除确认。
    await openLibrary();
    await selectTemplate(fileTemplate.name);
    await library()
      .getByRole("button", { name: "重命名模板", exact: true })
      .click();
    const rename = page.getByRole("dialog", {
      name: "重命名模板",
      exact: true,
    });
    await rename
      .getByLabel("模板名称", { exact: true })
      .fill("测试模板-重命名含附件");
    await rename.getByLabel("模板名称", { exact: true }).press("Enter");
    await rename.waitFor({ state: "hidden" });
    await library()
      .getByRole("heading", { name: "测试模板-重命名含附件", exact: true })
      .waitFor();
    await library()
      .getByRole("button", { name: "复制模板", exact: true })
      .click();
    const duplicate = page.getByRole("dialog", {
      name: "复制模板",
      exact: true,
    });
    await duplicate
      .getByLabel("模板名称", { exact: true })
      .fill("测试模板-待删除副本");
    await duplicate.getByLabel("模板名称", { exact: true }).press("Enter");
    await duplicate.waitFor({ state: "hidden" });
    await selectTemplate("测试模板-待删除副本");
    await library()
      .getByRole("button", { name: "删除模板", exact: true })
      .click();
    const deletion = page.getByRole("dialog", {
      name: "删除模板",
      exact: true,
    });
    await deletion.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal((await savedTemplates()).length, 4);
    assert.equal((await calls("mail_template_delete")).length, 0);
    await library()
      .getByRole("button", { name: "删除模板", exact: true })
      .click();
    await deletion
      .getByRole("button", { name: "确认删除", exact: true })
      .click();
    await deletion.waitFor({ state: "hidden" });
    assert.equal((await savedTemplates()).length, 3);
    await library()
      .getByRole("button", { name: "新建模板", exact: true })
      .click();
    const newConfirmation = page.getByRole("dialog", {
      name: "新建模板",
      exact: true,
    });
    await newConfirmation
      .getByRole("button", { name: "替换内容", exact: true })
      .click();
    await newConfirmation.waitFor({ state: "hidden" });
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "",
    );
    assert.equal((await editor().innerText()).trim(), "");
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.draftResourceIds()),
      { inlineImages: [], attachments: [] },
    );
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      "template-extra@example.test",
    );
    assert.match(
      await page.locator(".mail-audience-counts strong").innerText(),
      /61/,
    );
    await saveAs("测试模板-空白草稿");

    // 此处仅验证渲染器重载契约；真正 SQLite 与资源落盘由后台回归验证，夹具不冒充数据库。
    await page.reload();
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    assert.equal(await page.locator("#mail-recipients").inputValue(), "");
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "",
    );
    await openLibrary();
    assert.equal(await library().locator(".mail-template-item").count(), 4);
    await selectTemplate("测试模板-重命名含附件");
    await library()
      .getByRole("button", { name: "编辑模板", exact: true })
      .click();
    await replacement()
      .getByRole("button", { name: "替换内容", exact: true })
      .click();
    await replacement().waitFor({ state: "hidden" });
    await page
      .getByRole("button", { name: "移除附件 本机测试附件.txt", exact: true })
      .waitFor();
    assert.match(await editor().locator("img").getAttribute("src"), /^blob:/);
    assert.equal((await calls("start_mail_job")).length, 0);
    await page.locator("#mail-recipients").fill("template-send@example.test");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const sendConfirmation = page.getByRole("dialog", {
      name: "确认发送",
      exact: true,
    });
    await sendConfirmation
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page.waitForFunction(() =>
      window.__uiFixture.calls.some(
        (entry) => entry.command === "start_mail_job",
      ),
    );
    const draft = (await calls("start_mail_job")).at(-1).args.draft;
    assert.equal(draft.recipients, "template-send@example.test");
    assert.match(draft.inlineImageIds[0], /^applied-image-/);
    assert.match(draft.attachmentIds[0], /^applied-attachment-/);
    assert.match(draft.html, /src="cid:applied-image-/);
    assert.doesNotMatch(draft.html, /blob:|data:image|inline-fixture/);
    await page.evaluate(() => window.__uiFixture.finishMail());
    console.log(
      "Mail templates save/search/preview/apply/update/save-as/manage/reload, resource CID, scope preservation and failure/operation-lock boundaries checked",
    );
  } finally {
    await context.close();
  }
}

/**
 * 快捷发送无需 QQ 会话；全局手动排除在发送确认中可核对，运行任务时维护锁定。
 * 全部账号、模板和图片均为浏览器合成数据，不访问真实数据库或邮件服务器。
 */
async function checkQuickSendAndManualExclusions() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
    colorScheme: "light",
  });
  await context.addInitScript(installFixture, {
    manualScenario: true,
    initialSessionStatus: "signed_out",
    initialPasswordSaved: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const manualPage = () => page.locator(".manual-exclusions-page");
  const openManual = async () => {
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    await manualPage()
      .getByRole("heading", { name: "手动排除", exact: true })
      .waitFor();
    await page.waitForFunction(
      () =>
        !document.querySelector(".manual-exclusions-input textarea")?.disabled,
    );
  };
  const confirm = () =>
    page.getByRole("dialog", { name: "确认发送", exact: true });
  const editor = () =>
    page.getByRole("textbox", { name: "邮件正文", exact: true });
  try {
    await page.goto("http://127.0.0.1:1420");
    await openManual();
    await manualPage().getByText("暂无手动排除", { exact: true }).waitFor();
    await page.evaluate(() => window.__uiFixture.failNextManualRead());
    await manualPage()
      .getByRole("button", { name: "刷新", exact: true })
      .click();
    await manualPage().getByRole("alert").waitFor();
    await manualPage()
      .getByRole("button", { name: "重试", exact: true })
      .click();
    await page.waitForFunction(
      () =>
        !document.querySelector(".manual-exclusions-input textarea")?.disabled,
    );
    const input = manualPage().getByLabel("手动排除账号", { exact: true });
    await input.fill(
      "blocked@example.test\n12345678\nblocked@example.test\ninvalid",
    );
    await manualPage()
      .getByLabel("手动排除备注", { exact: true })
      .fill("合成排除回归");
    // 同步门闩阻止双击；保存失败保留输入和原记录供重试。
    await page.evaluate(() => {
      window.__uiFixture.failNextManualWrite();
      window.__uiFixture.holdNextManualWrite();
    });
    await manualPage()
      .getByRole("button", { name: "添加手动排除", exact: true })
      .evaluate((button) => {
        button.click();
        button.click();
      });
    await page.waitForFunction(() => window.__uiFixture.manualWritePending());
    assert.equal((await calls("add_manual_exclusions")).length, 1);
    await page.evaluate(() => window.__uiFixture.releaseManualWrite());
    await toastMessage(page, /排除名单保存失败/).waitFor();
    assert.match(await input.inputValue(), /blocked@example\.test/);
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.manualExclusions())).records
        .length,
      0,
    );
    await manualPage()
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /已添加 2 条，重复 1 条，无效 1 条/).waitFor();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.manualExclusions()))
        .revision,
      1,
    );
    const search = manualPage().getByLabel("搜索手动排除", { exact: true });
    await search.fill("not-found");
    await manualPage().getByText("没有匹配的记录", { exact: true }).waitFor();
    await search.fill("合成排除回归");
    assert.equal(await manualPage().locator("tbody tr").count(), 2);
    await search.fill("");
    assert.equal(
      await manualPage()
        .getByRole("heading", { name: "手动排除", exact: true })
        .evaluate((element) => getComputedStyle(element).fontSize),
      "24px",
    );
    // 快照只展示工作区，先关闭故障注入留下的浮层，不让测试提示掩盖标题或导航。
    await page
      .locator("[data-sonner-toast] [data-close-button]")
      .evaluateAll((buttons) => buttons.forEach((button) => button.click()));
    await page.screenshot({
      path: new URL("manual-exclusions-light.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    for (const width of testWidths([780, 375])) {
      await page.setViewportSize(testViewport(width, 900));
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 1,
        ),
        true,
        `manual exclusions overflow at ${width}`,
      );
    }
    await page.screenshot({
      path: new URL("manual-exclusions-narrow.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    await page.setViewportSize(testViewport(1200, 900));
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    assert.equal(
      await page
        .getByRole("button", { name: "快捷发送", exact: true })
        .getAttribute("aria-pressed"),
      "true",
    );
    await page
      .locator("#mail-recipients")
      .fill(
        "blocked@example.test\nallowed-one@example.test\nallowed-two@example.test\nallowed-one@example.test",
      );
    await page.getByLabel("主题", { exact: true }).fill("独立快捷发送模板");
    await editor().fill("无需选择任何 QQ 群，使用完整邮箱发送。");
    await pasteSyntheticImage(page);
    await editor().locator("img[data-inline-image-id]").waitFor();
    await page.getByRole("button", { name: "保存为模板", exact: true }).click();
    const save = page.getByRole("dialog", {
      name: "保存邮件模板",
      exact: true,
    });
    await save.getByLabel("模板名称", { exact: true }).fill("快捷发送模板");
    await save.getByRole("button", { name: "保存模板", exact: true }).click();
    await save.waitFor({ state: "hidden" });
    const recipientText = await page.locator("#mail-recipients").inputValue();
    await page.getByRole("button", { name: "全选输入", exact: true }).click();
    assert.deepEqual(
      await page.locator("#mail-recipients").evaluate((element) => ({
        focused: document.activeElement === element,
        start: element.selectionStart,
        end: element.selectionEnd,
      })),
      { focused: true, start: 0, end: recipientText.length },
    );
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      recipientText,
    );
    // 两种模式仅切换收件范围；邮件内容与图片共享，未激活的群补充地址不能混入快捷发送。
    await page.getByRole("button", { name: "群成员发送", exact: true }).click();
    assert.equal(await page.locator("#mail-recipients").inputValue(), "");
    await page.locator("#mail-recipients").fill("group-only@example.test");
    await page.getByRole("button", { name: "快捷发送", exact: true }).click();
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      recipientText,
    );
    await page.getByLabel("主题", { exact: true }).fill("临时修改内容");
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library
      .locator(".mail-template-item")
      .filter({ hasText: "快捷发送模板" })
      .click();
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
    await replace.waitFor({ state: "hidden" });
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "独立快捷发送模板",
    );
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      recipientText,
    );
    assert.equal(
      await editor().locator("img[data-inline-image-id]").count(),
      1,
    );
    await page.locator(".mail-heading").scrollIntoViewIfNeeded();
    await page.screenshot({
      path: new URL("quick-send-compose.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await confirm().waitFor();
    assert.match(await confirm().innerText(), /2\s*人/);
    await confirm().getByText("手动排除 1 个地址", { exact: true }).click();
    await confirm()
      .getByText("blocked@example.test", { exact: true })
      .waitFor();
    assert.equal((await calls("start_mail_job")).length, 0);
    await page.screenshot({
      path: new URL("quick-send-confirmation.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    await confirm()
      .getByRole("button", { name: "确认发送", exact: true })
      .click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const draft = (await calls("start_mail_job")).at(-1).args.draft;
    assert.equal("groupAudience" in draft, false);
    assert.equal(draft.inlineImageIds.length, 1);
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.actualMailRecipients()),
      ["allowed-one@example.test", "allowed-two@example.test"],
    );
    assert.equal((await calls("fetch_members")).length, 0);
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    await manualPage()
      .getByRole("heading", { name: "手动排除", exact: true })
      .waitFor();
    assert.equal(
      await manualPage()
        .getByLabel("手动排除账号", { exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(
      await manualPage()
        .getByRole("button", { name: "添加手动排除", exact: true })
        .isDisabled(),
      true,
    );
    await page.evaluate(() => window.__uiFixture.finishMail());
    await page.waitForFunction(
      () =>
        !document.querySelector(".manual-exclusions-input textarea")?.disabled,
    );
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    await page.locator("#mail-recipients").fill("blocked@example.test");
    const starts = (await calls("start_mail_job")).length;
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await toastMessage(page, /没有待发送收件人/).waitFor();
    assert.equal(await confirm().isVisible(), false);
    assert.equal((await calls("start_mail_job")).length, starts);
    await page.reload();
    await openManual();
    await manualPage()
      .getByText("blocked@example.test", { exact: true })
      .waitFor();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.manualExclusions())).records
        .length,
      2,
    );
    // 写事务已提交、派生名单刷新失败时自动补读，避免误以为未保存而反复提交。
    await page.evaluate(() =>
      window.__uiFixture.failNextManualOverlayRefresh(),
    );
    await manualPage()
      .getByLabel("手动排除账号", { exact: true })
      .fill("saved-after-refresh-error@example.test");
    await manualPage()
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /手动排除已保存，但当前名单刷新失败/).waitFor();
    await manualPage()
      .getByRole("cell", {
        name: "saved-after-refresh-error@example.test",
        exact: true,
      })
      .waitFor();
    assert.equal(
      await manualPage()
        .getByLabel("手动排除账号", { exact: true })
        .inputValue(),
      "saved-after-refresh-error@example.test",
    );
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.manualExclusions())).records
        .length,
      3,
    );
    console.log(
      "Quick send: no QQ session/group, separate modes, template/image reuse, manual preflight preview/zero, duplicate locks, failure/retry/committed-refresh failure and persisted manual list checked (synthetic only)",
    );
  } finally {
    await context.close();
  }

  const groupContext = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await groupContext.addInitScript(installFixture, {
    manualScenario: true,
    initialPasswordSaved: true,
  });
  const groupPage = await groupContext.newPage();
  groupPage.setDefaultTimeout(15000);
  groupPage.on("pageerror", (error) => pageErrors.push(error.message));
  const groupCalls = (command) =>
    groupPage.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const ready = (count) =>
    groupPage
      .getByRole("tab", { name: `筛选后成员 ${count}`, exact: true })
      .waitFor();
  try {
    await groupPage.goto("http://127.0.0.1:1420");
    await groupPage
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await groupPage
      .getByRole("button", { name: "获取成员", exact: true })
      .click();
    await ready(61);
    const snapshot = await groupPage.evaluate(() =>
      window.__uiFixture.snapshot(),
    );
    const manualConfirm = groupPage.getByRole("dialog", {
      name: "确认手动排除",
      exact: true,
    });
    await groupPage
      .getByRole("button", { name: "排除成员 10000002", exact: true })
      .click();
    await manualConfirm.waitFor();
    assert.match(await manualConfirm.innerText(), /所有群|全局/);
    await manualConfirm
      .getByRole("button", { name: "取消", exact: true })
      .click();
    assert.equal((await groupCalls("add_manual_exclusions")).length, 0);
    await groupPage
      .getByRole("button", { name: "排除成员 10000002", exact: true })
      .click();
    await manualConfirm
      .getByRole("button", { name: "确认排除", exact: true })
      .click();
    await ready(60);
    assert.equal(
      await groupPage.getByLabel("选择成员 10000002", { exact: true }).count(),
      0,
    );
    await groupPage.getByLabel("选择成员 10000003", { exact: true }).check();
    await groupPage.getByLabel("选择成员 10000004", { exact: true }).check();
    await groupPage
      .getByRole("button", { name: "排除所选 2 人", exact: true })
      .click();
    await manualConfirm
      .getByRole("button", { name: "确认排除", exact: true })
      .click();
    await ready(58);
    const after = await groupPage.evaluate(() => window.__uiFixture.snapshot());
    assert.equal(after.id, snapshot.id);
    assert.equal(after.finishedAt, snapshot.finishedAt);
    assert.equal(after.manualFilteredCount, 3);
    assert.equal(
      after.members.length + after.filteredMembers.length,
      after.sourceCount,
    );
    await groupPage.getByRole("tab", { name: "已过滤 4", exact: true }).click();
    assert.equal(
      await groupPage
        .locator(".filter-reason")
        .filter({ hasText: "手动排除" })
        .count(),
      3,
    );
    await groupPage
      .getByRole("button", { name: "QQ 号 TXT", exact: true })
      .click();
    await toastMessage(groupPage, "文件已保存").waitFor();
    const exportCall = (await groupCalls("export_members")).at(-1);
    assert.equal(exportCall.args.snapshotId, snapshot.id);
    assert.equal(exportCall.args.exclusionRevision, after.exclusionRevision);
    assert.equal((await groupCalls("fetch_members")).length, 1);
    await groupPage
      .getByRole("tab", { name: "筛选后成员 58", exact: true })
      .click();
    await groupPage
      .getByRole("button", { name: "给全部 58 人写信", exact: true })
      .click();
    assert.equal(
      await groupPage
        .getByRole("button", { name: "群成员发送", exact: true })
        .getAttribute("aria-pressed"),
      "true",
    );
    await groupPage.locator("#mail-recipients").fill("group-only@example.test");
    await groupPage
      .getByRole("button", { name: "快捷发送", exact: true })
      .click();
    await groupPage.locator("#mail-recipients").fill("quick-only@example.test");
    await groupPage
      .getByRole("button", { name: "群成员发送", exact: true })
      .click();
    assert.equal(
      await groupPage.locator("#mail-recipients").inputValue(),
      "group-only@example.test",
    );
    await groupPage
      .getByLabel("主题", { exact: true })
      .fill("群排除后的固定范围");
    await groupPage
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("移除手动排除不应自动增加已经选定的收件人。");
    await groupPage.evaluate(() => window.__uiFixture.failNextSnapshotView());
    await groupPage
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    const manual = groupPage.locator(".manual-exclusions-page");
    await manual.getByLabel("手动排除账号", { exact: true }).fill("10000001");
    await manual
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(groupPage, /已添加 1 条/).waitFor();
    const overlayRetry = groupPage.locator(".snapshot-overlay-retry");
    await overlayRetry.waitFor();
    assert.match(await overlayRetry.innerText(), /名单更新失败/);
    const readsBeforeRetry = (await groupCalls("get_snapshot_view")).length;
    await overlayRetry
      .getByRole("button", { name: "重新加载名单", exact: true })
      .evaluate((button) => {
        button.click();
        button.click();
      });
    await overlayRetry.waitFor({ state: "hidden" });
    assert.equal(
      (await groupCalls("get_snapshot_view")).length,
      readsBeforeRetry + 1,
    );
    assert.equal(
      (await groupCalls("get_snapshot_view")).at(-1).args.snapshotId,
      snapshot.id,
    );
    await manual
      .getByRole("checkbox", { name: "选择本页手动排除", exact: true })
      .check();
    await manual.getByRole("button", { name: "移除所选", exact: true }).click();
    const removal = manual.getByRole("group", {
      name: "确认移除手动排除",
      exact: true,
    });
    await removal.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal((await groupCalls("remove_manual_exclusions")).length, 0);
    await manual.getByRole("button", { name: "移除所选", exact: true }).click();
    await removal
      .getByRole("button", { name: "确认移除", exact: true })
      .click();
    await toastMessage(groupPage, /已移除 4 条/).waitFor();
    await groupPage
      .locator(".app-nav")
      .getByRole("button", { name: "成员提取", exact: true })
      .click();
    await ready(61);
    const restored = await groupPage.evaluate(() =>
      window.__uiFixture.snapshot(),
    );
    assert.equal(restored.filteredMembers.length, 1);
    assert.equal(restored.filteredMembers[0].reason, "sent");
    assert.equal(
      restored.members.some((member) => member.qqNumber === "10000001"),
      false,
    );
    await groupPage
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    await groupPage
      .locator(".mail-audience")
      .getByText(`本封已选 58 / 筛选后 ${restored.members.length} 人`, {
        exact: true,
      })
      .waitFor();
    await groupPage.getByRole("button", { name: "发送", exact: true }).click();
    const confirmation = groupPage.getByRole("dialog", {
      name: "确认发送",
      exact: true,
    });
    await confirmation.waitFor();
    const prepared = (await groupCalls("prepare_mail_recipients")).at(-1).args
      .input;
    assert.equal(prepared.groupAudience.snapshotId, snapshot.id);
    assert.equal(prepared.groupAudience.qqNumbers.length, 58);
    assert.equal(
      prepared.recipients.includes("quick-only@example.test"),
      false,
    );
    assert.equal(prepared.recipients.includes("10000002@qq.com"), false);
    await confirmation
      .getByRole("button", { name: "返回修改", exact: true })
      .click();
    assert.equal((await groupCalls("start_mail_job")).length, 0);
    await groupPage.getByRole("button", { name: /外观：跟随系统/ }).click();
    await groupPage.getByRole("button", { name: /外观：浅色/ }).click();
    await groupPage.locator(".mail-heading").scrollIntoViewIfNeeded();
    await groupPage.screenshot({
      path: new URL("quick-group-modes-dark.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    console.log(
      "Manual exclusions: row/cancel/bulk, immediate same-ID counts/reasons/export revision, no refetch, overlap restoration, group selection subset and mode isolation checked (synthetic only)",
    );
  } finally {
    await groupContext.close();
  }
}

/** 日志查看只读且手动刷新，失败可重试；任务进行时也不改变草稿、名单或发送状态。 */
/**
 * 模板交换的界面契约：原生选文件由隔离 IPC 模拟，资源往返、包限制和 SQLite 原子性由 Rust 验证。
 * 这里重点检查选择范围、取消/失败/重试、覆盖确认、双击门闩和草稿保留；本轮固定1920×1080。
 */
async function checkTemplateTransfer() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
  const importing = page.getByRole("dialog", {
    name: "导入邮件模板",
    exact: true,
  });
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const allTemplates = () =>
    page.evaluate(() => window.__uiFixture.savedTemplates());
  const selectExport = (name) =>
    library.getByRole("checkbox", {
      name: `选择导出模板 ${name}`,
      exact: true,
    });
  const beginImport = async (values) => {
    await page.evaluate(
      (items) => window.__uiFixture.setTemplateImportPackage(items),
      values,
    );
    await library
      .getByRole("button", { name: "导入模板", exact: true })
      .click();
    await importing.waitFor();
    await assertFixedDialogFooter(page);
  };
  const confirmImport = () =>
    importing.getByRole("button", { name: "确认导入", exact: true });
  const cancelImport = () =>
    importing.getByRole("button", { name: "取消导入", exact: true }).click();
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page
      .getByLabel("主题", { exact: true })
      .fill("导入导出不修改当前草稿");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("保留正文与收件人");
    await page.locator("#mail-recipients").fill("preserved@example.test");
    await page.evaluate(() => window.__uiFixture.seedTemplateTransfer());
    const originals = await allTemplates();
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    await library.locator(".mail-template-item").first().waitFor();
    await library
      .locator(".mail-template-item")
      .filter({ has: page.getByText("活动通知", { exact: true }) })
      .click();
    await library
      .getByRole("heading", { name: "活动通知", exact: true })
      .waitFor();
    await page.evaluate(() => window.__uiFixture.cancelNextTemplateExport());
    await library
      .getByRole("button", { name: "导出当前模板", exact: true })
      .click();
    assert.deepEqual((await calls("mail_template_export")).at(-1).args, {
      ids: [originals[0].id],
    });
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.exportedTemplatePackages()))
        .length,
      0,
    );

    await selectExport("内部资料").check();
    const search = library.getByLabel("搜索模板名称或主题", { exact: true });
    await search.fill("活动");
    await library
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await library
      .getByRole("button", { name: "导出已选 3 个模板", exact: true })
      .waitFor();
    await library
      .getByRole("button", { name: "取消全选（当前结果）", exact: true })
      .click();
    await library
      .getByRole("button", { name: "导出已选 1 个模板", exact: true })
      .waitFor();
    await library
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await library
      .getByRole("button", { name: "清除模板搜索", exact: true })
      .click();
    assert.equal(
      await search.evaluate((input) => document.activeElement === input),
      true,
    );
    assert.equal(await selectExport("内部资料").isChecked(), true);
    assert.equal(await selectExport("活动通知").isChecked(), true);
    await page.evaluate(() =>
      window.__uiFixture.holdNextTemplateTransfer("export"),
    );
    const exportSelected = library.getByRole("button", {
      name: "导出已选 3 个模板",
      exact: true,
    });
    await exportSelected.evaluate((button) => {
      button.click();
      button.click();
    });
    await page.waitForFunction(() =>
      window.__uiFixture.templateTransferPending(),
    );
    assert.equal(
      (await calls("mail_template_export")).length,
      2,
      "Double-click starts one export only",
    );
    assert.equal(
      await library
        .getByRole("button", { name: "导入模板", exact: true })
        .isDisabled(),
      true,
    );
    assert.equal(await selectExport("活动通知").isDisabled(), true);
    for (const button of await page.locator(".app-nav > button").all())
      assert.equal(await button.isDisabled(), true);
    await page.evaluate(() => window.__uiFixture.releaseTemplateTransfer());
    await page.waitForFunction(
      () => window.__uiFixture.exportedTemplatePackages().length === 1,
    );
    const exported = (
      await page.evaluate(() => window.__uiFixture.exportedTemplatePackages())
    )[0];
    assert.deepEqual(
      new Set(exported.ids),
      new Set([originals[0].id, originals[2].id, originals[3].id]),
    );
    assert.deepEqual(
      exported.templates.find((item) => item.id === originals[0].id)
        .inlineImages,
      originals[0].inlineImages,
    );
    await page.evaluate(() => window.__uiFixture.failNextTemplate("export"));
    await exportSelected.click();
    await toastMessage(page, "模板操作暂时失败，请重试。").waitFor();
    assert.equal(await selectExport("内部资料").isChecked(), true);
    await library
      .getByRole("button", { name: "清空导出选择", exact: true })
      .click();
    assert.equal(await selectExport("内部资料").isChecked(), false);

    // 取消原生文件选择不出现确认，不改库；取消已经预览的包也必须释放宿主令牌。
    await library
      .getByRole("button", { name: "导入模板", exact: true })
      .click();
    assert.equal(await importing.isVisible(), false);
    const incoming = [
      {
        ...originals[0],
        subject: "导入的新主题",
        html: '<p>导入正文</p><img src="cid:transfer-image@fixture.local">',
      },
      { ...originals[1], name: "迁移新增模板" },
    ];
    await beginImport(incoming);
    assert.equal(
      await importing
        .getByRole("radio", { name: "另存为副本", exact: true })
        .isChecked(),
      true,
    );
    assert.equal(
      await importing
        .getByRole("radio", { name: "覆盖同名模板", exact: true })
        .isDisabled(),
      false,
    );
    await cancelImport();
    await importing.waitFor({ state: "hidden" });
    assert.equal((await calls("mail_template_import_cancel")).length, 1);
    assert.deepEqual(await allTemplates(), originals);

    // 挂起确认时不能再次提交，Esc不能绕过提交锁；导入仅更新库，不自动应用模板。
    await beginImport(incoming);
    await page.evaluate(() =>
      window.__uiFixture.holdNextTemplateTransfer("import_commit"),
    );
    await confirmImport().evaluate((button) => {
      button.click();
      button.click();
    });
    await page.waitForFunction(() =>
      window.__uiFixture.templateTransferPending(),
    );
    assert.equal((await calls("mail_template_import_commit")).length, 1);
    assert.equal(
      await importing
        .getByRole("button", { name: "取消导入", exact: true })
        .isDisabled(),
      true,
    );
    await page.keyboard.press("Escape");
    assert.equal(await importing.isVisible(), true);
    await page.evaluate(() => window.__uiFixture.releaseTemplateTransfer());
    await importing.waitFor({ state: "hidden" });
    assert.equal((await allTemplates()).length, 6);
    assert.equal(
      (await allTemplates()).filter((item) => item.name === "活动通知").length,
      1,
    );
    assert.deepEqual(
      (await allTemplates()).find((item) => item.id === originals[0].id),
      originals[0],
    );
    await library
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "导入导出不修改当前草稿",
    );
    assert.equal(
      await page
        .getByRole("textbox", { name: "邮件正文", exact: true })
        .innerText(),
      "保留正文与收件人",
    );
    assert.equal(
      await page.locator("#mail-recipients").inputValue(),
      "preserved@example.test",
    );
    assert.equal((await calls("start_mail_job")).length, 0);
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    await library.locator(".mail-template-item").first().waitFor();

    await beginImport([incoming[0], { ...originals[1], name: "跳过策略新增" }]);
    await importing
      .getByRole("radio", { name: "跳过同名模板", exact: true })
      .check();
    await confirmImport().click();
    await importing.waitFor({ state: "hidden" });
    assert.equal((await allTemplates()).length, 7);
    assert.deepEqual(
      (await allTemplates()).find((item) => item.id === originals[0].id),
      originals[0],
    );

    await library
      .locator(".mail-template-item")
      .filter({ has: page.getByText("活动通知", { exact: true }) })
      .click();
    await beginImport([incoming[0]]);
    await importing
      .getByRole("radio", { name: "覆盖同名模板", exact: true })
      .check();
    assert.equal(await confirmImport().isDisabled(), true);
    await importing
      .getByRole("checkbox", {
        name: "我已确认覆盖同名模板，原内容及资源将被替换",
        exact: true,
      })
      .check();
    await page.evaluate(() =>
      window.__uiFixture.failNextTemplate("import_commit"),
    );
    await confirmImport().click();
    await toastMessage(page, "模板操作暂时失败，请重试。").waitFor();
    assert.equal(await importing.isVisible(), true);
    assert.deepEqual(
      (await allTemplates()).find((item) => item.id === originals[0].id),
      originals[0],
    );
    await confirmImport().click();
    await importing.waitFor({ state: "hidden" });
    assert.equal((await allTemplates()).length, 7);
    assert.equal(
      (await allTemplates()).find((item) => item.id === originals[0].id)
        .subject,
      incoming[0].subject,
    );
    await library.getByText("导入的新主题", { exact: true }).last().waitFor();

    // 源包同名歧义禁止覆盖；过期确认必须重新选文件，不能静默使用过期计划。
    await beginImport([incoming[0], incoming[0]]);
    assert.equal(
      await importing
        .getByRole("radio", { name: "覆盖同名模板", exact: true })
        .isDisabled(),
      true,
    );
    await page.screenshot({
      path: new URL(
        "template-import-preview-1920.png",
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    await cancelImport();
    await beginImport([incoming[0]]);
    await page.evaluate(() => window.__uiFixture.expireTemplateImport());
    await confirmImport().click();
    // 等待宿主过期响应真正落入错误态，不能用始终可见的“重新选择文件”作为完成条件。
    await importing
      .getByRole("alert")
      .filter({ hasText: "预览已失效" })
      .waitFor();
    assert.equal(await confirmImport().isDisabled(), true);
    assert.equal((await allTemplates()).length, 7);
    await cancelImport();
    await library
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "导入导出不修改当前草稿",
    );
    assert.equal((await calls("start_mail_job")).length, 0);
    console.log(
      "Template transfer UI: scoped selection, native-cancel, retained draft/resources, atomic-failure retry, import policies/overwrite confirmation, expired preview and duplicate-submit lock checked (synthetic only)",
    );
  } finally {
    await context.close();
  }
}

async function checkLogViewer() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { initialPasswordSaved: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const dialog = page.getByRole("dialog", { name: "查看日志", exact: true });
  const viewer = dialog.getByRole("textbox", { name: "最近日志", exact: true });
  const calls = (command) =>
    page.evaluate(
      (name) =>
        window.__uiFixture.calls.filter((entry) => entry.command === name),
      command,
    );
  const refresh = () =>
    dialog.getByRole("button", { name: "刷新", exact: true }).click();
  const open = () =>
    page.getByRole("button", { name: "查看日志", exact: true }).click();
  const close = () =>
    dialog.getByRole("button", { name: "关闭", exact: true }).click();
  try {
    await page.goto("http://127.0.0.1:1420");
    const synthetic =
      '{"event":"<img src=x onerror=alert(1)>","kind":"synthetic"}\n';
    await page.evaluate(
      (content) =>
        window.__uiFixture.setRecentLogs({ content, truncated: false }),
      synthetic,
    );
    await open();
    await viewer.waitFor();
    assert.equal(await viewer.inputValue(), synthetic);
    await dialog.getByRole("button", { name: "全选日志", exact: true }).click();
    assert.deepEqual(
      await viewer.evaluate((element) => ({
        focused: document.activeElement === element,
        start: element.selectionStart,
        end: element.selectionEnd,
      })),
      { focused: true, start: 0, end: synthetic.length },
    );
    await page.screenshot({
      path: new URL("log-text-selected.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    assert.equal(await viewer.getAttribute("readonly"), "");
    assert.equal(await dialog.locator("img").count(), 0);
    assert.deepEqual((await calls("get_recent_logs"))[0].args, {});

    // 手工刷新只产生一条IPC，关闭后晚到响应不能重新打开弹层或替换新打开的结果。
    await page.evaluate(() => {
      window.__uiFixture.holdNextLogRead();
      window.__uiFixture.setRecentLogs({
        content: "synthetic refreshed log\n",
        truncated: false,
      });
    });
    await refresh();
    await page.waitForFunction(() => window.__uiFixture.logReadPending());
    const readingButton = dialog.getByRole("button", {
      name: "读取中",
      exact: true,
    });
    assert.equal(await readingButton.isDisabled(), true);
    await readingButton.evaluate((button) => {
      button.click();
      button.click();
    });
    assert.equal((await calls("get_recent_logs")).length, 2);
    await close();
    await page.evaluate(() => window.__uiFixture.releaseLogRead());
    assert.equal(await dialog.isVisible(), false);
    await open();
    await viewer.waitFor();
    assert.equal(await viewer.inputValue(), "synthetic refreshed log\n");

    await page.evaluate(() => window.__uiFixture.failNextLogRead());
    await refresh();
    await dialog
      .getByRole("alert")
      .getByText("日志暂时无法读取，请检查访问权限后刷新重试。", {
        exact: true,
      })
      .waitFor();
    assert.equal(await viewer.inputValue(), "synthetic refreshed log\n");
    await dialog.getByRole("button", { name: "重试", exact: true }).click();
    await dialog.getByRole("button", { name: "刷新", exact: true }).waitFor();
    assert.equal(await dialog.getByRole("alert").count(), 0);
    await page.evaluate(() =>
      window.__uiFixture.setRecentLogs({ content: "", truncated: false }),
    );
    await refresh();
    await dialog.getByText("暂无日志", { exact: true }).waitFor();
    assert.equal(
      await dialog
        .getByRole("button", { name: "全选日志", exact: true })
        .isDisabled(),
      true,
    );
    await page.evaluate(() =>
      window.__uiFixture.setRecentLogs({
        content: "synthetic recent lines\n",
        truncated: true,
      }),
    );
    await refresh();
    await dialog
      .getByText("仅显示最近日志，完整记录可打开日志目录查看。", {
        exact: true,
      })
      .waitFor();
    await dialog
      .getByRole("button", { name: "打开日志目录", exact: true })
      .click();
    assert.equal((await calls("open_log_directory")).length, 1);
    const readsAfterManualRefresh = (await calls("get_recent_logs")).length;
    await page.waitForTimeout(1100);
    assert.equal(
      (await calls("get_recent_logs")).length,
      readsAfterManualRefresh,
    );
    const diagnostics = await calls("report_frontend_diagnostic");
    assert(
      !diagnostics.some((entry) =>
        ["get_recent_logs", "get_log_status", "open_log_directory"].includes(
          entry.args.input.command,
        ),
      ),
    );
    for (const width of testWidths([1200, 375])) {
      await page.setViewportSize(testViewport(width, 900));
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 1,
        ),
        true,
      );
      const bounds = await dialog.boundingBox();
      assert(bounds.x >= 0 && bounds.x + bounds.width <= width + 1);
    }
    await page.keyboard.press("Escape");
    assert.equal(await dialog.isVisible(), false);
    // 条件卸载原生dialog前必须close，让Sonner宿主返回body，后续业务提示继续可见可点击。
    await page.evaluate(async () => {
      const { notify } = await import("/frontend/lib/notify.ts");
      notify({ message: "日志关闭后的测试提示", tone: "success" });
    });
    await toastMessage(page, "日志关闭后的测试提示").waitFor();
    await checkToastReceivesPointer(toastMessage(page, "日志关闭后的测试提示"));
    await page.setViewportSize(testViewport(1200, 900));

    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.evaluate(() => window.__uiFixture.holdNextFetch());
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "running",
    );
    await open();
    await viewer.waitFor();
    assert.equal(
      await page.evaluate(() => window.__uiFixture.taskStatus()),
      "running",
    );
    await close();
    await page.evaluate(() => window.__uiFixture.releaseFetch());
    await page.waitForFunction(
      () => window.__uiFixture.taskStatus() === "completed",
    );

    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await page.locator("#mail-recipients").fill("log-viewer@example.test");
    await page.getByLabel("主题", { exact: true }).fill("日志查看不改变草稿");
    const editor = page.getByRole("textbox", { name: "邮件正文", exact: true });
    await editor.fill("合成邮件，验证查看日志不会改动正在发送的任务。");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await page.getByRole("button", { name: "确认发送", exact: true }).click();
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .waitFor();
    const before = await page.evaluate(() => window.__uiFixture.mail());
    await open();
    await viewer.waitFor();
    await refresh();
    await dialog.getByRole("button", { name: "刷新", exact: true }).waitFor();
    await close();
    assert.deepEqual(
      await page.evaluate(() => window.__uiFixture.mail()),
      before,
    );
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "日志查看不改变草稿",
    );
    assert.match(await editor.innerText(), /合成邮件/);
    assert.equal((await calls("start_mail_job")).length, 1);
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.locator(".mail-progress-cancelled").waitFor();
    assert.deepEqual(pageErrors, []);
    console.log(
      "Log viewer plain text/load/refresh/lock/error-retry/empty/truncation/directory/layout and task/draft isolation checked (synthetic logs only)",
    );
  } finally {
    await context.close();
  }
}

/** 诊断只传固定元数据；日志通道故障不能改变原命令，也不能递归产生故障事件。 */
async function checkFrontendDiagnostics() {
  const context = await browser.newContext({
    viewport: testViewport(1200, 900),
  });
  await context.addInitScript(installFixture);
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "查看日志", exact: true }).click();
    const logDialog = page.getByRole("dialog", {
      name: "查看日志",
      exact: true,
    });
    await logDialog
      .getByRole("button", { name: "打开日志目录", exact: true })
      .click();
    await page.waitForFunction(() =>
      window.__uiFixture.calls.some(
        (entry) => entry.command === "open_log_directory",
      ),
    );
    await logDialog.getByRole("button", { name: "关闭", exact: true }).click();
    const started = await page.evaluate(() =>
      window.__uiFixture.calls
        .filter((entry) => entry.command === "report_frontend_diagnostic")
        .map((entry) => entry.args.input),
    );
    assert(
      started.some(
        (entry) =>
          entry.command === "get_app_state" && entry.outcome === "started",
      ),
    );
    assert(
      started.some(
        (entry) =>
          entry.command === "get_app_state" && entry.outcome === "succeeded",
      ),
    );

    // 直接调用同一个业务边界验证敏感参数/结果/异常文本均不进入诊断事件。
    await page.evaluate(async () => {
      const bridge = await import("/frontend/lib/bridge.ts");
      const sentinel = "private-content-sentinel@example.test";
      await bridge.call("add_sent_records", {
        text: sentinel,
        password: "synthetic-password-sentinel",
        html: "<p>synthetic-body-sentinel</p>",
      });
      window.__uiFixture.failNextBulkResolution();
      await bridge
        .call("resolve_mail_deliveries", { ids: [31], accepted: true })
        .catch(() => {});
      for (let index = 0; index < 4; index += 1)
        await bridge.call("get_app_state");
      window.dispatchEvent(
        new ErrorEvent("error", {
          message: sentinel,
          filename:
            "https://private-url-sentinel.invalid/user-private/app-bundle.123.js?token=synthetic-token-sentinel",
          lineno: 35,
          colno: 17,
          error: new TypeError("private-stack-sentinel"),
        }),
      );
      window.dispatchEvent(
        new PromiseRejectionEvent("unhandledrejection", {
          promise: Promise.resolve(),
          reason: new Error("private-rejection-sentinel"),
        }),
      );
      window.dispatchEvent(
        new ErrorEvent("error", {
          message: sentinel,
          filename:
            "https://private-url-sentinel.invalid/private@email.js?token=synthetic-token-sentinel",
          lineno: -3,
          colno: 0,
          error: { name: sentinel },
        }),
      );
    });
    await page.waitForFunction(
      () =>
        window.__uiFixture.calls.filter(
          (entry) => entry.command === "report_frontend_fault",
        ).length === 3,
    );
    const diagnostics = await page.evaluate(() =>
      window.__uiFixture.calls.filter(
        (entry) => entry.command === "report_frontend_diagnostic",
      ),
    );
    for (const event of diagnostics) {
      assert.deepEqual(Object.keys(event.args), ["input"]);
      assert(
        Object.keys(event.args.input).every((key) =>
          ["command", "outcome", "durationMs", "errorCode"].includes(key),
        ),
      );
      assert(
        ["started", "succeeded", "failed"].includes(event.args.input.outcome),
      );
      assert(
        Number.isInteger(event.args.input.durationMs) &&
          event.args.input.durationMs >= 0,
      );
      assert(
        !event.args.input.command.startsWith("report_") &&
          event.args.input.command !== "get_log_status",
      );
    }
    assert.equal(
      diagnostics.filter(
        (entry) =>
          entry.args.input.command === "get_app_state" &&
          entry.args.input.outcome === "succeeded",
      ).length,
      1,
      "Successful repeated reads must be sampled",
    );
    assert(
      diagnostics.some(
        (entry) =>
          entry.args.input.command === "resolve_mail_deliveries" &&
          entry.args.input.outcome === "failed",
      ),
    );
    assert(!JSON.stringify(diagnostics).includes("sentinel"));
    assert(!JSON.stringify(diagnostics).includes("fixture_batch_resolution"));
    const faults = await page.evaluate(() =>
      window.__uiFixture.calls
        .filter((entry) => entry.command === "report_frontend_fault")
        .map((entry) => entry.args),
    );
    assert.deepEqual(faults, [
      {
        input: {
          kind: "error",
          errorType: "TypeError",
          source: "app-bundle.123.js",
          line: 35,
          column: 17,
        },
      },
      { input: { kind: "unhandledrejection", errorType: "Error" } },
      { input: { kind: "error", errorType: "unknown" } },
    ]);
    assert(!JSON.stringify(faults).includes("sentinel"));
    assert(!JSON.stringify(faults).includes("https://"));

    await page.evaluate(async () => {
      const bridge = await import("/frontend/lib/bridge.ts");
      window.__uiFixture.failDiagnosticReports(true);
      const result = await bridge.call("get_sent_history");
      window.__uiFixture.diagnosticBusinessPreserved = Array.isArray(
        result.records,
      );
    });
    assert.equal(
      await page.evaluate(() => window.__uiFixture.diagnosticBusinessPreserved),
      true,
    );
    await page.getByRole("button", { name: /已发送记录/ }).click();
    await page
      .getByRole("heading", { name: "已发送记录", exact: true })
      .waitFor();
    assert.equal(
      await page
        .getByRole("button", { name: "查看日志", exact: true })
        .isEnabled(),
      true,
    );
    await page.evaluate(async () => {
      const bridge = await import("/frontend/lib/bridge.ts");
      window.__uiFixture.failDiagnosticReports(false);
      window.__uiFixture.setDiagnosticUnavailable(true);
      await bridge.call("cancel_web_login");
      await bridge.call("cancel_web_login");
    });
    await toastMessage(page, "诊断日志暂时无法写入").waitFor();
    assert.equal(await toastMessage(page, "诊断日志暂时无法写入").count(), 1);
    for (const width of testWidths([1200, 780, 375])) {
      await page.setViewportSize(testViewport(width, 900));
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 1,
        ),
        true,
        `Diagnostic navigation overflow at ${width}`,
      );
      assert.equal(
        await page
          .getByRole("button", { name: "查看日志", exact: true })
          .isVisible(),
        true,
      );
    }
    assert.deepEqual(pageErrors, []);
  } finally {
    await context.close();
  }
}

/** 使用实际内容滚动及键盘焦点验证底部操作区，不能靠滚动整个dialog才能找到按钮。 */
async function assertFixedDialogFooter(page) {
  const dialogs = page.locator("dialog.dialog-shell[open]");
  for (let i = 0; i < (await dialogs.count()); i++) {
    const dialog = dialogs.nth(i);
    const result = await dialog.evaluate((el) => {
      const body = el.querySelector(":scope > .dialog-body");
      const footer = el.querySelector(":scope > .dialog-footer");
      const bounds = () => ({
        top: footer.getBoundingClientRect().top,
        bottom: footer.getBoundingClientRect().bottom,
      });
      const first = bounds();
      body.scrollTop = body.scrollHeight;
      const last = bounds();
      return {
        first,
        last,
        scroll: el.scrollTop,
        bodyHeight: body.clientHeight,
        overflow: body.scrollHeight > body.clientHeight,
        viewport: innerHeight,
        dialogTop: el.getBoundingClientRect().top,
        dialogBottom: el.getBoundingClientRect().bottom,
        footerStyle: {
          padding: getComputedStyle(footer).padding,
          boxSizing: getComputedStyle(el).boxSizing,
          flexShrink: getComputedStyle(footer).flexShrink,
        },
      };
    });
    assert.ok(result.bodyHeight > 0);
    // 短确认不强制制造滚动；超长主体另由真实模板/长内容场景覆盖固定操作区。
    assert.ok(result.bodyHeight > 0);
    assert.ok(result.first.top > result.dialogTop);
    assert.ok(result.first.bottom <= result.dialogBottom + 1);
    assert.equal(result.footerStyle.flexShrink, "0");
    assert.ok(result.first.bottom - result.first.top >= 64);
    assert.ok(result.first.top >= 0 && result.first.bottom <= result.viewport);
    assert.equal(result.first.top, result.last.top);
    assert.equal(result.first.bottom, result.last.bottom);
    assert.equal(result.scroll, 0);
    await dialog.locator(":scope > .dialog-body").evaluate((el) => {
      el.scrollTop = 0;
    });
    if (i === (await dialogs.count()) - 1) {
      const buttons = dialog.locator(":scope > .dialog-footer button:enabled");
      for (let j = 0; j < (await buttons.count()); j++) {
        await buttons.nth(j).click({ trial: true });
        await buttons.nth(j).focus();
        assert.equal(await dialog.evaluate((el) => el.scrollTop), 0);
      }
    }
  }
}

/** 只操作合成等待任务；验证一次跳过、按钮防重复、后续仍等待和停止优先。 */
async function checkSkipWaiting() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, { multiAccountScenario: true });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const publishWait = async (waitId) =>
    page.evaluate(
      (id) =>
        window.__uiFixture.publishMailState({
          id: "skip-job",
          status: "running",
          phase: "waiting",
          total: 10,
          sent: 1,
          failed: 0,
          skipped: 0,
          waitId: id,
          nextBatchAt: Date.now() + 900000,
          waitReason: "accountCooldown",
          current: "第2/10批",
          message: "正在等待账号冷却…",
        }),
      waitId,
    );
  const skip = () =>
    page.getByRole("button", { name: "跳过本次等待", exact: true });
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByRole("button", { name: "写邮件", exact: true }).click();
    await publishWait("wait-1");
    await skip().waitFor();
    await page.evaluate(() => window.__uiFixture.holdNextWaitSkip());
    await skip().evaluate((el) => {
      el.click();
      el.click();
    });
    await page.waitForFunction(() => window.__uiFixture.waitSkipPending());
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter((c) => c.command === "skip_mail_wait")
            .length,
      ),
      1,
    );
    await page.evaluate(() => window.__uiFixture.releaseWaitSkip());
    await skip().waitFor({ state: "hidden" });
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.mail())).sent,
      1,
    );
    await publishWait("wait-2");
    await skip().waitFor();
    await skip().scrollIntoViewIfNeeded();
    assert.equal(await skip().isEnabled(), true);
    await page.screenshot({
      path: new URL("interaction-wait.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
    // 旧请求到达新一轮等待后必须失败，新等待的截止时间不能被清掉。
    await page.evaluate(() => window.__uiFixture.holdNextWaitSkip());
    await skip().click();
    await page.waitForFunction(() => window.__uiFixture.waitSkipPending());
    await publishWait("wait-3");
    await page.evaluate(() => window.__uiFixture.releaseWaitSkip());
    await skip().waitFor();
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.mail())).waitId,
      "wait-3",
    );
    // 跳过IPC尚未完成也能停止；迟到响应不能恢复发送或清掉新状态。
    await page.evaluate(() => window.__uiFixture.holdNextWaitSkip());
    await skip().click();
    await page.waitForFunction(() => window.__uiFixture.waitSkipPending());
    await page
      .getByRole("button", { name: "停止后续发送", exact: true })
      .click();
    await page.waitForFunction(
      () => window.__uiFixture.mail().status === "cancelled",
    );
    await page.evaluate(() => window.__uiFixture.releaseWaitSkip());
    await page
      .getByRole("button", { name: "正在跳过…", exact: true })
      .waitFor({ state: "hidden" });
    assert.equal(
      (await page.evaluate(() => window.__uiFixture.mail())).status,
      "cancelled",
    );
    assert.equal(await skip().count(), 0);
  } finally {
    await context.close();
  }
  console.log(
    "1920x1080 wait skip: one batch, duplicate click, next wait, stale request and cancellation race passed (synthetic only)",
  );
}

/**
 * 检查展开的发送表单在真实渲染后的列线、控件尺寸和单位位置，而非仅检查CSS声明。
 * @param page 已打开写信页并展开发送选项的隔离页面；仅验收1920×1080，不读真实账号或发信。
 * @param name 合成场景名称，供截图定位主题、发送模式与锁定状态。
 */
async function assertSendOptionsAligned(page, name) {
  const settings = page.locator(".mail-delivery-settings");
  await settings.scrollIntoViewIfNeeded();
  await page.screenshot({
    path: new URL(`send-options-${name}.png`, artifacts).pathname.replace(
      /^\/(\w:)/,
      "$1",
    ),
  });
  const rows = await settings
    .locator('select, input[type="number"]')
    .evaluateAll((controls) =>
      controls.map((control) => {
        const label = control.closest("label");
        // 支持修复前的裸文本和修复后的显式span，确保回归能在旧布局上真实失败。
        const texts = [...label.childNodes].filter((node) =>
          node.nodeType === Node.TEXT_NODE
            ? node.textContent.trim()
            : node instanceof HTMLSpanElement,
        );
        const textRect = (node) => {
          const range = document.createRange();
          range.selectNodeContents(node);
          const rect = range.getBoundingClientRect();
          return { x: rect.x, y: rect.y, height: rect.height };
        };
        const rect = control.getBoundingClientRect();
        return {
          name: control.getAttribute("aria-label"),
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          label: textRect(texts[0]),
          unit: texts.length > 1 ? textRect(texts.at(-1)) : null,
        };
      }),
    );
  assert.ok(rows.length >= 3);
  const spread = (values) => Math.max(...values) - Math.min(...values);
  assert.ok(
    spread(rows.map((row) => row.x)) <= 1,
    `发送选项控件左边缘未对齐：${JSON.stringify(rows)}`,
  );
  assert.ok(spread(rows.map((row) => row.width)) <= 1);
  assert.ok(spread(rows.map((row) => row.height)) <= 1);
  assert.ok(rows.every((row) => row.height >= 36));
  assert.ok(spread(rows.map((row) => row.label.x)) <= 1);
  assert.ok(spread(rows.flatMap((row) => (row.unit ? [row.unit.x] : []))) <= 1);
  for (const row of rows) {
    assert.ok(
      Math.abs(row.label.y + row.label.height / 2 - row.y - row.height / 2) <=
        2,
    );
    if (row.unit)
      assert.ok(
        Math.abs(row.unit.y + row.unit.height / 2 - row.y - row.height / 2) <=
          2,
      );
  }
}

/** 本轮只检查1920×1080：布局精简保留操作入口，预计值与批次倒计时互不替代。 */
async function checkCompletionAndLayout() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
    timezoneId: "Asia/Shanghai",
    // 后半段使用虚拟时钟；关闭装饰过渡，避免主题截图停在120ms渐变的中间色。
    reducedMotion: "reduce",
  });
  await context.addInitScript(installFixture, {
    multiAccountScenario: true,
    initialPasswordSaved: true,
    countdownScenario: true,
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) => {
    // 沙箱邮件预览明确禁止脚本；浏览器对此产生的固定诊断是预期隔离行为，其他错误仍必须失败。
    const sandboxDiagnostic =
      "Blocked script execution in 'about:srcdoc' because the document's frame is sandboxed and the 'allow-scripts' permission is not set.";
    if (message.type() === "error" && message.text() !== sandboxDiagnostic)
      pageErrors.push(message.text());
  });
  const nav = (name) =>
    page.locator(".app-nav").getByRole("button", { name, exact: true });
  const screenshot = async (name) => {
    await assertFixedDialogFooter(page);
    // 操作会自然滚到刚使用的字段；交付截图统一从各工作区顶部展示完整结构，业务选择与表单不重置。
    await page.locator(".workbench-sidebar").evaluateAll((elements) => {
      for (const element of elements) element.scrollTop = 0;
    });
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth + 1,
      ),
      true,
    );
    await page.screenshot({
      path: new URL(
        `completion-layout-${name}.png`,
        artifacts,
      ).pathname.replace(/^\/(\w:)/, "$1"),
    });
    if (["accounts", "templates", "logs", "confirmation"].includes(name)) {
      const previousTheme = await page
        .locator("html")
        .getAttribute("data-theme");
      await page.evaluate(() => {
        document.documentElement.dataset.theme = "dark";
      });
      await assertFixedDialogFooter(page);
      await page.screenshot({
        path: new URL(
          `completion-layout-${name}-dark.png`,
          artifacts,
        ).pathname.replace(/^\/(\w:)/, "$1"),
      });
      await page.evaluate((theme) => {
        document.documentElement.dataset.theme = theme;
      }, previousTheme);
    }
  };
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByLabel("当前已登录账号", { exact: true }).waitFor();
    assert.equal(await page.locator(".source-bar").count(), 0);
    assert.equal(
      await page.getByRole("button", { name: "重新扫码", exact: true }).count(),
      1,
    );
    assert.equal(
      await page
        .locator(".header-actions")
        .getByRole("button", { name: "重新扫码", exact: true })
        .isVisible(),
      true,
    );
    assert.ok((await page.locator(".app-header").boundingBox()).height <= 72);
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    const exclusions = page.locator(".exclusion-picker");
    await exclusions
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await exclusions
      .getByRole("button", { name: "取消全选（当前结果）", exact: true })
      .click();
    assert.equal(
      await exclusions.locator('input[type="checkbox"]:checked').count(),
      0,
    );
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    assert.equal(
      await page.locator(".result-details").getAttribute("open"),
      null,
    );
    assert.equal(
      await page
        .getByRole("button", { name: "排除所选 0 人", exact: true })
        .count(),
      0,
    );
    await page
      .getByRole("button", { name: "全选成员 61 人", exact: true })
      .click();
    await page
      .getByRole("button", { name: "取消全选成员 61 人", exact: true })
      .click();
    const snapshot = await page.evaluate(() => window.__uiFixture.snapshot());
    await page
      .getByLabel(`选择成员 ${snapshot.members[1].qqNumber}`, { exact: true })
      .check();
    await screenshot("members-light");
    await page
      .getByRole("button", { name: "给已选 1 人写信", exact: true })
      .click();
    assert.match(
      await page.locator(".mail-audience-counts").innerText(),
      /本封已选 1 \/ 筛选后 61 人/,
    );
    await page
      .getByRole("button", { name: "查看群收件人（1）", exact: true })
      .click();
    await page
      .getByRole("button", { name: "收起群收件人", exact: true })
      .click();
    await page
      .getByLabel("主题", { exact: true })
      .fill("布局与预计时间合成验收");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("此草稿应在所有页面切换后保留。");
    assert.equal(await page.locator(".mail-sender").count(), 0);
    const accountToggle = page.getByRole("button", {
      name: "取消全选发件账号",
      exact: true,
    });
    await accountToggle.click();
    assert.equal(
      await page.locator(".mail-account-choices input:checked").count(),
      0,
    );
    await page
      .getByRole("button", { name: "全选发件账号", exact: true })
      .click();
    assert.equal(
      await page.locator(".mail-account-choices input:checked").count(),
      5,
    );
    // 原生折叠区保留全部业务参数；默认收起，再通过真实点击核对默认值与可操作性。
    const delivery = page.locator(".mail-delivery-settings");
    assert.equal(await delivery.getAttribute("open"), null);
    await delivery.locator("summary").click();
    assert.equal(
      await page.getByLabel("同账号发送间隔秒数", { exact: true }).inputValue(),
      "900",
    );
    assert.equal(
      await page.getByLabel("账号切换间隔秒数", { exact: true }).inputValue(),
      "60",
    );
    assert.match(
      await page
        .getByLabel("同账号发送间隔秒数", { exact: true })
        .getAttribute("title"),
      /取消后仍保留/,
    );
    // 过去仅校验默认值后就收起，遗漏了用户实际展开时的错位；现在保留全部组合的截图与矩形断言。
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
      }, theme);
      for (const mode of ["group", "quick"]) {
        const modeButton = page.getByRole("button", {
          name: mode === "group" ? "群成员发送" : "快捷发送",
          exact: true,
        });
        await modeButton.click();
        await modeButton.hover();
        // 分段控件不能套主按钮hover配色，否则浅色背景和蓝字会在深色主题下混为一色。
        const selectedStyle = await modeButton.evaluate(async (button) => {
          await Promise.all(
            button.getAnimations().map((animation) => animation.finished),
          );
          const probe = document.createElement("span");
          probe.style.backgroundColor = "var(--selected)";
          probe.style.color = "var(--primary)";
          button.appendChild(probe);
          const actual = getComputedStyle(button);
          const expected = getComputedStyle(probe);
          const result = {
            background: actual.backgroundColor,
            color: actual.color,
            expectedBackground: expected.backgroundColor,
            expectedColor: expected.color,
          };
          probe.remove();
          return result;
        });
        assert.equal(
          selectedStyle.background,
          selectedStyle.expectedBackground,
        );
        assert.equal(selectedStyle.color, selectedStyle.expectedColor);
        for (const sendMode of ["bcc", "individual"]) {
          await page
            .getByLabel("发送方式", { exact: true })
            .selectOption(sendMode);
          await assertSendOptionsAligned(page, `${theme}-${mode}-${sendMode}`);
        }
      }
    }
    await page.getByRole("button", { name: "群成员发送", exact: true }).click();
    await page.getByLabel("发送方式", { exact: true }).selectOption("bcc");
    await page.evaluate(() => {
      document.documentElement.dataset.theme = "light";
    });
    await delivery.locator("summary").click();
    await page
      .locator(".mail-composer")
      .evaluate((el) => (el.closest(".page-scroll").scrollTop = 0));
    // 正文很长时发送入口仍留在可视区；操作区占据正常文档空间，不遮挡正文尾部。
    const footerBounds = await page.locator(".mail-footer").boundingBox();
    assert.ok(
      footerBounds.y >= 0 && footerBounds.y + footerBounds.height <= 1080,
    );
    // 长正文与左侧配置分别滚动，发送按钮不移动；从底部聚焦收件输入也应落在独立模式栏下方。
    const rail = page.locator(".mail-workbench .workbench-sidebar");
    const draftRegion = page.locator(".mail-workbench .workbench-content");
    const railBounds = await rail.boundingBox();
    const draftBounds = await draftRegion.boundingBox();
    assert.ok(railBounds.x + railBounds.width < draftBounds.x);
    const editor = page.getByRole("textbox", { name: "邮件正文", exact: true });
    const draftText = await editor.innerText();
    await editor.fill(`${draftText}\n${"长正文滚动验收\n".repeat(100)}`);
    await draftRegion.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    const contentScroll = await draftRegion.evaluate((el) => el.scrollTop);
    assert.ok(contentScroll > 0);
    await rail.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    assert.equal(
      await draftRegion.evaluate((el) => el.scrollTop),
      contentScroll,
    );
    assert.ok(
      Math.abs(
        (await page.locator(".mail-footer").boundingBox()).y - footerBounds.y,
      ) <= 1,
    );
    const recipientInput = page.getByLabel("补充邮箱", { exact: true });
    await recipientInput.focus();
    const recipientBounds = await recipientInput.boundingBox();
    const modeBounds = await page.locator(".mail-mode-switch").boundingBox();
    assert.ok(recipientBounds.y >= modeBounds.y + modeBounds.height);
    await editor.fill(draftText);
    await draftRegion.evaluate((element) => (element.scrollTop = 0));
    await rail.evaluate((element) => (element.scrollTop = 0));
    assert.equal(await page.locator(".app-header > .app-nav").count(), 1);
    assert.equal(await page.locator(".app-shell > .app-nav").count(), 0);
    await screenshot("mail-light");
    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    const settings = page.getByRole("dialog", {
      name: "发件设置",
      exact: true,
    });
    await settings.waitFor();
    await screenshot("accounts");
    await settings
      .getByRole("button", { name: "关闭发件设置", exact: true })
      .click();
    await page.getByRole("button", { name: "保存为模板", exact: true }).click();
    const save = page.locator(".mail-template-save-dialog");
    await save.getByLabel("模板名称", { exact: true }).fill("布局合成模板");
    await save.getByRole("button", { name: "保存模板", exact: true }).click();
    await save.waitFor({ state: "hidden" });
    assert.equal(
      await page
        .getByRole("button", { name: "保存为模板", exact: true })
        .count(),
      0,
    );
    await page
      .getByRole("button", { name: "另存为模板", exact: true })
      .waitFor();
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const templates = page.getByRole("dialog", {
      name: "邮件模板",
      exact: true,
    });
    await templates
      .getByText("布局合成模板", { exact: true })
      .first()
      .waitFor();
    await screenshot("templates");
    await templates
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    await nav("手动排除").click();
    const manual = page.locator(".manual-exclusions-page");
    await manual
      .getByLabel("手动排除账号", { exact: true })
      .fill("layout@example.test");
    await manual
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /已添加 1 条/).waitFor();
    await manual
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await manual
      .getByRole("button", { name: "取消全选（当前结果）", exact: true })
      .click();
    assert.equal(
      await manual
        .getByRole("button", { name: "移除所选", exact: true })
        .count(),
      0,
    );
    await screenshot("manual-light");
    await page
      .locator(".app-nav")
      .getByRole("button", { name: /已发送记录/ })
      .click();
    const entry = page.locator(".history-entry-card");
    assert.notEqual(await entry.getAttribute("open"), null);
    await entry.getByLabel("添加已发送标记", { exact: true }).fill("12345678");
    await entry.locator("summary").click();
    await entry.locator("summary").click();
    assert.equal(
      await entry.getByLabel("添加已发送标记", { exact: true }).inputValue(),
      "12345678",
    );
    await entry.getByRole("button", { name: "添加记录", exact: true }).click();
    await toastMessage(page, /已添加/).waitFor();
    await entry.locator("summary").click();
    await screenshot("history-light");
    await page.getByRole("button", { name: "查看日志", exact: true }).click();
    const logs = page.getByRole("dialog", { name: "查看日志", exact: true });
    await logs.waitFor();
    await screenshot("logs");
    await logs.getByRole("button", { name: "关闭日志", exact: true }).click();
    await nav("写邮件").click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "布局与预计时间合成验收",
    );
    await page.getByRole("button", { name: "发送", exact: true }).click();
    const confirmation = page.getByRole("dialog", {
      name: "确认发送",
      exact: true,
    });
    await confirmation.waitFor();
    await screenshot("confirmation");
    await confirmation
      .getByRole("button", { name: "返回修改", exact: true })
      .click();

    // 固定本地时区和时钟，验证中文完整日期、跨日、迟到版本以及无每秒IPC轮询。
    await page.clock.install({ time: new Date("2026-11-10T06:00:00Z") });
    await page.clock.pauseAt(new Date("2026-11-10T06:00:01Z"));
    const waiting = await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        id: "layout-estimate",
        status: "running",
        phase: "waiting",
        total: 2860,
        sent: 25,
        failed: 0,
        skipped: 0,
        nextBatchAt: Date.now() + 900000,
        estimatedCompletionAt: Date.parse("2026-11-10T06:22:53Z"),
        message: "合成等待",
        current: "第 1/23 轮",
      }),
    );
    const estimate = page.getByLabel("预计完成时间", { exact: true });
    await estimate
      .getByText("2026年11月10日14:22:53", { exact: true })
      .waitFor();
    assert.equal(
      await estimate.locator("time").getAttribute("datetime"),
      "2026-11-10T06:22:53.000Z",
    );
    assert.equal(await estimate.getAttribute("aria-live"), "off");
    await delivery.locator("summary").click();
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
      }, theme);
      assert.equal(
        await delivery.locator("input:enabled, select:enabled").count(),
        0,
      );
      await assertSendOptionsAligned(page, `${theme}-locked`);
    }
    await delivery.locator("summary").click();
    await page.evaluate(() => {
      document.documentElement.dataset.theme = "light";
    });
    await page
      .locator(".mail-countdown")
      .getByText("15:00", { exact: true })
      .waitFor();
    const reads = await page.evaluate(() => window.__uiFixture.calls.length);
    await page.clock.runFor(2000);
    assert.equal(
      await page.evaluate(() => window.__uiFixture.calls.length),
      reads,
    );
    await page
      .locator(".mail-countdown")
      .getByText("14:58", { exact: true })
      .waitFor();
    await page.locator(".mail-progress").scrollIntoViewIfNeeded();
    await screenshot("estimate-light");
    await page.evaluate(
      () => (document.documentElement.dataset.theme = "dark"),
    );
    await screenshot("estimate-dark");
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        nextBatchAt: null,
        estimatedCompletionAt: Date.parse("2026-11-11T01:02:03Z"),
      }),
    );
    await estimate
      .getByText("2026年11月11日09:02:03", { exact: true })
      .waitFor();
    await page.locator(".mail-countdown").waitFor({ state: "hidden" });
    await page.evaluate(
      (value) => window.__uiFixture.emitMailState(value),
      waiting,
    );
    assert.match(await estimate.innerText(), /2026年11月11日09:02:03/);
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        estimatedCompletionAt: Date.now() - 1,
      }),
    );
    await estimate.getByText(/正在更新/).waitFor();
    for (const invalid of [null, -1, 0, "invalid", 9e15]) {
      await page.evaluate(
        (value) =>
          window.__uiFixture.publishMailState({ estimatedCompletionAt: value }),
        invalid,
      );
      await estimate.waitFor({ state: "hidden" });
    }
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({
        phase: "sending",
        estimatedCompletionAt: Date.now() + 60000,
      }),
    );
    await estimate.waitFor();
    await nav("手动排除").click();
    await screenshot("manual-dark");
    await page
      .locator(".app-nav")
      .getByRole("button", { name: /已发送记录/ })
      .click();
    await screenshot("history-dark");
    await nav("成员提取").click();
    await screenshot("members-dark");
    await nav("写邮件").click();
    await estimate.waitFor();
    // 页面重载只重读宿主当前任务，绝对预计值保留，不从配置重新计满全部间隔。
    const originalEstimate = await page.evaluate(
      () => window.__uiFixture.mail().estimatedCompletionAt,
    );
    await page.reload();
    await nav("写邮件").click();
    await estimate.waitFor();
    assert.equal(
      await page.evaluate(
        () => window.__uiFixture.mail().estimatedCompletionAt,
      ),
      originalEstimate,
    );
    await page.evaluate(() =>
      window.__uiFixture.publishMailState({ phase: "stopping" }),
    );
    await estimate.waitFor({ state: "hidden" });
    for (const status of ["cancelled", "completed", "failed"]) {
      await page.evaluate(
        (value) =>
          window.__uiFixture.publishMailState({
            status: value,
            phase: "idle",
            estimatedCompletionAt: Date.now() + 60000,
          }),
        status,
      );
      assert.equal(await estimate.count(), 0);
    }
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter((c) => c.command === "start_mail_job")
            .length,
      ),
      0,
    );
  } finally {
    await context.close();
  }

  // 恢复失败和过期提示是全局问题，精简后在写信页也必须可见，扫码入口仍在右上角。
  for (const initialSessionStatus of ["signed_out", "expired"]) {
    const context = await browser.newContext({
      viewport: testViewport(1920, 1080),
    });
    await context.addInitScript(installFixture, { initialSessionStatus });
    const page = await context.newPage();
    page.on("pageerror", (error) => pageErrors.push(error.message));
    try {
      await page.goto("http://127.0.0.1:1420");
      await page
        .locator(".header-actions")
        .getByRole("button", { name: "扫码登录", exact: true })
        .waitFor();
      await page.getByRole("button", { name: "写邮件", exact: true }).click();
      if (initialSessionStatus === "expired")
        await page.locator(`.session-${initialSessionStatus}`).waitFor();
      assert.equal(await page.locator(".source-bar").count(), 0);
    } finally {
      await context.close();
    }
  }
  console.log(
    "1920x1080 completion/layout: all four pages, account/template/log/confirmation dialogs, selection toggles, form preservation, local full datetime, phase/terminal/stale/invalid estimates and no timer IPC passed (synthetic only)",
  );
}

/** 1920×1080 下验证真实页面接线、全量排序、稳定勾选和末页缩减后的行为；所有数据均为合成。 */
async function checkListSorting() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
  });
  await context.addInitScript(installFixture, {
    sortingScenario: true,
    initialPasswordSaved: true,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      message.text() !==
        "Blocked script execution in 'about:srcdoc' because the document's frame is sandboxed and the 'allow-scripts' permission is not set."
    )
      pageErrors.push(message.text());
  });
  const nav = (name) =>
    page.locator(".app-nav").getByRole("button", { name, exact: true });
  const sources = () =>
    page.evaluate(() => window.__uiFixture.sortingSources());
  const screenshot = async (name) => {
    assert.ok(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
    );
    await page.screenshot({
      path: new URL(`list-sort-${name}.png`, artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
    });
  };
  try {
    await page.goto("http://127.0.0.1:1420");
    const groups = page.locator(".group-list");
    await groups.locator(".group-item").first().waitFor();
    const original = await sources();
    const expectedGroups = [
      "2群",
      "10群",
      "a2群",
      "a10群",
      "阿群",
      "B群",
      "波群",
      "李群",
      "中群",
      "中群",
      "张群",
      "!群",
      "😀群",
    ];
    assert.deepEqual(
      await groups.locator("strong").allTextContents(),
      expectedGroups,
    );
    assert.equal(
      await groups
        .locator(".group-item")
        .last()
        .locator(".group-avatar")
        .innerText(),
      "😀",
    );
    await groups.getByRole("button", { name: /^2群/ }).click();
    await page.evaluate(() => window.__uiFixture.reverseSortingGroups());
    await page.waitForFunction(() =>
      document
        .querySelector(".group-item.selected")
        ?.textContent.includes("900005"),
    );
    assert.deepEqual(
      await groups.locator("strong").allTextContents(),
      expectedGroups,
    );
    assert.deepEqual(
      await groups.locator(".group-item-content > span").allTextContents(),
      [
        "900005",
        "900003",
        "900004",
        "900006",
        "900010",
        "900002",
        "900011",
        "900012",
        "900001",
        "900009",
        "900013",
        "900007",
        "900008",
      ],
    );
    await page.getByLabel("搜索群名或群号", { exact: true }).fill("a");
    assert.deepEqual(await groups.locator("strong").allTextContents(), [
      "a2群",
      "a10群",
    ]);
    await page.getByLabel("搜索群名或群号", { exact: true }).fill("");
    assert.match(await groups.locator(".selected").innerText(), /900005/);
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    const exclusions = page.locator(".exclusion-picker");
    assert.deepEqual(
      await exclusions.locator(".exclusion-option small").allTextContents(),
      [
        "900003",
        "900004",
        "900006",
        "900010",
        "900002",
        "900011",
        "900012",
        "900001",
        "900009",
        "900013",
        "900007",
        "900008",
      ],
    );
    await exclusions
      .locator(".exclusion-option")
      .filter({ hasText: "900003" })
      .locator("input")
      .check();
    await exclusions.getByLabel("搜索排除群", { exact: true }).fill("a");
    await exclusions
      .getByRole("button", { name: "全选当前结果", exact: true })
      .click();
    await exclusions.getByLabel("搜索排除群", { exact: true }).fill("");
    assert.equal(await exclusions.locator("input:checked").count(), 3);
    await exclusions
      .getByRole("button", { name: "清空全部选择", exact: true })
      .click();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await page.getByLabel("过滤已发送记录", { exact: true }).uncheck();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 62", exact: true })
      .waitFor();
    const memberChoices = page.locator(
      '.selectable-member-table tbody input[type="checkbox"]',
    );
    await memberChoices.first().waitFor();
    await page.getByLabel("选择成员 10000001", { exact: true }).check();
    assert.equal(
      await memberChoices.first().getAttribute("aria-label"),
      "选择成员 10000001",
    );
    assert.equal(
      await memberChoices.last().getAttribute("aria-label"),
      "选择成员 10000050",
    );
    await page.getByRole("button", { name: "下一页成员", exact: true }).click();
    assert.equal(
      await memberChoices.first().getAttribute("aria-label"),
      "选择成员 10000051",
    );
    await page.getByLabel("选择成员 10000062", { exact: true }).check();
    await page
      .getByLabel("搜索成员QQ号、昵称或群名片", { exact: true })
      .fill("10000002");
    assert.equal(await memberChoices.count(), 1);
    await page
      .getByLabel("搜索成员QQ号、昵称或群名片", { exact: true })
      .fill("");
    assert.equal(
      await page.getByLabel("选择成员 10000001", { exact: true }).isChecked(),
      true,
    );
    await screenshot("members-light");
    await page
      .getByRole("button", { name: "给已选 2 人写信", exact: true })
      .click();
    await page
      .getByRole("button", { name: "查看群收件人（2）", exact: true })
      .click();
    assert.deepEqual(
      await page
        .locator(".mail-audience-roster tbody tr td:nth-child(2)")
        .allTextContents(),
      ["10000001", "10000062"],
    );
    assert.deepEqual((await sources()).members, original.members);
    assert.deepEqual(
      (await page.evaluate(() => window.__uiFixture.snapshot())).members.map(
        (m) => m.qqNumber,
      ),
      original.members.map((m) => m.qqNumber),
    );

    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library.locator(".mail-template-item").first().waitFor();
    assert.deepEqual(
      await library.locator(".mail-template-item strong").allTextContents(),
      [
        "2模板",
        "10模板",
        "a2模板",
        "a10模板",
        "B模板",
        "中模板",
        "!模板",
        "😀模板",
      ],
    );
    await library.getByLabel("选择导出模板 中模板", { exact: true }).check();
    await library
      .locator(".mail-template-item")
      .filter({ hasText: "中模板" })
      .click();
    await library
      .getByRole("button", { name: "重命名模板", exact: true })
      .click();
    const rename = page.getByRole("dialog", {
      name: "重命名模板",
      exact: true,
    });
    await rename.getByLabel("模板名称", { exact: true }).fill("1模板");
    await rename.getByRole("button", { name: "保存名称", exact: true }).click();
    await library
      .locator(".mail-template-item.selected strong")
      .getByText("1模板", { exact: true })
      .waitFor();
    assert.equal(
      await library.locator(".mail-template-item strong").first().innerText(),
      "1模板",
    );
    assert.equal(
      await library
        .getByLabel("选择导出模板 1模板", { exact: true })
        .isChecked(),
      true,
    );
    await library
      .getByRole("button", { name: "导出已选 1 个模板", exact: true })
      .click();
    await page.waitForFunction(
      () => window.__uiFixture.exportedTemplatePackages().length === 1,
    );
    assert.equal(
      (
        await page.evaluate(() => window.__uiFixture.exportedTemplatePackages())
      )[0].templates[0].name,
      "1模板",
    );
    await screenshot("templates-light");
    // 导入预览也按名称展示，取消后原模板库保持相同内容和原始包顺序。
    await page.evaluate(
      (values) => window.__uiFixture.setTemplateImportPackage(values),
      [original.templates[2], original.templates[7], original.templates[4]],
    );
    await library
      .getByRole("button", { name: "导入模板", exact: true })
      .click();
    const importing = page.getByRole("dialog", {
      name: "导入邮件模板",
      exact: true,
    });
    await importing.waitFor();
    assert.deepEqual(
      await importing
        .locator(".mail-template-import-list strong")
        .allTextContents(),
      ["2模板", "10模板", "😀模板"],
    );
    await importing
      .getByRole("button", { name: "取消导入", exact: true })
      .click();
    await importing.waitFor({ state: "hidden" });
    assert.equal((await sources()).templates.length, original.templates.length);
    await library.getByRole("button", { name: "取消", exact: true }).click();

    await page
      .locator(".app-nav")
      .getByRole("button", { name: /已发送记录/ })
      .click();
    const history = page.locator(".history-page");
    await history
      .getByRole("button", { name: "显示记录完整 QQ 号", exact: true })
      .click();
    const historyNumbers = history.locator(
      ".history-table-wrap tbody tr td:first-child",
    );
    assert.equal(await historyNumbers.first().innerText(), "20000");
    assert.equal(await historyNumbers.last().innerText(), "20049");
    assert.equal(
      await history.locator(".unresolved-email").first().innerText(),
      "pending-1@example.test",
    );
    assert.equal(
      await history.locator(".unresolved-email").last().innerText(),
      "pending-25@example.test",
    );
    await history
      .getByLabel("选择待核实 pending-1@example.test", { exact: true })
      .check();
    await history
      .getByRole("button", { name: "下一页待核实", exact: true })
      .click();
    assert.equal(
      await history.locator(".unresolved-email").first().innerText(),
      "pending-26@example.test",
    );
    await history
      .getByLabel("选择待核实 pending-28@example.test", { exact: true })
      .check();
    await history
      .getByLabel("搜索待核实投递", { exact: true })
      .fill("pending-1@");
    assert.equal(
      await history
        .getByLabel("选择待核实 pending-1@example.test", { exact: true })
        .isChecked(),
      true,
    );
    await history.getByLabel("搜索待核实投递", { exact: true }).fill("");
    assert.equal(
      await history.locator(".unresolved-list input:checked").count(),
      1,
    );
    await history.getByRole("button", { name: "下一页", exact: true }).click();
    assert.equal(await historyNumbers.first().innerText(), "20050");
    await history.getByLabel("搜索已发送记录", { exact: true }).fill("20001");
    assert.equal(await historyNumbers.first().innerText(), "20001");
    await history.getByLabel("搜索已发送记录", { exact: true }).fill("");
    assert.equal(await historyNumbers.first().innerText(), "20000");
    assert.deepEqual((await sources()).records, original.records);
    await screenshot("history-light");

    await nav("手动排除").click();
    const manual = page.locator(".manual-exclusions-page");
    const manualEmails = manual.locator("tbody tr td:nth-child(2)");
    await manual.getByLabel("选择排除记录 1", { exact: true }).check();
    assert.match(await manualEmails.first().innerText(), /^30000@qq.com/);
    assert.match(await manualEmails.last().innerText(), /^30024@qq.com/);
    await manual.getByRole("button", { name: "下一页", exact: true }).click();
    assert.match(await manualEmails.first().innerText(), /^30025@qq.com/);
    await manual
      .getByRole("button", { name: "移除排除记录 26", exact: true })
      .click();
    await manual.getByRole("button", { name: "确认移除", exact: true }).click();
    await toastMessage(page, /已移除 1 条/).waitFor();
    assert.match(await manualEmails.first().innerText(), /^30000@qq.com/);
    await manual
      .getByLabel("手动排除账号", { exact: true })
      .fill("30026@qq.com");
    await manual
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /已添加 1 条/).waitFor();
    // 若页码只在 render 时钳制、没有同步状态，此时会意外返回第二页。
    assert.match(await manualEmails.first().innerText(), /^30000@qq.com/);
    assert.equal(
      await manual.getByLabel("选择排除记录 1", { exact: true }).isChecked(),
      true,
    );
    await manual.getByLabel("搜索手动排除", { exact: true }).fill("30026");
    assert.match(await manualEmails.first().innerText(), /^30026@qq.com/);
    await manual.getByLabel("搜索手动排除", { exact: true }).fill("");
    assert.equal(
      await manual.getByLabel("选择排除记录 1", { exact: true }).isChecked(),
      true,
    );
    await screenshot("manual-light");
    await page.evaluate(
      () => (document.documentElement.dataset.theme = "dark"),
    );
    await screenshot("manual-dark");
    await nav("成员提取").click();
    await screenshot("members-dark");
    // 已过滤标签使用同一完整集合排序，不能沿用远端返回的反向顺序。
    await page.evaluate(() => {
      for (const qq of ["10000001", "10000002", "10000010"])
        window.__uiFixture.setFriendRelation(qq, "friend");
    });
    await page.getByLabel("排除我的 QQ 好友", { exact: true }).check();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.getByRole("tab", { name: "已过滤 3", exact: true }).click();
    const revealMembers = page.getByRole("button", {
      name: "显示完整 QQ 号",
      exact: true,
    });
    if (await revealMembers.count()) await revealMembers.click();
    assert.deepEqual(
      await page
        .locator(".filtered-member-table tbody tr .qq-number")
        .allTextContents(),
      ["10000001", "10000002", "10000010"],
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter((c) => c.command === "start_mail_job")
            .length,
      ),
      0,
    );
  } finally {
    await context.close();
  }
  console.log(
    "1920x1080 list sorting: groups/exclusions, complete member and history pagination, stable selections, sender audience, template rename/export identity, manual last-page recovery, Chinese pinyin A-Z, Unicode avatar and light/dark layout passed (synthetic only)",
  );
}

/** 共享浮层验收覆盖真实Sonner样式顺序和原生模态层，不使用占位栏验收规则。 */
async function checkTopFeedbackLayout() {
  await checkFloatingFeedback({
    browser,
    installFixture,
    artifacts,
    pageErrors,
    assertFixedDialogFooter,
    checkToastReceivesPointer,
  });
}

/**
 * 逐页检查展开、长内容、搜索无结果与键盘状态；所有数据仍来自隔离夹具。
 * 几何与命中检查关注用户是否能看清、点到控件，截图留给人工逐项核对。
 */
async function checkDetailPolish() {
  const context = await browser.newContext({
    viewport: testViewport(1920, 1080),
    reducedMotion: "reduce",
    colorScheme: "light",
  });
  await context.addInitScript(() => {
    if (window.top !== window) return;
    sessionStorage.setItem(
      "__ui_fixture_mail_templates",
      JSON.stringify(
        Array.from({ length: 14 }, (_, index) => ({
          id: `detail-template-${index}`,
          name:
            index === 0
              ? "长名称模板用于核对换行与选择框的间距".repeat(3)
              : `工作模板 ${index + 1}`,
          subject: "主题和预览独立显示，导出选择不影响当前草稿",
          html: "<p>隔离界面验收正文。</p>",
          inlineImages: [],
          attachments: [],
          updatedAt: "2026-11-12T06:22:53Z",
        })),
      ),
    );
  });
  await context.addInitScript(installFixture, {
    multiAccountScenario: true,
    initialPasswordSaved: true,
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const capture = async (name) => {
    await assertFixedDialogFooter(page);
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
      true,
    );
    for (const theme of ["light", "dark"]) {
      await page.evaluate((value) => {
        document.documentElement.dataset.theme = value;
      }, theme);
      // 只检查当前可见搜索框；无结果与清除按钮不能改变行高，深浅主题使用相同几何。
      const searches = await page
        .locator(".search-field, .mail-template-search")
        .evaluateAll((elements) =>
          elements
            .map((el) => el.getBoundingClientRect())
            .filter((r) => r.width > 0),
        );
      assert.ok(
        searches.every((r) => Math.abs(r.height - 40) <= 1),
        `${name}/${theme}: search heights differ`,
      );
      if (name === "members-results") {
        const selected = page.locator(".group-item.selected");
        await selected.hover();
        const colors = await selected.evaluate((el) => {
          const probe = document.createElement("span");
          probe.style.backgroundColor = "var(--selected)";
          el.append(probe);
          const result = [
            getComputedStyle(el).backgroundColor,
            getComputedStyle(probe).backgroundColor,
          ];
          probe.remove();
          return result;
        });
        assert.equal(
          colors[0],
          colors[1],
          "hover must preserve selected group",
        );
      }
      await page.screenshot({
        path: new URL(
          `detail-${name}-${theme}.png`,
          artifacts,
        ).pathname.replace(/^\/(\w:)/, "$1"),
      });
    }
    await page.evaluate(() => {
      document.documentElement.dataset.theme = "light";
    });
  };
  try {
    await page.goto("http://127.0.0.1:1420");
    await page.getByLabel("当前已登录账号", { exact: true }).waitFor();
    await page
      .locator(".group-list")
      .getByRole("button", { name: /目标测试群/ })
      .click();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await capture("members-expanded");
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page
      .getByRole("tab", { name: "筛选后成员 61", exact: true })
      .waitFor();
    await page.locator(".result-details > summary").click();
    await page.locator(".group-item.selected").hover();
    await capture("members-results");
    await page
      .getByRole("button", { name: "给全部 61 人写信", exact: true })
      .click();
    await page.getByLabel("主题", { exact: true }).fill("细节状态验收");
    await page
      .getByRole("textbox", { name: "邮件正文", exact: true })
      .fill("草稿应跨页面和弹窗保留。");
    await page.locator(".mail-delivery-settings > summary").click();
    await page.locator(".mail-workbench .workbench-sidebar").evaluate((el) => {
      el.scrollTop = 180;
    });
    await capture("mail-expanded");
    const rail = page.locator(".mail-workbench .workbench-sidebar");
    const mode = await page.locator(".mail-mode-switch").boundingBox();
    const railBox = await rail.boundingBox();
    assert.ok(
      railBox.y >= mode.y + mode.height + 8,
      "mode controls must sit outside the scrolling content",
    );
    const recipientPanel = await page
      .locator(".mail-recipient-panel")
      .boundingBox();
    assert.ok(
      Math.abs(mode.x - recipientPanel.x) <= 1 &&
        Math.abs(
          mode.x + mode.width - recipientPanel.x - recipientPanel.width,
        ) <= 1,
      "fixed mode bar and scrolling cards must share both column edges",
    );
    await rail.evaluate((el) => {
      el.scrollTop = el.scrollHeight;
    });
    await capture("mail-options");
    await page.getByRole("button", { name: "发件设置", exact: true }).click();
    await page
      .getByLabel("SMTP 服务器", { exact: true })
      .fill("smtp.example.test");
    await capture("accounts-dirty");
    const accountGeometry = await page
      .locator(".mail-settings-grid")
      .evaluate((el) =>
        [...el.querySelectorAll("input,select")].map((control) => {
          const r = control.getBoundingClientRect();
          return {
            type: control.tagName + ":" + control.type,
            x: r.x,
            y: r.y,
            height: r.height,
          };
        }),
      );
    console.log(
      "Detail audit account controls:",
      JSON.stringify(accountGeometry),
    );
    assert.ok(
      accountGeometry.every((r) => r.height === 40),
      "all account inputs/selects need identical height",
    );
    const encryption = accountGeometry.find(
      (r) => r.type === "SELECT:select-one",
    );
    const port = accountGeometry.find((r) => r.type === "INPUT:number");
    assert.equal(encryption.y, port.y, "encryption and port must share a row");
    await page
      .getByRole("button", { name: "关闭发件设置", exact: true })
      .click();
    await page.getByRole("button", { name: "选择模板", exact: true }).click();
    const library = page.getByRole("dialog", { name: "邮件模板", exact: true });
    await library.locator(".mail-template-item").first().waitFor();
    await library.locator(".mail-template-item").first().click();
    await library.locator("iframe").waitFor();
    await library.locator(".mail-template-export-check input").first().check();
    await capture("templates-long");
    const cardHeights = await library
      .locator(".mail-template-row")
      .evaluateAll((elements) =>
        elements.map((el) => el.getBoundingClientRect().height),
      );
    assert.ok(
      Math.max(...cardHeights) - Math.min(...cardHeights) <= 1,
      "long template titles must not break card alignment",
    );
    await library
      .getByLabel("搜索模板名称或主题", { exact: true })
      .fill("没有这个模板");
    await capture("templates-empty");
    await library
      .getByRole("button", { name: "清除搜索", exact: true })
      .click();
    await library
      .getByRole("button", { name: "关闭邮件模板", exact: true })
      .click();
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "已发送记录", exact: true })
      .click();
    await capture("history");
    const unresolvedGeometry = await page
      .locator(".unresolved-row")
      .evaluateAll((elements) =>
        elements.map((el) => {
          const checkbox = el.querySelector("input").getBoundingClientRect();
          const name = el
            .querySelector(".unresolved-email")
            .getBoundingClientRect();
          return {
            x: checkbox.right <= name.left,
            delta: Math.abs(checkbox.top - name.top),
          };
        }),
      );
    assert.ok(
      unresolvedGeometry.every((r) => r.x && r.delta <= 5),
      "unresolved checkbox and details must share the first row",
    );
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    await page
      .getByLabel("手动排除账号", { exact: true })
      .fill("detail@example.test");
    await page
      .getByRole("button", { name: "添加手动排除", exact: true })
      .click();
    await toastMessage(page, /已添加 1 条/).waitFor();
    await capture("manual");
    await page.keyboard.press("Tab");
    await page.locator(".manual-workbench .workbench-sidebar").focus();
    assert.equal(
      await page
        .locator(".manual-workbench .workbench-sidebar")
        .evaluate((el) => getComputedStyle(el).outlineStyle),
      "solid",
    );
    await capture("keyboard-focus");
    await page.getByRole("button", { name: "重新扫码", exact: true }).click();
    await page.locator(".qr-stage img").waitFor();
    await capture("login");
    const loginHeader = await page
      .locator(".login-dialog .dialog-head")
      .boundingBox();
    const loginHeading = await page
      .getByRole("heading", { name: "扫码登录", exact: true })
      .boundingBox();
    assert.ok(
      loginHeading.y >= loginHeader.y &&
        loginHeading.y + loginHeading.height <=
          loginHeader.y + loginHeader.height,
      "QR title must share the close-button row",
    );
    await page
      .getByRole("button", { name: "关闭扫码登录", exact: true })
      .click();
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    assert.equal(
      await page.getByLabel("主题", { exact: true }).inputValue(),
      "细节状态验收",
    );
    assert.equal(
      await page
        .getByRole("textbox", { name: "邮件正文", exact: true })
        .innerText(),
      "草稿应跨页面和弹窗保留。",
    );
    // 动效使用真实系统媒体偏好；读取样式并等待浏览器动画完成，避免固定sleep与中间态截图。
    await page.emulateMedia({ reducedMotion: "no-preference" });
    await page
      .locator(".app-nav")
      .getByRole("button", { name: "手动排除", exact: true })
      .click();
    const active = page.locator(".page-frame:not([hidden])");
    assert.equal(
      await active.evaluate((el) => getComputedStyle(el).animationName),
      "workbench-reveal",
    );
    assert.equal(
      await active.evaluate((el) => getComputedStyle(el).animationDuration),
      "0.16s",
    );
    await active.evaluate(async (el) => {
      await Promise.all(el.getAnimations().map((a) => a.finished));
    });
    await page.getByRole("button", { name: "重新扫码", exact: true }).click();
    const qr = page.locator(".login-dialog[open]");
    assert.equal(
      await qr.evaluate((el) => getComputedStyle(el).animationDuration),
      "0.16s",
    );
    await qr.evaluate(async (el) => {
      await Promise.all(el.getAnimations().map((a) => a.finished));
    });
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.equal(
      await active.evaluate((el) => getComputedStyle(el).animationName),
      "none",
    );
    assert.equal(
      await qr.evaluate((el) => getComputedStyle(el).animationName),
      "none",
    );
    assert.equal(
      await qr.evaluate(
        (el) => getComputedStyle(el, "::backdrop").animationName,
      ),
      "none",
    );
    console.log(
      "1920x1080 detail states captured: expanded filters, results, options, dirty settings, long/empty templates, history, exclusions and QR; draft preserved",
    );
  } finally {
    await context.close();
  }
}

try {
  // 用户本轮仅要求1920×1080；可独立执行本缺陷的完整业务流程，不重跑其他尺寸。
  if (production) {
    if (process.env.UI_CHECK_ONLY === "production-about")
      await checkAbout({ browser, installFixture, artifacts, pageErrors });
    else if (process.env.UI_CHECK_ONLY === "production-multi-group")
      await checkMultiGroup({ browser, installFixture, artifacts, pageErrors });
    else if (process.env.UI_CHECK_ONLY === "production-options")
      await checkRememberedOptions({
        browser,
        installFixture,
        artifacts,
        pageErrors,
      });
    else
      await checkProductionFeedback({
        browser,
        installFixture,
        artifacts,
        pageErrors,
        checkToastReceivesPointer,
      });
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "about") {
    await checkAbout({ browser, installFixture, artifacts, pageErrors });
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "multi-group") {
    await checkMultiGroup({ browser, installFixture, artifacts, pageErrors });
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "remember-options") {
    await checkRememberedOptions({
      browser,
      installFixture,
      artifacts,
      pageErrors,
    });
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "filter-preferences") {
    await checkFriendFiltering();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "detail-layout") {
    await checkDetailPolish();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "top-feedback") {
    await checkTopFeedbackLayout();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "terminal-layout") {
    await checkCompletionAndLayout();
    await checkTopFeedbackLayout();
    assert.deepEqual(pageErrors, []);
  } else if (
    ["apple-clean", "terminal-design", "detail-polish"].includes(
      process.env.UI_CHECK_ONLY,
    )
  ) {
    if (
      ["terminal-design", "detail-polish"].includes(process.env.UI_CHECK_ONLY)
    )
      await checkTopFeedbackLayout();
    if (process.env.UI_CHECK_ONLY === "detail-polish")
      await checkDetailPolish();
    await checkRememberedOptions({
      browser,
      installFixture,
      artifacts,
      pageErrors,
    });
    await checkFriendFiltering();
    await checkAbout({ browser, installFixture, artifacts, pageErrors });
    await checkMailCountdown();
    await checkMultiGroup({ browser, installFixture, artifacts, pageErrors });
    await checkMultiSenderAccounts();
    await checkSavedMailPassword();
    await checkThemePersistence();
    await checkRecipientPreparationAndFailurePolicy();
    await checkHistorySenderAttribution();
    await checkHistoryLoadingAndRetry();
    await checkBulkHistoryResolution();
    await checkLogViewer();
    await checkQuickSendAndManualExclusions();
    await checkSkipWaiting();
    await checkScanAccountSwitch();
    await checkListSorting();
    await checkCompletionAndLayout();
    await checkFilteredMemberRecovery();
    await checkTemplateTransfer();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "interaction-polish") {
    await checkSkipWaiting();
    await checkListSorting();
    await checkCompletionAndLayout();
    await checkFilteredMemberRecovery();
    await checkTemplateTransfer();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "qr-session") {
    await checkScanAccountSwitch();
    await checkListSorting();
    await checkCompletionAndLayout();
    await checkFilteredMemberRecovery();
    await checkTemplateTransfer();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "list-sort") {
    await checkListSorting();
    await checkCompletionAndLayout();
    await checkFilteredMemberRecovery();
    await checkTemplateTransfer();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "completion-layout") {
    await checkCompletionAndLayout();
    await checkFilteredMemberRecovery();
    await checkTemplateTransfer();
    assert.deepEqual(pageErrors, []);
  } else if (process.env.UI_CHECK_ONLY === "filter-recovery") {
    await checkFilteredMemberRecovery();
    assert.deepEqual(pageErrors, []);
  } else {
    await checkQuickSendAndManualExclusions();
    const context = await browser.newContext({
      viewport: testViewport(1200, 900),
    });
    await context.addInitScript(installFixture);
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.goto("http://127.0.0.1:1420");
    console.log("Fixture initialized");
    await checkContextMenuBlocked(page, page.locator(".app-header"));
    // 登录入口集中在全局右上角，成员页不再重复创建一整行工具栏。
    assert.equal(await page.locator(".source-bar").count(), 0);
    await page
      .locator(".header-actions")
      .getByRole("button", { name: "重新扫码", exact: true })
      .waitFor();
    const navigationTop = await page
      .locator(".app-nav")
      .evaluate((element) => element.getBoundingClientRect().top);
    await page.getByRole("button", { name: /目标测试群/ }).click();
    await page.getByRole("button", { name: /排除其他群的成员/ }).click();
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .check();
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.getByRole("tab", { name: "筛选后成员 61" }).waitFor();
    console.log("Fetch and cross-group exclusion rendered");
    const fetchCall = await page.evaluate(() =>
      window.__uiFixture.calls.find(
        (entry) => entry.command === "fetch_members",
      ),
    );
    assert.deepEqual(fetchCall.args, {
      groupId: "900001",
      excludeGroupIds: ["900002"],
      excludeSent: true,
      excludeFriends: false,
    });
    await page.getByRole("tab", { name: "已过滤 1" }).click();
    await page
      .locator(".filter-reason")
      .filter({ hasText: "排除群重合" })
      .waitFor();
    await page.getByRole("tab", { name: "筛选后成员 61" }).click();
    await page
      .getByRole("textbox", { name: "搜索成员QQ号、昵称或群名片" })
      .fill("测试成员2");
    assert.match(await page.locator(".export-bar").innerText(), /全部 61 人/);
    await page.getByRole("button", { name: "导出 Excel", exact: true }).click();
    await toastMessage(page, "文件已保存").waitFor();
    assert.equal(await page.locator(".app-shell > .notice").count(), 0);
    assert.equal(
      await page
        .locator(".app-nav")
        .evaluate((element) => element.getBoundingClientRect().top),
      navigationTop,
      "Transient feedback must not move the page navigation",
    );
    await page
      .getByRole("textbox", { name: "搜索成员QQ号、昵称或群名片" })
      .fill("");
    await page.getByRole("button", { name: /已发送记录/ }).click();
    console.log("Opening history workspace");
    await page
      .getByRole("heading", { name: "已发送记录", exact: true })
      .waitFor();
    assert.equal(await page.getByRole("dialog").count(), 0);
    assert.match(
      await page.locator('.app-nav [aria-current="page"]').innerText(),
      /^已发送记录\s*1$/,
    );
    await page.getByRole("button", { name: /删除发送记录/ }).click();
    await page
      .getByRole("button", { name: "返回成员提取", exact: true })
      .click();
    await page.getByRole("button", { name: /已发送记录/ }).click();
    assert.equal(
      await page.getByRole("button", { name: "确认删除", exact: true }).count(),
      0,
    );
    await page
      .locator(".history-table-wrap td")
      .filter({ hasText: "导入记录" })
      .waitFor();
    await page.getByRole("button", { name: /删除发送记录/ }).click();
    await page.getByRole("button", { name: "确认删除", exact: true }).click();
    await page.getByText("还没有已发送记录，可以在上方添加或导入。").waitFor();
    await page.getByRole("heading", { name: "待核实投递 2 条" }).waitFor();
    await page
      .getByRole("button", {
        name: "核实为已发送 10000333@qq.com",
        exact: true,
      })
      .click();
    assert.match(
      await page.locator(".resolution-confirm").innerText(),
      /人工核实记录.*不作为 SMTP 送达证明/,
    );
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "resolve_mail_delivery",
          ).length,
      ),
      0,
      "Opening a resolution confirmation must not mutate history",
    );
    await page.screenshot({
      path: new URL("history-resolution.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    for (const width of testWidths([780, 375])) {
      await page.setViewportSize(testViewport(width, 900));
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth + 1,
        ),
        true,
        `Unresolved delivery confirmation must fit the history page at ${width}px`,
      );
      await page.screenshot({
        path: new URL(`history-page-${width}.png`, artifacts).pathname.replace(
          /^\/(\w:)/,
          "$1",
        ),
        fullPage: false,
      });
    }
    await page.setViewportSize(testViewport(1200, 900));
    await page.getByRole("button", { name: "取消核实", exact: true }).click();
    await page
      .getByRole("button", {
        name: "核实为未发送 person@example.test",
        exact: true,
      })
      .click();
    await page
      .getByRole("button", { name: "返回成员提取", exact: true })
      .click();
    await page.getByRole("button", { name: /已发送记录/ }).click();
    assert.equal(await page.locator(".resolution-confirm").count(), 0);
    await page
      .getByRole("button", {
        name: "核实为已发送 10000333@qq.com",
        exact: true,
      })
      .click();
    await page.getByRole("button", { name: "确认已发送", exact: true }).click();
    await page
      .locator(".history-table-wrap td")
      .filter({ hasText: "人工核实已发送" })
      .waitFor();
    await page.getByRole("heading", { name: "待核实投递 1 条" }).waitFor();
    await page
      .getByRole("button", {
        name: "核实为未发送 person@example.test",
        exact: true,
      })
      .click();
    assert.match(
      await page.locator(".resolution-confirm").innerText(),
      /解除这些地址的重发限制.*不会立即发信/,
    );
    await page.getByRole("button", { name: "确认未发送", exact: true }).click();
    await page.getByText("没有待核实投递。", { exact: true }).waitFor();
    const resolutions = await page.evaluate(() =>
      window.__uiFixture.calls
        .filter((entry) => entry.command === "resolve_mail_delivery")
        .map((entry) => entry.args),
    );
    assert.deepEqual(resolutions, [
      { id: 31, accepted: true },
      { id: 32, accepted: false },
    ]);
    assert.equal(
      await page.evaluate(
        () =>
          window.__uiFixture.calls.filter(
            (entry) => entry.command === "start_mail_job",
          ).length,
      ),
      0,
      "Resolving history must not send mail",
    );
    await page
      .getByRole("button", { name: "返回成员提取", exact: true })
      .click();
    console.log("History delete and explicit delivery resolution checked");
    const snapshotBeforeCancel = await page.evaluate(
      () => window.__uiFixture.snapshot()?.id,
    );
    await page.evaluate(() => window.__uiFixture.holdNextFetch());
    await page.getByRole("button", { name: "获取成员", exact: true }).click();
    await page.getByRole("button", { name: "取消获取", exact: true }).click();
    await page.evaluate(() => window.__uiFixture.releaseFetch());
    await toastMessage(page, /已取消/).waitFor();
    assert.equal(await page.locator(".task-strip").count(), 0);
    assert.equal(
      await page.evaluate(() => window.__uiFixture.snapshot()?.id),
      snapshotBeforeCancel,
      "Cancelling a refresh must preserve the previous completed result",
    );
    await page
      .locator(".exclusion-options")
      .getByLabel(/排除测试群/)
      .uncheck();
    await page.getByRole("button", { name: "重新扫码", exact: true }).click();
    const qrDialog = page.getByRole("dialog", {
      name: "扫码登录",
      exact: true,
    });
    await qrDialog.waitFor();
    await page.waitForFunction(() =>
      window.__uiFixture.calls.some(
        (entry) => entry.command === "poll_web_login",
      ),
    );
    assert.equal(
      (await qrDialog.locator(".login-hint").innerText()).trim(),
      "请使用手机 QQ 扫码",
    );
    await qrDialog
      .getByRole("button", { name: "刷新二维码", exact: true })
      .click();
    await page.waitForFunction(
      () =>
        window.__uiFixture.calls.filter(
          (entry) => entry.command === "poll_web_login",
        ).length >= 2,
    );
    assert.equal(
      (await qrDialog.locator(".login-hint").innerText()).trim(),
      "请使用手机 QQ 扫码",
    );
    await page.screenshot({
      path: new URL("qr-login-consistent.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await page.evaluate(() => window.__uiFixture.setQrStatus("scanned"));
    await qrDialog
      .locator(".login-hint")
      .filter({ hasText: "已扫码，请在手机上确认登录" })
      .waitFor();
    await page.evaluate(() => window.__uiFixture.setQrStatus("expired"));
    await qrDialog
      .locator(".login-hint")
      .filter({ hasText: "二维码已过期，请刷新" })
      .waitFor();
    await page.getByRole("button", { name: "关闭扫码登录" }).click();
    await page.screenshot({
      path: new URL("members-desktop.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    for (const width of testWidths([960, 700, 375])) {
      await page.setViewportSize(testViewport(width, 900));
      const fits = await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth + 1,
      );
      assert.equal(fits, true, `horizontal overflow at ${width}`);
    }
    await page.screenshot({
      path: new URL("members-narrow.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await page.setViewportSize(testViewport(1200, 900));
    await page.getByRole("button", { name: /外观：跟随系统/ }).click();
    await page.getByRole("button", { name: /外观：浅色/ }).click();
    await page.screenshot({
      path: new URL("members-dark.png", artifacts).pathname.replace(
        /^\/(\w:)/,
        "$1",
      ),
      fullPage: false,
    });
    await checkGroupMailFlow(page);
    assert.deepEqual(pageErrors, []);
    await context.close();

    await checkHistoryLoadingAndRetry();
    await checkHistorySenderAttribution();
    await checkManualExclusionRefreshRace();
    await checkBulkHistoryResolution();
    await checkRecipientPreparationAndFailurePolicy();
    await checkMailCountdown();
    await checkMultiAccountRoundProgress();
    await checkScanAccountSwitch();
    await checkInitializationRetryEntrypoints();
    await checkSelectionScopeInteractions();
    await checkFilteredMemberRecovery();
    await checkGroupIsolationAndLayout();
    await checkSavedMailPassword();
    await checkMultiSenderAccounts();
    await checkThemePersistence();
    await checkFriendFiltering();
    await checkMailTemplates();
    await checkTemplateTransfer();
    await checkLogViewer();
    await checkFrontendDiagnostics();

    // A fresh renderer must retain events received while its initial IPC read is in flight.
    const initializationContext = await browser.newContext();
    await initializationContext.addInitScript(installFixture, {
      initializeMailRace: true,
    });
    const initializationPage = await initializationContext.newPage();
    initializationPage.on("pageerror", (error) =>
      pageErrors.push(error.message),
    );
    await initializationPage.goto("http://127.0.0.1:1420");
    await initializationPage
      .getByRole("button", { name: "写邮件", exact: true })
      .click();
    await initializationPage.waitForFunction(
      () => document.querySelector(".mail-footer .primary")?.disabled === false,
    );
    await initializationPage.locator(".mail-progress-completed").waitFor();
    assert.match(
      await initializationPage.locator(".mail-progress").innerText(),
      /初始化期间先收到的新任务状态/,
    );
    assert.deepEqual(pageErrors, []);
    await initializationContext.close();

    const plain = await browser.newPage();
    await plain.goto("http://127.0.0.1:1420");
    await plain.getByText("请在桌面程序中运行。", { exact: true }).waitFor();
    assert.equal(
      await plain.getByRole("button", { name: /获取成员/ }).count(),
      0,
    );
    await plain.close();
    console.log(
      "UI regression passed: IPC contracts, synthetic friend relations/read-failure toast/old-result preservation/unknown/overlap/legacy boundaries (not QQ HTTP evidence), sent filtering/history workspace, page navigation/return and preserved drafts/member choices, history loading/reopen/error/retry, explicit unresolved-delivery resolution/cancel, QR account switch/cancel/expiration/logout/initial response race, consistent QR wait/scanned/expired states, Sonner feedback without layout shifts and clickable above dialog scrims, image paste/picker/removal/size-and-type errors/registration failure/navigation lock/draft preservation/safe preview/CID submission, mail-template save/search/preview/apply/edit/update/save-as/management/reload and atomic failure/operation-lock/resource/scope boundaries, template light/dark and 375/780/1200 modal layouts, 100-group independent scrolling, reported and acquired counts, per-group snapshots and export scope, saved SMTP credential reuse/clear/identity binding, full supplemental email validation/multiline/brand and delimiter deduplication, row/checkbox/keyboard/text-drag selection and unresolved restrictions, cross-page member selection, frozen group audience/replacement, group payload and delivery feedback, initial/start/cancel IPC races, 375–1200 layouts, persisted page and native-window theme IPC, browser guard, compact mail confirmation/BCC/cancel (no real SMTP).",
    );
  }
} finally {
  await browser.close();
  if (production)
    await new Promise((resolve, reject) =>
      server.httpServer.close((error) => (error ? reject(error) : resolve())),
    );
  else await server.close();
}
