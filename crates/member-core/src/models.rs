//! 本地历史数据可带有未知来源标识，保留原值；群总人数缺失时不以数组长度伪造。
//! 作者：ahui。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// 已核实的当前登录账号；QQ 号为数字字符串，不使用内部 UID/OpenID。
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub qq_number: String,
    pub nickname: String,
    /// 当前读取来源为 web；旧快照保留原始标识，不能被误标为本次网页登录结果。
    pub source: String,
}

/// 从当前账号实时群列表选出的群；人数与身份缺失时不能猜测。
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    pub member_count: Option<usize>,
    pub role: String,
}

/// 成员与获取账号的好友关系；旧名单和未识别字段必须保留 unknown，不能猜成非好友。
#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FriendRelation {
    /// 本次响应按应用明确映射识别为获取账号的好友。
    Friend,
    /// 本次响应按应用明确映射识别为非好友。
    NotFriend,
    /// 来源未提供、值未识别或同一号码出现冲突，不能用于完成好友过滤。
    #[default]
    #[serde(other)]
    Unknown,
}

/// 最小成员记录；预览渲染为纯文本，导出时按文本处理昵称和号码。
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub qq_number: String,
    pub nickname: String,
    pub card: String,
    /// 仅对 Snapshot.account 有效；缺失字段的历史 JSON 自动恢复为未知关系。
    #[serde(default)]
    pub friend_relation: FriendRelation,
}

/// 读取模块的本次结果；不能混入历史名单，也不能以成员条数回填 reported_count。
#[derive(Clone, Debug)]
pub struct FetchResult {
    pub members: Vec<Member>,
    pub reported_count: Option<usize>,
    pub freshness_confirmed: bool,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

/// 当前任务从QQ网页好友列表取得的去重集合，只用于为目标群绑定关系，不序列化或持久化全量好友。
/// 提供方必须验证成功状态、完整结构、分页/总数信号和会话身份；不可靠的结果必须返回错误。
/// account绑定本次请求身份，不能与其他账号或其他任务的缓存集合混用。
pub struct FriendList {
    /// 当前好友接口请求前后核实的QQ账号与来源，不是页面传入的身份。
    pub account: Account,
    /// 已验证响应中的规范化去重数字QQ集合；只在当前任务内存中存在。
    pub qq_numbers: HashSet<String>,
}

/// 单个排除群的读取和去重摘要。removed_count 只统计本群新增排除的人，跨群不重复计数。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExclusionSummary {
    pub group: Group,
    pub acquired_count: usize,
    pub reported_count: Option<usize>,
    pub status: String,
    pub removed_count: usize,
}

/// 已过滤成员保留可解释原因，不能只给出统计数字却无法查看明细。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FilteredMember {
    pub member: Member,
    /// group、friend、sent、manual 按群重合、好友、已发送、手动排除的顺序且只计一次。
    pub reason: String,
    pub detail: String,
}

/// 用户明确恢复的当前名单例外；保留原过滤明细，不能用于撤销全局手动排除或发送历史。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRestoration {
    /// 本次调整所依据的不可变版本，便于追溯；旧版本在正常历史保留期内仍可读取。
    pub parent_snapshot_id: String,
    /// 本机调整时间（RFC3339，含时区）；不冒充重新获取 QQ 成员的时间。
    pub adjusted_at: String,
    /// 累计已恢复成员及其首次过滤原因；每人只出现一次，重新获取的新快照默认清空。
    pub members: Vec<FilteredMember>,
}

/// 多群任务中的一个真实来源；原始QQ归属只用于追溯，不随后续过滤或恢复成员改写。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSourceGroup {
    pub group: Group,
    /// 本群合法去重成员，不等于合并名单人数；用于保留跨群来源和查重依据。
    pub qq_numbers: Vec<String>,
    pub reported_count: Option<usize>,
    pub status: String,
}

/// 核验后的不可变导出快照，列表搜索不影响范围；单群沿用group，
/// 多群group仅作组合存储键，真实群及原始成员归属在source_groups中。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub account: Account,
    pub group: Group,
    /// 旧单群JSON缺省为空；多群逐群核验后合并，不能把各群参考人数之和当作去重人数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_groups: Vec<SnapshotSourceGroup>,
    pub started_at: String,
    pub finished_at: String,
    pub members: Vec<Member>,
    /// 排除前有效唯一QQ数；单群与参考人数核验，多群为并集人数，完整性由各来源分别核验。
    pub source_count: usize,
    /// 当前仍被群重合过滤的唯一 QQ 号数；恢复时扣减，不按各排除群原统计相加。
    pub excluded_count: usize,
    pub exclusions: Vec<ExclusionSummary>,
    /// 是否要求排除获取账号好友；旧快照缺失时保持关闭，不改变原名单。
    #[serde(default)]
    pub exclude_friends: bool,
    /// 当前仍被好友过滤的人数，不重复计群重合，明确恢复时扣减。
    #[serde(default)]
    pub friend_filtered_count: usize,
    /// 原始目标中关系未知的人数；历史快照默认零，成功过滤必须为零，失败不覆盖已保存快照。
    #[serde(default)]
    pub friend_unknown_count: usize,
    /// not_requested 或 completed；旧快照默认为未请求，不把未知状态当成过滤成功。
    #[serde(default = "default_friend_filter_status")]
    pub friend_filter_status: String,
    pub sent_filtered_count: usize,
    /// 本机全局手动排除叠加层新增移除人数；旧快照默认零，不影响原始获取或已发送记录。
    #[serde(default)]
    pub manual_filtered_count: usize,
    /// 应用手动排除时读取的全局版本；零兼容旧快照，不能作为实时发送授权。
    #[serde(default)]
    pub exclusion_revision: u64,
    pub filtered_members: Vec<FilteredMember>,
    /// 旧 JSON 缺失时表示从未调整；只由宿主从已保存过滤明细生成，页面不能提供成员数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restoration: Option<SnapshotRestoration>,
    pub exclude_sent: bool,
    pub reported_count: Option<usize>,
    /// matched、partial、unverified；数量一致不等于同一时刻的全量保证。
    pub status: String,
    pub message: String,
    pub duplicate_count: usize,
    pub invalid_count: usize,
    pub warnings: Vec<String>,
}

/// 兼容新增好友过滤字段之前保存的 JSON，无需修改 SQLite 表结构或重写历史数据。
fn default_friend_filter_status() -> String {
    "not_requested".into()
}

/// 可安全返回页面的错误；不得放入 Cookie、Token、响应正文或含凭据的 URL。
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl AppError {
    /// 创建经过调用方整理的错误。code 为稳定标识，message 提供可执行的下一步。
    #[track_caller]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        // 兜底覆盖提前返回的业务异常；仅保存稳定错误码，消息可能含路径或业务数据，不能自动记录。
        let caller = std::panic::Location::caller();
        let source = caller
            .file()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("unknown");
        crate::diagnostics::event(
            "WARN",
            "application",
            "error_created",
            serde_json::json!({"code":code,"source":source,"line":caller.line()}),
        );
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

/// 业务结果，错误消息已经清理，可直接显示给用户。
pub type AppResult<T> = Result<T, AppError>;

/// 只有阶段文字的反馈；总量未知时不制造百分比，不传递账号凭据。
pub type Progress = Arc<dyn Fn(String) + Send + Sync>;

/// 成员读取边界；调用方负责超时、单任务、群成员资格与前后身份核对。
#[async_trait]
pub trait MemberProvider: Send + Sync {
    /// 返回稳定来源标识，当前网页读取模块返回 web。
    fn source(&self) -> &'static str;
    /// 从当前服务核实登录账号，失败时不能借用上次保存的身份。
    async fn account(&self) -> AppResult<Account>;
    /// 返回当前账号已加入的群，缺失人数保留 None。
    async fn groups(&self) -> AppResult<Vec<Group>>;
    /// 按同一有效会话读取本任务好友集合；只有显式请求排除好友时才调用，不允许默默回退成员fr。
    /// 不支持该能力的来源返回错误，兼容已有实现；取消、身份变化、部分或异常响应不能返回集合。
    async fn friends(&self, _cancel: CancellationToken) -> AppResult<FriendList> {
        Err(AppError::new(
            "friend_list_unavailable",
            "当前无法读取 QQ 好友列表，本次名单未更新。",
        ))
    }
    /// 获取本次成员；cancel 触发后停止后续请求，不能把迟到结果混入新任务。
    async fn fetch(
        &self,
        group: &Group,
        progress: Progress,
        cancel: CancellationToken,
    ) -> AppResult<FetchResult>;
}

/// 扫码登录只向界面返回二维码图片，不把 qrsig、skey 等凭据暴露到前端。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrCode {
    pub image_data_url: String,
    pub expires_in: u32,
}

/// wait、scanned、success、expired 为正常扫码状态；接口故障通过 AppError 表达。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPoll {
    pub status: String,
    pub message: String,
}
