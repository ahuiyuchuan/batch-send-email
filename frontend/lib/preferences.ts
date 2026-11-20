/**
 * 本机非敏感操作偏好；与邮件内容、凭据、收件名单及发送计划分开保存。
 * 版本或字段损坏时逐项回退默认值，存储不可用不能阻断当前操作。
 * @author ahui
 */
export interface MailOptions {
  skipSent: boolean;
  continueOnFailure: boolean;
  sendMode: "bcc" | "individual";
  batchSize: number;
  intervalSeconds: number;
  accountSwitchIntervalSeconds: number;
  open: boolean;
}

/** 排除群只保存公开群标识；按登录来源和 QQ 账号隔离，不保存群成员或好友关系。 */
export interface ListFilterPreferences {
  excludeSent: boolean;
  excludeFriends: boolean;
  excludeIds: string[];
  open: boolean;
}

const MAIL_KEY = "qq-group-exporter-mail-options-v1";
const FILTER_KEY = "qq-group-exporter-list-filter-v1:";
export const DEFAULT_MAIL_OPTIONS: Readonly<MailOptions> = {
  skipSent: true,
  continueOnFailure: false,
  sendMode: "bcc",
  batchSize: 20,
  intervalSeconds: 900,
  accountSwitchIntervalSeconds: 60,
  open: false,
};
const DEFAULT_FILTER: Readonly<ListFilterPreferences> = {
  excludeSent: true,
  excludeFriends: false,
  excludeIds: [],
  open: false,
};

/** 只接受当前版本对象；localStorage 拒绝访问、无效 JSON 或旧版本均按首次使用处理。 */
function read(key: string): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(
      window.localStorage.getItem(key) ?? "null",
    );
    if (
      value &&
      typeof value === "object" &&
      "version" in value &&
      value.version === 1
    )
      return value as Record<string, unknown>;
  } catch {
    // 偏好不影响业务正确性；恢复默认值后仍由原有预检和宿主复核发送参数。
  }
  return {};
}

/** 严格校验整数范围；编辑中的空值、小数、越界值不能覆盖上次合法设置。 */
function integer(
  value: unknown,
  min: number,
  max: number,
  fallback: number,
): number {
  return typeof value === "number" &&
    Number.isInteger(value) &&
    value >= min &&
    value <= max
    ? value
    : fallback;
}

/** 仅读取白名单字段，未知字段不进入运行状态，也不回写邮件内容等额外数据。 */
function mailOptions(
  value: Record<string, unknown>,
  fallback: Readonly<MailOptions>,
): MailOptions {
  return {
    skipSent:
      typeof value.skipSent === "boolean" ? value.skipSent : fallback.skipSent,
    continueOnFailure:
      typeof value.continueOnFailure === "boolean"
        ? value.continueOnFailure
        : fallback.continueOnFailure,
    sendMode:
      value.sendMode === "bcc" || value.sendMode === "individual"
        ? value.sendMode
        : fallback.sendMode,
    batchSize: integer(value.batchSize, 1, 1000, fallback.batchSize),
    intervalSeconds: integer(
      value.intervalSeconds,
      1,
      3600,
      fallback.intervalSeconds,
    ),
    accountSwitchIntervalSeconds: integer(
      value.accountSwitchIntervalSeconds,
      0,
      3600,
      fallback.accountSwitchIntervalSeconds,
    ),
    open: typeof value.open === "boolean" ? value.open : fallback.open,
  };
}

/** @returns 上次合法发送选项；首次使用仍为密送20人、同账号900秒、切换60秒。 */
export function readMailOptions(): MailOptions {
  return mailOptions(read(MAIL_KEY), DEFAULT_MAIL_OPTIONS);
}

/**
 * 保存合法字段；个别数值正在编辑时保留该字段上次合法值，其余选择仍即时保存。
 * @param value 当前表单选项，不含账号凭据或邮件内容。
 * @returns false 表示存储失败，当前表单仍可使用，调用方应提醒一次。
 */
export function saveMailOptions(value: MailOptions): boolean {
  try {
    window.localStorage.setItem(
      MAIL_KEY,
      JSON.stringify({
        version: 1,
        ...mailOptions({ ...value }, readMailOptions()),
      }),
    );
    return true;
  } catch {
    return false;
  }
}

/**
 * 按来源和 QQ 账号读取上次筛选；切换目标群不清空，但目标群必须在实际使用时剔除。
 * @param binding 当前 source:qqNumber；未登录为空，不读取其他账号设置。
 * @returns 独立数组和合法开关；不采用历史快照中的旧筛选覆盖用户的新选择。
 */
export function readListFilter(binding: string): ListFilterPreferences {
  const value = binding ? read(FILTER_KEY + encodeURIComponent(binding)) : {};
  return {
    excludeSent:
      typeof value.excludeSent === "boolean"
        ? value.excludeSent
        : DEFAULT_FILTER.excludeSent,
    excludeFriends:
      typeof value.excludeFriends === "boolean"
        ? value.excludeFriends
        : DEFAULT_FILTER.excludeFriends,
    excludeIds: Array.isArray(value.excludeIds)
      ? [
          ...new Set(
            value.excludeIds.filter(
              (id): id is string =>
                typeof id === "string" && /^\d{1,32}$/.test(id),
            ),
          ),
        ].slice(0, 10000)
      : [],
    open: typeof value.open === "boolean" ? value.open : false,
  };
}

/**
 * 只在用户明确修改筛选时保存当前账号；群列表暂时为空或恢复快照不会删除保存的选择。
 * @param binding 当前 source:qqNumber；空值拒绝保存，防止产生无账号的共享排除配置。
 * @param value 当前开关与排除群标识，不能包含成员名单。
 * @returns 是否保存成功；失败仅影响下次恢复，不阻止本次获取成员。
 */
export function saveListFilter(
  binding: string,
  value: ListFilterPreferences,
): boolean {
  if (!binding) return false;
  try {
    window.localStorage.setItem(
      FILTER_KEY + encodeURIComponent(binding),
      JSON.stringify({ version: 1, ...value }),
    );
    return true;
  } catch {
    return false;
  }
}
