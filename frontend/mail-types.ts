/** 邮件页面与 Rust SMTP 命令之间的固定模型；已保存授权码只暴露可用状态。@author ahui */
import type { Snapshot } from "./types";

/**
 * 由成员工作区明确交接的收件范围。保留完整来源快照用于核对；重新获取或切群
 * 不能悄悄替换正在编辑的邮件。提交后台时只传 snapshotId 与选中 QQ 号重新校验。
 */
export interface GroupMailAudience {
  snapshot: Snapshot;
  qqNumbers: string[];
  scope: "all" | "selected";
}

/** 本机全局投递状态；待核实优先于已有成功历史，available 不表示邮箱已开通。 */
export interface MemberMailStatus {
  qqNumber: string;
  status: "available" | "sent" | "unresolved";
  origin: string;
}

/** 后台由已存快照生成的任务来源，不采信页面提供的群名或人数。 */
export interface MailGroupContext {
  snapshotId: string;
  groupId: string;
  groupName: string;
  memberCount: number;
}
export interface PublicMailSettings {
  host: string;
  port: number;
  tlsMode: "tls" | "starttls";
  username: string;
  fromAddress: string;
  fromName: string;
}

/** 公开配置与加密凭据状态；宿主不会把已保存密码或密文返回页面。 */
export interface MailSettingsView extends PublicMailSettings {
  passwordSaved: boolean;
  passwordStatus: "saved" | "missing" | "unavailable" | "mismatch";
  passwordMessage: string;
}

/** 密码只在编辑和启动发送时短暂存在内存，提交后从页面状态清空。 */
export interface MailSettings extends PublicMailSettings {
  password: string;
}

/** 独立保存的发件账号；稳定 id 用于选中与发送绑定，授权码只返回状态。 */
export interface MailSenderAccount extends MailSettingsView {
  id: string;
  name: string;
}

/** 账号的权威顺序及版本；增删改、授权码变更或重排都会使旧发送确认失效。 */
export interface MailSenderAccounts {
  revision: number;
  accounts: MailSenderAccount[];
}

/** 宿主按实际待发送地址均分的账号摘要；零人账号保留，避免暗中改变用户选择。 */
export interface MailAccountAssignment {
  accountId: string;
  name: string;
  fromAddress: string;
  recipientCount: number;
  messageCount: number;
}

/** 原生文件选择器登记的附件，只向页面提供句柄与名称，不暴露任意文件读取能力。 */
export interface MailAttachment {
  id: string;
  name: string;
  size: number;
}

/**
 * 原生后台校验后的正文图片。草稿字节在内存中用于编辑和预览，模板只读预览也复用此模型；
 * 只有明确保存模板才建立独立持久副本。发送只提交草稿 id，由后台读取登记内容并生成 CID，
 * 不能提交任意路径或外链；模板预览的 id 不具备草稿发信权限，应用后会换成新 id 与 CID。
 */
export interface MailInlineImage extends MailAttachment {
  contentId: string;
  mimeType: string;
  dataBase64: string;
}

/** 模板列表只读取检索与计数信息，不登记资源，也不包含任何收件人或发件凭据。 */
export interface MailTemplateSummary {
  id: string;
  name: string;
  subject: string;
  attachmentCount: number;
  inlineImageCount: number;
  updatedAt: string;
}

/** 原生保存对话框的结果；取消时 saved 为 false、count 为 0，不视为导出失败。 */
export interface MailTemplateExportResult {
  saved: boolean;
  count: number;
}

/** 同名模板默认另存副本；覆盖仅在宿主确认名称映射无歧义且用户再次确认后允许。 */
export type MailTemplateImportPolicy = "copy" | "skip" | "overwrite";

/** 导入预览只含名称、主题和资源计数，不把模板正文、附件字节或文件路径交给页面。 */
export interface MailTemplateImportEntry {
  name: string;
  subject: string;
  inlineImageCount: number;
  attachmentCount: number;
  /** true 表示名称存在冲突；具体覆盖映射由宿主固定，页面不得根据名称猜测模板 id。 */
  conflict: boolean;
}

/**
 * 宿主完整校验 .qqmailtemplates 文件后产生的五分钟预览；token 只存当前内存，
 * 模板库变化后必须重新选文件。文件为 UTF-8 JSON v1，最多 128 MiB，解码资源最多 80 MiB。
 */
export interface MailTemplateImportPreview {
  token: string;
  templates: MailTemplateImportEntry[];
  conflictCount: number;
  overwriteAllowed: boolean;
  /** 本次选择文件的字节数，仅用于预览说明；宿主仍独立校验文件与资源边界。 */
  totalBytes: number;
}

/** 导入事务的真实结果；templates 仅返回新增或替换的摘要，不包含跳过的旧模板。 */
export interface MailTemplateImportResult {
  added: number;
  replaced: number;
  skipped: number;
  templates: MailTemplateSummary[];
}

/**
 * 预览返回模板保存的资源；应用返回本次草稿新登记的资源和新 CID。
 * html 只允许引用 inlineImages 中的 CID，页面仍会清理后才用于编辑或沙箱预览。
 */
export interface MailTemplateContent {
  id: string;
  name: string;
  subject: string;
  html: string;
  inlineImages: MailInlineImage[];
  attachments: MailAttachment[];
}

/** 显式保存的内容快照；附件默认不保存，updateId 缺省时始终新建模板。 */
export interface MailTemplateInput {
  name: string;
  subject: string;
  html: string;
  inlineImageIds: string[];
  attachmentIds: string[];
  includeAttachments: boolean;
  updateId?: string;
}

/** SMTP 返回的收件地址和安全错误；成功表示服务器接受，不等于收件箱实际送达。 */
export interface MailFailure {
  recipient: string;
  message: string;
}

export interface MailView {
  id: string;
  status: "idle" | "running" | "completed" | "cancelled" | "failed";
  /** 运行细分阶段；旧状态未提供时只按 status 展示，不推测等待时间。 */
  phase?: "idle" | "sending" | "waiting" | "stopping";
  /**
   * 等待来源；combined 表示同账号冷却和切换间隔都尚未到期，实际等待取较晚期限而非相加。
   * 旧宿主省略时仅展示通用等待标题与宿主 message，不从倒计时长短猜测原因。
   */
  waitReason?: "accountCooldown" | "accountSwitch" | "combined" | null;
  /**
   * 本轮下一批最近可发送的 UTC Unix 毫秒期限，仅 running/waiting 时有意义，包含首轮等待。
   * 各账号可发送期限取同账号冷却与切换间隔的较晚值；宿主取本轮最近可发送账号的期限，
   * 页面不能按新配置自行重算，已就绪账号存在时不显示等待。
   */
  nextBatchAt?: number | null;
  /** 宿主单次等待标识；跳过须携带任务ID和本标识，不可复用到后续等待。 */
  waitId?: string | null;
  /** 整个冻结发送队列预计结束的UTC毫秒；由宿主按实际冷却/切换和本任务批次耗时估算，非送达承诺。 */
  estimatedCompletionAt?: number | null;
  /** 宿主发布状态时单调递增；读取不递增，用于拒绝同计数但阶段已过期的响应。 */
  stateRevision?: number;
  total: number;
  sent: number;
  failed: number;
  skipped: number;
  /** 新宿主区分已发送跳过和手动排除；旧状态只提供skipped合计。 */
  skippedSent?: number;
  manualExcluded?: number;
  /** 宿主生成的当前/待发轮次摘要，包含轮次、账号和全局批次；旧宿主可仅给收件地址。 */
  current: string;
  message: string;
  failures: MailFailure[];
  groupAudience?: MailGroupContext | null;
}

/** 后台按真实全库历史形成的本次收件计划；令牌只在当前草稿确认中使用，不写入模板。 */
export interface MailRecipientPlan {
  id: string;
  total: number;
  skipped: number;
  groupMemberCount: number;
  extraCount: number;
  manualExcluded?: number;
  excludedRecipients?: string[];
  exclusionRevision?: number;
  /** 多账号路径固定目录版本和逐账号分配；旧单账号宿主允许省略。 */
  accountRevision?: number | null;
  accountAssignments?: MailAccountAssignment[];
  /** 实际 SMTP 邮件封数；密送按每个账号独立分批，不能用总人数直接计算。 */
  totalMessageCount?: number;
  /** 随机均分只在预检时执行，实际分配在确认期间固定，发送不会再次随机。 */
  allocationMode?: "ordered" | "random";
}

/** 固定提交快照；宿主验证收件计划、正文及资源，确认后不再静默改变本次收件范围。 */
export interface MailDraft {
  recipients: string;
  subject: string;
  html: string;
  attachmentIds: string[];
  inlineImageIds: string[];
  skipSent: boolean;
  /** 默认关闭；开启时只继续后续批次，不自动重试失败批次。 */
  continueOnFailure: boolean;
  /** 五分钟有效的本次确认令牌；历史变化时宿主拒绝，要求重新读取并确认人数。 */
  recipientPlanId?: string;
  /** 本次明确选中的稳定账号 id；按目录顺序解释，显式空集合不可发送。 */
  accountIds?: string[];
  /** 与预检相同的目录版本；账号、授权码或顺序变化时宿主拒绝旧确认。 */
  accountRevision?: number;
  /** 新页面固定使用随机均分；宿主保留 ordered 以兼容已有调用方。 */
  allocationMode?: "ordered" | "random";
  sendMode: "bcc" | "individual";
  batchSize: number;
  /**
   * 同一发件账号两批之间的最短间隔，单位秒，范围 1..3600，默认 900（15 分钟）。
   * 从各账号上批结束起计时；每轮各账号最多一批，按两项间隔判断就绪，无就绪账号才等待。
   * 取消或结束保留已生效的剩余冷却；新任务降低此值不能缩短旧冷却，页面只展示宿主期限。
   */
  intervalSeconds: number;
  /**
   * 切换到不同发件账号前的最短间隔，单位秒，范围 0..3600，默认 60；0 表示关闭。
   * 从上一账号实际 SMTP 批次结束起计时，同一账号不额外等待；与同账号冷却取较晚期限，
   * 最后一批完成后不追加等待。页面只提交设置，是否需要等待由宿主判断。
   */
  accountSwitchIntervalSeconds: number;
  groupAudience?: { snapshotId: string; qqNumbers: string[] };
}
