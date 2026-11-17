/** 本机 SMTP 写信页面；附件、发送队列和历史记录均由原生后台控制。@author ahui */
import DialogLayout from "./DialogLayout";
import WorkbenchLayout from "./WorkbenchLayout";
import WorkspaceHeader from "./WorkspaceHeader";
import {
  Bold,
  Eye,
  FileText,
  ImagePlus,
  Italic,
  List,
  LoaderCircle,
  Paperclip,
  RefreshCw,
  Save,
  Send,
  Settings2,
  SkipForward,
  Square,
  Underline,
  Undo2,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent,
} from "react";
import { call, errorText, hasDesktopBridge } from "../lib/bridge";
import { notify } from "../lib/notify";
import { readMailOptions, saveMailOptions } from "../lib/preferences";
import ConfirmationEstimate from "./ConfirmationEstimate";
import { snapshotGroupLabel } from "../lib/group-selection";
import GroupAudiencePanel from "./GroupAudiencePanel";
import MailTemplateDialog from "./MailTemplateDialog";
import SenderAccountsDialog from "./SenderAccountsDialog";
import type { Snapshot } from "../types";
import type {
  MailAttachment,
  MailInlineImage,
  GroupMailAudience,
  MailDraft,
  MailRecipientPlan,
  MailSettings,
  MailSenderAccounts,
  MailView,
  MailTemplateContent,
  MailTemplateInput,
  MailTemplateSummary,
} from "../mail-types";
import "../mail.css";

interface Props {
  snapshot: Snapshot | null;
  audience: GroupMailAudience | null;
  onChooseMembers: () => void;
  /** 定位被群范围拦截的具体成员；保持正文、附件和两种模式的收件输入。 */
  onLocateMember: (qqNumber: string) => void;
  onOpenHistory: () => void;
  onHistoryChanged: () => void;
  onRunningChange?: (running: boolean) => void;
  /** 图片、附件或模板异步操作期间固定草稿，不允许主导航改变当前编辑上下文。 */
  onEditorOperationChange?: (pending: boolean) => void;
  /** 人工维护或发送结束后递增，用于使旧的实际收件计数立即失效。 */
  recipientHistoryRevision?: number;
  /** 全局手动排除变更使旧确认失效，正文、模板资源和两种收件输入均保留。 */
  exclusionRevision?: number;
  audienceHandoffRevision?: number;
  disabled?: boolean;
}

const EMPTY_MAIL: MailView = {
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
  message: "尚未开始发送",
  failures: [],
};

/** 与宿主限制保持一致；正文图片与普通附件共享原始字节预算，发送前仍由宿主复核。 */
const MAX_INLINE_IMAGES = 10;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const MAX_CONTENT_BYTES = 18 * 1024 * 1024;
const INLINE_IMAGE_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
]);

/** 编辑用 blob 地址只在本机有效，不能进入真正的邮件 HTML。 */
interface EditorInlineImage extends MailInlineImage {
  objectUrl: string;
}

/** 将已核实的 CID 引用映射回当前资源；其他图片立即删除，不能保留模板里的任意 src。 */
function templateHtmlForEditor(
  value: MailTemplateContent,
  images: ReadonlyMap<string, EditorInlineImage | MailInlineImage>,
): string {
  const document = new DOMParser().parseFromString(value.html, "text/html");
  const byCid = new Map(
    [...images.values()].map((image) => [image.contentId, image]),
  );
  for (const element of Array.from(document.querySelectorAll("img"))) {
    const src = element.getAttribute("src") ?? "";
    const image = src.startsWith("cid:") ? byCid.get(src.slice(4)) : undefined;
    if (!image) {
      element.remove();
      continue;
    }
    for (const attribute of Array.from(element.attributes))
      element.removeAttribute(attribute.name);
    element.setAttribute("data-inline-image-id", image.id);
    element.setAttribute(
      "src",
      "objectUrl" in image && typeof image.objectUrl === "string"
        ? image.objectUrl
        : `data:${image.mimeType};base64,${image.dataBase64}`,
    );
    element.setAttribute("alt", image.name);
  }
  return document.body.innerHTML;
}

/** 沙箱只加载登记图片字节；模板和当前正文共用同一预览排版与禁止联网的 CSP。 */
function mailPreviewDocument(cleanHtml: string): string {
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; form-action 'none'"><style>body{font:15px/1.7 system-ui,sans-serif;color:#202938;background:#fff;padding:24px;overflow-wrap:anywhere}p{margin:0 0 1em}li{margin:.25em 0}img{max-width:100%;height:auto;vertical-align:middle}</style></head><body>${cleanHtml}</body></html>`;
}

/**
 * 解析收件输入，仅用于本机预览和确认数量；原生后台仍必须独立校验。
 * 群成员的 QQ 邮箱由名单单独生成；补充输入要求完整邮箱，避免把不完整地址猜成 QQ 邮箱。
 * @param value 用户输入的完整邮箱，推荐每行一个，也兼容常见中文/英文分隔符。
 * @returns 去重后的候选邮箱和未识别项，不自动丢弃错误输入。
 */
export function parseRecipientInput(value: string): {
  addresses: string[];
  invalid: string[];
} {
  const addresses = new Set<string>();
  const invalid: string[] = [];
  for (const original of value.split(/[\s,;，；、]+/u).filter(Boolean)) {
    const separator = original.lastIndexOf("@");
    const address =
      separator > 0
        ? `${original.slice(0, separator)}@${original.slice(separator + 1).toLocaleLowerCase()}`
        : original;
    if (/^[^\s<>@]+@[^\s<>@]+\.[^\s<>@]+$/.test(address)) {
      addresses.add(address);
    } else {
      invalid.push(original);
    }
  }
  return { addresses: [...addresses], invalid };
}

/**
 * 只移除明确指定的输入项及其重复项，保留其他原文、分隔符和未完成地址，避免纠错时重写整份输入。
 * @param value 当前模式的手工输入；不含不可编辑的群成员范围。
 * @param targets 要移除的标准邮箱或原始无效项，与解析器使用同一套分隔和域名归一化规则。
 * @returns 移除后的输入，仅裁去首尾空白；剩余错误项供用户继续修正。
 */
function removeRecipientTokens(
  value: string,
  targets: readonly string[],
): string {
  const removing = new Set(targets);
  return value
    .replace(/[^\s,;，；、]+/gu, (token) => {
      const parsed = parseRecipientInput(token);
      return removing.has(parsed.addresses[0] ?? token) ? "" : token;
    })
    .trim();
}

/**
 * 只保留基础格式和当前草稿登记过的正文图片；脚本、外链和任意 HTML 图片都被移除。
 * @param html 编辑器生成的 HTML；不能将未清理内容放入预览或 IPC。
 * @param images 当前草稿已由宿主验证的图片；不提供时移除全部图片，兼容纯文本调用方。
 * @param mode send 使用 CID 引用，preview 仅使用登记字节生成本机 data 预览。
 * @returns 受限 HTML，Rust 后台会使用自己的清理器再次处理。
 */
export function sanitizeMailHtml(
  html: string,
  images: ReadonlyMap<string, MailInlineImage> = new Map(),
  mode: "send" | "preview" = "send",
): string {
  const document = new DOMParser().parseFromString(html, "text/html");
  const allowed = new Set([
    "P",
    "DIV",
    "BR",
    "STRONG",
    "B",
    "EM",
    "I",
    "U",
    "UL",
    "OL",
    "LI",
    "BLOCKQUOTE",
    "SPAN",
  ]);
  const blocked = new Set([
    "SCRIPT",
    "STYLE",
    "IFRAME",
    "OBJECT",
    "EMBED",
    "SVG",
    "MATH",
    "VIDEO",
    "AUDIO",
    "FORM",
    "INPUT",
    "LINK",
    "META",
    "BASE",
  ]);
  const clean = (parent: Element) => {
    for (const element of Array.from(parent.children)) {
      if (element.tagName === "IMG") {
        const image = images.get(
          element.getAttribute("data-inline-image-id") ?? "",
        );
        if (!image || !INLINE_IMAGE_TYPES.has(image.mimeType)) {
          element.remove();
          continue;
        }
        // 从可信登记重新构造属性，不沿用编辑区的 src、事件或样式。
        for (const attribute of Array.from(element.attributes))
          element.removeAttribute(attribute.name);
        element.setAttribute(
          "src",
          mode === "preview"
            ? `data:${image.mimeType};base64,${image.dataBase64}`
            : `cid:${image.contentId}`,
        );
        element.setAttribute("alt", image.name);
        continue;
      }
      if (blocked.has(element.tagName)) {
        element.remove();
        continue;
      }
      clean(element);
      if (!allowed.has(element.tagName)) {
        element.replaceWith(...Array.from(element.childNodes));
        continue;
      }
      for (const attribute of Array.from(element.attributes)) {
        element.removeAttribute(attribute.name);
      }
    }
  };
  clean(document.body);
  return document.body.innerHTML;
}

/** 格式化原生返回的附件大小；超限判断仍由后台按实际读取的文件内容执行。 */
function formatBytes(size: number): string {
  return size < 1024 * 1024
    ? `${Math.max(1, Math.round(size / 1024))} KB`
    : `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * 事件和命令响应可能乱序。宿主版本号先拦截旧阶段（即使人数相同），计数只能前进，
 * 首次观察到终态后不能再接受迟到的 running 或其他终态。旧状态缺少版本号按 0 兼容。
 * @param previous 已显示的最后状态。
 * @param incoming 新收到的事件、初态或命令返回状态。
 * @returns true 表示可以替换页面状态，false 表示这是一条迟到的旧状态。
 */
export function shouldApplyMailState(
  previous: MailView,
  incoming: MailView,
): boolean {
  const previousRevision = previous.stateRevision ?? 0;
  const incomingRevision = incoming.stateRevision ?? 0;
  // 新宿主每次发布严格递增，重复版本也不能更换阶段；未带版本的旧状态仅在尚无新版本时兼容。
  if (previousRevision > 0 && incomingRevision <= previousRevision)
    return false;
  if (!previous.id || previous.id !== incoming.id) return true;
  const terminal = previous.status !== "idle" && previous.status !== "running";
  if (terminal && incoming.status !== previous.status) return false;
  return (
    incoming.sent >= previous.sent &&
    incoming.failed >= previous.failed &&
    incoming.skipped >= previous.skipped &&
    incoming.total >= previous.total
  );
}

/**
 * 两种时间展示共用的本地时钟；只更新子组件，不触发计划失效、历史刷新或每秒IPC读取。
 * 切页和重新挂载仍依据宿主截止时间，窗口恢复前台时立即校准；卸载会清理计时器及监听器。
 * @param deadline 宿主返回且已校验的UTC Unix毫秒期限，用于期限变化时立即校准。
 * @returns 本机当前UTC Unix毫秒；实际调度仍使用宿主的单调时钟。
 */
function useDisplayClock(deadline: number) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const updateClock = () => setNow(Date.now());
    updateClock();
    const timer = window.setInterval(updateClock, 1000);
    // 窗口后台时计时器可能被节流，回到前台立即按原截止时间校准，不能把等待重新计满。
    window.addEventListener("focus", updateClock);
    document.addEventListener("visibilitychange", updateClock);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", updateClock);
      document.removeEventListener("visibilitychange", updateClock);
    };
  }, [deadline]);
  return now;
}

/** 最近可发送期限倒计时；仅展示宿主期限，不发起每秒IPC或改变实际发送时机。 */
function BatchCountdown({ deadline }: { deadline: number }) {
  const now = useDisplayClock(deadline);
  const seconds = Math.max(0, Math.ceil((deadline - now) / 1000));
  const remaining = `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  // 0 只说明本机时钟已到最近可发送期限，实际发信仍由宿主决定，不能冒充下一批已经发送。
  return (
    <p className="mail-countdown" aria-label="发送间隔倒计时" aria-live="off">
      {seconds > 0 ? (
        <>
          距离下一批可发送还有 <span>{remaining}</span>
        </>
      ) : (
        "正在准备下一批"
      )}
    </p>
  );
}

/** 预计队列处理完的本机日期时间；超时仍在执行时明确更新中，不把过期预计时间冒充完成结果。 */
function CompletionEstimate({ deadline }: { deadline: number }) {
  const now = useDisplayClock(deadline);
  const date = new Date(deadline);
  const pad = (value: number) => String(value).padStart(2, "0");
  const formatted = `${date.getFullYear()}年${pad(date.getMonth() + 1)}月${pad(date.getDate())}日${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return (
    <p
      className="mail-completion-estimate"
      aria-label="预计完成时间"
      aria-live="off"
    >
      {now < deadline ? (
        <>
          预计完成时间：<time dateTime={date.toISOString()}>{formatted}</time>
          <span>（本机时间，随实际耗时更新）</span>
        </>
      ) : (
        <>预计完成时间：正在更新，当前批次耗时超出估算。</>
      )}
    </p>
  );
}

/**
 * 编辑和发送分别由明确操作触发，切换主导航保留草稿；不会在渲染、预览或导入名单时发送。
 * 只有用户在确认摘要中点击发送，才调用 start_mail_job。
 */
export default function MailComposer({
  snapshot,
  audience: providedAudience,
  onChooseMembers,
  onLocateMember,
  onOpenHistory,
  onHistoryChanged,
  onRunningChange,
  onEditorOperationChange,
  recipientHistoryRevision = 0,
  exclusionRevision = 0,
  audienceHandoffRevision = 0,
  disabled = false,
}: Props) {
  const [recipients, setRecipients] = useState("");
  const [quickRecipients, setQuickRecipients] = useState("");
  const [mode, setMode] = useState<"quick" | "group">(
    providedAudience ? "group" : "quick",
  );
  const audience = mode === "group" ? providedAudience : null;
  const activeRecipients = mode === "quick" ? quickRecipients : recipients;
  const [subject, setSubject] = useState("");
  const [html, setHtml] = useState("");
  const [previewDocument, setPreviewDocument] = useState("");
  const [attachments, setAttachments] = useState<MailAttachment[]>([]);
  const [accountCatalog, setAccountCatalog] = useState<MailSenderAccounts>({
    revision: 0,
    accounts: [],
  });
  const accountCatalogRef = useRef(accountCatalog);
  const [selectedAccountIds, setSelectedAccountIds] = useState<string[]>([]);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [accountEditorDirty, setAccountEditorDirty] = useState(false);
  const [accountsReadError, setAccountsReadError] = useState(false);
  const [accountsLoading, setAccountsLoading] = useState(hasDesktopBridge);
  const accountsReadPending = useRef(false);
  const [settingsReady, setSettingsReady] = useState(!hasDesktopBridge());
  const [mail, setMail] = useState<MailView>(EMPTY_MAIL);
  const [savedOptions] = useState(readMailOptions);
  const [skipSent, setSkipSent] = useState(savedOptions.skipSent);
  const [continueOnFailure, setContinueOnFailure] = useState(
    savedOptions.continueOnFailure,
  );
  const [deliverySettingsOpen, setDeliverySettingsOpen] = useState(
    savedOptions.open,
  );
  const optionsStorageWarning = useRef(false);
  const [recipientPlan, setRecipientPlan] = useState<{
    value: MailRecipientPlan;
    key: string;
    expiresAt: number;
  } | null>(null);
  const [sendMode, setSendMode] = useState(savedOptions.sendMode);
  const [batchSize, setBatchSize] = useState(savedOptions.batchSize);
  const [intervalSeconds, setIntervalSeconds] = useState(
    savedOptions.intervalSeconds,
  );
  // 保留空输入供用户重新填写；不能用 Number("") 静默解释为 0，关闭切换间隔必须明确输入 0。
  const [accountSwitchIntervalInput, setAccountSwitchIntervalInput] = useState(
    String(savedOptions.accountSwitchIntervalSeconds),
  );
  const accountSwitchIntervalSeconds = Number(accountSwitchIntervalInput);
  // 每个字段独立校验保存；切换间隔清空时用 NaN 保留上次值，不能把空输入保存为关闭间隔。
  const optionsFingerprint = JSON.stringify([
    skipSent,
    continueOnFailure,
    sendMode,
    batchSize,
    intervalSeconds,
    accountSwitchIntervalInput,
    deliverySettingsOpen,
  ]);
  const lastOptionsFingerprint = useRef(optionsFingerprint);
  useEffect(() => {
    if (lastOptionsFingerprint.current === optionsFingerprint) return;
    lastOptionsFingerprint.current = optionsFingerprint;
    if (
      !saveMailOptions({
        skipSent,
        continueOnFailure,
        sendMode,
        batchSize,
        intervalSeconds,
        accountSwitchIntervalSeconds: accountSwitchIntervalInput.trim()
          ? accountSwitchIntervalSeconds
          : Number.NaN,
        open: deliverySettingsOpen,
      }) &&
      !optionsStorageWarning.current
    ) {
      optionsStorageWarning.current = true;
      notify({
        message: "选项已应用，但未能保存，下次启动可能需要重新设置。",
        tone: "error",
      });
    }
  }, [
    optionsFingerprint,
    skipSent,
    continueOnFailure,
    sendMode,
    batchSize,
    intervalSeconds,
    accountSwitchIntervalInput,
    accountSwitchIntervalSeconds,
    deliverySettingsOpen,
  ]);
  const [confirmationOpen, setConfirmationOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [imagePending, setImagePending] = useState(false);
  const [templatePending, setTemplatePending] = useState(false);
  const [templateOpen, setTemplateOpen] = useState(false);
  const [templateContext, setTemplateContext] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [newTemplateMode, setNewTemplateMode] = useState(false);
  const [saveTemplateName, setSaveTemplateName] = useState("");
  const [saveTemplateUpdateId, setSaveTemplateUpdateId] = useState<
    string | null
  >(null);
  const [includeTemplateAttachments, setIncludeTemplateAttachments] =
    useState(false);
  const [replaceTemplate, setReplaceTemplate] = useState<{
    value: MailTemplateContent | null;
    intent: "apply" | "edit" | "new";
  } | null>(null);
  // 更换整个编辑节点可清空浏览器撤销栈，避免 Ctrl+Z 恢复已被宿主释放的旧资源。
  const [editorRevision, setEditorRevision] = useState(0);
  const replacementHtml = useRef<string | null>(null);
  const [mailReady, setMailReady] = useState(!hasDesktopBridge());
  const [mailStateError, setMailStateError] = useState(false);
  const [mailStateLoading, setMailStateLoading] = useState(hasDesktopBridge);
  const [mailStateReadRevision, setMailStateReadRevision] = useState(0);
  const mailStatePending = useRef(false);
  // 单次等待操作独立加锁，不能阻止用户在跳过响应到达前点击停止。
  const skipWaitPending = useRef(false);
  const [skippingWait, setSkippingWait] = useState(false);
  const editor = useRef<HTMLDivElement>(null);
  const recipientsInput = useRef<HTMLTextAreaElement>(null);
  const previewDialog = useRef<HTMLDialogElement>(null);
  const confirmDialog = useRef<HTMLDialogElement>(null);
  const saveTemplateDialog = useRef<HTMLDialogElement>(null);
  const replaceTemplateDialog = useRef<HTMLDialogElement>(null);
  const preparedDraft = useRef<MailDraft | null>(null);
  // 与宿主五分钟计划寿命一致；到期撤回确认，避免仍显示可发送的过期人数和结束时间。
  useEffect(() => {
    if (!confirmationOpen || !recipientPlan) return;
    const timer = setTimeout(
      () => {
        preparedDraft.current = null;
        confirmDialog.current?.close();
        setConfirmationOpen(false);
        setRecipientPlan(null);
        notify({
          message: "发送确认已过期，请重新点击发送确认人数。",
          tone: "error",
        });
      },
      Math.max(0, recipientPlan.expiresAt - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [confirmationOpen, recipientPlan]);
  const callbacks = useRef({
    onHistoryChanged,
    onRunningChange,
    onEditorOperationChange,
  });
  callbacks.current = {
    onHistoryChanged,
    onRunningChange,
    onEditorOperationChange,
  };
  const lastMail = useRef(EMPTY_MAIL);
  const mailRevision = useRef(0);
  const submissionPending = useRef(false);
  // 停止任务独立使用同步门闩；账号维护复用草稿异步门闩，避免与预检并行。
  const stopOperationPending = useRef(false);
  const reportedFinishedJob = useRef("");
  const imageRegistry = useRef(new Map<string, EditorInlineImage>());
  const editorSelection = useRef<Range | null>(null);
  // 同步门闩覆盖 React 下一次渲染前的间隙；图片、附件和模板不能并发更换宿主草稿资源。
  const imageOperationPending = useRef(false);
  const disposed = useRef(false);
  // Group addresses are derived from the confirmed member range. The editable
  // field holds supplements only, so correcting text cannot silently remove members.
  const manualParsed = parseRecipientInput(activeRecipients);
  const groupAddresses = [...new Set(audience?.qqNumbers ?? [])].map(
    (qq) => `${qq}@qq.com`,
  );
  const groupAddressSet = new Set(groupAddresses);
  const validGroupNumbers = new Set(
    audience?.snapshot.members.map((member) => member.qqNumber) ?? [],
  );
  const parsed = {
    addresses: [...new Set([...groupAddresses, ...manualParsed.addresses])],
    invalid: manualParsed.invalid,
  };
  const extraCount = manualParsed.addresses.filter(
    (address) => !groupAddressSet.has(address),
  ).length;
  // Supplements must not reintroduce excluded or unchecked source members. The
  // native host independently checks the same rule against its saved snapshot.
  const blockedAddresses = new Set(
    audience
      ? [
          ...audience.snapshot.members,
          ...audience.snapshot.filteredMembers.map((entry) => entry.member),
        ]
          .map((member) => `${member.qqNumber}@qq.com`)
          .filter((address) => !groupAddressSet.has(address))
      : [],
  );
  const blockedSupplements = manualParsed.addresses.filter((address) =>
    blockedAddresses.has(address),
  );
  const filteredSupplementReasons = new Map(
    audience?.snapshot.filteredMembers.map((item) => [
      `${item.member.qqNumber}@qq.com`,
      item,
    ]) ?? [],
  );

  /** 两种模式各自维护输入；纠错不能改写已绑定群成员，也不能清空另一模式的草稿。 */
  function removeRecipientIssues(addresses: readonly string[]) {
    if (locked) return;
    const update = (value: string) => removeRecipientTokens(value, addresses);
    if (mode === "quick") setQuickRecipients(update);
    else setRecipients(update);
    recipientsInput.current?.focus();
  }
  const running = mail.status === "running";
  const waiting = running && mail.phase === "waiting";
  const stopping = running && mail.phase === "stopping";
  const nextBatchAt =
    waiting &&
    typeof mail.nextBatchAt === "number" &&
    Number.isFinite(mail.nextBatchAt) &&
    mail.nextBatchAt > 0
      ? mail.nextBatchAt
      : null;
  // 确认计数只匹配本次候选范围、开关和历史版本；任何任务进展都使旧计划失效。
  const recipientPlanKey = JSON.stringify({
    rawRecipients: activeRecipients,
    mode,
    exclusions: exclusionRevision,
    recipients: parsed.addresses.join(";"),
    skipSent,
    continueOnFailure,
    sendMode,
    batchSize,
    intervalSeconds,
    accountSwitchIntervalInput,
    accounts: accountCatalog.revision,
    accountIds: accountCatalog.accounts
      .filter((account) => selectedAccountIds.includes(account.id))
      .map((account) => account.id),
    group: audience
      ? {
          snapshotId: audience.snapshot.id,
          qqNumbers: [...new Set(audience.qqNumbers)],
        }
      : null,
    history: recipientHistoryRevision,
    task: [
      mail.id,
      mail.status,
      mail.total,
      mail.sent,
      mail.failed,
      mail.skipped,
    ],
  });
  const latestRecipientPlanKey = useRef(recipientPlanKey);
  latestRecipientPlanKey.current = recipientPlanKey;
  const activeRecipientPlan =
    recipientPlan?.key === recipientPlanKey &&
    recipientPlan.expiresAt > Date.now()
      ? recipientPlan.value
      : null;
  const selectedAccounts = accountCatalog.accounts.filter((account) =>
    selectedAccountIds.includes(account.id),
  );
  const estimatedRecipientCount =
    activeRecipientPlan?.total ?? parsed.addresses.length;
  // 均分以后每个账号独立密送分批，不能直接对所有收件人的总数取整。
  const estimatedBatches =
    activeRecipientPlan?.totalMessageCount ??
    selectedAccounts.reduce((sum, _account, index) => {
      const count =
        Math.floor(estimatedRecipientCount / selectedAccounts.length) +
        (index < estimatedRecipientCount % selectedAccounts.length ? 1 : 0);
      return (
        sum +
        (sendMode === "individual"
          ? count
          : Math.ceil(count / Math.max(1, batchSize)))
      );
    }, 0);
  const locked = busy || running || disabled || !mailReady || !settingsReady;
  const activeInlineImages = currentInlineImages(html);

  useEffect(() => {
    preparedDraft.current = null;
    confirmDialog.current?.close();
  }, [recipientPlanKey]);

  useEffect(() => {
    if (!recipientPlan) return;
    const timer = setTimeout(
      () => {
        setRecipientPlan(null);
        preparedDraft.current = null;
        if (confirmDialog.current?.open) {
          confirmDialog.current.close();
          notify({
            message: "发送确认已过期，请重新确认收件人数。",
            tone: "error",
          });
        }
      },
      Math.max(0, recipientPlan.expiresAt - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [recipientPlan]);

  useLayoutEffect(() => {
    if (replacementHtml.current !== null && editor.current) {
      editor.current.innerHTML = replacementHtml.current;
      replacementHtml.current = null;
      editorSelection.current = null;
    }
  }, [editorRevision]);

  /** 模板、附件和收件预检共用草稿门闩；失败交给发起处提示，不吞掉异常或更换内容。 */
  async function performTemplateOperation<T>(
    operation: () => Promise<T>,
  ): Promise<T> {
    if (locked || imageOperationPending.current || submissionPending.current)
      throw new Error("请等待当前操作完成后重试。");
    imageOperationPending.current = true;
    callbacks.current.onEditorOperationChange?.(true);
    setBusy(true);
    setTemplatePending(true);
    try {
      return await operation();
    } finally {
      imageOperationPending.current = false;
      if (!disposed.current) {
        callbacks.current.onEditorOperationChange?.(false);
        setTemplatePending(false);
        setBusy(false);
      }
    }
  }

  /** 预览不创建 blob 或登记宿主资源，只展开已通过后台校验的字节。 */
  function previewTemplate(value: MailTemplateContent): string {
    const images = new Map(
      value.inlineImages.map((image) => [image.id, image]),
    );
    return mailPreviewDocument(
      sanitizeMailHtml(templateHtmlForEditor(value, images), images, "preview"),
    );
  }

  /**
   * 只计算仍在正文中的登记图片；删除后的资源保留到草稿卸载，允许原生撤销恢复。
   * 相同资源重复出现只占一份邮件附件字节，发送时 CID 引用仍可使用多次。
   */
  function currentInlineImages(source: string): EditorInlineImage[] {
    const parsedDocument = new DOMParser().parseFromString(source, "text/html");
    const ids = new Set(
      Array.from(
        parsedDocument.querySelectorAll("img[data-inline-image-id]"),
      ).map((element) => element.getAttribute("data-inline-image-id") ?? ""),
    );
    return [...ids].flatMap((id) => {
      const image = imageRegistry.current.get(id);
      return image ? [image] : [];
    });
  }

  useEffect(() => {
    disposed.current = false;
    return () => {
      disposed.current = true;
      // Blob 和宿主缓存仅属于当前草稿；切换同级页面不会卸载写信区，因此仍保留草稿。
      for (const image of imageRegistry.current.values()) {
        URL.revokeObjectURL(image.objectUrl);
        void call("mail_remove_inline_image", { id: image.id }).catch(() => {});
      }
      imageRegistry.current.clear();
    };
  }, []);

  /** Every status source shares this gate; a delayed command return cannot
   * resurrect a finished queue or reduce already confirmed recipient counts. */
  function applyMailState(value: MailView) {
    const previous = lastMail.current;
    if (!shouldApplyMailState(previous, value)) return;
    mailRevision.current += 1;
    lastMail.current = value;
    setMail(value);
    callbacks.current.onRunningChange?.(
      submissionPending.current || value.status === "running",
    );
    const terminal = value.status !== "idle" && value.status !== "running";
    if (
      terminal &&
      value.id &&
      reportedFinishedJob.current !== value.id &&
      (previous.status === "running" || submissionPending.current)
    ) {
      reportedFinishedJob.current = value.id;
      callbacks.current.onHistoryChanged();
    }
  }

  useEffect(() => {
    if (!hasDesktopBridge()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let receivedStateEvent = false;
    mailStatePending.current = true;
    setMailReady(false);
    setMailStateLoading(true);
    // 先订阅再读取；重试只恢复状态通道，不重读设置、不改草稿或取消已有任务。
    // 读取期间收到事件或开始提交后，迟到初态不能覆盖新状态。
    void (async () => {
      try {
        const cleanup = await listen<MailView>("mail-state", (event) => {
          if (!disposed) {
            receivedStateEvent = true;
            applyMailState(event.payload);
            setMailStateError(false);
            setMailReady(true);
          }
        });
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const revision = mailRevision.current;
        const initial = await call<MailView>("get_mail_state");
        if (!disposed) {
          if (revision === mailRevision.current && !submissionPending.current)
            applyMailState(initial);
          setMailStateError(false);
          setMailReady(true);
        }
      } catch {
        if (!disposed && !receivedStateEvent) {
          // 订阅成功但初读失败也须释放该监听；手动重试重新建立唯一通道，不能累积重复订阅。
          unlisten?.();
          unlisten = undefined;
          setMailStateError(true);
          setMailReady(false);
        }
      } finally {
        if (!disposed) {
          mailStatePending.current = false;
          setMailStateLoading(false);
        }
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [mailStateReadRevision]);

  /** 应用完整权威目录，保留仍存在的选择；首次读取及新增账号默认加入本次发送。 */
  function applyAccountCatalog(value: MailSenderAccounts) {
    const previousIds = new Set(
      accountCatalogRef.current.accounts.map((account) => account.id),
    );
    accountCatalogRef.current = value;
    setAccountCatalog(value);
    setSelectedAccountIds((previous) =>
      value.accounts
        .filter(
          (account) =>
            previous.includes(account.id) || !previousIds.has(account.id),
        )
        .map((account) => account.id),
    );
  }

  /** 打开维护即撤销旧确认；编辑账号不会改动收件人、正文或任何模板资源。 */
  function openAccountSettings() {
    if (imageOperationPending.current || submissionPending.current || running)
      return;
    preparedDraft.current = null;
    setRecipientPlan(null);
    confirmDialog.current?.close();
    setSettingsOpen(true);
  }

  /** 读取失败关闭发送入口并允许显式重试，不能把读取失败解释成“没有账号”。 */
  async function loadAccounts() {
    if (accountsReadPending.current || disposed.current) return;
    accountsReadPending.current = true;
    setAccountsLoading(true);
    setAccountsReadError(false);
    setSettingsReady(false);
    try {
      const value = await call<MailSenderAccounts>("mail_accounts");
      if (!disposed.current) {
        applyAccountCatalog(value);
        setSettingsReady(true);
      }
    } catch (error) {
      if (!disposed.current) {
        setAccountsReadError(true);
        notify({ message: errorText(error), tone: "error" });
      }
    } finally {
      accountsReadPending.current = false;
      if (!disposed.current) setAccountsLoading(false);
    }
  }

  useEffect(() => {
    if (hasDesktopBridge()) void loadAccounts();
  }, []);

  useEffect(() => {
    // An explicit member handoff invalidates an old confirmation but preserves
    // the actual draft, attachments and supplementary addresses.
    preparedDraft.current = null;
    confirmDialog.current?.close();
  }, [audience]);

  // 只有成员工作区的明确handoff才切回群模式；普通切群或后台状态刷新不会污染快捷输入。
  useEffect(() => {
    if (providedAudience) setMode("group");
  }, [audienceHandoffRevision]);

  /** 保留光标选择后执行基础格式；WebView2 原生编辑命令同时提供撤销历史。 */
  function format(
    command: "bold" | "italic" | "underline" | "insertUnorderedList" | "undo",
  ) {
    if (
      locked ||
      imageOperationPending.current ||
      submissionPending.current ||
      !editor.current
    )
      return;
    // 优先使用仍有效的实时正文选区；键盘移入工具栏后才回退到记住的选区，不采用其他输入框的选区。
    const selection = window.getSelection();
    const liveRange = selection?.rangeCount ? selection.getRangeAt(0) : null;
    const range =
      liveRange && editor.current.contains(liveRange.commonAncestorContainer)
        ? liveRange.cloneRange()
        : editorSelection.current;
    editor.current.focus();
    if (range && editor.current.contains(range.commonAncestorContainer)) {
      selection?.removeAllRanges();
      selection?.addRange(range);
    }
    document.execCommand("styleWithCSS", false, "false");
    document.execCommand(command);
    setHtml(editor.current.innerHTML);
    rememberEditorSelection();
  }

  /** 记住编辑区光标；打开系统文件选择器和异步登记图片后仍插入到原来位置。 */
  function rememberEditorSelection() {
    const selection = window.getSelection();
    if (!editor.current || !selection?.rangeCount) return;
    const range = selection.getRangeAt(0);
    if (editor.current.contains(range.commonAncestorContainer))
      editorSelection.current = range.cloneRange();
  }

  /**
   * 先完整校验本次选择，再修改正文，避免多图部分插入。宿主会再次验证真实格式和大小。
   * 删除正文图片不删除缓存；只有失败的新增资源和整个草稿卸载才释放宿主句柄。
   */
  async function insertInlineImages(load: () => Promise<MailInlineImage[]>) {
    if (
      locked ||
      imageOperationPending.current ||
      submissionPending.current ||
      !editor.current
    )
      return;
    rememberEditorSelection();
    imageOperationPending.current = true;
    callbacks.current.onEditorOperationChange?.(true);
    setImagePending(true);
    setBusy(true);
    let selected: MailInlineImage[] = [];
    const created: EditorInlineImage[] = [];
    let inserted = false;
    try {
      selected = await load();
      if (disposed.current || !selected.length) return;
      const current = currentInlineImages(editor.current?.innerHTML ?? "");
      if (current.length + selected.length > MAX_INLINE_IMAGES)
        throw new Error("正文最多插入 10 张图片，请先移除部分图片后重试。");
      if (
        selected.some(
          (image) =>
            !INLINE_IMAGE_TYPES.has(image.mimeType) ||
            !image.id ||
            !image.contentId ||
            image.size <= 0 ||
            image.size > MAX_IMAGE_BYTES,
        )
      )
        throw new Error(
          "仅支持 PNG、JPEG、GIF、WebP 图片，单张不能超过 5 MiB。",
        );
      const totalBytes = [...attachments, ...current, ...selected].reduce(
        (sum, item) => sum + item.size,
        0,
      );
      if (totalBytes > MAX_CONTENT_BYTES)
        throw new Error(
          "正文图片与附件合计不能超过 18 MiB，请先移除部分内容。",
        );
      for (const image of selected) {
        const bytes = Uint8Array.from(atob(image.dataBase64), (character) =>
          character.charCodeAt(0),
        );
        created.push({
          ...image,
          objectUrl: URL.createObjectURL(
            new Blob([bytes], { type: image.mimeType }),
          ),
        });
      }
      const root = editor.current;
      if (!root) return;
      const fragment = document.createElement("div");
      for (const image of created) {
        const element = document.createElement("img");
        element.src = image.objectUrl;
        element.alt = image.name;
        element.setAttribute("data-inline-image-id", image.id);
        fragment.append(element);
      }
      // 登记期间锁定编辑；仅在同步插入时临时恢复编辑属性，让 WebView2 记录一次可撤销操作。
      const editable = root.contentEditable;
      root.contentEditable = "true";
      try {
        root.focus();
        const range = editorSelection.current;
        const selection = window.getSelection();
        if (selection) {
          selection.removeAllRanges();
          if (range && root.contains(range.commonAncestorContainer)) {
            selection.addRange(range);
          } else {
            const end = document.createRange();
            end.selectNodeContents(root);
            end.collapse(false);
            selection.addRange(end);
          }
        }
        if (!document.execCommand("insertHTML", false, fragment.innerHTML))
          throw new Error("图片未能插入，请点击正文中的插入位置后重试。");
      } finally {
        root.contentEditable = editable;
      }
      for (const image of created) imageRegistry.current.set(image.id, image);
      inserted = true;
      setHtml(root.innerHTML);
      rememberEditorSelection();
    } catch (error) {
      if (!disposed.current)
        notify({ message: errorText(error), tone: "error" });
    } finally {
      if (!inserted) {
        for (const image of created) URL.revokeObjectURL(image.objectUrl);
        for (const image of selected)
          void call("mail_remove_inline_image", { id: image.id }).catch(
            () => {},
          );
      }
      imageOperationPending.current = false;
      if (!disposed.current) {
        callbacks.current.onEditorOperationChange?.(false);
        setImagePending(false);
        setBusy(false);
      }
    }
  }

  /** 文件选择器仅返回宿主验证后的图片；取消选择不改变正文或当前提示。 */
  function pickInlineImages() {
    void insertInlineImages(() =>
      call<MailInlineImage[]>("mail_pick_inline_images"),
    );
  }

  /**
   * 截图优先按图片导入；其他粘贴仅保留纯文本，不能借 HTML 引入外部图片、脚本或跟踪资源。
   * 多图粘贴先验证所有文件，再逐一登记；登记失败会释放此前本次新增的宿主句柄。
   */
  function pasteEditor(event: ClipboardEvent<HTMLDivElement>) {
    event.preventDefault();
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    const files = Array.from(event.clipboardData.files);
    const images = files.filter((file) => file.type.startsWith("image/"));
    if (images.length) {
      void insertInlineImages(async () => {
        if (
          images.length > MAX_INLINE_IMAGES ||
          images.some(
            (file) =>
              !INLINE_IMAGE_TYPES.has(file.type) ||
              !file.size ||
              file.size > MAX_IMAGE_BYTES,
          )
        )
          throw new Error(
            "仅支持 PNG、JPEG、GIF、WebP 图片，单张不超过 5 MiB，正文最多 10 张。",
          );
        const registered: MailInlineImage[] = [];
        try {
          for (const file of images) {
            const base64 = await new Promise<string>((resolve, reject) => {
              const reader = new FileReader();
              reader.onload = () =>
                resolve(String(reader.result).split(",", 2)[1] ?? "");
              reader.onerror = () =>
                reject(new Error("读取剪贴板图片失败，请重新复制后重试。"));
              reader.onabort = () =>
                reject(new Error("读取剪贴板图片已中断，请重试。"));
              reader.readAsDataURL(file);
            });
            registered.push(
              await call<MailInlineImage>("mail_add_inline_image", {
                base64,
                mimeType: file.type,
              }),
            );
          }
          return registered;
        } catch (error) {
          for (const image of registered)
            void call("mail_remove_inline_image", { id: image.id }).catch(
              () => {},
            );
          throw error;
        }
      });
      return;
    }
    document.execCommand(
      "insertText",
      false,
      event.clipboardData.getData("text/plain"),
    );
    setHtml(editor.current?.innerHTML ?? "");
  }

  /** 删除所有同 CID 引用，保留资源支持撤销；使用原生编辑命令维持同一套撤销交互。 */
  function removeInlineImage(id: string) {
    if (
      locked ||
      imageOperationPending.current ||
      submissionPending.current ||
      !editor.current
    )
      return;
    const root = editor.current;
    const elements = Array.from(
      root.querySelectorAll("img[data-inline-image-id]"),
    ).filter((element) => element.getAttribute("data-inline-image-id") === id);
    root.focus();
    for (const element of elements) {
      const range = document.createRange();
      range.selectNode(element);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      document.execCommand("delete");
    }
    setHtml(root.innerHTML);
    rememberEditorSelection();
  }

  /** 附件只通过本机选择器加入，页面不接受任意文件路径或拖放路径。 */
  async function pickAttachments() {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    let selected: MailAttachment[] = [];
    try {
      await performTemplateOperation(async () => {
        selected = await call<MailAttachment[]>("mail_pick_attachments");
        const images = currentInlineImages(editor.current?.innerHTML ?? html);
        const total = [...attachments, ...selected, ...images].reduce(
          (sum, item) => sum + item.size,
          0,
        );
        if (total > MAX_CONTENT_BYTES) {
          // 选择器已登记的本次附件需回收；原有附件和正文保持不变。
          await Promise.all(
            selected.map((item) =>
              call("mail_remove_attachment", { id: item.id }),
            ),
          );
          throw new Error(
            "正文图片与附件合计不能超过 18 MiB，请先移除部分内容。",
          );
        }
        setAttachments((previous) => [
          ...new Map(
            [...previous, ...selected].map((item) => [item.id, item]),
          ).values(),
        ]);
      });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    }
  }

  /** 删除后台登记的附件句柄后再更新页面，避免显示已删而后台仍发送该附件。 */
  async function removeAttachment(id: string) {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    try {
      await performTemplateOperation(async () => {
        await call("mail_remove_attachment", { id });
        setAttachments((previous) => previous.filter((item) => item.id !== id));
      });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    }
  }

  /**
   * 账号维护与草稿资源共用门闩；任何维护尝试撤销旧确认，失败不影响草稿和选中范围。
   * 凭据清理的部分失败可能已提交目录，需读取权威状态后才能继续发送，避免显示旧配置。
   */
  async function performAccountOperation<T>(
    operation: () => Promise<T>,
  ): Promise<T> {
    return performTemplateOperation(async () => {
      preparedDraft.current = null;
      setRecipientPlan(null);
      confirmDialog.current?.close();
      try {
        return await operation();
      } catch (error) {
        const code =
          error && typeof error === "object" && "code" in error
            ? error.code
            : "";
        if (
          typeof code === "string" &&
          code.startsWith("sender_account_partial_")
        ) {
          try {
            const value = await call<MailSenderAccounts>("mail_accounts");
            if (!disposed.current) applyAccountCatalog(value);
          } catch {
            if (!disposed.current) {
              setSettingsReady(false);
              setAccountsReadError(true);
            }
          }
        }
        throw error;
      }
    });
  }

  /**
   * 先验证内容，再由宿主读取全库已发送记录形成固定计划；每次确认都重新准备，不复用旧人数。
   * 此步骤只读取收件范围，不执行 SMTP；异步期间使用资源门闩固定页面和草稿。
   */
  async function prepareSend() {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    if (mode === "group" && !audience) {
      notify({
        message: "请先选择群成员，或切换快捷发送填写邮箱。",
        tone: "error",
      });
      return;
    }
    if (
      audience &&
      (!groupAddresses.length ||
        (audience.snapshot.exclusionRevision ?? 0) < exclusionRevision ||
        audience.qqNumbers.some((qq) => !validGroupNumbers.has(qq)))
    ) {
      notify({
        message: "群收件范围已失效，请返回成员页重新确认。",
        tone: "error",
      });
      return;
    }
    if (blockedSupplements.length) {
      notify({
        message: `补充收件人中有 ${blockedSupplements.length} 位属于本群已过滤或未勾选成员。请移除这些补充地址；需要调整时返回成员页重新确认。`,
        tone: "error",
      });
      recipientsInput.current?.focus();
      return;
    }
    if (!parsed.addresses.length || parsed.invalid.length) {
      notify({
        message: parsed.invalid.length
          ? `有 ${parsed.invalid.length} 个地址不完整，请填写完整邮箱。`
          : mode === "quick"
            ? "请填写完整的收件邮箱。"
            : "请选择群成员，或填写补充邮箱。",
        tone: "error",
      });
      document.getElementById("mail-recipients")?.focus();
      return;
    }
    if (parsed.addresses.length > 10000) {
      notify({
        message:
          "本次群成员与补充地址合计不能超过 10000 位，请返回调整成员范围。",
        tone: "error",
      });
      return;
    }
    if (!subject.trim()) {
      notify({ message: "请填写邮件主题。", tone: "error" });
      document.getElementById("mail-subject")?.focus();
      return;
    }
    const source = editor.current?.innerHTML ?? html;
    const images = currentInlineImages(source);
    if (
      images.length > MAX_INLINE_IMAGES ||
      [...attachments, ...images].reduce((sum, item) => sum + item.size, 0) >
        MAX_CONTENT_BYTES
    ) {
      notify({
        message:
          "正文最多 10 张图片，图片与附件合计不能超过 18 MiB，请移除部分内容。",
        tone: "error",
      });
      return;
    }
    const clean = sanitizeMailHtml(source, imageRegistry.current);
    const text =
      new DOMParser().parseFromString(clean, "text/html").body.textContent ??
      "";
    if (!text.trim() && !images.length) {
      notify({ message: "请填写邮件正文。", tone: "error" });
      editor.current?.focus();
      return;
    }
    if (accountEditorDirty) {
      notify({
        message: "发件账号有未保存的修改，请先保存配置后发送。",
        tone: "error",
      });
      openAccountSettings();
      return;
    }
    if (!selectedAccounts.length) {
      notify({
        message: accountCatalog.accounts.length
          ? "请至少选择一个本次使用的发件账号。"
          : "请先保存发件账号及授权码，再选择账号发送。",
        tone: "error",
      });
      if (!accountCatalog.accounts.length) openAccountSettings();
      return;
    }
    if (
      !Number.isInteger(intervalSeconds) ||
      intervalSeconds < 1 ||
      intervalSeconds > 3600
    ) {
      notify({
        message: "同账号间隔请填写 1 到 3600 的整数秒。",
        tone: "error",
      });
      return;
    }
    if (
      !accountSwitchIntervalInput.trim() ||
      !Number.isInteger(accountSwitchIntervalSeconds) ||
      accountSwitchIntervalSeconds < 0 ||
      accountSwitchIntervalSeconds > 3600
    ) {
      notify({
        message: "账号切换间隔请填写 0 到 3600 的整数秒。",
        tone: "error",
      });
      return;
    }
    if (!Number.isInteger(batchSize) || batchSize < 1 || batchSize > 1000) {
      notify({
        message: "每封人数请填写 1 到 1000 的整数，并遵循发件服务的实际限制。",
        tone: "error",
      });
      return;
    }
    const draft: MailDraft = {
      recipients: parsed.addresses.join(";"),
      subject: subject.trim(),
      html: clean,
      attachmentIds: attachments.map((attachment) => attachment.id),
      inlineImageIds: images.map((image) => image.id),
      skipSent,
      continueOnFailure,
      sendMode,
      batchSize,
      intervalSeconds,
      accountSwitchIntervalSeconds,
      accountIds: selectedAccounts.map((account) => account.id),
      accountRevision: accountCatalog.revision,
      allocationMode: "random",
      ...(audience
        ? {
            groupAudience: {
              snapshotId: audience.snapshot.id,
              qqNumbers: [...new Set(audience.qqNumbers)],
            },
          }
        : {}),
    };
    const key = recipientPlanKey;
    preparedDraft.current = null;
    setRecipientPlan(null);
    try {
      await performTemplateOperation(async () => {
        const plan = await call<MailRecipientPlan>("prepare_mail_recipients", {
          input: {
            recipients: draft.recipients,
            skipSent: draft.skipSent,
            accountIds: draft.accountIds,
            accountRevision: draft.accountRevision,
            sendMode: draft.sendMode,
            batchSize: draft.batchSize,
            allocationMode: draft.allocationMode,
            ...(draft.groupAudience
              ? { groupAudience: draft.groupAudience }
              : {}),
          },
        });
        if (disposed.current || latestRecipientPlanKey.current !== key) return;
        if (
          !plan.id ||
          [
            plan.total,
            plan.skipped,
            plan.groupMemberCount,
            plan.extraCount,
            plan.manualExcluded ?? 0,
          ].some((value) => !Number.isInteger(value) || value < 0) ||
          plan.groupMemberCount + plan.extraCount !== plan.total ||
          plan.accountRevision !== accountCatalog.revision ||
          !Array.isArray(plan.accountAssignments) ||
          plan.accountAssignments.length !== selectedAccounts.length ||
          new Set(
            plan.accountAssignments.map((assignment) => assignment.accountId),
          ).size !== selectedAccounts.length ||
          plan.accountAssignments.some((assignment) => {
            const account = selectedAccounts.find(
              (item) => item.id === assignment.accountId,
            );
            return (
              !account ||
              assignment.fromAddress !== account.fromAddress ||
              !Number.isInteger(assignment.recipientCount) ||
              assignment.recipientCount <
                Math.floor(plan.total / selectedAccounts.length) ||
              assignment.recipientCount >
                Math.ceil(plan.total / selectedAccounts.length) ||
              assignment.messageCount !==
                (sendMode === "individual"
                  ? assignment.recipientCount
                  : Math.ceil(assignment.recipientCount / batchSize))
            );
          }) ||
          plan.accountAssignments.reduce(
            (sum, assignment) => sum + assignment.recipientCount,
            0,
          ) !== plan.total ||
          plan.totalMessageCount !==
            plan.accountAssignments.reduce(
              (sum, assignment) => sum + assignment.messageCount,
              0,
            )
        )
          throw new Error("账号分配或收件人数未能确认，请重新操作。");
        setRecipientPlan({
          value: plan,
          key,
          expiresAt: Date.now() + 5 * 60 * 1000,
        });
        if (plan.total === 0) {
          notify({
            message:
              (plan.manualExcluded ?? 0) > 0
                ? `已手动排除 ${plan.manualExcluded} 个地址${plan.skipped > 0 ? `，跳过 ${plan.skipped} 个已发送地址` : ""}，没有待发送收件人。`
                : plan.skipped > 0
                  ? `已跳过 ${plan.skipped} 个已发送地址，没有待发送收件人。`
                  : "没有待发送收件人。",
            tone: "success",
          });
          return;
        }
        // 先固定随机分配再检查参与账号；分配为 0 的账号不会连接 SMTP，也不要求凭据。
        const unavailable = plan.accountAssignments.find(
          (assignment) =>
            assignment.recipientCount > 0 &&
            !selectedAccounts.find(
              (account) => account.id === assignment.accountId,
            )?.passwordSaved,
        );
        if (unavailable)
          throw {
            code: "missing_smtp_credentials",
            message: `本次参与发送的账号 ${unavailable.fromAddress} 授权码不可用，请在发件设置中补充后保存。`,
          };
        preparedDraft.current = { ...draft, recipientPlanId: plan.id };
        confirmDialog.current?.showModal();
        setConfirmationOpen(true);
      });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
      const code =
        error && typeof error === "object" && "code" in error ? error.code : "";
      if (code === "missing_smtp_credentials") openAccountSettings();
      if (
        code === "sender_accounts_changed" ||
        code === "mail_accounts_changed"
      )
        await loadAccounts();
    }
  }

  /** 用户确认摘要后提交后台队列；只传账号 id 和公开兼容字段，页面不读取已保存授权码。 */
  async function sendPrepared() {
    if (
      locked ||
      imageOperationPending.current ||
      submissionPending.current ||
      !preparedDraft.current ||
      !selectedAccounts.length
    )
      return;
    if (
      !activeRecipientPlan ||
      preparedDraft.current.recipientPlanId !== activeRecipientPlan.id
    ) {
      preparedDraft.current = null;
      confirmDialog.current?.close();
      notify({
        message: "收件范围已变化，请重新确认人数后发送。",
        tone: "error",
      });
      return;
    }
    // Lock the parent before waiting for IPC, so navigation cannot replace the
    // audience between confirmation and the host publishing its running state.
    submissionPending.current = true;
    mailRevision.current += 1;
    callbacks.current.onRunningChange?.(true);
    setBusy(true);
    setConfirmationOpen(false);
    confirmDialog.current?.close();
    try {
      const value = await call<MailView>("start_mail_job", {
        draft: preparedDraft.current,
        // 兼容宿主旧入口的公开参数；新路径由稳定账号 id 读取各自凭据，不传临时授权码。
        settings: {
          host: selectedAccounts[0].host,
          port: selectedAccounts[0].port,
          tlsMode: selectedAccounts[0].tlsMode,
          username: selectedAccounts[0].username,
          fromAddress: selectedAccounts[0].fromAddress,
          fromName: selectedAccounts[0].fromName,
          password: "",
        } satisfies MailSettings,
      });
      applyMailState(value);
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
      if (
        error &&
        typeof error === "object" &&
        "code" in error &&
        (error.code === "sender_accounts_changed" ||
          error.code === "mail_accounts_changed")
      )
        await loadAccounts();
    } finally {
      submissionPending.current = false;
      callbacks.current.onRunningChange?.(
        lastMail.current.status === "running",
      );
      preparedDraft.current = null;
      setRecipientPlan(null);
      setBusy(false);
    }
  }

  /** 只跳过当前任务当前等待；旧轮次/重复点击由宿主再次校验，不修改后续间隔或账号冷却。 */
  async function skipCurrentWait() {
    if (
      !running ||
      stopping ||
      !waiting ||
      !mail.waitId ||
      skipWaitPending.current
    )
      return;
    skipWaitPending.current = true;
    setSkippingWait(true);
    try {
      const value = await call<MailView>("skip_mail_wait", {
        jobId: mail.id,
        waitId: mail.waitId,
      });
      applyMailState(value);
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    } finally {
      skipWaitPending.current = false;
      setSkippingWait(false);
    }
  }

  /** 当前一封 SMTP 交互结束后停止后续批次，避免超时状态不明时自动重试造成重复投递。 */
  async function stopSending() {
    if (!running || stopping || busy || stopOperationPending.current) return;
    stopOperationPending.current = true;
    setBusy(true);
    try {
      const value = await call<MailView>("cancel_mail_job");
      applyMailState(value);
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    } finally {
      stopOperationPending.current = false;
      callbacks.current.onRunningChange?.(
        lastMail.current.status === "running",
      );
      setBusy(false);
    }
  }

  /** 仅打开预览时展开图片字节，避免编辑每个文字都复制整封图片的 base64。 */
  function openPreview() {
    if (busy || imageOperationPending.current || submissionPending.current)
      return;
    const cleanPreview = sanitizeMailHtml(
      editor.current?.innerHTML ?? html,
      imageRegistry.current,
      "preview",
    );
    setPreviewDocument(mailPreviewDocument(cleanPreview));
    previewDialog.current?.showModal();
  }

  /** 替换前总要确认，包含清空模板附件的情况；取消不调用宿主，也不更换模板上下文。 */
  function requestTemplateReplacement(
    value: MailTemplateContent | null,
    intent: "apply" | "edit" | "new",
  ) {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    preparedDraft.current = null;
    confirmDialog.current?.close();
    setReplaceTemplate({ value, intent });
    replaceTemplateDialog.current?.showModal();
  }

  /**
   * 后台先验证全部持久资源，再原子替换当前句柄。前端完整构造新图片后一次提交视图，
   * 只释放旧 blob，不逐条移除已经由宿主替换的旧句柄；新编辑节点清除旧撤销记录。
   */
  async function confirmTemplateReplacement() {
    if (!replaceTemplate || locked || imageOperationPending.current) return;
    const request = replaceTemplate;
    const created = new Map<string, EditorInlineImage>();
    try {
      await performTemplateOperation(async () => {
        const value = request.value
          ? await call<MailTemplateContent>("mail_template_apply", {
              id: request.value.id,
            })
          : (await call<void>("mail_clear_draft_content"), null);
        if (disposed.current) return;
        for (const image of value?.inlineImages ?? []) {
          const bytes = Uint8Array.from(atob(image.dataBase64), (character) =>
            character.charCodeAt(0),
          );
          created.set(image.id, {
            ...image,
            objectUrl: URL.createObjectURL(
              new Blob([bytes], { type: image.mimeType }),
            ),
          });
        }
        const cleaned = value
          ? sanitizeMailHtml(templateHtmlForEditor(value, created), created)
          : "";
        const editorHtml = value
          ? templateHtmlForEditor({ ...value, html: cleaned }, created)
          : "";
        for (const image of imageRegistry.current.values())
          URL.revokeObjectURL(image.objectUrl);
        imageRegistry.current = created;
        editorSelection.current = null;
        replacementHtml.current = editorHtml;
        setSubject(value?.subject ?? "");
        setHtml(editorHtml);
        setAttachments(value?.attachments ?? []);
        setEditorRevision((current) => current + 1);
        setTemplateContext(value ? { id: value.id, name: value.name } : null);
        setNewTemplateMode(request.intent === "new");
        preparedDraft.current = null;
        confirmDialog.current?.close();
        replaceTemplateDialog.current?.close();
        setReplaceTemplate(null);
        setTemplateOpen(false);
      });
      notify({
        message:
          request.intent === "new"
            ? "已进入新模板编辑。"
            : request.intent === "edit"
              ? "修改后点击“更新模板”保存。"
              : "模板已应用。",
        tone: "success",
      });
    } catch (error) {
      if (imageRegistry.current !== created)
        for (const image of created.values())
          URL.revokeObjectURL(image.objectUrl);
      notify({ message: errorText(error), tone: "error" });
    }
  }

  /** 保存入口每次默认不包含附件；只有显式更新才携带原模板 id，其他操作始终另存。 */
  function openSaveTemplate(update: boolean) {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    if (update && !templateContext) return;
    setSaveTemplateUpdateId(update ? (templateContext?.id ?? null) : null);
    setSaveTemplateName(
      update
        ? (templateContext?.name ?? "")
        : templateContext
          ? `${[...templateContext.name].slice(0, 75).join("")} 副本`
          : [...subject.trim()].slice(0, 80).join(""),
    );
    setIncludeTemplateAttachments(false);
    saveTemplateDialog.current?.showModal();
  }

  /** 保存当前内容的固定快照，允许空主题/正文的半成品；发送校验仍独立要求完整邮件。 */
  async function saveCurrentTemplate() {
    if (locked || imageOperationPending.current || submissionPending.current)
      return;
    const name = saveTemplateName.trim();
    if (!name || [...name].length > 80) {
      notify({ message: "模板名称请填写 1 到 80 个字符。", tone: "error" });
      document.getElementById("mail-template-save-name")?.focus();
      return;
    }
    const source = editor.current?.innerHTML ?? html;
    const images = currentInlineImages(source);
    const input: MailTemplateInput = {
      name,
      subject,
      html: sanitizeMailHtml(source, imageRegistry.current),
      inlineImageIds: images.map((image) => image.id),
      attachmentIds: includeTemplateAttachments
        ? attachments.map((item) => item.id)
        : [],
      includeAttachments: includeTemplateAttachments,
      ...(saveTemplateUpdateId ? { updateId: saveTemplateUpdateId } : {}),
    };
    try {
      const value = await performTemplateOperation(() =>
        call<MailTemplateSummary>("mail_template_save", { input }),
      );
      setTemplateContext({ id: value.id, name: value.name });
      setNewTemplateMode(false);
      saveTemplateDialog.current?.close();
      notify({
        message: saveTemplateUpdateId ? "模板已更新。" : "模板已保存。",
        tone: "success",
      });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    }
  }

  return (
    <section className="mail-composer" aria-labelledby="mail-heading">
      <WorkspaceHeader
        className="mail-heading"
        headingId="mail-heading"
        title="写邮件"
        actions={
          <div className="mail-heading-actions">
            <button
              className="button"
              disabled={locked}
              onClick={() => {
                if (
                  !imageOperationPending.current &&
                  !submissionPending.current
                )
                  setTemplateOpen(true);
              }}
            >
              <FileText size={16} />
              选择模板
            </button>
            {!templateContext && !newTemplateMode && (
              <button
                className="button"
                disabled={locked}
                onClick={() => openSaveTemplate(false)}
              >
                <Save size={16} />
                保存为模板
              </button>
            )}
            <button
              className="button"
              disabled={locked}
              onClick={() => {
                if (!imageOperationPending.current) openAccountSettings();
              }}
            >
              <Settings2 size={16} />
              发件设置
            </button>
          </div>
        }
      />
      {accountsReadError && !settingsOpen && (
        <div className="mail-state-retry" role="alert">
          <span>发件账号暂不可用，请重新加载后继续。</span>
          <button
            className="button"
            disabled={accountsLoading}
            aria-busy={accountsLoading}
            onClick={() => void loadAccounts()}
          >
            {accountsLoading ? "正在加载发件账号…" : "重新加载发件账号"}
          </button>
        </div>
      )}
      {mailStateError && (
        <div className="mail-state-retry" role="alert">
          <span>发送状态暂不可用，请重新加载后继续。</span>
          <button
            className="button"
            disabled={mailStateLoading}
            aria-busy={mailStateLoading}
            onClick={() => {
              if (mailStatePending.current) return;
              mailStatePending.current = true;
              setMailStateLoading(true);
              setMailStateReadRevision((value) => value + 1);
            }}
          >
            {mailStateLoading ? (
              <LoaderCircle size={15} className="spin" aria-hidden="true" />
            ) : (
              <RefreshCw size={15} aria-hidden="true" />
            )}
            {mailStateLoading ? "正在加载发送状态…" : "重新加载发送状态"}
          </button>
        </div>
      )}
      {/* 配置与内容并列：左侧只决定发给谁/如何发，右侧保持同一编辑器和资源列表。 */}
      <WorkbenchLayout
        className="mail-workbench"
        sidebarLabel="收件范围与发送配置"
        contentLabel="邮件内容与发送状态"
        sidebarHeader={
          <div className="mail-mode-switch" role="group" aria-label="发送模式">
            <button
              className="button"
              aria-pressed={mode === "quick"}
              disabled={locked}
              onClick={() => setMode("quick")}
            >
              快捷发送
            </button>
            <button
              className="button"
              aria-pressed={mode === "group"}
              disabled={locked}
              onClick={() => setMode("group")}
            >
              群成员发送
            </button>
          </div>
        }
        sidebar={
          <>
            {mode === "group" && (
              <GroupAudiencePanel
                audience={audience}
                currentSnapshot={snapshot}
                disabled={locked}
                onChooseMembers={onChooseMembers}
              />
            )}
            <section
              className="workbench-panel mail-recipient-panel"
              aria-label="收件地址"
            >
              <div className="mail-field-row">
                <label htmlFor="mail-recipients">
                  {mode === "quick" ? "收件邮箱" : "补充邮箱"}
                </label>
                <div className="mail-recipient-field">
                  <textarea
                    ref={recipientsInput}
                    id="mail-recipients"
                    value={activeRecipients}
                    onChange={(event) =>
                      mode === "quick"
                        ? setQuickRecipients(event.target.value)
                        : setRecipients(event.target.value)
                    }
                    disabled={locked}
                    rows={2}
                    aria-describedby={[
                      ...(manualParsed.invalid.length
                        ? ["mail-recipient-error"]
                        : []),
                      ...(blockedSupplements.length
                        ? ["mail-recipient-blocked"]
                        : []),
                    ].join(" ")}
                    aria-invalid={
                      manualParsed.invalid.length > 0 ||
                      blockedSupplements.length > 0
                    }
                    placeholder={"someone@qq.com\nsomeone@gmail.com"}
                  />
                  {activeRecipients && (
                    <div className="mail-recipient-footer">
                      <button
                        className="mail-text-action"
                        disabled={locked || !activeRecipients}
                        onClick={() => {
                          // 只选择当前模式的手工输入，便于批量复制或修改，不改变已绑定的群成员名单。
                          recipientsInput.current?.focus();
                          recipientsInput.current?.select();
                        }}
                      >
                        全选输入
                      </button>
                      {extraCount > 0 && (
                        <span>
                          {mode === "quick" ? "有效" : "已补充"} {extraCount}{" "}
                          个邮箱
                        </span>
                      )}
                    </div>
                  )}
                  {manualParsed.invalid.length > 0 && (
                    <div
                      id="mail-recipient-error"
                      className="mail-recipient-boundary error-text"
                      role="alert"
                    >
                      <div className="mail-recipient-issue-heading">
                        <span>
                          有 {manualParsed.invalid.length}{" "}
                          个地址不完整，请填写完整邮箱。
                        </span>
                        <button
                          className="mail-text-action"
                          disabled={locked}
                          onClick={() =>
                            removeRecipientIssues(manualParsed.invalid)
                          }
                        >
                          移除全部无效项
                        </button>
                      </div>
                      <ul className="mail-recipient-issues">
                        {manualParsed.invalid.map((value, index) => (
                          <li key={`${index}:${value}`}>
                            <code>{value}</code>
                            <button
                              className="mail-text-action"
                              disabled={locked}
                              aria-label={`移除无效项 ${value}`}
                              onClick={() => removeRecipientIssues([value])}
                            >
                              移除
                            </button>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  {blockedSupplements.length > 0 && (
                    <div
                      id="mail-recipient-blocked"
                      className="mail-recipient-boundary error-text"
                      role="alert"
                    >
                      <div className="mail-recipient-issue-heading">
                        <span>
                          有 {blockedSupplements.length}{" "}
                          位补充地址是本群已过滤或未选成员，请移除或返回成员页调整。
                        </span>
                        <button
                          className="mail-text-action"
                          disabled={locked}
                          onClick={() =>
                            removeRecipientIssues(blockedSupplements)
                          }
                        >
                          移除全部受阻地址
                        </button>
                      </div>
                      <ul className="mail-recipient-issues">
                        {blockedSupplements.map((address) => {
                          const item = filteredSupplementReasons.get(address);
                          const reason =
                            item?.reason === "group"
                              ? "排除群重合"
                              : item?.reason === "friend"
                                ? "QQ 好友"
                                : item?.reason === "sent"
                                  ? "已发送记录"
                                  : item?.reason === "manual"
                                    ? "手动排除"
                                    : "尚未勾选";
                          return (
                            <li key={address}>
                              <span>
                                <code>{address}</code>
                                <small>
                                  {reason}
                                  {item?.detail ? ` · ${item.detail}` : ""}
                                </small>
                              </span>
                              <div>
                                <button
                                  className="mail-text-action"
                                  disabled={locked}
                                  aria-label={`移除补充地址 ${address}`}
                                  onClick={() =>
                                    removeRecipientIssues([address])
                                  }
                                >
                                  移除地址
                                </button>
                                <button
                                  className="mail-text-action"
                                  disabled={locked}
                                  aria-label={`调整成员 ${address}`}
                                  onClick={() =>
                                    onLocateMember(
                                      address.slice(0, address.indexOf("@")),
                                    )
                                  }
                                >
                                  定位成员
                                </button>
                              </div>
                            </li>
                          );
                        })}
                      </ul>
                    </div>
                  )}
                </div>
              </div>
            </section>
            <fieldset className="mail-account-picker" disabled={locked}>
              <legend>本次发件账号</legend>
              <div className="mail-account-picker-tools">
                <span>
                  已选 {selectedAccounts.length} /{" "}
                  {accountCatalog.accounts.length} 个
                </span>
                <button
                  className="mail-text-action"
                  disabled={locked || !accountCatalog.accounts.length}
                  aria-pressed={
                    accountCatalog.accounts.length > 0 &&
                    selectedAccounts.length === accountCatalog.accounts.length
                  }
                  onClick={() =>
                    setSelectedAccountIds(
                      selectedAccounts.length === accountCatalog.accounts.length
                        ? []
                        : accountCatalog.accounts.map((account) => account.id),
                    )
                  }
                >
                  {accountCatalog.accounts.length > 0 &&
                  selectedAccounts.length === accountCatalog.accounts.length
                    ? "取消全选发件账号"
                    : "全选发件账号"}
                </button>
              </div>
              <div className="mail-account-choices">
                {accountCatalog.accounts.map((account) => (
                  <label key={account.id}>
                    <input
                      type="checkbox"
                      aria-label={`本次使用 ${account.fromAddress}`}
                      checked={selectedAccountIds.includes(account.id)}
                      onChange={(event) =>
                        setSelectedAccountIds((previous) =>
                          event.target.checked
                            ? [...previous, account.id]
                            : previous.filter((id) => id !== account.id),
                        )
                      }
                    />
                    <span>
                      <strong>{account.name}</strong>
                      <span>{account.fromAddress}</span>
                      {!account.passwordSaved && (
                        <small className="mail-field-error">
                          授权码不可用，请在发件设置中更新
                        </small>
                      )}
                    </span>
                  </label>
                ))}
              </div>
              {settingsReady && !accountCatalog.accounts.length && (
                <button
                  className="mail-text-action"
                  disabled={locked}
                  onClick={openAccountSettings}
                >
                  添加发件账号
                </button>
              )}
            </fieldset>

            {/* 低频发送参数按需展开；原状态、校验与发送计划仍由同一组件负责。 */}
            <details
              className="mail-delivery-settings"
              open={deliverySettingsOpen}
              onToggle={(event) =>
                setDeliverySettingsOpen(event.currentTarget.open)
              }
            >
              <summary>发送选项</summary>
              <div className="mail-send-options">
                <label>
                  <input
                    type="checkbox"
                    checked={skipSent}
                    disabled={locked}
                    onChange={(event) => setSkipSent(event.target.checked)}
                  />
                  跳过已发送
                </label>
                <label title="失败后跳过本批，继续后续批次；不会自动重试失败批次。">
                  <input
                    type="checkbox"
                    checked={continueOnFailure}
                    disabled={locked}
                    onChange={(event) =>
                      setContinueOnFailure(event.target.checked)
                    }
                  />
                  失败后继续发送
                </label>
                {/* 数值和选择字段共用标签/控件/单位三列；无单位的方式选择也保留末列，避免左右错位。 */}
                <label className="mail-option-field">
                  <span>发送方式</span>
                  <select
                    aria-label="发送方式"
                    value={sendMode}
                    disabled={locked}
                    onChange={(event) =>
                      setSendMode(event.target.value as "bcc" | "individual")
                    }
                  >
                    <option value="bcc">密送分批</option>
                    <option value="individual">逐人发送</option>
                  </select>
                </label>
                {sendMode === "bcc" && (
                  <label className="mail-option-field">
                    <span>每封人数</span>
                    <input
                      aria-label="每封收件人数"
                      type="number"
                      min={1}
                      max={1000}
                      step={1}
                      value={batchSize}
                      disabled={locked}
                      onChange={(event) =>
                        setBatchSize(Number(event.target.value))
                      }
                    />
                    <span className="mail-option-unit">人</span>
                  </label>
                )}
                <label className="mail-option-field">
                  <span>同账号间隔</span>
                  <input
                    aria-label="同账号发送间隔秒数"
                    title="上批结束后计时；取消后仍保留剩余冷却，与账号切换间隔取较晚时间"
                    type="number"
                    min={1}
                    max={3600}
                    step={1}
                    value={intervalSeconds}
                    disabled={locked}
                    onChange={(event) =>
                      setIntervalSeconds(Number(event.target.value))
                    }
                  />
                  <span className="mail-option-unit">秒</span>
                </label>
                <label className="mail-option-field">
                  <span>账号切换间隔</span>
                  <input
                    aria-label="账号切换间隔秒数"
                    title="不同账号之间的等待时间，0表示关闭；与同账号冷却取较晚时间"
                    type="number"
                    min={0}
                    max={3600}
                    step={1}
                    value={accountSwitchIntervalInput}
                    disabled={locked}
                    onChange={(event) =>
                      setAccountSwitchIntervalInput(event.target.value)
                    }
                  />
                  <span className="mail-option-unit">秒</span>
                </label>
              </div>
            </details>
          </>
        }
      >
        {mail.status !== "idle" && (
          <section
            className={`mail-progress mail-progress-${mail.status}${mail.status === "completed" && mail.failed > 0 ? " mail-progress-failed" : ""}`}
            aria-label="发送状态"
          >
            <div aria-live="polite">
              <strong>
                {running
                  ? stopping
                    ? "正在停止发送"
                    : waiting
                      ? "等待发送间隔"
                      : "正在发送"
                  : mail.status === "completed"
                    ? mail.failed > 0
                      ? "发送结束，部分投递未确认"
                      : "发送任务已结束"
                    : mail.status === "cancelled"
                      ? "已停止发送"
                      : "发送任务未完成"}
              </strong>
              <span>
                服务器接受 {mail.sent} · 失败 {mail.failed} · 跳过{" "}
                {mail.skipped} · 共 {mail.total}
              </span>
            </div>
            <p aria-live="polite">{mail.message}</p>
            {nextBatchAt !== null && (
              <BatchCountdown key={`batch-${mail.id}`} deadline={nextBatchAt} />
            )}
            {waiting && !stopping && mail.waitId && (
              <div className="mail-wait-actions">
                <button
                  className="button"
                  disabled={skippingWait || busy}
                  title="仅立即尝试当前待发的一批，后续仍遵守原间隔"
                  onClick={() => void skipCurrentWait()}
                >
                  {skippingWait ? (
                    <LoaderCircle size={15} className="spin" />
                  ) : (
                    <SkipForward size={15} />
                  )}
                  {skippingWait ? "正在跳过…" : "跳过本次等待"}
                </button>
              </div>
            )}
            {running &&
              !stopping &&
              typeof mail.estimatedCompletionAt === "number" &&
              Number.isFinite(mail.estimatedCompletionAt) &&
              mail.estimatedCompletionAt > 0 &&
              mail.estimatedCompletionAt <= 8_640_000_000_000_000 && (
                <CompletionEstimate
                  key={`completion-${mail.id}`}
                  deadline={mail.estimatedCompletionAt}
                />
              )}
            {mail.groupAudience && (
              <p className="mail-job-source">
                本次任务来源：{mail.groupAudience.groupName || "未命名群"}（
                {mail.groupAudience.groupId}）· 群成员{" "}
                {mail.groupAudience.memberCount} 人
              </p>
            )}
            {running && mail.current && (
              <p className="mail-current">
                {waiting ? "待发送" : "当前"}：{mail.current}
              </p>
            )}
            {mail.failures.length > 0 && (
              <details>
                <summary>查看未成功项（{mail.failures.length}）</summary>
                <ul>
                  {mail.failures.map((failure, index) => (
                    <li key={`${failure.recipient}-${index}`}>
                      <span>{failure.recipient}</span>
                      <span>{failure.message}</span>
                    </li>
                  ))}
                </ul>
              </details>
            )}
            {!running && (
              <div className="mail-result-actions">
                <button
                  className="mail-text-action"
                  disabled={busy || disabled}
                  onClick={onChooseMembers}
                >
                  返回成员查看发送状态
                </button>
                <button
                  className="mail-text-action"
                  disabled={busy || disabled}
                  onClick={onOpenHistory}
                >
                  核实投递记录
                </button>
              </div>
            )}
          </section>
        )}
        {(templateContext || newTemplateMode) && (
          <div className="mail-template-context" aria-label="当前模板">
            <span>
              <FileText size={15} aria-hidden="true" />
              {templateContext
                ? `模板：${templateContext.name}`
                : "正在编辑新模板"}
            </span>
            <div>
              {templateContext && (
                <button
                  className="mail-text-action"
                  disabled={locked}
                  onClick={() => openSaveTemplate(true)}
                >
                  更新模板
                </button>
              )}
              <button
                className="mail-text-action"
                disabled={locked}
                onClick={() => openSaveTemplate(false)}
              >
                {newTemplateMode ? "保存模板" : "另存为模板"}
              </button>
              <button
                className="icon-button"
                disabled={locked}
                aria-label="结束模板编辑"
                onClick={() => {
                  setTemplateContext(null);
                  setNewTemplateMode(false);
                }}
              >
                <X size={15} />
              </button>
            </div>
          </div>
        )}
        <section className="mail-draft-panel" aria-label="邮件草稿">
          <div className="mail-field-row">
            <label htmlFor="mail-subject">主题</label>
            <input
              id="mail-subject"
              value={subject}
              onChange={(event) => setSubject(event.target.value)}
              disabled={locked}
              maxLength={200}
              placeholder="填写邮件主题"
            />
          </div>
          {attachments.length > 0 && (
            <ul className="mail-attachments" aria-label="邮件附件">
              {attachments.map((attachment) => (
                <li key={attachment.id}>
                  <Paperclip size={14} />
                  <span>
                    {attachment.name}
                    <small>{formatBytes(attachment.size)}</small>
                  </span>
                  <button
                    className="icon-button"
                    disabled={locked}
                    aria-label={`移除附件 ${attachment.name}`}
                    onClick={() => void removeAttachment(attachment.id)}
                  >
                    <X size={14} />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="mail-editor-shell">
            <div
              className="mail-format-bar"
              role="toolbar"
              aria-label="正文格式"
            >
              {(
                [
                  { command: "bold", title: "加粗", icon: Bold },
                  { command: "italic", title: "斜体", icon: Italic },
                  { command: "underline", title: "下划线", icon: Underline },
                  {
                    command: "insertUnorderedList",
                    title: "项目列表",
                    icon: List,
                  },
                  { command: "undo", title: "撤销", icon: Undo2 },
                ] as const
              ).map((item) => (
                <button
                  key={item.command}
                  type="button"
                  className="mail-format-button"
                  disabled={locked}
                  aria-label={item.title}
                  title={item.title}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => format(item.command)}
                >
                  <item.icon size={17} />
                </button>
              ))}
              <span className="mail-format-divider" />
              <button
                type="button"
                className="mail-format-button mail-insert-image"
                disabled={locked}
                aria-label="插入图片"
                title="插入图片，也可按Ctrl+V粘贴；最多10张，单张不超过5 MiB"
                onMouseDown={(event) => event.preventDefault()}
                onClick={pickInlineImages}
              >
                <ImagePlus size={17} />
                插入图片
              </button>
              <button
                className="mail-format-button mail-insert-image"
                title="最多10个附件，与正文图片合计不超过18 MiB"
                disabled={locked}
                onClick={() => void pickAttachments()}
              >
                <Paperclip size={15} />
                添加附件
              </button>
              <button
                className="mail-preview-button"
                disabled={busy}
                onClick={openPreview}
              >
                <Eye size={15} />
                预览
              </button>
            </div>
            <div
              key={editorRevision}
              ref={editor}
              className="mail-editor"
              role="textbox"
              aria-label="邮件正文"
              aria-multiline="true"
              contentEditable={!locked}
              suppressContentEditableWarning
              data-placeholder="在这里填写邮件正文…"
              onInput={() => setHtml(editor.current?.innerHTML ?? "")}
              onKeyUp={rememberEditorSelection}
              onMouseUp={rememberEditorSelection}
              onDrop={(event) => event.preventDefault()}
              onPaste={pasteEditor}
            />
            {(imagePending || activeInlineImages.length > 0) && (
              <div
                className="mail-image-help"
                role={imagePending ? "status" : undefined}
              >
                {imagePending ? (
                  <>
                    <LoaderCircle size={14} className="spin" />
                    正在添加正文图片…
                  </>
                ) : (
                  <>图片 {activeInlineImages.length} / 10 · 单张不超过 5 MiB</>
                )}
              </div>
            )}
          </div>
          {activeInlineImages.length > 0 && (
            <ul className="mail-inline-images" aria-label="正文图片">
              {activeInlineImages.map((image) => (
                <li key={image.id}>
                  <img src={image.objectUrl} alt="" />
                  <span>
                    {image.name}
                    <small>{formatBytes(image.size)}</small>
                  </span>
                  <button
                    className="icon-button"
                    disabled={locked}
                    aria-label={`移除正文图片 ${image.name}`}
                    onClick={() => removeInlineImage(image.id)}
                  >
                    <X size={14} />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
      </WorkbenchLayout>
      <footer className="mail-footer">
        <p className="mail-batch-summary">
          {activeRecipientPlan
            ? `实际待发送 ${activeRecipientPlan.total} 人 · 预计 ${estimatedBatches} 封邮件${activeRecipientPlan.skipped > 0 ? ` · 已跳过 ${activeRecipientPlan.skipped} 个已发送地址` : ""}`
            : `候选收件人 ${parsed.addresses.length} 人`}
        </p>
        {running ? (
          <button
            className="button"
            disabled={busy || stopping}
            onClick={() => void stopSending()}
          >
            {busy ? (
              <LoaderCircle className="spin" size={16} />
            ) : (
              <Square size={15} />
            )}
            停止后续发送
          </button>
        ) : (
          <button
            className="button primary"
            disabled={locked}
            onClick={() => void prepareSend()}
          >
            {busy ? (
              <LoaderCircle className="spin" size={16} />
            ) : (
              <Send size={16} />
            )}
            发送
          </button>
        )}
      </footer>

      <MailTemplateDialog
        open={templateOpen}
        busy={templatePending}
        onClose={() => {
          if (!imageOperationPending.current) setTemplateOpen(false);
        }}
        onNew={() => requestTemplateReplacement(null, "new")}
        onUse={requestTemplateReplacement}
        onChanged={(value) =>
          setTemplateContext((current) =>
            current?.id === value.id
              ? { id: value.id, name: value.name }
              : current,
          )
        }
        onDeleted={(id) =>
          setTemplateContext((current) => (current?.id === id ? null : current))
        }
        perform={performTemplateOperation}
        previewDocument={previewTemplate}
      />

      <dialog
        ref={replaceTemplateDialog}
        className="mail-dialog mail-template-replace-dialog dialog-shell"
        aria-labelledby="mail-template-replace-heading"
        onCancel={(event) => {
          if (templatePending) event.preventDefault();
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-template-replace-heading">
                {replaceTemplate?.intent === "new"
                  ? "新建模板"
                  : "替换邮件内容"}
              </h2>
              <button
                className="icon-button"
                disabled={templatePending}
                aria-label="关闭替换确认"
                onClick={() => replaceTemplateDialog.current?.close()}
              >
                <X size={18} />
              </button>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              <button
                className="button"
                disabled={templatePending}
                onClick={() => replaceTemplateDialog.current?.close()}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={templatePending}
                onClick={() => void confirmTemplateReplacement()}
              >
                {templatePending && <LoaderCircle size={16} className="spin" />}
                替换内容
              </button>
            </div>
          }
        >
          <p className="mail-dialog-note">
            {replaceTemplate?.intent === "new"
              ? "将清空当前主题、正文和附件，开始编辑新模板。"
              : `将用“${replaceTemplate?.value?.name ?? ""}”替换当前主题、正文和附件。`}
          </p>
          <p className="mail-dialog-note">收件人和发送设置保留。</p>
          {replaceTemplate?.value && (
            <div className="mail-template-replace-counts">
              正文图片 {replaceTemplate.value.inlineImages.length} 张 · 附件{" "}
              {replaceTemplate.value.attachments.length} 个
            </div>
          )}
        </DialogLayout>
      </dialog>

      <dialog
        ref={saveTemplateDialog}
        className="mail-dialog mail-template-save-dialog dialog-shell"
        aria-labelledby="mail-template-save-heading"
        onCancel={(event) => {
          if (templatePending) event.preventDefault();
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-template-save-heading">
                {saveTemplateUpdateId ? "更新邮件模板" : "保存邮件模板"}
              </h2>
              <button
                className="icon-button"
                disabled={templatePending}
                aria-label="关闭模板保存"
                onClick={() => saveTemplateDialog.current?.close()}
              >
                <X size={18} />
              </button>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              <button
                className="button"
                disabled={templatePending}
                onClick={() => saveTemplateDialog.current?.close()}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={templatePending}
                onClick={() => void saveCurrentTemplate()}
              >
                {templatePending ? (
                  <LoaderCircle size={16} className="spin" />
                ) : (
                  <Save size={16} />
                )}
                {saveTemplateUpdateId ? "更新模板" : "保存模板"}
              </button>
            </div>
          }
        >
          <label
            className="mail-template-name-field"
            htmlFor="mail-template-save-name"
          >
            模板名称
            <input
              id="mail-template-save-name"
              autoFocus
              value={saveTemplateName}
              maxLength={80}
              disabled={templatePending}
              onChange={(event) => setSaveTemplateName(event.target.value)}
              onKeyDown={(event) => {
                // 中文候选词确认不触发保存；重复按键由同步资源门闩继续拦截。
                if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  void saveCurrentTemplate();
                }
              }}
              placeholder="填写便于查找的名称"
            />
          </label>
          <p className="mail-dialog-note">
            {saveTemplateUpdateId
              ? "保存后覆盖原模板的主题、正文及所选附件。"
              : "保存主题、正文和正文图片，方便下次使用。"}
          </p>
          <label className="mail-template-include-attachments">
            <input
              type="checkbox"
              checked={includeTemplateAttachments}
              disabled={templatePending || attachments.length === 0}
              onChange={(event) =>
                setIncludeTemplateAttachments(event.target.checked)
              }
            />
            同时保存附件（{attachments.length} 个）
          </label>
        </DialogLayout>
      </dialog>

      <SenderAccountsDialog
        open={settingsOpen}
        catalog={accountCatalog}
        busy={
          busy ||
          running ||
          disabled ||
          !mailReady ||
          imagePending ||
          templatePending
        }
        loading={accountsLoading}
        readError={accountsReadError}
        onReload={() => void loadAccounts()}
        onClose={() => {
          if (!imageOperationPending.current && !submissionPending.current)
            setSettingsOpen(false);
        }}
        onChanged={applyAccountCatalog}
        onEditingChange={setAccountEditorDirty}
        perform={performAccountOperation}
      />

      <dialog
        ref={previewDialog}
        className="mail-dialog mail-preview-dialog"
        aria-labelledby="mail-preview-heading"
      >
        <div className="mail-dialog-heading">
          <h2 id="mail-preview-heading">正文预览</h2>
          <button
            className="icon-button"
            aria-label="关闭正文预览"
            onClick={() => previewDialog.current?.close()}
          >
            <X size={18} />
          </button>
        </div>
        <iframe title="安全邮件正文预览" sandbox="" srcDoc={previewDocument} />
        <p className="mail-dialog-note">邮箱中的实际排版可能略有差异。</p>
      </dialog>

      <dialog
        ref={confirmDialog}
        className="mail-dialog mail-confirm-dialog dialog-shell"
        onClose={() => setConfirmationOpen(false)}
        aria-labelledby="mail-confirm-heading"
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-confirm-heading">确认发送</h2>
              <button
                className="icon-button"
                disabled={busy}
                aria-label="关闭发送确认"
                onClick={() => confirmDialog.current?.close()}
              >
                <X size={18} />
              </button>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              <button
                className="button"
                disabled={busy}
                onClick={() => confirmDialog.current?.close()}
              >
                返回修改
              </button>
              <button
                className="button primary"
                disabled={locked}
                onClick={() => void sendPrepared()}
              >
                <Send size={16} />
                确认发送
              </button>
            </div>
          }
        >
          <div className="mail-confirm-summary" aria-label="本次收件人数">
            <span>本次收件人</span>
            <strong>
              {activeRecipientPlan?.total ?? 0}
              <small> 人</small>
            </strong>
            <p>
              {(activeRecipientPlan?.groupMemberCount ?? 0) > 0 &&
                `群成员 ${activeRecipientPlan?.groupMemberCount} 人`}
              {(activeRecipientPlan?.groupMemberCount ?? 0) > 0 &&
                (activeRecipientPlan?.extraCount ?? 0) > 0 &&
                " · "}
              {(activeRecipientPlan?.extraCount ?? 0) > 0 &&
                `${mode === "quick" ? "收件邮箱" : "补充邮箱"} ${activeRecipientPlan?.extraCount} 个`}
            </p>
          </div>
          <ul className="mail-account-allocation" aria-label="本次账号分配">
            {activeRecipientPlan?.accountAssignments?.map((assignment) => (
              <li key={assignment.accountId}>
                <div>
                  <strong>
                    账号{" "}
                    {accountCatalog.accounts.findIndex(
                      (account) => account.id === assignment.accountId,
                    ) + 1}{" "}
                    · {assignment.name}
                  </strong>
                  <span>{assignment.fromAddress}</span>
                </div>
                <div>
                  <strong>{assignment.recipientCount} 人</strong>
                  <span>{assignment.messageCount} 封</span>
                  {assignment.recipientCount === 0 && <small>本次不发送</small>}
                </div>
              </li>
            ))}
          </ul>

          {(activeRecipientPlan?.manualExcluded ?? 0) > 0 && (
            <details className="mail-manual-exclusions">
              <summary>
                手动排除 {activeRecipientPlan?.manualExcluded} 个地址
              </summary>
              <ul>
                {activeRecipientPlan?.excludedRecipients?.map((email) => (
                  <li key={email}>{email}</li>
                ))}
              </ul>
            </details>
          )}
          <dl>
            <div>
              <dt>预计结束时间</dt>
              <dd>
                {confirmationOpen && activeRecipientPlan && (
                  <ConfirmationEstimate
                    key={activeRecipientPlan.id}
                    planId={activeRecipientPlan.id}
                    intervalSeconds={intervalSeconds}
                    accountSwitchIntervalSeconds={accountSwitchIntervalSeconds}
                  />
                )}
              </dd>
            </div>
            {audience && (
              <div>
                <dt>来源群</dt>
                <dd>
                  {audience.snapshot.group.name || "未命名群"}
                  <span className="mail-confirm-secondary">
                    （{snapshotGroupLabel(audience.snapshot)}）
                  </span>
                </dd>
              </div>
            )}

            <div>
              <dt>发送方式</dt>
              <dd>
                {sendMode === "bcc"
                  ? `密送分批，每封最多 ${batchSize} 人`
                  : "逐人发送"}
                {` · 预计 ${estimatedBatches} 封`}
              </dd>
            </div>
            <div>
              <dt>同账号间隔</dt>
              <dd>
                {intervalSeconds} 秒（
                {intervalSeconds === 900 ? "15 分钟；" : ""}
                从该账号上批结束起计时）
              </dd>
            </div>
            <div>
              <dt>账号切换间隔</dt>
              <dd>
                {accountSwitchIntervalSeconds === 0
                  ? "0 秒（已关闭）"
                  : `${accountSwitchIntervalSeconds} 秒（切换至不同账号前，从上一账号上批结束起计时）`}
              </dd>
            </div>

            <div>
              <dt>失败策略</dt>
              <dd>
                {continueOnFailure
                  ? "失败后继续后续批次，不自动重试"
                  : "失败后停止后续发送"}
              </dd>
            </div>
            <div>
              <dt>主题</dt>
              <dd>{subject}</dd>
            </div>
            {(attachments.length > 0 || activeInlineImages.length > 0) && (
              <div>
                <dt>邮件内容</dt>
                <dd>
                  {attachments.length > 0 && `附件 ${attachments.length} 个`}
                  {attachments.length > 0 &&
                    activeInlineImages.length > 0 &&
                    " · "}
                  {activeInlineImages.length > 0 &&
                    `正文图片 ${activeInlineImages.length} 张`}
                </dd>
              </div>
            )}
          </dl>
          {(activeRecipientPlan?.skipped ?? 0) > 0 && (
            <p className="mail-dialog-note">
              已跳过 {activeRecipientPlan?.skipped} 个已发送地址。
            </p>
          )}
          {!skipSent && (
            <p className="mail-dialog-note">
              本次不跳过已发送地址，请确认不会重复发送。
            </p>
          )}
        </DialogLayout>
      </dialog>
    </section>
  );
}
