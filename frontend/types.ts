/** 与 Rust camelCase 序列化模型对应的页面数据；不包含 Cookie、Token 等凭据。@author ahui */
export type Source = "web";

export interface Account {
  qqNumber: string;
  nickname: string;
  /** 保留获取时的原始来源标识；旧快照中的未知标识只作历史证明，不提供连接入口。 */
  source: string;
}

export interface Group {
  id: string;
  name: string;
  memberCount: number | null;
  role: string;
}

export interface Member {
  qqNumber: string;
  nickname: string;
  card: string;
  /** 仅相对于获取快照中的账号；旧快照缺失或未识别关系均不能当成非好友。 */
  friendRelation?: "friend" | "not_friend" | "unknown";
}

/** 每个排除群的原获取核验结果；removedCount 是原新增排除数，不随本名单恢复改写。 */
export interface Exclusion {
  group: Group;
  acquiredCount: number;
  reportedCount: number | null;
  status: string;
  removedCount: number;
}

/** 保留过滤原因，用户可以核对为何某位成员未进入最终名单。 */
export interface FilteredMember {
  member: Member;
  reason: "group" | "sent" | "friend" | "manual";
  detail: string;
}

/** 获取或明确恢复生成的固定版本；搜索和翻页只影响展示，不能改写此对象。 */
export interface Snapshot {
  id: string;
  account: Account;
  group: Group;
  /** 多群组合的实际来源；旧单群缺省。qqNumbers保留本群原始归属，合并结果按QQ号去重。 */
  sourceGroups?: {
    group: Group;
    qqNumbers: string[];
    reportedCount: number | null;
    status: "matched" | "partial" | "unverified";
  }[];
  startedAt: string;
  finishedAt: string;
  members: Member[];
  sourceCount: number;
  excludedCount: number;
  exclusions: Exclusion[];
  sentFilteredCount: number;
  /** 当前全局手动排除派生数量；旧快照按0兼容，不改变原群/好友/已发过滤原因。 */
  manualFilteredCount?: number;
  exclusionRevision?: number;
  filteredMembers: FilteredMember[];
  /** 当前名单的明确恢复例外；旧快照缺省。记录不重复计入人数，不代表解除全局排除或发送限制。 */
  restoration?: {
    parentSnapshotId: string;
    adjustedAt: string;
    members: FilteredMember[];
  };
  excludeSent: boolean;
  /** 旧快照没有好友字段时视为未请求过滤，不补造当时的好友关系。 */
  excludeFriends?: boolean;
  friendFilteredCount?: number;
  friendUnknownCount?: number;
  friendFilterStatus?: "not_requested" | "completed";
  reportedCount: number | null;
  status: "matched" | "partial" | "unverified";
  message: string;
  duplicateCount: number;
  invalidCount: number;
  warnings: string[];
}

export interface TaskView {
  id: string;
  status: "idle" | "running" | "completed" | "cancelled" | "failed";
  message: string;
}

/** 当前进程的 QQ 登录状态；关闭程序即释放，不保存期限或恢复凭据。 */
export interface SessionView {
  status: "signed_out" | "active" | "expired";
  message: string;
}

export interface AppView {
  version: string;
  source: Source | null;
  account: Account | null;
  groups: Group[];
  snapshot: Snapshot | null;
  task: TaskView;
  runtime: string;
  historyCount: number;
  exclusionRevision?: number;
  databasePath: string;
  session: SessionView;
}

/** 本机全局排除项。QQ号会归一化到数字QQ邮箱，普通邮箱不能反推QQ身份。 */
export interface ManualExclusion {
  id: number;
  email: string;
  qqNumber: string;
  note: string;
  createdAt: string;
}
export interface ManualExclusionsView {
  revision: number;
  records: ManualExclusion[];
}
/** 批量维护由宿主事务处理，计数只在成功后展示，失败不乐观改列表。 */
export interface ManualExclusionResult {
  view: ManualExclusionsView;
  added: number;
  skipped: number;
  invalid: number;
  removed: number;
}

export interface QrCode {
  imageDataUrl: string;
  expiresIn: number;
}

export interface LoginPoll {
  status: "wait" | "scanned" | "success" | "expired";
  message: string;
}

export type ExportFormat = "xlsx" | "csv" | "txt";

export interface ExportResult {
  saved: boolean;
  path: string | null;
}

/** SMTP 接受或用户维护的已发送标记；人工核实不能作为服务器接受证明。 */
export interface SentRecord {
  id: number;
  qqNumber: string;
  email: string;
  sentAt: string;
  origin: string;
  note: string;
  /** 发送或人工核实来源保留的邮箱快照；旧、独立人工与导入记录缺省/空串表示未记录，不回填现账号。 */
  senderAddress?: string;
  /** 本次投递开始时的账号名称；后续账号改名、删除不影响此快照，缺省/空串表示未记录。 */
  senderName?: string;
}

/** SMTP 结果不明确的收件人；核实前后台禁止再次向该地址投递。 */
export interface UnresolvedDelivery {
  id: number;
  email: string;
  updatedAt: string;
  message: string;
  /** 本次不明投递尝试的发件邮箱快照，供人工到对应发件箱核对；旧记录缺省/空串不猜测归属。 */
  senderAddress?: string;
  /** 尝试开始时冻结的账号名称；独立于现有账号目录，缺省/空串表示未记录。 */
  senderName?: string;
}

export interface SentHistory {
  records: SentRecord[];
  total: number;
  unresolved: UnresolvedDelivery[];
}

export interface ImportCounts {
  added: number;
  skipped: number;
  invalid: number;
}
