/**
 * batch-send-email桌面工作区。所有账号、任务、名单和保存动作由 Rust 校验；
 * 页面只管理当前选择、搜索、分页及扫码会话提示，不保存登录凭据。
 * @author ahui
 */
import WorkspaceHeader from "./components/WorkspaceHeader";
import { version as packageVersion } from "../package.json";
import DialogLayout from "./components/DialogLayout";
import {
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  Download,
  Eye,
  EyeOff,
  FileSpreadsheet,
  FolderOpen,
  Info,
  LoaderCircle,
  QrCode as QrCodeIcon,
  RefreshCw,
  Search,
  Users,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import AppToolbar, { type WorkspacePage } from "./components/AppToolbar";
import AboutPage from "./components/AboutPage";
import { useUpdates } from "./lib/updates";
import "./about.css";
import FilterOptions from "./components/FilterOptions";
import HistoryPage from "./components/HistoryPage";
import MailComposer from "./components/MailComposer";
import MemberMailActions from "./components/MemberMailActions";
import AudienceReplaceDialog from "./components/AudienceReplaceDialog";
import FeedbackHost from "./components/FeedbackHost";
import LogViewerDialog from "./components/LogViewerDialog";
import ManualExclusionsPage from "./components/ManualExclusionsPage";
import { notify } from "./lib/notify";
import { applyTheme, getSavedTheme, type ThemeChoice } from "./lib/theme";
import {
  groupSelectionKey,
  snapshotGroupIds,
  snapshotGroupLabel,
} from "./lib/group-selection";
import {
  readListFilter,
  saveListFilter,
  type ListFilterPreferences,
} from "./lib/preferences";
import type { GroupMailAudience, MemberMailStatus } from "./mail-types";
import {
  call,
  errorText,
  hasDesktopBridge,
  resolveMailDelivery,
  resolveMailDeliveries,
  subscribeState,
  LOG_STATUS_EVENT,
  type LogStatus,
} from "./lib/bridge";
import {
  displayTime,
  filterMembers,
  maskedNumber,
  sortListByLabel,
} from "./lib/view";
import appIcon from "./assets/app-icon.png";
import type {
  AppView,
  ExportFormat,
  ExportResult,
  ImportCounts,
  LoginPoll,
  QrCode,
  SentHistory,
  Snapshot,
  ManualExclusionsView,
  ManualExclusionResult,
} from "./types";

const EMPTY_VIEW: AppView = {
  // 宿主尚未返回状态或初始化失败时，也使用本次构建版本，不显示历史测试版本。
  version: packageVersion,
  source: null,
  account: null,
  groups: [],
  snapshot: null,
  task: { id: "", status: "idle", message: "" },
  runtime: "WebView2",
  historyCount: 0,
  databasePath: "",
  session: {
    status: "signed_out",
    message: "请扫码登录，关闭程序后需重新扫码。",
  },
};

/** 扫码模态框的显示数据；state 只包含图片和普通提示，不包含二维码凭据。 */
interface LoginState {
  open: boolean;
  code: QrCode | null;
  pending: boolean;
  status: "wait" | "scanned" | "expired";
  error: string | null;
}

const EMPTY_LOGIN: LoginState = {
  open: false,
  code: null,
  pending: false,
  status: "wait",
  error: null,
};

// 轮询仅推进真实状态；同一状态始终使用相同文案，避免后端同义提示造成无意义跳变。
const LOGIN_HINTS = {
  wait: "请使用手机 QQ 扫码",
  scanned: "已扫码，请在手机上确认登录",
  expired: "二维码已过期，请刷新",
} as const;

/** 原生 dialog 负责焦点约束和 Esc；异步轮询由上层管理，关闭不会保留二维码。 */
function LoginDialog({
  login,
  onClose,
  onRefresh,
}: {
  login: LoginState;
  onClose: () => void;
  onRefresh: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const element = dialog.current;
    if (login.open && element && !element.open) element.showModal();
    if (!login.open && element?.open) element.close();
  }, [login.open]);

  return (
    <dialog
      ref={dialog}
      className="login-dialog"
      aria-labelledby="login-heading"
      aria-describedby="login-hint"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      {/* 标题与关闭入口共用一行；通知宿主插在此标题行之后，与其它弹窗层级一致。 */}
      <div className="dialog-head">
        <h2 id="login-heading">扫码登录</h2>
        <button
          className="icon-button"
          aria-label="关闭扫码登录"
          onClick={onClose}
        >
          <X size={18} />
        </button>
      </div>
      <div
        className={`qr-stage ${login.status === "expired" ? "is-expired" : ""}`}
      >
        {login.pending ? (
          <div className="qr-placeholder">
            <LoaderCircle className="spin" size={28} />
          </div>
        ) : login.code &&
          /^data:image\/(png|jpeg|gif|webp);base64,/.test(
            login.code.imageDataUrl,
          ) ? (
          <img
            src={login.code.imageDataUrl}
            alt="手机 QQ 登录二维码"
            width="220"
            height="220"
          />
        ) : (
          <div className="qr-placeholder">
            <QrCodeIcon size={52} />
            <span>二维码暂不可用</span>
          </div>
        )}
        {login.status === "expired" && (
          <span className="qr-expired">二维码已过期</span>
        )}
      </div>
      <div
        id="login-hint"
        className={`login-hint ${login.error ? "error-text" : ""}`}
        role={login.error ? "alert" : "status"}
      >
        {login.status === "scanned" && <Check size={17} aria-hidden="true" />}
        {login.error ||
          (login.pending ? "正在获取二维码…" : LOGIN_HINTS[login.status])}
      </div>
      <button
        className="button wide-button"
        disabled={login.pending}
        onClick={onRefresh}
      >
        <RefreshCw size={16} aria-hidden="true" />
        {login.pending ? "正在刷新…" : "刷新二维码"}
      </button>
      <p className="privacy-hint">
        以本次扫码确认的账号登录，关闭程序后需重新扫码
      </p>
    </dialog>
  );
}

/** 应用主工作区；短暂网络操作和长任务分别加锁，取消不会伪造已获取结果。 */
export default function App() {
  const desktop = hasDesktopBridge();
  const [view, setView] = useState<AppView>(EMPTY_VIEW);
  const [loading, setLoading] = useState(desktop);
  // 工作区初始化失败需要就地重试；只重建状态读取通道，不重置邮件草稿或本机数据。
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  const [bootstrapRevision, setBootstrapRevision] = useState(0);
  const bootstrapPending = useRef(false);
  const [action, setAction] = useState<string | null>(null);
  const actionLock = useRef(false);
  const stateEventEpoch = useRef(0);
  const [selectedGroupIds, setSelectedGroupIds] = useState<string[]>([]);
  const selectedGroupId = groupSelectionKey(selectedGroupIds);
  // 原单群查看入口仍可直接定位；多选由明确的复选框及批量操作维护。
  const setSelectedGroupId = (id: string) =>
    setSelectedGroupIds(id ? [id] : []);
  // 已保存名单按账号和群隔离；切群先切换展示，再只读加载该群记录，不沿用最近其他群的结果。
  const [groupSnapshots, setGroupSnapshots] = useState<Map<string, Snapshot>>(
    new Map(),
  );
  const [snapshotRead, setSnapshotRead] = useState<{
    key: string;
    pending: boolean;
    error: string | null;
  }>({ key: "", pending: false, error: null });
  const snapshotReadEpoch = useRef(0);
  const [snapshotReadRevision, setSnapshotReadRevision] = useState(0);
  // 调整已绑定邮件时固定使用邮件自己的版本；即使其他群已获取更新，也不能悄悄换人。
  const [audienceEditingSnapshot, setAudienceEditingSnapshot] =
    useState<Snapshot | null>(null);
  const [groupQuery, setGroupQuery] = useState("");
  const [memberQuery, setMemberQuery] = useState("");
  // 跨页定位必须等固定版本切换完成，避免被快照切换时的搜索重置覆盖。
  const [memberLocation, setMemberLocation] = useState<{
    snapshotId: string;
    qqNumber: string;
    tab: "ready" | "filtered";
  } | null>(null);
  const [manualSearchRequest, setManualSearchRequest] = useState<{
    query: string;
    revision: number;
  }>();
  const [restoreConfirmation, setRestoreConfirmation] = useState<{
    snapshot: Snapshot;
    qqNumber: string;
    reason: string;
  } | null>(null);
  const restoreDialog = useRef<HTMLDialogElement>(null);
  const [restoreError, setRestoreError] = useState("");
  // 清除搜索后归还输入焦点，方便连续查找；成员选择和当前名单版本保持不变。
  const groupSearchInput = useRef<HTMLInputElement>(null);
  const memberSearchInput = useRef<HTMLInputElement>(null);
  const [filterChoice, setFilterChoice] = useState<{
    binding: string;
    value: ListFilterPreferences;
  } | null>(null);
  const filterStorageWarning = useRef(false);
  const [excludeQuery, setExcludeQuery] = useState("");
  const [resultTab, setResultTab] = useState<"ready" | "filtered">("ready");
  const [history, setHistory] = useState<SentHistory>({
    records: [],
    total: 0,
    unresolved: [],
  });
  const [historyLoading, setHistoryLoading] = useState(false);
  const [logAvailable, setLogAvailable] = useState(true);
  const [logsOpen, setLogsOpen] = useState(false);
  const logWarningShown = useRef(false);
  const historyEpoch = useRef(0);
  const [historyQuery, setHistoryQuery] = useState("");
  const [historyText, setHistoryText] = useState("");
  const [pendingDeleteId, setPendingDeleteId] = useState<number | null>(null);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [historyReturnPage, setHistoryReturnPage] =
    useState<Exclude<WorkspacePage, "history">>("members");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(50);
  const [revealed, setRevealed] = useState(false);
  const [theme, setTheme] = useState<ThemeChoice>(getSavedTheme);
  const [activePage, setActivePage] = useState<WorkspacePage>("members");
  const updates = useUpdates(desktop);
  const [mailRunning, setMailRunning] = useState(false);
  const [manualBusy, setManualBusy] = useState(false);
  const [manualRevision, setManualRevision] = useState(0);
  const [manualConfirmation, setManualConfirmation] = useState<string[] | null>(
    null,
  );
  const manualConfirmDialog = useRef<HTMLDialogElement>(null);
  const overlayEpoch = useRef(0);
  // 失败的旧版本仍须保留以供辨认；独立重试只重读本机同一快照，不重新获取群成员。
  const [overlayRetryRevision, setOverlayRetryRevision] = useState(0);
  const [overlayRead, setOverlayRead] = useState<{
    pending: boolean;
    error: string | null;
  }>({ pending: false, error: null });
  const overlayPending = useRef(false);
  const [mailHandoffRevision, setMailHandoffRevision] = useState(0);
  // 图片登记需要恢复编辑区光标；短暂锁定导航和账号操作，避免编辑区隐藏后插入失败。
  const [mailEditorBusy, setMailEditorBusy] = useState(false);
  // Selection belongs to one immutable result. Search, pagination and page changes
  // do not clear it; fetching another snapshot does, without mutating an existing draft.
  const [selectionsBySnapshot, setSelectionsBySnapshot] = useState<
    Map<string, Set<string>>
  >(new Map());
  const [mailAudience, setMailAudience] = useState<GroupMailAudience | null>(
    null,
  );
  const [pendingAudience, setPendingAudience] =
    useState<GroupMailAudience | null>(null);
  const [memberMailStatuses, setMemberMailStatuses] = useState<
    Map<string, MemberMailStatus>
  >(new Map());
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [statusRevision, setStatusRevision] = useState(0);
  const statusEpoch = useRef(0);
  const [login, setLogin] = useState<LoginState>(EMPTY_LOGIN);
  const loginEpoch = useRef(0);
  const qrPending = useRef(false);
  const lastFetchGroupId = useRef("");
  const announcedTask = useRef("");
  const mounted = useRef(true);
  const priorBinding = useRef("");
  const running = view.task.status === "running";
  const locked =
    running ||
    mailRunning ||
    mailEditorBusy ||
    manualBusy ||
    !!action ||
    login.open;
  // 普通读取允许离开页面；维护提交过程中先等结果，避免用户把结果误认作另一页的操作。
  const navigationBusy =
    mailEditorBusy || manualBusy || (!!action && action !== "history-load");
  const exclusionRevision = Math.max(
    view.exclusionRevision ?? 0,
    manualRevision,
  );
  const accountBinding = view.account
    ? `${view.account.source}:${view.account.qqNumber}`
    : "";
  // 按账号读取、按明确操作写入，避免异步快照恢复或短暂空群列表覆盖上次筛选。
  const savedFilter = useMemo(
    () => readListFilter(accountBinding),
    [accountBinding],
  );
  const filter =
    filterChoice?.binding === accountBinding ? filterChoice.value : savedFilter;
  const excludeSent = filter.excludeSent;
  const excludeFriends = filter.excludeFriends;
  const excludeOpen = filter.open;
  // 目标群不能排除自身；已退出群也不提交，但被动刷新不改写持久偏好。
  const availableGroupIds = new Set(view.groups.map((group) => group.id));
  const excludeIds = filter.excludeIds.filter(
    (id) => !selectedGroupIds.includes(id) && availableGroupIds.has(id),
  );

  /** 只修改下次获取所用条件，既有快照及已交给写信页的收件范围仍保持不可变。 */
  function changeFilter(patch: Partial<ListFilterPreferences>) {
    const value = { ...filter, ...patch };
    setFilterChoice({ binding: accountBinding, value });
    if (
      !saveListFilter(accountBinding, value) &&
      !filterStorageWarning.current
    ) {
      filterStorageWarning.current = true;
      notify({
        message: "筛选已应用，但未能保存，下次启动可能需要重新选择。",
        tone: "error",
      });
    }
  }
  const selectedSnapshotKey = `${accountBinding}:${selectedGroupId}`;
  // 群列表可能不给人数；仅用同账号、同群已核实的 reportedCount 补未知值，不能拿过滤后人数推总数。
  const displayGroups = useMemo(
    () =>
      sortListByLabel(
        view.groups.map((group) => {
          const saved = groupSnapshots.get(`${accountBinding}:${group.id}`);
          return group.memberCount === null && saved?.reportedCount != null
            ? { ...group, memberCount: saved.reportedCount }
            : group;
        }),
        (group) => group.name.trim() || group.id,
        (group) => group.id,
      ),
    [view.groups, groupSnapshots, accountBinding],
  );
  const editingSnapshot =
    audienceEditingSnapshot?.group.id === selectedGroupId
      ? audienceEditingSnapshot
      : null;
  const liveTargets = displayGroups.filter((group) =>
    selectedGroupIds.includes(group.id),
  );
  const liveSelectedGroup =
    liveTargets.length === 1
      ? liveTargets[0]
      : liveTargets.length > 1 && liveTargets.length === selectedGroupIds.length
        ? {
            id: selectedGroupId,
            name: `已选 ${liveTargets.length} 个群聊`,
            memberCount: null,
            role: "",
          }
        : undefined;
  const selectedGroup = editingSnapshot?.group ?? liveSelectedGroup;
  const latestMatchesSelection =
    view.snapshot?.group.id === selectedGroupId &&
    `${view.snapshot.account.source}:${view.snapshot.account.qqNumber}` ===
      accountBinding;
  const snapshot =
    editingSnapshot ??
    (latestMatchesSelection ? view.snapshot : null) ??
    groupSnapshots.get(selectedSnapshotKey) ??
    null;
  const snapshotLoading =
    snapshotRead.key === selectedSnapshotKey && snapshotRead.pending;
  const snapshotError =
    snapshotRead.key === selectedSnapshotKey ? snapshotRead.error : null;
  const canFetchSelectedGroup =
    !!liveSelectedGroup &&
    !!view.account &&
    (!editingSnapshot ||
      `${editingSnapshot.account.source}:${editingSnapshot.account.qqNumber}` ===
        accountBinding);
  const selectedReferenceCount = editingSnapshot
    ? editingSnapshot.reportedCount
    : (snapshot?.reportedCount ?? selectedGroup?.memberCount ?? null);
  const selectedQqNumbers = snapshot
    ? (selectionsBySnapshot.get(snapshot.id) ?? new Set<string>())
    : new Set<string>();

  /** 勾选始终写回当前版本；翻页和切群保留已有勾选，重新获取的新版本从空选择开始。 */
  function setSelectedQqNumbers(
    update: Set<string> | ((current: Set<string>) => Set<string>),
  ) {
    if (!snapshot) return;
    setSelectionsBySnapshot((current) => {
      const next = new Map(current);
      const previous = current.get(snapshot.id) ?? new Set<string>();
      next.set(
        snapshot.id,
        typeof update === "function" ? update(previous) : update,
      );
      return next;
    });
  }
  const snapshotMembers = snapshot?.members ?? [];
  // 所有选择入口共用完整状态检查；不能在状态尚未读齐时通过表头提前选入未知收件人。
  const selectionStatusPending = snapshotMembers.some(
    (member) => !memberMailStatuses.has(member.qqNumber),
  );
  const memberSelectionDisabled =
    locked ||
    statusLoading ||
    !!statusError ||
    selectionStatusPending ||
    (snapshot?.exclusionRevision ?? 0) < exclusionRevision;
  const selectedRecipientCount = snapshotMembers.filter(
    (member) =>
      selectedQqNumbers.has(member.qqNumber) &&
      memberMailStatuses.get(member.qqNumber)?.status !== "unresolved",
  ).length;
  const unresolvedRecipientCount = snapshotMembers.filter(
    (member) =>
      memberMailStatuses.get(member.qqNumber)?.status === "unresolved",
  ).length;
  const sentRecipientCount = snapshotMembers.filter(
    (member) => memberMailStatuses.get(member.qqNumber)?.status === "sent",
  ).length;
  // 先对完整展示集合排序再搜索/分页；勾选仍绑定 QQ 号，不使用显示行号。
  const previewMembers = useMemo(
    () =>
      sortListByLabel(
        resultTab === "filtered"
          ? (snapshot?.filteredMembers ?? []).map((entry) => entry.member)
          : (snapshot?.members ?? []),
        (member) => member.qqNumber,
        (member) => member.qqNumber,
      ),
    [snapshot, resultTab],
  );
  const filteredMembers = useMemo(
    () => filterMembers(previewMembers, memberQuery),
    [previewMembers, memberQuery],
  );
  const filteredReasons = new Map(
    (snapshot?.filteredMembers ?? []).map((entry) => [
      entry.member.qqNumber,
      entry,
    ]),
  );
  const canExport =
    !!snapshot &&
    (snapshotMembers.length > 0 ||
      (snapshot.sourceCount > 0 &&
        (snapshot.filteredMembers?.length ?? 0) === snapshot.sourceCount));
  const pages = Math.max(1, Math.ceil(filteredMembers.length / pageSize));
  const currentPage = Math.min(page, pages);
  const visibleMembers = filteredMembers.slice(
    (currentPage - 1) * pageSize,
    currentPage * pageSize,
  );
  const selectableMatchedMembers = filteredMembers.filter(
    (member) =>
      memberMailStatuses.has(member.qqNumber) &&
      memberMailStatuses.get(member.qqNumber)?.status !== "unresolved",
  );
  const selectableVisibleMembers = visibleMembers.filter(
    (member) =>
      memberMailStatuses.has(member.qqNumber) &&
      memberMailStatuses.get(member.qqNumber)?.status !== "unresolved",
  );
  const allMatchedSelected =
    selectableMatchedMembers.length > 0 &&
    selectableMatchedMembers.every((member) =>
      selectedQqNumbers.has(member.qqNumber),
    );
  const allVisibleSelected =
    selectableVisibleMembers.length > 0 &&
    selectableVisibleMembers.every((member) =>
      selectedQqNumbers.has(member.qqNumber),
    );
  const filteredGroups = displayGroups.filter((group) =>
    `${group.name} ${group.id}`
      .toLocaleLowerCase()
      .includes(groupQuery.trim().toLocaleLowerCase()),
  );

  useEffect(() => {
    mounted.current = true;
    if (!desktop) return;
    bootstrapPending.current = true;
    setLoading(true);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    // 扫码或任务会发布较新的状态；初始化读取若迟到，不能覆盖已经接收的事件。
    let receivedStateEvent = false;
    void (async () => {
      try {
        const stop = await subscribeState((next) => {
          if (!disposed) {
            receivedStateEvent = true;
            stateEventEpoch.current += 1;
            setView(next);
            setBootstrapError(null);
          }
        });
        if (disposed) {
          stop();
          return;
        }
        unlisten = stop;
        const initial = await call<AppView>("get_app_state");
        if (!disposed && !receivedStateEvent) {
          setView(initial);
          setBootstrapError(null);
        }
      } catch (error) {
        // 已收到实时事件即已有可信状态；迟到初读失败不能覆盖它或让工作区误报失败。
        if (!disposed && !receivedStateEvent)
          setBootstrapError(errorText(error));
      } finally {
        if (!disposed) {
          bootstrapPending.current = false;
          setLoading(false);
        }
      }
    })();
    return () => {
      disposed = true;
      mounted.current = false;
      unlisten?.();
    };
  }, [desktop, bootstrapRevision]);

  useEffect(() => {
    if (!desktop) return;
    let stopped = false;
    const apply = (value: LogStatus) => {
      if (stopped || typeof value?.available !== "boolean") return;
      setLogAvailable(value.available);
      if (!value.available && !logWarningShown.current) {
        logWarningShown.current = true;
        notify({
          message:
            value.message ||
            "诊断日志暂时无法写入，请检查本机数据目录权限后重启。",
          tone: "error",
        });
      }
    };
    const onStatus = (event: Event) =>
      apply((event as CustomEvent<LogStatus>).detail);
    const check = () => {
      void call<LogStatus>("get_log_status")
        .then(apply)
        .catch(() =>
          apply({
            available: false,
            message: "诊断日志状态读取失败，请重启后检查。",
          }),
        );
    };
    window.addEventListener(LOG_STATUS_EVENT, onStatus);
    check();
    const timer = setInterval(check, 60_000);
    return () => {
      stopped = true;
      clearInterval(timer);
      window.removeEventListener(LOG_STATUS_EVENT, onStatus);
    };
  }, [desktop]);

  useEffect(() => {
    let disposed = false;
    void applyTheme(theme).catch((error: unknown) => {
      if (!disposed) notify({ message: errorText(error), tone: "error" });
    });
    return () => {
      disposed = true;
    };
  }, [theme]);

  // 长任务只在结束时提示一次；初次加载的旧状态和轮询重发不能产生重复浮层。
  useEffect(() => {
    const task = view.task;
    if (
      !lastFetchGroupId.current ||
      !task.id ||
      (task.status !== "failed" && task.status !== "cancelled")
    )
      return;
    const identity = `${task.id}:${task.status}`;
    if (announcedTask.current === identity) return;
    announcedTask.current = identity;
    notify({
      message:
        task.message ||
        (task.status === "cancelled" ? "获取已取消" : "获取失败，请重试"),
      tone: task.status === "failed" ? "error" : "info",
    });
  }, [view.task]);

  // 主导航固定，各工作区独立滚动；切页只让目标页回到顶部，不丢失名单和草稿。
  useEffect(() => {
    document
      .querySelector<HTMLElement>(
        ".page-frame:not([hidden]) .main-content, .page-frame.page-scroll:not([hidden])",
      )
      ?.scrollTo({ top: 0, left: 0, behavior: "instant" });
  }, [activePage]);

  useEffect(() => {
    const incoming = view.snapshot;
    if (!incoming) return;
    const key = `${incoming.account.source}:${incoming.account.qqNumber}:${incoming.group.id}`;
    setGroupSnapshots((current) => {
      if (current.get(key) === incoming) return current;
      const next = new Map(current);
      next.set(key, incoming);
      return next;
    });
  }, [view.snapshot]);

  /** 全局排除变更只重读原ID的本机派生名单；不联网、不换群版本，也不自动把恢复成员加回已选范围。 */
  useEffect(() => {
    if (!desktop || !exclusionRevision) return;
    const epoch = ++overlayEpoch.current;
    const known = [
      view.snapshot,
      audienceEditingSnapshot,
      mailAudience?.snapshot,
      pendingAudience?.snapshot,
      ...groupSnapshots.values(),
    ].filter((value): value is Snapshot => !!value);
    const ids = [
      ...new Set(
        known
          .filter((value) => (value.exclusionRevision ?? 0) < exclusionRevision)
          .map((value) => value.id),
      ),
    ];
    if (!ids.length) {
      overlayPending.current = false;
      setOverlayRead({ pending: false, error: null });
      return;
    }
    overlayPending.current = true;
    setOverlayRead((current) => ({ ...current, pending: true }));
    void Promise.allSettled(
      ids.map(
        async (snapshotId) =>
          [
            snapshotId,
            await call<Snapshot>("get_snapshot_view", { snapshotId }),
          ] as const,
      ),
    )
      .then((entries) => {
        if (!mounted.current || epoch !== overlayEpoch.current) return;
        // 某个已淘汰旧快照失败不能挡住其他名单重算；保留失败版本并提供同ID重试，发送/导出仍拒绝旧规则。
        const updated = new Map(
          entries.flatMap((entry) =>
            entry.status === "fulfilled" ? [entry.value] : [],
          ),
        );
        setOverlayRead((current) => ({
          ...current,
          error: entries.some((entry) => entry.status === "rejected")
            ? "部分名单更新失败，请重新加载后再导出或发送。"
            : null,
        }));
        setView((current) =>
          current.snapshot && updated.has(current.snapshot.id)
            ? { ...current, snapshot: updated.get(current.snapshot.id)! }
            : current,
        );
        setGroupSnapshots(
          (current) =>
            new Map(
              [...current].map(([key, value]) => [
                key,
                updated.get(value.id) ?? value,
              ]),
            ),
        );
        setAudienceEditingSnapshot((current) =>
          current ? (updated.get(current.id) ?? current) : current,
        );
        setMailAudience((current) => {
          if (!current) return current;
          const source = updated.get(current.snapshot.id);
          if (!source) return current;
          const available = new Set(
            source.members.map((member) => member.qqNumber),
          );
          return {
            ...current,
            snapshot: source,
            qqNumbers: current.qqNumbers.filter((qq) => available.has(qq)),
          };
        });
        setPendingAudience((current) => {
          if (!current) return current;
          const source = updated.get(current.snapshot.id);
          if (!source) return current;
          const available = new Set(
            source.members.map((member) => member.qqNumber),
          );
          return {
            ...current,
            snapshot: source,
            qqNumbers: current.qqNumbers.filter((qq) => available.has(qq)),
          };
        });
        setSelectionsBySnapshot(
          (current) =>
            new Map(
              [...current].map(([id, chosen]) => {
                const source = updated.get(id);
                if (!source) return [id, chosen];
                const available = new Set(
                  source.members.map((member) => member.qqNumber),
                );
                return [
                  id,
                  new Set([...chosen].filter((qq) => available.has(qq))),
                ];
              }),
            ),
        );
        setStatusRevision((value) => value + 1);
      })
      .catch(() => {
        if (epoch === overlayEpoch.current && mounted.current)
          setOverlayRead((current) => ({
            ...current,
            error: "名单更新失败，请重新加载后再导出或发送。",
          }));
      })
      .finally(() => {
        if (epoch !== overlayEpoch.current || !mounted.current) return;
        overlayPending.current = false;
        setOverlayRead((current) => ({ ...current, pending: false }));
      });
  }, [
    desktop,
    exclusionRevision,
    view.snapshot?.id,
    mailAudience?.snapshot.id,
    audienceEditingSnapshot?.id,
    pendingAudience?.snapshot.id,
    overlayRetryRevision,
  ]);

  /** 读取本机群快照不会联网；代次检查防止快速切群后把迟到结果写到当前群。 */
  useEffect(() => {
    const epoch = ++snapshotReadEpoch.current;
    if (!desktop || !view.account || !liveSelectedGroup || editingSnapshot) {
      setSnapshotRead({
        key: selectedSnapshotKey,
        pending: false,
        error: null,
      });
      return;
    }
    const key = selectedSnapshotKey;
    setSnapshotRead({ key, pending: true, error: null });
    void call<Snapshot | null>("get_group_snapshot", {
      ...(selectedGroupIds.length > 1
        ? { groupIds: selectedGroupIds }
        : { groupId: selectedGroupId }),
    })
      .then((saved) => {
        if (epoch !== snapshotReadEpoch.current || !mounted.current) return;
        if (
          saved &&
          (saved.group.id !== selectedGroupId ||
            `${saved.account.source}:${saved.account.qqNumber}` !==
              accountBinding)
        )
          throw new Error("该群的已保存名单不匹配，请重新加载。");
        if (saved)
          setGroupSnapshots((current) => {
            const previous = current.get(key);
            // 获取事件可能已带来更新版本，迟到的本机读取不能降回旧版本。
            if (
              previous &&
              new Date(previous.finishedAt).getTime() >
                new Date(saved.finishedAt).getTime()
            )
              return current;
            const next = new Map(current);
            next.set(key, saved);
            return next;
          });
      })
      .catch((error: unknown) => {
        if (epoch === snapshotReadEpoch.current && mounted.current)
          setSnapshotRead({ key, pending: false, error: errorText(error) });
      })
      .finally(() => {
        if (epoch === snapshotReadEpoch.current && mounted.current)
          setSnapshotRead((current) => ({ ...current, pending: false }));
      });
    return () => {
      ++snapshotReadEpoch.current;
    };
  }, [
    desktop,
    accountBinding,
    selectedGroupId,
    !!liveSelectedGroup,
    editingSnapshot?.id,
    snapshotReadRevision,
  ]);

  useEffect(() => {
    const binding = `${view.source ?? ""}:${view.account?.qqNumber ?? ""}`;
    if (binding !== priorBinding.current) {
      priorBinding.current = binding;
      setSelectedGroupId("");
      setAudienceEditingSnapshot(null);
      setGroupQuery("");
      setRevealed(false);
    }
    if (
      selectedGroupId &&
      selectedGroupIds.some(
        (id) => !view.groups.some((group) => group.id === id),
      ) &&
      audienceEditingSnapshot?.group.id !== selectedGroupId
    )
      setSelectedGroupIds((current) =>
        current.filter((id) => view.groups.some((group) => group.id === id)),
      );
  }, [
    view.source,
    view.account?.qqNumber,
    view.groups,
    selectedGroupId,
    audienceEditingSnapshot?.id,
  ]);

  useEffect(() => {
    setMemberQuery("");
    setPage(1);
  }, [snapshot?.id]);

  // 数据缩减时同步真实页码，避免稍后新增数据又跳回已经失效的旧末页。
  useEffect(() => {
    setPage((current) => Math.min(current, pages));
  }, [pages]);

  useEffect(() => {
    if (!memberLocation || snapshot?.id !== memberLocation.snapshotId) return;
    setMemberQuery(memberLocation.qqNumber);
    setResultTab(memberLocation.tab);
    setPage(1);
    setRevealed(true);
    memberSearchInput.current?.focus();
    setMemberLocation(null);
  }, [snapshot?.id, memberLocation]);

  // Fetch full-library recipient states, not the history page's limited preview.
  // Epoch checks discard late results after a different snapshot or history update.
  useEffect(() => {
    const epoch = ++statusEpoch.current;
    setMemberMailStatuses(new Map());
    setStatusError(null);
    if (!snapshot) {
      setStatusLoading(false);
      return;
    }
    setStatusLoading(true);
    const qqNumbers = snapshot.members.map((member) => member.qqNumber);
    // 合并名单可能超过单次状态接口的10000人上限；串行分块并一次性应用，不能误把漏查项当作可发。
    const readStatuses = async () => {
      const statuses: MemberMailStatus[] = [];
      for (let offset = 0; offset < qqNumbers.length; offset += 10000) {
        if (epoch !== statusEpoch.current || !mounted.current) return [];
        statuses.push(
          ...(await call<MemberMailStatus[]>("get_member_mail_status", {
            qqNumbers: qqNumbers.slice(offset, offset + 10000),
          })),
        );
      }
      return statuses;
    };
    void readStatuses()
      .then((statuses) => {
        if (epoch !== statusEpoch.current || !mounted.current) return;
        const next = new Map(
          statuses.map((status) => [status.qqNumber, status]),
        );
        if (qqNumbers.some((qq) => !next.has(qq)))
          throw new Error("发送状态返回不完整。");
        setMemberMailStatuses(next);
        setSelectedQqNumbers(
          (selected) =>
            new Set(
              [...selected].filter(
                (qq) => next.get(qq)?.status !== "unresolved",
              ),
            ),
        );
      })
      .catch((error: unknown) => {
        if (epoch === statusEpoch.current && mounted.current)
          setStatusError(errorText(error));
      })
      .finally(() => {
        if (epoch === statusEpoch.current && mounted.current)
          setStatusLoading(false);
      });
    return () => {
      ++statusEpoch.current;
    };
  }, [snapshot?.id, view.historyCount, statusRevision]);

  useEffect(() => {
    setExcludeQuery("");
  }, [selectedGroupId, accountBinding]);

  // 删除确认属于当前页面；切到其他工作区即撤销，不影响已提交的后台操作。
  useEffect(() => {
    if (activePage !== "history") setPendingDeleteId(null);
  }, [activePage]);

  // 一次只轮询一个请求；每次成功后再等两秒。关闭、刷新或卸载都会使旧回调失效。
  useEffect(() => {
    if (
      !login.open ||
      !login.code ||
      login.pending ||
      login.status === "expired" ||
      login.error
    )
      return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const epoch = loginEpoch.current;
    const poll = async () => {
      try {
        const result = await call<LoginPoll>("poll_web_login");
        if (disposed || epoch !== loginEpoch.current) return;
        if (result.status === "success") {
          const stateEpoch = stateEventEpoch.current;
          const next = await call<AppView>("get_app_state");
          if (disposed || epoch !== loginEpoch.current) return;
          // 同时保留二维码轮询代次与全局状态事件代次，避免迟到读覆盖新的会话状态。
          if (stateEpoch === stateEventEpoch.current) setView(next);
          setLogin(EMPTY_LOGIN);
          notify({
            message: "登录成功，请选择要获取成员的群。",
            tone: "success",
          });
          return;
        }
        setLogin((current) => ({
          ...current,
          status:
            result.status === "scanned"
              ? "scanned"
              : result.status === "expired"
                ? "expired"
                : "wait",
        }));
        if (result.status !== "expired") timer = setTimeout(poll, 2000);
      } catch (error) {
        if (!disposed && epoch === loginEpoch.current)
          setLogin((current) => ({
            ...current,
            error: errorText(error),
          }));
      }
    };
    timer = setTimeout(poll, 1200);
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [
    login.open,
    login.code,
    login.pending,
    login.status === "expired",
    login.error,
  ]);

  /**
   * 串行执行界面操作，阻止快速重复提交；后台仍必须独立验证权限与任务状态。
   * @param label 当前操作标识，仅用于按钮状态。
   * @param work 已确定的原生命令动作，失败时统一显示可恢复消息。
   * @returns true 表示本次动作完成；被其他操作占用或发生异常时返回 false，调用方保留未提交状态。
   */
  async function perform(label: string, work: () => Promise<void>) {
    if (actionLock.current) return false;
    actionLock.current = true;
    setAction(label);
    try {
      await work();
      return true;
    } catch (error) {
      if (mounted.current) {
        // 核实失败可能只是记录已变更；保留确认供重试，不能用加载错误把整页永久禁用。
        if (label.startsWith("history") && label !== "history-resolve")
          setHistoryError(errorText(error));
        else notify({ message: errorText(error), tone: "error" });
      }
      return false;
    } finally {
      actionLock.current = false;
      if (mounted.current) setAction(null);
    }
  }

  /**
   * 应用原生命令返回的状态；命令等待期间收到的后台事件优先于可能迟到的命令快照。
   * @param command 仅限当前页面硬编码的 AppView 命令名，不接受用户输入。
   * @param args 命令的命名参数，无参命令可省略。
   * @returns 状态读取完成；失败沿用调用方的统一错误提示，不清除当前可用状态。
   */
  async function readAppView(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<void> {
    const epoch = stateEventEpoch.current;
    const next = await call<AppView>(command, args);
    if (mounted.current && epoch === stateEventEpoch.current) setView(next);
  }

  /** 获取新二维码，不保存二维码图片和连接凭据；旧代次响应不能重新打开已关闭的对话框。 */
  async function startWebLogin() {
    if (qrPending.current || running || mailRunning || actionLock.current)
      return;
    qrPending.current = true;
    const epoch = ++loginEpoch.current;
    setLogin({ ...EMPTY_LOGIN, open: true, pending: true });
    try {
      const code = await call<QrCode>("start_web_login");
      if (epoch === loginEpoch.current && mounted.current) {
        setLogin({
          ...EMPTY_LOGIN,
          open: true,
          code,
        });
      }
    } catch (error) {
      if (epoch === loginEpoch.current && mounted.current) {
        setLogin({
          ...EMPTY_LOGIN,
          open: true,
          error: errorText(error),
        });
      }
    } finally {
      qrPending.current = false;
    }
  }

  /** 先使页面回调失效再通知后台取消；取消失败仍关闭二维码，不继续自动轮询。 */
  function closeWebLogin() {
    loginEpoch.current += 1;
    setLogin(EMPTY_LOGIN);
    void call<void>("cancel_web_login").catch((error: unknown) => {
      if (mounted.current) notify({ message: errorText(error), tone: "error" });
    });
  }

  /** 查看历史可从成员行或邮件结果进入，保持同一份本机核实入口。 */
  function openMailHistory() {
    if ((actionLock.current && action !== "history-load") || login.open) return;
    if (activePage !== "history") setHistoryReturnPage(activePage);
    setActivePage("history");
    // 读取途中允许离开和返回；复用正在进行的读取，不能让可点击的导航按钮毫无响应。
    if (action !== "history-load") void perform("history-load", refreshHistory);
  }

  /** 已发送过滤的处理入口定位到该账号记录，不要求用户从全量记录里再次查找。 */
  function locateMemberHistory(qqNumber: string) {
    if (navigationBusy || login.open) return;
    setHistoryQuery(qqNumber);
    openMailHistory();
  }

  /** 全局排除入口可查看，实际维护受获取/发送锁约束；不改变已发送历史。 */
  function openManualExclusions() {
    if (navigationBusy || login.open) return;
    setActivePage("manual");
  }

  /** 定位完整邮箱对应的全局规则；不自动勾选、删除或清空管理页的其他输入。 */
  function locateManualExclusion(qqNumber: string) {
    if (navigationBusy || login.open) return;
    setManualSearchRequest((current) => ({
      query: `${qqNumber}@qq.com`,
      revision: (current?.revision ?? 0) + 1,
    }));
    openManualExclusions();
  }

  function requestRestore(qqNumber: string) {
    if (
      locked ||
      !snapshot ||
      (snapshot.exclusionRevision ?? 0) < exclusionRevision
    )
      return;
    const entry = filteredReasons.get(qqNumber);
    if (!entry || entry.reason === "manual") return;
    setRestoreError("");
    setRestoreConfirmation({ snapshot, qqNumber, reason: entry.reason });
    restoreDialog.current?.showModal();
  }

  /** 保存成功才切换版本，保留此前勾选；恢复不自动扩大已确认的邮件范围。 */
  async function confirmRestore() {
    if (locked || !restoreConfirmation) return;
    const fixed = restoreConfirmation;
    await perform("restore-members", async () => {
      setRestoreError("");
      try {
        const next = await call<Snapshot>("restore_filtered_members", {
          snapshotId: fixed.snapshot.id,
          qqNumbers: [fixed.qqNumber],
          expectedRevision: fixed.snapshot.exclusionRevision ?? 0,
        });
        const available = new Set(
          next.members.map((member) => member.qqNumber),
        );
        setSelectionsBySnapshot((current) => {
          const updated = new Map(current);
          updated.set(
            next.id,
            new Set(
              [...(current.get(fixed.snapshot.id) ?? [])].filter((qq) =>
                available.has(qq),
              ),
            ),
          );
          return updated;
        });
        setView((current) =>
          current.snapshot?.id === fixed.snapshot.id
            ? { ...current, snapshot: next }
            : current,
        );
        setGroupSnapshots(
          (current) =>
            new Map(
              [...current].map(([key, value]) => [
                key,
                value.id === fixed.snapshot.id ? next : value,
              ]),
            ),
        );
        setAudienceEditingSnapshot(next);
        setMemberLocation({
          snapshotId: next.id,
          qqNumber: fixed.qqNumber,
          tab: available.has(fixed.qqNumber) ? "ready" : "filtered",
        });
        restoreDialog.current?.close();
        setRestoreConfirmation(null);
        notify({
          message: available.has(fixed.qqNumber)
            ? "已恢复到当前名单，请勾选后点击“给已选 N 人写信”更新收件范围。"
            : "已解除名单过滤，但该成员仍在全局手动排除中，请移除对应手动排除。",
          tone: "success",
        });
      } catch (error) {
        // 失败保留固定确认和可见原因，可原地重试；不要在请求完成前清空选择或关闭对话框。
        setRestoreError(errorText(error));
        throw error;
      }
    });
  }

  function requestManualExclusion(qqNumbers: string[]) {
    if (locked || !qqNumbers.length) return;
    setManualConfirmation([...new Set(qqNumbers)]);
    manualConfirmDialog.current?.showModal();
  }

  /** 成员单条/批量共用一个事务；成功后由全局revision触发同版本名单重算和勾选收缩。 */
  async function confirmManualExclusion() {
    if (!manualConfirmation || locked) return;
    await perform("manual-exclusion-add", async () => {
      const result = await call<ManualExclusionResult>(
        "add_manual_exclusions",
        { text: manualConfirmation.join("\n"), note: "" },
      );
      setManualRevision(result.view.revision);
      manualConfirmDialog.current?.close();
      setManualConfirmation(null);
      notify({
        message: `已手动排除 ${result.added} 人，重复 ${result.skipped} 人`,
        tone: "success",
      });
    });
  }

  /** 复选框、整行和键盘共用可选边界；未读状态与待核实投递都不能进入发送范围。 */
  function memberSelectable(qqNumber: string): boolean {
    return (
      resultTab === "ready" &&
      !memberSelectionDisabled &&
      memberMailStatuses.has(qqNumber) &&
      memberMailStatuses.get(qqNumber)?.status !== "unresolved"
    );
  }

  /** 只勾选最终名单中可处理的成员；待核实投递不能借整行点击重新进入收件范围。 */
  function toggleMember(qqNumber: string) {
    if (!memberSelectable(qqNumber)) return;
    setSelectedQqNumbers((current) => {
      const next = new Set(current);
      if (next.has(qqNumber)) next.delete(qqNumber);
      else next.add(qqNumber);
      return next;
    });
  }

  /** 本页操作只影响当前可见且可选择的行，隐藏页已选项始终保留。 */
  function toggleVisibleMembers() {
    if (resultTab !== "ready" || memberSelectionDisabled) return;
    setSelectedQqNumbers((current) => {
      const next = new Set(current);
      for (const member of selectableVisibleMembers) {
        if (allVisibleSelected) next.delete(member.qqNumber);
        else next.add(member.qqNumber);
      }
      return next;
    });
  }

  /** 全选/取消只作用于当前搜索匹配的可选成员，跨页生效且保留搜索范围外的选择。 */
  function toggleMatchedMembers() {
    if (resultTab !== "ready" || memberSelectionDisabled) return;
    setSelectedQqNumbers((current) => {
      const next = new Set(current);
      for (const member of selectableMatchedMembers) {
        if (allMatchedSelected) next.delete(member.qqNumber);
        else next.add(member.qqNumber);
      }
      return next;
    });
  }

  /** 将最终快照和明确范围一起交给写信页；搜索不改变“全部”的人数。 */
  function composeForMembers(scope: "all" | "selected") {
    if (!snapshot || locked || statusLoading || statusError) return;
    if ((snapshot.exclusionRevision ?? 0) < exclusionRevision) {
      notify({
        message: "手动排除已变化，请等待名单更新后再选择收件人。",
        tone: "error",
      });
      return;
    }
    const members =
      scope === "all"
        ? snapshot.members
        : snapshot.members.filter((member) =>
            selectedQqNumbers.has(member.qqNumber),
          );
    if (!members.length) return;
    if (members.length > 10000) {
      notify({
        message:
          "每次最多给10000人写信，请在成员列表勾选部分成员后再操作；完整名单仍可导出。",
        tone: "error",
      });
      return;
    }
    if (
      members.some(
        (member) =>
          !memberMailStatuses.has(member.qqNumber) ||
          memberMailStatuses.get(member.qqNumber)?.status === "unresolved",
      )
    ) {
      notify({
        message: "请先刷新发送状态，并核实结果不明的投递。",
        tone: "error",
      });
      return;
    }
    const next: GroupMailAudience = {
      snapshot,
      qqNumbers: members.map((member) => member.qqNumber),
      scope,
    };
    const unchanged =
      mailAudience?.snapshot.id === next.snapshot.id &&
      mailAudience.qqNumbers.length === next.qqNumbers.length &&
      mailAudience.qqNumbers.every((qq, index) => qq === next.qqNumbers[index]);
    if (mailAudience && !unchanged) {
      setPendingAudience(next);
      return;
    }
    setMailAudience(next);
    setMailHandoffRevision((value) => value + 1);
    setActivePage("mail");
  }

  /** 返回邮件来源群及其固定版本，恢复已确认勾选；其他群的新结果不能替代邮件原名单。 */
  function chooseMembersForMail() {
    if (navigationBusy) return;
    setActivePage("members");
    setResultTab("ready");
    setGroupQuery("");
    setMemberQuery("");
    setPage(1);
    if (mailAudience) {
      const source = mailAudience.snapshot;
      setSelectedGroupIds(snapshotGroupIds(source));
      setAudienceEditingSnapshot(source);
      setSelectionsBySnapshot((current) => {
        const next = new Map(current);
        next.set(source.id, new Set(mailAudience.qqNumbers));
        return next;
      });
    }
  }

  /** 从补充地址直接回到其原名单和对应标签，避免用户猜测群、页码或被遮蔽的号码。 */
  function locateMemberForMail(qqNumber: string) {
    if (navigationBusy || !mailAudience) return;
    chooseMembersForMail();
    setMemberLocation({
      snapshotId: mailAudience.snapshot.id,
      qqNumber,
      tab: mailAudience.snapshot.members.some(
        (member) => member.qqNumber === qqNumber,
      )
        ? "ready"
        : "filtered",
    });
  }

  /** 选择当前账号的群时退出邮件历史版本，右侧只显示该群已有记录或空态。 */
  function selectGroup(groupId: string) {
    if (locked) return;
    setAudienceEditingSnapshot(null);
    setSelectedGroupId(groupId);
    setResultTab("ready");
    setMemberQuery("");
    setPage(1);
    document.querySelector<HTMLElement>(".main-content")?.scrollTo({ top: 0 });
  }

  /** 明确勾选目标群；搜索/翻页不会改变集合，200群上限与宿主相同，超限保留原选择。 */
  function changeTargetGroups(ids: string[]) {
    if (locked) return;
    const next = [...new Set(ids)];
    if (next.length > 200) {
      notify({
        message: "一次最多选择200个目标群，请缩小搜索范围后重试。",
        tone: "error",
      });
      return;
    }
    setAudienceEditingSnapshot(null);
    setSelectedGroupIds(next);
    setResultTab("ready");
    setMemberQuery("");
    setPage(1);
  }

  /** 导出永远使用后台快照，格式明确传入；原生保存对话框取消不显示成功消息。 */
  function exportMembers(format: ExportFormat) {
    if (!snapshot || !canExport || locked) return;
    if ((snapshot.exclusionRevision ?? 0) < exclusionRevision) {
      notify({
        message: "手动排除已变化，请等待名单更新后再导出。",
        tone: "error",
      });
      return;
    }
    void perform("export", async () => {
      const result = await call<ExportResult>("export_members", {
        format,
        snapshotId: snapshot.id,
        exclusionRevision: snapshot.exclusionRevision ?? 0,
      });
      if (result.saved)
        notify({
          message: result.path ? `文件已保存：${result.path}` : "文件已保存。",
          tone: "success",
        });
    });
  }

  /** 读取 SQLite 发送历史与待核实投递；查看不改变当前名单固定快照。 */
  async function refreshHistory() {
    const epoch = ++historyEpoch.current;
    setHistoryLoading(true);
    setHistoryError(null);
    try {
      const next = await call<SentHistory>("get_sent_history");
      if (epoch === historyEpoch.current && mounted.current) setHistory(next);
    } catch (error) {
      if (epoch === historyEpoch.current && mounted.current)
        setHistoryError(errorText(error));
      throw error;
    } finally {
      if (epoch === historyEpoch.current) setHistoryLoading(false);
    }
  }

  /** 在原生对话框内导入或粘贴记录后刷新计数；导出名单不会自动写入已发送记录。 */
  function importHistory(fromFile: boolean) {
    if (running || mailRunning || historyLoading) return;
    void perform("history", async () => {
      const result = fromFile
        ? await call<ImportCounts | null>("import_sent_records")
        : await call<ImportCounts>("add_sent_records", {
            text: historyText,
          });
      if (!result) return;
      // 文件导入不应丢失用户尚未提交的手输内容；有无效行时保留文本供更正。
      if (!fromFile && result.invalid === 0) setHistoryText("");
      setPendingDeleteId(null);
      await refreshHistory();
      setStatusRevision((revision) => revision + 1);
      await readAppView("get_app_state");
      notify({
        message: `已添加 ${result.added} 条，跳过 ${result.skipped} 条，无效 ${result.invalid} 条。重新获取名单后应用新的过滤记录。`,
        tone: "success",
      });
    });
  }

  /** 人工核实只更新记录；不会发送邮件，下一次获取才应用新的名单过滤依据。 */
  async function resolveDelivery(
    id: number,
    accepted: boolean,
  ): Promise<boolean> {
    return resolveDeliveries([id], accepted, true);
  }

  /**
   * 提交二次确认中固定的范围；单条保留旧接口，批量由后台整批事务校验，不在页面逐条循环。
   * 成功后同步历史与成员状态；失败保留页面选择和确认，允许重试或主动刷新剔除失效记录。
   */
  async function resolveDeliveries(
    ids: number[],
    accepted: boolean,
    single = false,
  ): Promise<boolean> {
    if (
      running ||
      mailRunning ||
      historyLoading ||
      historyError ||
      actionLock.current
    )
      return false;
    const frozenIds = [...new Set(ids)];
    if (!frozenIds.length) return false;
    return perform("history-resolve", async () => {
      ++historyEpoch.current;
      const next = single
        ? await resolveMailDelivery(frozenIds[0], accepted)
        : await resolveMailDeliveries(frozenIds, accepted);
      setHistory(next);
      setStatusRevision((revision) => revision + 1);
      // 整批事务已经提交，后续只读计数失败不能伪装成核实失败并诱导用户再次提交。
      let stateRefreshFailed = false;
      try {
        await readAppView("get_app_state");
      } catch {
        stateRefreshFailed = true;
      }
      notify({
        message: `${
          accepted
            ? "已按人工核实标记为已发送，这不是 SMTP 送达证明。重新获取名单后应用新的过滤记录。"
            : "已确认未发送，已解除该地址的待核实限制；不会自动重发，请自行核对后操作。"
        }${stateRefreshFailed ? "页面计数暂未同步，请刷新记录。" : ""}`,
        tone: "success",
      });
    });
  }

  const statusLabel =
    snapshot?.status === "matched"
      ? "数量一致"
      : snapshot?.status === "partial"
        ? "名单不完整"
        : "待核验";

  if (!desktop)
    return (
      <main className="standalone-message">
        <div className="app-mark">
          <img src={appIcon} alt="" draggable={false} />
        </div>
        <h1>batch-send-email</h1>
        <p>请在桌面程序中运行。</p>
        <p className="secondary">
          直接打开页面无法连接 QQ 或访问本机导出功能。
        </p>
      </main>
    );

  return (
    <div
      className="app-shell"
      // 桌面界面使用自己的操作按钮；关闭浏览器默认菜单，同时保留键盘复制粘贴和原生编辑。
      onContextMenu={(event) => event.preventDefault()}
    >
      <FeedbackHost theme={theme} />
      <AppToolbar
        activePage={activePage}
        account={view.account}
        expired={view.session?.status === "expired"}
        revealed={revealed}
        version={view.version}
        theme={theme}
        navigationBusy={navigationBusy || login.open}
        locked={locked}
        mailRunning={mailRunning}
        audienceCount={mailAudience?.qqNumbers.length ?? null}
        historyCount={view.historyCount}
        logAvailable={logAvailable}
        updateAvailable={updates.result?.status === "available"}
        onNavigate={(page) => {
          if (page === "history") openMailHistory();
          else if (page === "manual") openManualExclusions();
          else setActivePage(page);
        }}
        onLogin={() => void startWebLogin()}
        onLogout={() =>
          void perform("logout", async () => {
            await readAppView("logout");
            setRevealed(false);
          })
        }
        onTheme={() =>
          setTheme(
            theme === "system"
              ? "light"
              : theme === "light"
                ? "dark"
                : "system",
          )
        }
        onLogs={() => setLogsOpen(true)}
      />
      {bootstrapError && (
        <div className="snapshot-overlay-retry" role="alert">
          <span>工作区加载失败：{bootstrapError}</span>
          <button
            className="button"
            disabled={loading || !!action}
            aria-busy={loading}
            onClick={() => {
              if (bootstrapPending.current) return;
              bootstrapPending.current = true;
              setLoading(true);
              setBootstrapRevision((revision) => revision + 1);
            }}
          >
            <RefreshCw
              size={15}
              className={loading ? "spin" : undefined}
              aria-hidden="true"
            />
            {loading ? "正在加载工作区…" : "重新加载工作区"}
          </button>
        </div>
      )}
      {overlayRead.error && (
        <div className="snapshot-overlay-retry" role="alert">
          <CircleAlert size={16} aria-hidden="true" />
          <span>{overlayRead.error}</span>
          <button
            className="button"
            disabled={overlayRead.pending}
            aria-busy={overlayRead.pending}
            onClick={() => {
              // 同步门闩覆盖首次点击到 effect 运行前的间隙，快速连点也只触发一轮重读。
              if (overlayPending.current) return;
              overlayPending.current = true;
              setOverlayRead((current) => ({ ...current, pending: true }));
              setOverlayRetryRevision((value) => value + 1);
            }}
          >
            {overlayRead.pending ? (
              <LoaderCircle size={15} className="spin" aria-hidden="true" />
            ) : (
              <RefreshCw size={15} aria-hidden="true" />
            )}
            {overlayRead.pending ? "正在加载名单…" : "重新加载名单"}
          </button>
        </div>
      )}
      {view.session?.status === "expired" && view.session.message && (
        <div className="session-message session-expired" role="status">
          <Info size={15} aria-hidden="true" />
          <span>{view.session.message}</span>
        </div>
      )}
      <section
        className="page-frame member-page"
        hidden={activePage !== "members"}
      >
        <div className="workspace">
          <aside className="group-sidebar" aria-label="选择群聊">
            <div className="section-top">
              <h2>我加入的群</h2>
              <span className="count-badge">{view.groups.length}</span>
              <button
                className="icon-button"
                disabled={locked || !view.account}
                aria-label="刷新群列表"
                onClick={() =>
                  void perform("groups", async () =>
                    readAppView("refresh_groups"),
                  )
                }
              >
                <RefreshCw
                  size={16}
                  className={action === "groups" ? "spin" : ""}
                />
              </button>
            </div>
            <div className="search-field">
              <Search size={16} aria-hidden="true" />
              <input
                ref={groupSearchInput}
                aria-label="搜索群名或群号"
                placeholder="搜索群名或群号"
                value={groupQuery}
                disabled={!view.account || running}
                onChange={(event) => setGroupQuery(event.target.value)}
              />
              {groupQuery && (
                <button
                  type="button"
                  className="icon-button search-clear"
                  aria-label="清除群搜索"
                  title="清除群搜索"
                  disabled={!view.account || running}
                  onClick={() => {
                    setGroupQuery("");
                    groupSearchInput.current?.focus();
                  }}
                >
                  <X size={14} aria-hidden="true" />
                </button>
              )}
            </div>
            {view.account && view.groups.length > 0 && (
              <div className="target-group-actions" aria-label="目标群批量选择">
                <button
                  className="button"
                  disabled={locked || !filteredGroups.length}
                  onClick={() => {
                    const all = filteredGroups.every((group) =>
                      selectedGroupIds.includes(group.id),
                    );
                    const matched = new Set(
                      filteredGroups.map((group) => group.id),
                    );
                    changeTargetGroups(
                      all
                        ? selectedGroupIds.filter((id) => !matched.has(id))
                        : [...selectedGroupIds, ...matched],
                    );
                  }}
                >
                  {filteredGroups.length > 0 &&
                  filteredGroups.every((group) =>
                    selectedGroupIds.includes(group.id),
                  )
                    ? "取消全选当前群"
                    : "全选当前群"}
                </button>
                {selectedGroupIds.length > 0 && (
                  <button
                    className="button"
                    disabled={locked}
                    onClick={() => changeTargetGroups([])}
                  >
                    清空选择
                  </button>
                )}
                <span className="secondary">
                  已选 {selectedGroupIds.length} 个
                </span>
              </div>
            )}
            <div className="group-list" tabIndex={0} aria-label="群聊列表">
              {editingSnapshot && !canFetchSelectedGroup && (
                <div
                  className="group-item selected saved-group-item"
                  aria-label="邮件已保存群"
                >
                  <span className="group-avatar" aria-hidden="true">
                    群
                  </span>
                  <span className="group-item-content">
                    <strong>{editingSnapshot.group.name || "未命名群"}</strong>
                    <span>{snapshotGroupLabel(editingSnapshot)}</span>
                    <small>邮件已保存名单</small>
                  </span>
                </div>
              )}
              {loading ? (
                <div className="sidebar-empty">
                  <LoaderCircle className="spin" size={22} />
                  <p>正在连接应用…</p>
                </div>
              ) : !view.account ? (
                <div className="sidebar-empty">
                  <Users size={27} />
                  <p>登录后查看群列表</p>
                </div>
              ) : !view.groups.length ? (
                <div className="sidebar-empty">
                  <FolderOpen size={27} />
                  <p>暂无可显示的群</p>
                </div>
              ) : !filteredGroups.length ? (
                <div className="sidebar-empty">
                  <Search size={25} />
                  <p>没有找到匹配的群</p>
                </div>
              ) : (
                filteredGroups.map((group) => (
                  <div className="group-choice" key={group.id}>
                    <input
                      type="checkbox"
                      className="target-group-checkbox"
                      aria-label={`选择目标群 ${group.name || group.id}`}
                      checked={selectedGroupIds.includes(group.id)}
                      disabled={locked}
                      onChange={(event) =>
                        changeTargetGroups(
                          event.target.checked
                            ? [...selectedGroupIds, group.id]
                            : selectedGroupIds.filter((id) => id !== group.id),
                        )
                      }
                    />
                    <button
                      className={`group-item ${selectedGroupIds.includes(group.id) && (!editingSnapshot || canFetchSelectedGroup) ? "selected" : ""}`}
                      title="查看此群；勾选左侧复选框可合并多个群"
                      disabled={locked}
                      key={group.id}
                      aria-pressed={
                        group.id === selectedGroupId &&
                        (!editingSnapshot || canFetchSelectedGroup)
                      }
                      onClick={() => selectGroup(group.id)}
                    >
                      <span className="group-avatar" aria-hidden="true">
                        {/* 取完整 Unicode 字符，避免表情或扩展汉字被截成无效代理项。 */}
                        {Array.from(group.name.trim() || "群")[0]}
                      </span>
                      <span className="group-item-content">
                        <strong>{group.name.trim() || "未命名群"}</strong>
                        <span>{group.id}</span>
                        <small>
                          {group.memberCount !== null
                            ? `${group.memberCount.toLocaleString()} 人`
                            : (groupSnapshots.get(
                                  `${accountBinding}:${group.id}`,
                                )?.sourceCount ?? 0) > 0
                              ? `已获取 ${groupSnapshots.get(`${accountBinding}:${group.id}`)?.sourceCount.toLocaleString()} 人`
                              : "人数未知"}
                          {group.role === "owner"
                            ? " · 群主"
                            : group.role === "admin"
                              ? " · 管理员"
                              : ""}
                        </small>
                      </span>
                      {group.id === selectedGroupId &&
                        (!editingSnapshot || canFetchSelectedGroup) && (
                          <span className="selection-dot" aria-hidden="true" />
                        )}
                    </button>
                  </div>
                ))
              )}
            </div>
            {selectedGroup && canFetchSelectedGroup && (
              <FilterOptions
                groups={displayGroups}
                targetId={selectedGroupId}
                targetIds={selectedGroupIds}
                excludeIds={excludeIds}
                query={excludeQuery}
                open={excludeOpen}
                excludeSent={excludeSent}
                excludeFriends={excludeFriends}
                disabled={locked}
                onOpen={() => changeFilter({ open: !excludeOpen })}
                onQuery={setExcludeQuery}
                onChange={(excludeIds) => changeFilter({ excludeIds })}
                onSent={(excludeSent) => changeFilter({ excludeSent })}
                onFriends={(excludeFriends) => changeFilter({ excludeFriends })}
              />
            )}
          </aside>
          <main className="main-content" tabIndex={0} aria-label="群成员工作区">
            {!view.account && !snapshot ? (
              <section className="welcome" aria-label="群成员空白工作区">
                <Users size={42} strokeWidth={1.2} aria-hidden="true" />
                <h2>群成员</h2>
              </section>
            ) : (
              <>
                {selectedGroup && (
                  <WorkspaceHeader
                    className="group-summary"
                    headingId="current-group-heading"
                    title={selectedGroup.name || "未命名群"}
                    meta={
                      <>
                        {selectedGroup
                          ? selectedGroupIds.length > 1
                            ? "按 QQ 号合并去重 · 各群分别核验"
                            : `群号 ${selectedGroup.id} · ${selectedReferenceCount === null ? "参考人数未知" : `参考人数 ${selectedReferenceCount.toLocaleString()} 人`}`
                          : "在左侧选择你加入的群，再开始获取成员。"}
                      </>
                    }
                    actions={
                      <div className="summary-actions">
                        {running ? (
                          <>
                            <span className="running-label">
                              <LoaderCircle size={16} className="spin" />
                              获取中
                            </span>
                            <button
                              className="button"
                              disabled={!!action}
                              onClick={() =>
                                void perform("cancel", async () =>
                                  readAppView("cancel_fetch"),
                                )
                              }
                            >
                              {action === "cancel" ? "正在取消…" : "取消获取"}
                            </button>
                          </>
                        ) : (
                          <button
                            className="button primary"
                            disabled={locked || !canFetchSelectedGroup}
                            onClick={() =>
                              void perform("fetch", async () => {
                                lastFetchGroupId.current = selectedGroupId;
                                setAudienceEditingSnapshot(null);
                                await readAppView("fetch_members", {
                                  ...(selectedGroupIds.length > 1
                                    ? { groupIds: selectedGroupIds }
                                    : { groupId: selectedGroupId }),
                                  excludeGroupIds: excludeIds,
                                  excludeSent,
                                  excludeFriends,
                                });
                              })
                            }
                          >
                            <RefreshCw size={16} />
                            {action === "fetch" ? "正在开始…" : "获取成员"}
                          </button>
                        )}
                      </div>
                    }
                  />
                )}
                {editingSnapshot && (
                  <div className="saved-audience-note" role="status">
                    <strong>正在调整邮件中的已保存名单</strong>
                    <span>
                      原账号 {editingSnapshot.account.qqNumber} ·{" "}
                      {displayTime(editingSnapshot.finishedAt)}
                    </span>
                    <span>
                      已恢复本封邮件的成员勾选，修改后请点击“给已选成员写信”确认。
                    </span>
                    {!canFetchSelectedGroup && (
                      <span>
                        该名单不属于当前可获取的群，可继续调整已保存成员；如需更新，请登录原账号后选择该群。
                      </span>
                    )}
                  </div>
                )}
                {view.task.message && running && (
                  <div className="task-strip" role="status">
                    <LoaderCircle size={15} className="spin" />
                    <span>{view.task.message}</span>
                  </div>
                )}
                {snapshotError && (
                  <div className="snapshot-read-error" role="alert">
                    <CircleAlert size={16} aria-hidden="true" />
                    <span>{snapshotError}</span>
                    <button
                      className="text-button"
                      disabled={snapshotLoading}
                      onClick={() =>
                        setSnapshotReadRevision((revision) => revision + 1)
                      }
                    >
                      重新加载名单
                    </button>
                  </div>
                )}
                {snapshot ? (
                  <>
                    <section
                      className={`result-summary result-${snapshot.status}`}
                      aria-label="本次结果与人数核验"
                    >
                      <div className="result-top">
                        <div>
                          <span className="eyebrow">
                            {running
                              ? "上次已获取"
                              : snapshot.sourceGroups?.length
                                ? "多群合并去重"
                                : "目标群已获取"}
                          </span>
                          <div className="result-count">
                            <strong>
                              {snapshot.sourceCount.toLocaleString()}
                            </strong>
                            <span>
                              {snapshot.sourceGroups?.length ? (
                                "人"
                              ) : (
                                <>
                                  /{" "}
                                  {snapshot.reportedCount === null
                                    ? "未知"
                                    : snapshot.reportedCount.toLocaleString()}{" "}
                                  人
                                </>
                              )}
                            </span>
                          </div>
                        </div>
                        <span
                          className={`status-tag status-${snapshot.status}`}
                        >
                          {snapshot.status === "matched" ? (
                            <Check size={14} />
                          ) : (
                            <CircleAlert size={14} />
                          )}
                          {statusLabel}
                        </span>
                      </div>
                      <div className="filter-counts">
                        <span>
                          群重合排除{" "}
                          <strong>
                            {snapshot.excludedCount.toLocaleString()}
                          </strong>
                        </span>
                        <span>
                          已发送过滤{" "}
                          <strong>
                            {snapshot.sentFilteredCount.toLocaleString()}
                          </strong>
                        </span>
                        {(snapshot.excludeFriends ||
                          (snapshot.friendFilteredCount ?? 0) > 0) && (
                          <span>
                            好友排除{" "}
                            <strong>
                              {snapshot.excludeFriends &&
                              snapshot.friendFilterStatus !== "completed"
                                ? "待核验"
                                : (
                                    snapshot.friendFilteredCount ?? 0
                                  ).toLocaleString()}
                            </strong>
                          </span>
                        )}
                        <span>
                          最终名单{" "}
                          <strong>
                            {snapshot.members.length.toLocaleString()}
                          </strong>
                        </span>
                        <span>
                          手动排除{" "}
                          <strong>{snapshot.manualFilteredCount ?? 0}</strong>
                        </span>
                      </div>
                      {(snapshot.exclusions ?? []).some(
                        (item) => item.status !== "matched",
                      ) && (
                        <p className="exclusion-warning">
                          排除待核验：部分排除群尚未确认完整，最终名单可能仍有遗漏过滤。
                        </p>
                      )}
                      <details
                        className="result-details"
                        open={
                          snapshot.status !== "matched" ||
                          snapshot.warnings.length > 0
                        }
                      >
                        <summary>
                          获取详情 · {displayTime(snapshot.finishedAt)}
                          {snapshot.warnings.length
                            ? ` · ${snapshot.warnings.length} 条说明`
                            : ""}
                        </summary>
                        <p className="result-message">
                          {snapshot.restoration ? "原获取说明：" : ""}
                          {snapshot.message}
                        </p>
                        {snapshot.warnings.length > 0 && (
                          <ul>
                            {snapshot.warnings.map((warning, index) => (
                              <li key={index}>{warning}</li>
                            ))}
                          </ul>
                        )}
                      </details>
                    </section>
                    {!!snapshot.sourceGroups?.length && (
                      <details className="multi-group-sources">
                        <summary>
                          来源群（{snapshot.sourceGroups.length}）
                        </summary>
                        <ul>
                          {snapshot.sourceGroups.map((source) => (
                            <li key={source.group.id}>
                              <span>
                                {source.group.name || "未命名群"}
                                <small>{source.group.id}</small>
                              </span>
                              <span>
                                {source.qqNumbers.length} /{" "}
                                {source.reportedCount ?? "未知"} 人 ·{" "}
                                {source.status === "matched"
                                  ? "数量一致"
                                  : source.status === "partial"
                                    ? "部分"
                                    : "待核验"}
                              </span>
                            </li>
                          ))}
                        </ul>
                      </details>
                    )}
                    <div
                      className="result-tabs"
                      role="tablist"
                      aria-label="名单查看范围"
                    >
                      <button
                        role="tab"
                        aria-selected={resultTab === "ready"}
                        onClick={() => {
                          setResultTab("ready");
                          setMemberQuery("");
                          setPage(1);
                        }}
                      >
                        筛选后成员 <span>{snapshot.members.length}</span>
                      </button>
                      <button
                        role="tab"
                        aria-selected={resultTab === "filtered"}
                        onClick={() => {
                          setResultTab("filtered");
                          setMemberQuery("");
                          setPage(1);
                        }}
                      >
                        已过滤 <span>{snapshot.filteredMembers.length}</span>
                      </button>
                    </div>
                    {resultTab === "ready" && (
                      <MemberMailActions
                        total={snapshot.members.length}
                        selected={selectedRecipientCount}
                        matched={selectableMatchedMembers.length}
                        allMatchedSelected={allMatchedSelected}
                        query={memberQuery}
                        unresolved={unresolvedRecipientCount}
                        sent={sentRecipientCount}
                        loading={
                          statusLoading ||
                          (!statusError && selectionStatusPending)
                        }
                        refreshing={statusLoading}
                        error={statusError}
                        disabled={
                          locked ||
                          (snapshot.exclusionRevision ?? 0) < exclusionRevision
                        }
                        onAll={() => composeForMembers("all")}
                        onSelected={() => composeForMembers("selected")}
                        onSelectMatched={toggleMatchedMembers}
                        onClear={() => setSelectedQqNumbers(new Set())}
                        onRefresh={() =>
                          setStatusRevision((revision) => revision + 1)
                        }
                        onHistory={openMailHistory}
                        onExclude={() =>
                          requestManualExclusion([...selectedQqNumbers])
                        }
                      />
                    )}
                    {snapshot.restoration && (
                      <p className="member-recovery-note" role="status">
                        本名单已手动恢复 {snapshot.restoration.members.length}{" "}
                        人（{displayTime(snapshot.restoration.adjustedAt)}
                        ）。重新获取将按原规则过滤；恢复不清除发送记录或全局排除，原获取统计保持不变。
                      </p>
                    )}
                    {resultTab === "filtered" && (
                      <p className="member-recovery-note">
                        需要保留某位成员时，可点击“恢复到名单”；手动排除请移除对应全局规则。恢复后须勾选并重新交接给写邮件页。
                      </p>
                    )}
                    <section
                      className="member-section"
                      aria-labelledby="members-heading"
                    >
                      <div className="members-toolbar">
                        <div>
                          <h2 id="members-heading">
                            {resultTab === "ready"
                              ? "成员名单"
                              : "已过滤成员与原因"}
                          </h2>
                          <span className="secondary">
                            {memberQuery
                              ? `匹配 ${filteredMembers.length.toLocaleString()} 人`
                              : `共 ${previewMembers.length.toLocaleString()} 人`}
                          </span>
                        </div>
                        <div className="member-tools">
                          <button
                            className="icon-button"
                            aria-label={
                              revealed ? "隐藏完整 QQ 号" : "显示完整 QQ 号"
                            }
                            aria-pressed={revealed}
                            onClick={() => setRevealed(!revealed)}
                          >
                            {revealed ? (
                              <EyeOff size={17} />
                            ) : (
                              <Eye size={17} />
                            )}
                          </button>
                          <div className="search-field member-search">
                            <Search size={16} aria-hidden="true" />
                            <input
                              ref={memberSearchInput}
                              aria-label="搜索成员QQ号、昵称或群名片"
                              placeholder="搜索 QQ 号、昵称或群名片"
                              value={memberQuery}
                              onChange={(event) => {
                                setMemberQuery(event.target.value);
                                setPage(1);
                              }}
                            />
                            {memberQuery && (
                              <button
                                type="button"
                                className="icon-button search-clear"
                                aria-label="清除成员搜索"
                                title="清除成员搜索"
                                onClick={() => {
                                  // 回到第一页展示完整预览，不清除搜索范围外已勾选的成员。
                                  setMemberQuery("");
                                  setPage(1);
                                  memberSearchInput.current?.focus();
                                }}
                              >
                                <X size={14} aria-hidden="true" />
                              </button>
                            )}
                          </div>
                        </div>
                      </div>
                      <div className="table-wrap">
                        <table
                          className={
                            resultTab === "ready"
                              ? "selectable-member-table"
                              : "filtered-member-table"
                          }
                        >
                          <thead>
                            <tr>
                              {resultTab === "ready" && (
                                <th className="member-select-column">
                                  <input
                                    type="checkbox"
                                    aria-label={
                                      allVisibleSelected
                                        ? "取消全选本页成员"
                                        : "全选本页成员"
                                    }
                                    checked={allVisibleSelected}
                                    ref={(element) => {
                                      if (element)
                                        element.indeterminate =
                                          !allVisibleSelected &&
                                          selectableVisibleMembers.some(
                                            (member) =>
                                              selectedQqNumbers.has(
                                                member.qqNumber,
                                              ),
                                          );
                                    }}
                                    disabled={
                                      memberSelectionDisabled ||
                                      !selectableVisibleMembers.length
                                    }
                                    onChange={toggleVisibleMembers}
                                  />
                                </th>
                              )}
                              <th className="index-column">序号</th>
                              <th>QQ 号</th>
                              <th>QQ 昵称</th>
                              <th>
                                {resultTab === "filtered"
                                  ? "过滤原因"
                                  : "群名片"}
                              </th>
                              {resultTab === "ready" && (
                                <th className="member-status-column">
                                  发送状态
                                </th>
                              )}
                              <th
                                className={
                                  resultTab === "filtered"
                                    ? "member-filter-action-column"
                                    : "member-action-column"
                                }
                              >
                                操作
                              </th>
                            </tr>
                          </thead>
                          <tbody>
                            {visibleMembers.length ? (
                              visibleMembers.map((member, index) => (
                                <tr
                                  key={member.qqNumber}
                                  tabIndex={
                                    memberSelectable(member.qqNumber)
                                      ? 0
                                      : undefined
                                  }
                                  aria-selected={
                                    resultTab === "ready"
                                      ? selectedQqNumbers.has(member.qqNumber)
                                      : undefined
                                  }
                                  data-selectable={
                                    memberSelectable(member.qqNumber) ||
                                    undefined
                                  }
                                  onClick={(event) => {
                                    // 行内控件各自执行动作；拖选昵称/号码只选择文字，不改变收件人。
                                    if (
                                      (event.target as Element).closest(
                                        "input,button,a,label,select,textarea,[role='button']",
                                      )
                                    )
                                      return;
                                    const selection = window.getSelection();
                                    if (
                                      selection &&
                                      !selection.isCollapsed &&
                                      selection.anchorNode &&
                                      event.currentTarget.contains(
                                        selection.anchorNode,
                                      )
                                    )
                                      return;
                                    toggleMember(member.qqNumber);
                                  }}
                                  onKeyDown={(event) => {
                                    if (
                                      event.target !== event.currentTarget ||
                                      event.repeat ||
                                      (event.key !== " " &&
                                        event.key !== "Enter")
                                    )
                                      return;
                                    event.preventDefault();
                                    toggleMember(member.qqNumber);
                                  }}
                                  className={
                                    resultTab === "ready" &&
                                    selectedQqNumbers.has(member.qqNumber)
                                      ? "member-selected"
                                      : undefined
                                  }
                                >
                                  {resultTab === "ready" && (
                                    <td className="member-select-column">
                                      <input
                                        type="checkbox"
                                        aria-label={`选择成员 ${member.qqNumber}`}
                                        checked={selectedQqNumbers.has(
                                          member.qqNumber,
                                        )}
                                        disabled={
                                          !memberSelectable(member.qqNumber)
                                        }
                                        onChange={() =>
                                          toggleMember(member.qqNumber)
                                        }
                                      />
                                    </td>
                                  )}
                                  <td className="row-index">
                                    {(currentPage - 1) * pageSize + index + 1}
                                  </td>
                                  <td className="qq-number">
                                    {maskedNumber(member.qqNumber, revealed)}
                                  </td>
                                  <td>
                                    {member.nickname || (
                                      <span className="empty-value">—</span>
                                    )}
                                  </td>
                                  <td>
                                    {resultTab === "filtered" ? (
                                      <span className="filter-reason">
                                        {filteredReasons.get(member.qqNumber)
                                          ?.reason === "sent"
                                          ? "已发送记录"
                                          : filteredReasons.get(member.qqNumber)
                                                ?.reason === "manual"
                                            ? "手动排除"
                                            : filteredReasons.get(
                                                  member.qqNumber,
                                                )?.reason === "friend"
                                              ? "QQ 好友"
                                              : filteredReasons.get(
                                                    member.qqNumber,
                                                  )?.reason === "group"
                                                ? "排除群重合"
                                                : "其他过滤原因"}
                                        <small>
                                          {
                                            filteredReasons.get(member.qqNumber)
                                              ?.detail
                                          }
                                        </small>
                                      </span>
                                    ) : (
                                      member.card || (
                                        <span className="empty-value">—</span>
                                      )
                                    )}
                                  </td>
                                  {resultTab === "ready" && (
                                    <td className="member-status-cell">
                                      <span
                                        className={`member-mail-status member-mail-status-${memberMailStatuses.get(member.qqNumber)?.status ?? "loading"}`}
                                      >
                                        {!memberMailStatuses.has(
                                          member.qqNumber,
                                        )
                                          ? statusError
                                            ? "状态未读取"
                                            : "读取中"
                                          : memberMailStatuses.get(
                                                member.qqNumber,
                                              )?.status === "unresolved"
                                            ? "待核实"
                                            : memberMailStatuses.get(
                                                  member.qqNumber,
                                                )?.status === "sent"
                                              ? "已有发送记录"
                                              : "无发送记录"}
                                      </span>
                                      {memberMailStatuses.get(member.qqNumber)
                                        ?.status === "unresolved" && (
                                        <button
                                          className="text-button"
                                          onClick={openMailHistory}
                                        >
                                          去核实
                                        </button>
                                      )}
                                    </td>
                                  )}
                                  {resultTab === "ready" && (
                                    <td className="member-action-cell">
                                      <button
                                        className="text-button"
                                        disabled={locked}
                                        aria-label={`排除成员 ${member.qqNumber}`}
                                        onClick={(event) => {
                                          event.stopPropagation();
                                          requestManualExclusion([
                                            member.qqNumber,
                                          ]);
                                        }}
                                      >
                                        排除
                                      </button>
                                    </td>
                                  )}
                                  {resultTab === "filtered" && (
                                    <td className="member-filter-action-cell">
                                      {filteredReasons.get(member.qqNumber)
                                        ?.reason === "manual" ? (
                                        <button
                                          className="text-button"
                                          disabled={locked}
                                          aria-label={`移除手动排除 ${member.qqNumber}`}
                                          onClick={() =>
                                            locateManualExclusion(
                                              member.qqNumber,
                                            )
                                          }
                                        >
                                          移除手动排除
                                        </button>
                                      ) : (
                                        <button
                                          className="text-button"
                                          disabled={
                                            locked ||
                                            (snapshot.exclusionRevision ?? 0) <
                                              exclusionRevision
                                          }
                                          aria-label={`恢复成员 ${member.qqNumber}`}
                                          onClick={() =>
                                            requestRestore(member.qqNumber)
                                          }
                                        >
                                          恢复到名单
                                        </button>
                                      )}
                                      {filteredReasons.get(member.qqNumber)
                                        ?.reason === "sent" && (
                                        <button
                                          className="text-button"
                                          disabled={locked}
                                          onClick={() =>
                                            locateMemberHistory(member.qqNumber)
                                          }
                                        >
                                          查看发送记录
                                        </button>
                                      )}
                                    </td>
                                  )}
                                </tr>
                              ))
                            ) : (
                              <tr>
                                <td
                                  colSpan={resultTab === "ready" ? 7 : 5}
                                  className="empty-table"
                                >
                                  没有匹配的成员，试试其他关键词。
                                </td>
                              </tr>
                            )}
                          </tbody>
                        </table>
                      </div>
                      <div className="pagination">
                        <div className="pagination-controls">
                          <label>
                            每页
                            <select
                              value={pageSize}
                              onChange={(event) => {
                                setPageSize(Number(event.target.value));
                                setPage(1);
                              }}
                            >
                              <option value={50}>50 条</option>
                              <option value={100}>100 条</option>
                            </select>
                          </label>
                          <button
                            className="icon-button"
                            disabled={currentPage <= 1}
                            aria-label="上一页成员"
                            onClick={() => setPage(currentPage - 1)}
                          >
                            <ChevronLeft size={17} />
                          </button>
                          <span>
                            {currentPage} / {pages}
                          </span>
                          <button
                            className="icon-button"
                            disabled={currentPage >= pages}
                            aria-label="下一页成员"
                            onClick={() => setPage(currentPage + 1)}
                          >
                            <ChevronRight size={17} />
                          </button>
                        </div>
                      </div>
                    </section>
                    <footer className="export-bar">
                      <div>
                        <strong>
                          {snapshot.status === "partial"
                            ? "导出已获取的名单"
                            : snapshot.status === "unverified"
                              ? "导出待核验名单"
                              : "导出本次名单"}
                        </strong>
                        <span>
                          全部 {snapshot.members.length.toLocaleString()} 人
                          {snapshot.status !== "matched"
                            ? " · 文件将标明核验状态"
                            : ""}
                        </span>
                      </div>
                      <div className="export-buttons">
                        <button
                          className="button"
                          disabled={locked || !canExport}
                          onClick={() => exportMembers("txt")}
                        >
                          <Download size={15} />
                          QQ 号 TXT
                        </button>
                        <button
                          className="button"
                          disabled={locked || !canExport}
                          onClick={() => exportMembers("csv")}
                        >
                          CSV
                        </button>
                        <button
                          className="button primary"
                          disabled={locked || !canExport}
                          onClick={() => exportMembers("xlsx")}
                        >
                          {action === "export" ? (
                            <LoaderCircle className="spin" size={16} />
                          ) : (
                            <FileSpreadsheet size={16} />
                          )}
                          导出 Excel
                        </button>
                      </div>
                    </footer>
                  </>
                ) : (
                  <section className="no-results">
                    <div className="empty-icon">
                      {snapshotLoading ? (
                        <LoaderCircle size={30} className="spin" />
                      ) : (
                        <FileSpreadsheet size={34} strokeWidth={1.4} />
                      )}
                    </div>
                    <h3>
                      {snapshotLoading
                        ? "正在加载该群名单…"
                        : selectedGroup
                          ? "该群还没有已保存名单"
                          : "请先选择群聊"}
                    </h3>
                  </section>
                )}
              </>
            )}
          </main>
        </div>
      </section>
      <section
        className="page-frame page-scroll"
        hidden={activePage !== "mail"}
        tabIndex={0}
        aria-label="写邮件工作区"
      >
        <MailComposer
          recipientHistoryRevision={statusRevision}
          exclusionRevision={exclusionRevision}
          audienceHandoffRevision={mailHandoffRevision}
          snapshot={snapshot}
          audience={mailAudience}
          onChooseMembers={chooseMembersForMail}
          onLocateMember={locateMemberForMail}
          onOpenHistory={openMailHistory}
          disabled={running || !!action || login.open}
          onRunningChange={setMailRunning}
          onEditorOperationChange={setMailEditorBusy}
          onHistoryChanged={() => {
            setStatusRevision((revision) => revision + 1);
            void Promise.all([
              readAppView("get_app_state"),
              refreshHistory(),
            ]).catch((error: unknown) =>
              notify({ message: errorText(error), tone: "error" }),
            );
          }}
        />
      </section>
      <AudienceReplaceDialog
        previous={mailAudience}
        next={pendingAudience}
        onCancel={() => setPendingAudience(null)}
        onConfirm={() => {
          if (pendingAudience) {
            setMailAudience(pendingAudience);
            setMailHandoffRevision((value) => value + 1);
            setActivePage("mail");
          }
          setPendingAudience(null);
        }}
      />
      <LoginDialog
        login={login}
        onClose={closeWebLogin}
        onRefresh={() => void startWebLogin()}
      />
      {logsOpen && <LogViewerDialog onClose={() => setLogsOpen(false)} />}
      <dialog
        ref={restoreDialog}
        className="audience-replace-dialog dialog-shell"
        aria-labelledby="restore-confirm-heading"
        onCancel={(event) => {
          event.preventDefault();
          if (action) return;
          restoreDialog.current?.close();
          setRestoreConfirmation(null);
        }}
      >
        <DialogLayout
          heading={<h2 id="restore-confirm-heading">恢复此成员到名单？</h2>}
          footer={
            <div className="audience-replace-actions dialog-footer">
              <button
                className="button"
                disabled={!!action}
                onClick={() => {
                  restoreDialog.current?.close();
                  setRestoreConfirmation(null);
                }}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={locked}
                onClick={() => void confirmRestore()}
              >
                {action === "restore-members" ? "正在恢复…" : "确认恢复"}
              </button>
            </div>
          }
        >
          <p>
            将 {restoreConfirmation?.qqNumber}@qq.com 从当前名单的
            {restoreConfirmation?.reason === "group"
              ? "排除群重合"
              : restoreConfirmation?.reason === "friend"
                ? "QQ 好友"
                : "已发送"}
            过滤中恢复，生成新版本并保留原过滤原因。其他成员的过滤规则继续生效。
          </p>
          <p>
            恢复仅限本名单，重新获取会重置。全局手动排除、发送前“跳过已发送”和待核实投递仍独立生效；已确认的邮件收件范围需重新交接。
          </p>
          {restoreConfirmation?.reason === "group" &&
            restoreConfirmation.snapshot.excludeFriends &&
            restoreConfirmation.snapshot.filteredMembers.some(
              (entry) =>
                entry.member.qqNumber === restoreConfirmation.qqNumber &&
                entry.member.friendRelation === "friend",
            ) && (
              <p>
                该成员同时是获取账号的 QQ
                好友，本次恢复也会将其作为好友过滤的名单例外保留。
              </p>
            )}
          {restoreError && (
            <p className="error-text" role="alert">
              {restoreError}
            </p>
          )}
        </DialogLayout>
      </dialog>
      <dialog
        ref={manualConfirmDialog}
        className="audience-replace-dialog dialog-shell"
        aria-labelledby="manual-confirm-heading"
        onCancel={(event) => {
          event.preventDefault();
          if (action) return;
          manualConfirmDialog.current?.close();
          setManualConfirmation(null);
        }}
      >
        <DialogLayout
          heading={<h2 id="manual-confirm-heading">确认手动排除</h2>}
          footer={
            <div className="audience-replace-actions dialog-footer">
              <button
                className="button"
                disabled={!!action}
                onClick={() => {
                  manualConfirmDialog.current?.close();
                  setManualConfirmation(null);
                }}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={locked}
                onClick={() => void confirmManualExclusion()}
              >
                确认排除
              </button>
            </div>
          }
        >
          <p>
            将 {manualConfirmation?.length ?? 0}{" "}
            人加入全局手动排除，立即应用到名单和后续邮件，不会标记已发送。
          </p>
        </DialogLayout>
      </dialog>
      <section
        className="page-frame page-scroll"
        hidden={activePage !== "manual"}
        tabIndex={0}
        aria-label="手动排除工作区"
      >
        <ManualExclusionsPage
          searchRequest={manualSearchRequest}
          active={activePage === "manual"}
          revision={exclusionRevision}
          running={running || mailRunning || !!action || mailEditorBusy}
          onChanged={(value: ManualExclusionsView) =>
            setManualRevision(value.revision)
          }
          onBusyChange={setManualBusy}
        />
      </section>
      <section
        className="page-frame page-scroll"
        hidden={activePage !== "history"}
        tabIndex={0}
        aria-label="已发送记录工作区"
      >
        <HistoryPage
          active={activePage === "history"}
          returnLabel={
            historyReturnPage === "mail"
              ? "返回写邮件"
              : historyReturnPage === "manual"
                ? "返回手动排除"
                : historyReturnPage === "about"
                  ? "返回关于"
                  : "返回成员提取"
          }
          error={historyError}
          history={history}
          loading={historyLoading}
          busy={!!action}
          navigationBusy={navigationBusy}
          running={running || mailRunning}
          revealed={revealed}
          text={historyText}
          query={historyQuery}
          pendingDeleteId={pendingDeleteId}
          onText={setHistoryText}
          onQuery={setHistoryQuery}
          onBack={() => setActivePage(historyReturnPage)}
          onRefresh={() => void perform("history-load", refreshHistory)}
          onReveal={() => setRevealed((value) => !value)}
          onImport={importHistory}
          onResolve={resolveDelivery}
          onResolveMany={(ids, accepted) => resolveDeliveries(ids, accepted)}
          onDelete={(id) => {
            if (pendingDeleteId !== id) {
              setPendingDeleteId(id);
              return;
            }
            void perform("history-delete", async () => {
              await call<void>("remove_sent_record", { id });
              setPendingDeleteId(null);
              await refreshHistory();
              setStatusRevision((revision) => revision + 1);
              await readAppView("get_app_state");
              notify({
                message: "记录已删除，重新获取名单后使用新的过滤记录。",
                tone: "success",
              });
            });
          }}
          onCancelDelete={() => setPendingDeleteId(null)}
        />
      </section>
      <section
        className="page-frame about-page"
        hidden={activePage !== "about"}
        aria-labelledby="about-heading"
      >
        <AboutPage
          version={view.version}
          result={updates.result}
          checking={updates.checking}
          error={updates.error}
          onCheck={updates.check}
        />
      </section>
    </div>
  );
}
