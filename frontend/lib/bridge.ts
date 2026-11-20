/** Tauri 命令边界。浏览器预览不会自动接入网络，也不会填入虚构名单。@author ahui */
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppView, SentHistory } from "../types";

/** 日志状态不含目录或底层错误；写入失败仅影响排障能力，不能改变业务命令的结果。 */
export interface LogStatus {
  available: boolean;
  message: string;
}

/** 固定日志文件的受限尾部；内容仅供只读显示，不允许作为 HTML、命令或路径执行。 */
export interface RecentLogs {
  content: string;
  truncated: boolean;
}

/**
 * 仅允许源码登记的业务命令进入诊断，防止动态值被误当作命令写入日志。
 * get_recent_logs/get_log_status/open_log_directory 与上报命令故意不在此列，读取日志不会记录自身。
 */
const DIAGNOSTIC_COMMANDS = new Set([
  "get_app_state",
  "get_group_snapshot",
  "get_snapshot_view",
  "restore_filtered_members",
  "start_web_login",
  "poll_web_login",
  "cancel_web_login",
  "refresh_groups",
  "fetch_members",
  "cancel_fetch",
  "logout",
  // 更新诊断仅记录固定命令/结果，绝不记录URL、发布正文或网络错误原文。
  "get_update_status",
  "check_updates",
  "open_external_url",
  "export_members",
  "get_sent_history",
  "add_sent_records",
  "import_sent_records",
  "remove_sent_record",
  "resolve_mail_delivery",
  "resolve_mail_deliveries",
  "get_mail_state",
  "get_member_mail_status",
  "get_mail_settings",
  "save_mail_settings",
  "clear_mail_password",
  "mail_accounts",
  "mail_account_save",
  "mail_account_remove",
  "mail_accounts_reorder",
  "mail_account_clear_password",
  "mail_pick_attachments",
  "mail_remove_attachment",
  "mail_add_inline_image",
  "mail_pick_inline_images",
  "mail_remove_inline_image",
  "mail_template_list",
  "mail_template_preview",
  "mail_template_save",
  "mail_template_rename",
  "mail_template_duplicate",
  "mail_template_delete",
  "mail_template_apply",
  "mail_template_export",
  "mail_template_import_preview",
  "mail_template_import_commit",
  "mail_template_import_cancel",
  "mail_clear_draft_content",
  "start_mail_job",
  "prepare_mail_recipients",
  "skip_mail_wait",
  "cancel_mail_job",
  "get_manual_exclusions",
  "add_manual_exclusions",
  "remove_manual_exclusions",
]);

/** 错误码只取固定已知枚举；未知 code、异常消息、堆栈、参数与返回内容均不上传。 */
const SAFE_DIAGNOSTIC_CODES = new Set([
  "task_running",
  "mail_running",
  "not_connected",
  "session_expired",
  "web_login_expired",
  "friend_relation_unknown",
  "friend_filter_incomplete",
  "delivery_selection",
  "delivery_missing",
  "delivery_not_resolvable",
  "delivery_unresolved",
  "mail_preparation_expired",
  "no_eligible_recipients",
  "group_mail_selection",
  "group_mail_snapshot_missing",
  "group_mail_excluded_recipient",
  "group_mail_recipient_missing",
  "mail_settings",
  "mail_credentials",
  "sender_account_duplicate",
  "sender_account_limit",
  "sender_account_name",
  "sender_account_id",
  "sender_account_missing",
  "sender_account_order",
  "sender_accounts_changed",
  "mail_accounts_changed",
  "mail_allocation_mode",
  "sender_accounts_storage",
  // 冷却存储故障只上报固定错误分类；不上传身份、期限或底层数据库内容。
  "sender_cooldown_identity",
  "sender_cooldown_time",
  "sender_cooldown_storage",
  "sender_account_selection",
  "sender_account_partial_save",
  "sender_account_partial_remove",
  "sender_account_partial_clear",
  "missing_smtp_credentials",
  "smtp_password_mismatch",
  "mail_recipients",
  "mail_attachment",
  "mail_inline_image",
  "template_missing",
  "template_invalid",
  "template_import_expired",
  "template_import_token",
  "template_import_stale",
  "database_error",
  "manual_exclusion_input",
  "manual_exclusion_state",
  "manual_exclusion_changed",
  "manual_exclusion_read",
  "manual_exclusion_write",
  "manual_exclusion_refresh",
  "exclusion_revision_changed",
  "manual_excluded",
  "snapshot_restoration",
]);
const sampledCommandAt = new Map<string, number>();
export const LOG_STATUS_EVENT = "app-diagnostic-log-status";

/** 安全发布宿主日志状态；不读取报错对象，也不把日志写入失败再次记为业务错误。 */
function publishLogStatus(value: LogStatus) {
  if (typeof value?.available !== "boolean") return;
  window.dispatchEvent(
    new CustomEvent<LogStatus>(LOG_STATUS_EVENT, {
      detail: {
        available: value.available,
        message: typeof value.message === "string" ? value.message : "",
      },
    }),
  );
}

/**
 * 日志使用直接 invoke 且完全旁路 call，避免递归；不等待落盘、不改变业务 promise。
 * 仅构造固定诊断字段，不允许调用者传入 args、payload、错误消息或用户内容。
 */
function reportCommand(
  command: string,
  outcome: "started" | "succeeded" | "failed",
  durationMs: number,
  error?: unknown,
) {
  try {
    let errorCode: string | undefined;
    if (
      error &&
      typeof error === "object" &&
      "code" in error &&
      typeof error.code === "string" &&
      SAFE_DIAGNOSTIC_CODES.has(error.code)
    )
      errorCode = error.code;
    void invoke<LogStatus>("report_frontend_diagnostic", {
      input: {
        command,
        outcome,
        durationMs: Math.max(0, Math.round(durationMs)),
        ...(errorCode ? { errorCode } : {}),
      },
    })
      .then(publishLogStatus)
      .catch(() => {});
  } catch {
    // 日志通道自身不可用也不能打断实际操作，不再次上报以免形成递归风暴。
  }
}

/** 可选故障位置只接受包名与数值坐标，不能携带完整 URL、堆栈或用户内容。 */
export interface FrontendFaultLocation {
  errorType?: string;
  source?: string;
  line?: number;
  column?: number;
}
const SAFE_ERROR_TYPES = new Set([
  "Error",
  "TypeError",
  "ReferenceError",
  "SyntaxError",
  "RangeError",
  "URIError",
  "EvalError",
  "unknown",
]);

/** 未捕获错误仅记录白名单类型与包内位置，不发送 message、URL、DOM、堆栈或拒绝原因。 */
export function reportFrontendFault(
  kind: "error" | "unhandledrejection",
  location: FrontendFaultLocation = {},
): void {
  if (!hasDesktopBridge()) return;
  const errorType =
    location.errorType && SAFE_ERROR_TYPES.has(location.errorType)
      ? location.errorType
      : "unknown";
  const source =
    kind === "error" &&
    location.source &&
    /^[a-zA-Z0-9_.-]{1,100}\.js$/.test(location.source)
      ? location.source
      : undefined;
  const coordinate = (value: number | undefined) =>
    typeof value === "number" &&
    Number.isInteger(value) &&
    value > 0 &&
    value <= 2_147_483_647
      ? value
      : undefined;
  const line = kind === "error" ? coordinate(location.line) : undefined;
  const column = kind === "error" ? coordinate(location.column) : undefined;
  try {
    void invoke<LogStatus>("report_frontend_fault", {
      input: {
        kind,
        errorType,
        ...(source ? { source } : {}),
        ...(line ? { line } : {}),
        ...(column ? { column } : {}),
      },
    })
      .then(publishLogStatus)
      .catch(() => {});
  } catch {
    // 不报告日志失败本身，也不抛出新的未捕获错误。
  }
}

/** 判断是否有真实桌面 IPC；单独打开页面时仅显示运行提示。 */
export function hasDesktopBridge(): boolean {
  return isTauri();
}

/**
 * 调用已由 Rust 显式登记的命令；错误交给操作入口显示。
 * @param command 原生命令名称，不能来自群名称、昵称等外部数据。
 * @param args 该命令所需的命名参数，可省略；敏感凭据仅随本次所需命令短暂传入。
 * @returns 后台校验后返回的业务结果。
 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!hasDesktopBridge()) {
    return Promise.reject({
      message: "请在桌面程序中运行。",
      code: "desktop_required",
    });
  }
  const startedAt = performance.now();
  const diagnostic = DIAGNOSTIC_COMMANDS.has(command);
  const sampled = command.startsWith("get_") || command.startsWith("poll_");
  const recordSuccess =
    diagnostic &&
    (!sampled ||
      startedAt - (sampledCommandAt.get(command) ?? -Infinity) >= 60_000);
  if (recordSuccess) {
    sampledCommandAt.set(command, startedAt);
    reportCommand(command, "started", 0);
  }
  try {
    const result = await invoke<T>(command, args);
    if (recordSuccess)
      reportCommand(command, "succeeded", performance.now() - startedAt);
    return result;
  } catch (error) {
    // 轮询成功最多每分钟记录一次，但每次失败都留下安全类型与耗时供排障。
    if (diagnostic)
      reportCommand(command, "failed", performance.now() - startedAt, error);
    throw error;
  }
}

/** 订阅后台任务状态；返回解绑函数，防止窗口或热更新后重复响应。 */
export function subscribeState(
  onState: (state: AppView) => void,
): Promise<() => void> {
  return listen<AppView>("app-state", (event) => onState(event.payload));
}

/**
 * 提交用户核实后的投递结论；该命令不执行 SMTP，也不把人工结论称为送达。
 * @param id 待核实投递的数据库标识，由当前历史列表提供。
 * @param accepted true 表示人工确认已发送，false 表示确认未发送并允许后续重试。
 * @returns 更新后的本机发送历史与尚未核实列表；失败由调用入口显示并保留原列表。
 */
export function resolveMailDelivery(
  id: number,
  accepted: boolean,
): Promise<SentHistory> {
  return call<SentHistory>("resolve_mail_delivery", { id, accepted });
}

/**
 * 整批提交已由用户明确核实的固定投递范围；宿主在同一事务内核对全部记录后才写入结论。
 * @param ids 当前待核实列表中的正整数数据库标识，不能为空；调用方应在二次确认时固定范围。
 * @param accepted true 添加人工核实已发送记录；false 只解除重发限制，不会自动发送邮件。
 * @returns 完整更新历史；任一标识失效时整批拒绝，失败不允许前端假定部分成功。
 */
export function resolveMailDeliveries(
  ids: number[],
  accepted: boolean,
): Promise<SentHistory> {
  return call<SentHistory>("resolve_mail_deliveries", { ids, accepted });
}

/** 只提取经过后台清理的错误消息；不把对象或异常堆栈直接呈现给用户。 */
export function errorText(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  if (typeof error === "string" && error.trim()) {
    try {
      const parsed: unknown = JSON.parse(error);
      if (parsed && typeof parsed === "object" && "message" in parsed)
        return errorText(parsed);
    } catch {
      // Tauri 也可能直接返回已经清理过的字符串，不强制要求 JSON。
    }
    return error;
  }
  return "操作未完成，请检查连接后重试。";
}
