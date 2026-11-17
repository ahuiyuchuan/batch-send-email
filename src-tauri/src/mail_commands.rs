//! SMTP 任务、原生附件选择和发送状态。作者：ahui。
//! 多收件人默认 BCC；服务器明确接受才记成功，不明批次禁止自动重发，默认停止后续批次。
//! 用户可为本次任务选择失败后继续未尝试的批次；该选择不改变失败批次的 unknown 状态。
//! 每轮每账号最多一批，同轮优先发送已就绪账号；只有本轮全部剩余账号冷却时才等最早截止。
//! 冷却按规范化发件邮箱跨任务保存，取消/结束不清零；重启恢复SQLite期限，运行期使用单调时钟。
//! 切换不同发件邮箱另受默认60秒的进程内间隔约束；与账号冷却取较晚期限，不叠加等待。

use crate::{
    history_commands::update_count,
    recipient_commands::{self, PreparedAccountSelection},
    state::AppState,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use member_core::smtp_credential_store::SmtpCredentialBinding;
use member_core::{
    database::{Database, GroupMailAudience, MemberMailStatus, PreparedMailRecipients},
    diagnostics,
    mail::{self, MailAttachment, MailSendContext, MailSender, MailSettings},
    models::*,
    sender_cooldowns::normalize_sender_cooldown_identity,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;

/// 对页面返回的单个不确定收件人，错误信息已清理，不包含SMTP响应正文或密码。
#[derive(Clone, Serialize, Default)]
pub struct MailFailure {
    recipient: String,
    message: String,
}

/// 当前待发账号尚未就绪的约束来源；combined表示两种期限都在未来，实际只等较晚者。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MailWaitReason {
    AccountCooldown,
    AccountSwitch,
    Combined,
}

/// 邮件任务人数按实际收件人计数；一批可有多个收件人，但每人只计一次。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailView {
    pub id: String,
    pub status: String,
    /// idle/sending/waiting/stopping；展示阶段独立于任务终态，不能据此推断 SMTP 投递结果。
    pub phase: String,
    /// 本轮下一批最早可发的UTC Unix毫秒；目标账号自身冷却与不同邮箱切换间隔取较晚值。
    /// 真正等待使用共享单调期限；首轮也可继承上个任务的约束，重启只恢复持久账号冷却。
    pub next_batch_at: Option<i64>,
    /// 当前冻结队列预计处理完的UTC Unix毫秒，只用于展示，不影响调度、冷却或投递判定。
    /// 包含剩余账号冷却、切换间隔和批次耗时；停止/终态清空。旧客户端可忽略此新增字段。
    pub estimated_completion_at: Option<i64>,
    /// 仅waiting时存在；旧客户端可忽略，发送、停止或终态立即清除，避免显示过期等待原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_reason: Option<MailWaitReason>,
    /// 当前单次等待的随机标识；仅waiting可用，不能用旧任务/旧轮次请求跳过新的等待。
    pub wait_id: Option<String>,
    /// 单次跳过信号仅存在内存，不序列化；不等于取消任务，不写冷却池或SQLite。
    #[serde(skip)]
    wait_skip: Option<CancellationToken>,
    /// 每次发布时在 mail 锁内递增，创建下一任务沿用版本；前端丢弃迟到事件，查询不消耗版本。
    pub state_revision: u64,
    pub total: usize,
    pub sent: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 成功历史过滤人数；skipped 为此值加手动排除总数，保持任务总人数守恒。
    pub skipped_sent: usize,
    /// 手动排除仅表示用户不向这些地址发送，不等于成功投递，也不会写入已发送历史。
    pub manual_excluded: usize,
    pub current: String,
    pub message: String,
    pub failures: Vec<MailFailure>,
    /// 来源由已保存名单验证后产生；独立手工邮件为 None，不能用当前页面群名替代。
    pub group_audience: Option<GroupMailAudience>,
}

impl Default for MailView {
    fn default() -> Self {
        Self {
            id: String::new(),
            status: "idle".into(),
            phase: "idle".into(),
            next_batch_at: None,
            estimated_completion_at: None,
            wait_reason: None,
            wait_id: None,
            wait_skip: None,
            state_revision: 0,
            total: 0,
            sent: 0,
            failed: 0,
            skipped: 0,
            skipped_sent: 0,
            manual_excluded: 0,
            current: String::new(),
            message: "准备好收件人和内容后开始发送。".into(),
            failures: vec![],
            group_audience: None,
        }
    }
}

/// SQLite 只持久化这些公开设置；授权码独立加密，不能出现在此类型或页面响应中。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicMailSettings {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls_mode: String,
    pub(crate) username: String,
    pub(crate) from_address: String,
    pub(crate) from_name: String,
}

impl Default for PublicMailSettings {
    fn default() -> Self {
        Self {
            host: "smtp.qq.com".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: String::new(),
            from_address: String::new(),
            from_name: String::new(),
        }
    }
}

impl PublicMailSettings {
    /// 只在宿主内存中组合认证输入；传入空串可构造匹配键，不能直接用于 SMTP 发送。
    pub(crate) fn with_password(&self, password: String) -> MailSettings {
        MailSettings {
            host: self.host.clone(),
            port: self.port,
            tls_mode: self.tls_mode.clone(),
            username: self.username.clone(),
            password,
            from_address: self.from_address.clone(),
            from_name: self.from_name.clone(),
        }
    }
}

/// 保留原公开字段的平铺协议；只返回凭据是否可用，不返回密码、密文或文件内容。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSettingsView {
    #[serde(flatten)]
    pub(crate) settings: PublicMailSettings,
    pub(crate) password_saved: bool,
    /// saved/missing/mismatch/unavailable；读取失败仍允许用户查看并重填公开配置。
    pub(crate) password_status: &'static str,
    pub(crate) password_message: &'static str,
}

/// 原生选择或模板应用的草稿附件；页面只引用句柄，不能传任意文件路径。
/// 普通编辑期间仅驻内存，只有明确保存模板时才复制到独立 SQLite 资源行。
#[derive(Clone)]
pub struct StoredAttachment {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    /// 正文图片与普通附件共享内存仓库，但句柄类别不可互换。
    pub(crate) inline: Option<mail::InlineImageMetadata>,
}

#[derive(Serialize)]
pub struct AttachmentView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) size: usize,
}

/// 图片仅返回受限的本机字节供预览；不含路径，不写入浏览器存储。
/// 明确保存模板时由模板命令另存独立副本，本视图仍只是当前草稿或只读预览。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineImageView {
    pub(crate) id: String,
    pub(crate) content_id: String,
    pub(crate) name: String,
    pub(crate) size: usize,
    pub(crate) mime_type: String,
    pub(crate) data_base64: String,
}

/// 删除图片后暂存字节以支持编辑器撤销；上限独立于实际邮件的 10 张 / 18 MiB。
const MAX_CACHED_IMAGES: usize = 64;
const MAX_CACHED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// 用户明确确认后提交的草稿，当前版本支持BCC分批或逐人发送。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailDraft {
    recipients: String,
    subject: String,
    html: String,
    attachment_ids: Vec<String>,
    /// 只提交正文当前仍引用的图片；缺省为空，兼容不含图片的既有调用。
    #[serde(default)]
    inline_image_ids: Vec<String>,
    skip_sent: bool,
    /// 仅本次任务生效，默认失败停止；开启后跳过当前失败批次继续后续，不重试/拆分或保存到模板。
    #[serde(default)]
    continue_on_failure: bool,
    /// 可空以兼容旧调用；存在时复用发送前已确认的收件范围，设置校验失败不会消费该计划。
    #[serde(default)]
    recipient_plan_id: Option<String>,
    /// 每账号上批结果处理完成后默认间隔900秒；显式1—3600秒有效，已生效的旧截止不缩短。
    /// 同轮优先已就绪账号，不因其他邮箱冷却而等待；不作为邮件服务商限额承诺。
    #[serde(default = "default_interval")]
    interval_seconds: u64,
    /// 切换不同规范化From邮箱时，从上一实际SMTP批次结果处理完成起等待的秒数。
    /// 缺省60、允许0—3600，0关闭；与自身冷却取较晚期限，不相加；仅在本进程跨任务保留上次完成时刻。
    #[serde(default = "default_account_switch_interval")]
    account_switch_interval_seconds: u64,
    #[serde(default = "default_mode")]
    send_mode: String,
    #[serde(default = "default_batch")]
    batch_size: usize,
    /// 可空以兼容手工草稿；存在时必须通过本机不可变名单核验，不能静默忽略。
    #[serde(default)]
    group_audience: Option<GroupAudienceSelection>,
    /// 缺省兼容临时单账号配置；显式提供时仅从已保存目录取账号和凭据，禁止静默回退。
    #[serde(default)]
    account_ids: Option<Vec<String>>,
    /// 与确认计划绑定的目录版本，公开配置、顺序及授权码任何变化均要求重新确认。
    #[serde(default)]
    account_revision: Option<u64>,
    /// 新界面明确提交random；旧IPC省略时保留ordered，随机只发生在预检并随计划冻结。
    #[serde(default = "default_allocation_mode")]
    allocation_mode: String,
}

/// 页面只提交快照标识和所选号码，群名、账号与人数由后台查库决定。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupAudienceSelection {
    pub(crate) snapshot_id: String,
    pub(crate) qq_numbers: Vec<String>,
}

fn default_mode() -> String {
    "bcc".into()
}
fn default_batch() -> usize {
    20
}

/// 默认间隔与页面初始值共用协议；只对缺少 intervalSeconds 的草稿生效。
fn default_interval() -> u64 {
    900
}

/// 新旧IPC省略此字段均使用60秒，显式0保留即时切换能力；不改变已持久化的账号冷却期限。
fn default_account_switch_interval() -> u64 {
    60
}

/// u64反序列化已拒绝负数和非整数；宿主仍必须拒绝超过产品范围的值，且在任何任务/网络副作用前执行。
fn validate_account_switch_interval(seconds: u64) -> AppResult<()> {
    if seconds <= 3600 {
        Ok(())
    } else {
        Err(AppError::new(
            "mail_account_switch_interval",
            "切换账号间隔应为0到3600秒的整数，0表示关闭。",
        ))
    }
}

/// 缺省顺序分配只用于兼容既有IPC，新界面使用随机均分并明确显示确认结果。
fn default_allocation_mode() -> String {
    "ordered".into()
}

/// 版本与快照在同一锁内生成；事件发送放到锁外，避免 IPC 阻塞状态查询或取消按钮。
fn mail_snapshot_for_publish(state: &AppState) -> MailView {
    let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
    // 不回绕到旧版本。达到 u64 上限需要同进程发布超过 1.8×10^19 次，实际运行不可达。
    view.state_revision = view.state_revision.saturating_add(1);
    view.clone()
}

fn publish_mail(app: &AppHandle, state: &AppState) -> MailView {
    let view = mail_snapshot_for_publish(state);
    let _ = app.emit("mail-state", &view);
    view
}

/// 仅在运行且未进入停止阶段时切换为发送；取消与批次切换争锁时不得复活已停止的倒计时。
fn begin_batch_view(view: &mut MailView, index: usize, total: usize, count: usize) -> bool {
    if view.status != "running"
        || view.phase == "stopping"
        || index == 0
        || index > total
        || count == 0
    {
        return false;
    }
    view.phase = "sending".into();
    view.next_batch_at = None;
    view.wait_reason = None;
    view.wait_id = None;
    view.wait_skip = None;
    view.current = format!("第 {index}/{total} 批，{count} 位收件人");
    view.message = "正在等待邮件服务器确认…".into();
    true
}

/// 等待展示指向本轮最早就绪的剩余账号；只有本轮无任何可发账号才进入此状态。
/// deadline由共享单调期限剩余换算为UTC，不能为已就绪账号或尾批之后凭空创建倒计时。
fn wait_for_batch_view(
    view: &mut MailView,
    next_index: usize,
    total: usize,
    count: usize,
    deadline: i64,
    reason: MailWaitReason,
) -> bool {
    if view.status != "running"
        || view.phase == "stopping"
        || next_index == 0
        || next_index > total
        || count == 0
    {
        return false;
    }
    view.phase = "waiting".into();
    view.wait_id = Some(uuid::Uuid::new_v4().to_string());
    view.wait_skip = Some(CancellationToken::new());
    view.next_batch_at = Some(deadline);
    view.wait_reason = Some(reason);
    view.current = format!("第 {next_index}/{total} 批，{count} 位收件人");
    view.message = match reason {
        MailWaitReason::AccountCooldown => "本轮剩余账号正在冷却，等待最早可发送的账号…",
        MailWaitReason::AccountSwitch => "正在等待切换发件账号的间隔…",
        MailWaitReason::Combined => "账号冷却与切换间隔同时生效，等待较晚的可发送时间…",
    }
    .into();
    true
}

/// 取消立即撤销展示截止时间；发送中的一封仍按现有逻辑等待明确结果，等待中的下一批不再发送。
fn stop_mail_view(view: &mut MailView) {
    if view.status == "running" && view.phase != "stopping" {
        let waiting = view.phase == "waiting";
        view.phase = "stopping".into();
        view.next_batch_at = None;
        view.estimated_completion_at = None;
        view.wait_reason = None;
        view.wait_id = None;
        view.wait_skip = None;
        if waiting {
            view.current.clear();
        }
        view.message = if waiting {
            "正在停止，不再发送后续批次。".into()
        } else {
            "正在停止，当前一封确认后不再发送后续批次。".into()
        };
    }
}

/// 记录已经尝试的一批结果，返回是否继续未尝试的后续批次；失败项始终保留为待核实，不自动重试。
///
/// 数据库已在调用前保存本批结果；continue_on_failure 是启动时固定的本次选择，不能随页面切换变化。
/// 关闭时保留原有 failed 终态；开启时保持 running，直到所有批次处理结束或用户取消。
fn apply_batch_outcome(
    view: &mut MailView,
    batch: &[String],
    result: &AppResult<()>,
    continue_on_failure: bool,
) -> bool {
    match result {
        Ok(()) => {
            view.sent += batch.len();
            view.message = "邮件服务器已接受，已写入发送记录。".into();
            true
        }
        Err(error) => {
            view.failed += batch.len();
            if continue_on_failure {
                view.message = format!(
                    "本批投递未确认，已跳过本批并继续后续批次；本批不会自动重试。{}",
                    error.message
                );
            } else {
                view.status = "failed".into();
                view.phase = "idle".into();
                view.next_batch_at = None;
                view.estimated_completion_at = None;
                view.wait_reason = None;
                view.wait_id = None;
                view.wait_skip = None;
                view.message = format!(
                    "本批投递未确认，任务已停止，请核实后再发送。{}",
                    error.message
                );
            }
            for recipient in batch {
                view.failures.push(MailFailure {
                    recipient: recipient.clone(),
                    // AppError 只含核心层的固定安全文案；保留原因，避免下一批倒计时覆盖主提示后无法查看。
                    message: error.message.clone(),
                });
            }
            continue_on_failure
        }
    }
}

/// 冷却账本不是投递结果；保存故障只停止后续队列，不覆盖已经确认的sent/failed和共享历史。
/// 预登记失败发生在begin_delivery前，整批未投递且不应伪造unknown；完成刷新失败则由内存+pending兜底。
fn fail_cooldown_storage(view: &mut MailView, smtp_attempted: bool) {
    view.status = "failed".into();
    view.phase = "idle".into();
    view.next_batch_at = None;
    view.estimated_completion_at = None;
    view.wait_reason = None;
    view.wait_id = None;
    view.wait_skip = None;
    view.message = if smtp_attempted {
        "账号冷却记录保存失败，已停止后续发送；本批结果请以发送记录和待核实列表为准。内存冷却已保留，重启后将保守恢复。"
    } else {
        "账号冷却记录无法保存，本批尚未发出，任务已停止。请检查本机数据存储后重试。"
    }.into();
}

/// 任务结束只改变运行中的状态；默认失败终态不被覆盖，取消不会变成完成或启动新批次。
/// 全部批次已尝试但存在失败时，completed 仅表示队列结束，必须明确提示仍有待核实记录。
fn finalize_mail_view(view: &mut MailView, cancelled: bool) {
    if view.status == "running" {
        if cancelled {
            view.status = "cancelled".into();
            view.message = if view.failed > 0 {
                format!(
                    "已停止后续发送；{} 位收件人投递待核实，已确认接受的邮件保留记录。",
                    view.failed
                )
            } else {
                "已停止后续发送；已确认接受的邮件保留记录。".into()
            };
        } else {
            view.status = "completed".into();
            view.message = if view.failed > 0 {
                format!("本次批次已全部处理；服务器接受 {} 人，{} 人投递待核实，请到已发送记录核对。待核实批次未自动重试。", view.sent, view.failed)
            } else {
                "本次发送结束。成功指邮件服务器接受，不代表已进入收件箱。".into()
            };
        }
    }
    view.phase = "idle".into();
    view.next_batch_at = None;
    view.estimated_completion_at = None;
    view.wait_reason = None;
    view.wait_id = None;
    view.wait_skip = None;
    view.current.clear();
}

/// 只允许当前任务结束自己的状态；清令牌和最终发布须由调用方持有 operation 门闩串行完成。
/// 迟到的旧执行体不能清除新任务的取消令牌，也不能把新任务的阶段或倒计时重置为 idle。
fn finalize_current_mail(state: &AppState, id: &str, cancelled: bool) -> Option<MailView> {
    let finished = {
        let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
        if view.id != id {
            return None;
        }
        finalize_mail_view(&mut view, cancelled);
        view.clone()
    };
    *state.mail_cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
    Some(finished)
}

#[tauri::command]
pub fn get_mail_state(state: State<'_, Arc<AppState>>) -> MailView {
    state.mail.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// 批量查看成员在本机的发送状态；只读且不受历史界面最近 10000 条限制。
/// 数据库 I/O 在后台线程执行，值必须为 0—10000 个有效数字 QQ；未决优先于成功历史。
#[tauri::command]
pub async fn get_member_mail_status(
    state: State<'_, Arc<AppState>>,
    qq_numbers: Vec<String>,
) -> AppResult<Vec<MemberMailStatus>> {
    let database = state.database()?;
    tokio::task::spawn_blocking(move || database.member_mail_status(&qq_numbers))
        .await
        .map_err(|_| {
            AppError::new(
                "member_mail_status_error",
                "成员发送状态读取中断，请刷新后重试。",
            )
        })?
}

/// 只读取公开配置；凭据损坏或不属于当前 Windows 用户时保留配置并给出可操作提示。
#[tauri::command]
pub async fn get_mail_settings(state: State<'_, Arc<AppState>>) -> AppResult<MailSettingsView> {
    // 与保存/清除串行化，避免页面在密文替换与公共配置提交之间读到临时不匹配状态。
    let _operation = state.operation.lock().await;
    Ok(settings_view(&state, read_public_mail_settings(&state)?))
}

fn read_public_mail_settings(state: &AppState) -> AppResult<PublicMailSettings> {
    Ok(
        crate::sender_account_commands::first_account_settings(state)?
            .map(|settings| PublicMailSettings {
                host: settings.host,
                port: settings.port,
                tls_mode: settings.tls_mode,
                username: settings.username,
                from_address: settings.from_address,
                from_name: settings.from_name,
            })
            .unwrap_or_default(),
    )
}

/// 解析凭据状态后立即丢弃内存明文；异常只影响密码状态，不隐藏仍可编辑的公开配置。
fn settings_view(state: &AppState, settings: PublicMailSettings) -> MailSettingsView {
    let catalog = crate::sender_account_commands::account_catalog_view(state);
    let (password_saved, password_status, password_message) = match catalog {
        Ok(catalog) => match catalog.accounts.first() {
            Some(first)
                if SmtpCredentialBinding::from_settings(
                    &first.settings.with_password(String::new()),
                ) == SmtpCredentialBinding::from_settings(
                    &settings.with_password(String::new()),
                ) =>
            {
                (
                    first.password_saved,
                    first.password_status,
                    first.password_message,
                )
            }
            Some(_) => (
                false,
                "mismatch",
                "当前服务器或发件身份与已保存账号不匹配，请输入新授权码后保存。",
            ),
            None => (
                false,
                "missing",
                "尚未保存授权码，请输入后保存；也可仅用于本次发送。",
            ),
        },
        Err(_) => (
            false,
            "unavailable",
            "已保存授权码无法读取或解密，请重新输入并保存，或先清除已保存授权码。",
        ),
    };
    MailSettingsView {
        settings,
        password_saved,
        password_status,
        password_message,
    }
}

/// 明确点击保存才持久化授权码；空值保留现有密文，但只允许匹配身份的配置复用。
/// 旧接口映射目录首账号；使用新凭据文件与目录版本事务，保存失败不替换已有授权码。
#[tauri::command]
pub async fn save_mail_settings(
    state: State<'_, Arc<AppState>>,
    settings: PublicMailSettings,
    password: Option<String>,
) -> AppResult<MailSettingsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    crate::sender_account_commands::save_legacy_account(
        &state,
        &settings.with_password(password.unwrap_or_default()),
    )?;
    Ok(settings_view(&state, settings))
}

/// 清除只影响以后从本机恢复的 SMTP 授权码，不清空公开设置、QQ 登录或发送记录。
#[tauri::command]
pub async fn clear_mail_password(state: State<'_, Arc<AppState>>) -> AppResult<MailSettingsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    crate::sender_account_commands::clear_legacy_account_password(&state)?;
    let settings = read_public_mail_settings(&state)?;
    Ok(settings_view(&state, settings))
}

/// 空密码从独立密文读取；每次按实际提交配置核对绑定，配置变更不能静默借用旧授权码。
fn resolve_mail_password(state: &AppState, settings: &mut MailSettings) -> AppResult<()> {
    crate::sender_account_commands::legacy_resolve_password(state, settings)
}

/// 每次最多十个、总计18MiB；拒绝目录和超大文件，读取之前先检查长度。
#[tauri::command]
pub async fn mail_pick_attachments(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> AppResult<Vec<AttachmentView>> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择普通附件（最多10个，总计18MiB）")
        .pick_files(move |paths| {
            let _ = sender.send(paths);
        });
    let paths = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "附件选择中断，请重试。"))?
        .unwrap_or_default();
    let (existing_count, existing_size) = {
        let files = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
        let normal: Vec<_> = files
            .values()
            .filter(|file| file.inline.is_none())
            .collect();
        (
            normal.len(),
            normal.iter().map(|file| file.bytes.len()).sum::<usize>(),
        )
    };
    if existing_count + paths.len() > 10 {
        return Err(AppError::new(
            "attachment_count",
            "最多选择10个附件，请先移除部分附件。",
        ));
    }
    let mut selected = Vec::new();
    let mut total = existing_size;
    for path in paths {
        let path = path
            .into_path()
            .map_err(|_| AppError::new("attachment_path", "请选择本机文件。"))?;
        let metadata = std::fs::metadata(&path)
            .map_err(|_| AppError::new("attachment_read", "无法读取所选附件。"))?;
        if !metadata.is_file() || metadata.len() > mail::MAX_ATTACHMENT_BYTES as u64 {
            return Err(AppError::new(
                "attachment_size",
                "附件总大小不能超过18MiB。",
            ));
        }
        if total + metadata.len() as usize > mail::MAX_ATTACHMENT_BYTES {
            return Err(AppError::new(
                "attachment_size",
                "附件总大小不能超过18MiB。",
            ));
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "附件".into());
        // A selected file can grow after metadata is checked. Bound the actual read
        // as well, so another process cannot force an unbounded memory allocation.
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| AppError::new("attachment_read", "无法读取附件内容。"))?;
        let remaining = mail::MAX_ATTACHMENT_BYTES - total;
        let mut bytes = Vec::new();
        file.take((remaining + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::new("attachment_read", "无法读取附件内容。"))?;
        if bytes.len() > remaining {
            return Err(AppError::new(
                "attachment_size",
                "附件读取期间大小发生变化，总大小不能超过18MiB。",
            ));
        }
        total += bytes.len();
        selected.push((
            uuid::Uuid::new_v4().to_string(),
            StoredAttachment {
                name,
                bytes,
                inline: None,
            },
        ));
    }
    let mut result = Vec::new();
    let mut attachments = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
    for (id, file) in selected {
        result.push(AttachmentView {
            id: id.clone(),
            name: file.name.clone(),
            size: file.bytes.len(),
        });
        attachments.insert(id, file);
    }
    Ok(result)
}

#[tauri::command]
pub async fn mail_remove_attachment(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let mut stored = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
    if stored.get(&id).is_some_and(|file| file.inline.is_none()) {
        stored.remove(&id);
    }
    Ok(())
}

/// 登记一次粘贴的图片。先限制 Base64 长度再分配解码内存，损坏输入不会污染草稿仓库。
#[tauri::command]
pub async fn mail_add_inline_image(
    state: State<'_, Arc<AppState>>,
    base64: String,
    mime_type: String,
) -> AppResult<InlineImageView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    if base64.len() > mail::MAX_INLINE_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(AppError::new(
            "inline_image_size",
            "每张正文图片不能超过 5 MiB。",
        ));
    }
    let bytes = STANDARD
        .decode(&base64)
        .map_err(|_| AppError::new("inline_image_format", "图片内容无法读取，请重新复制图片。"))?;
    mail::validate_inline_image(&bytes, &mime_type)?;
    let mut images = register_inline_images(&state, vec![(bytes, mime_type)])?;
    Ok(images.remove(0))
}

/// 仅从用户主动选择的本机文件插入正文图；整批校验成功后才一次登记，取消返回空列表。
#[tauri::command]
pub async fn mail_pick_inline_images(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> AppResult<Vec<InlineImageView>> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("插入正文图片")
        .add_filter("图片", &["png", "jpg", "jpeg", "gif", "webp"])
        .pick_files(move |paths| {
            let _ = sender.send(paths);
        });
    let paths = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "图片选择中断，请重试。"))?
        .unwrap_or_default();
    if paths.len() > mail::MAX_INLINE_IMAGES {
        return Err(AppError::new(
            "inline_image_count",
            "每次最多插入 10 张正文图片。",
        ));
    }
    let mut selected = Vec::new();
    for path in paths {
        let path = path
            .into_path()
            .map_err(|_| AppError::new("inline_image_path", "请选择本机图片文件。"))?;
        let metadata = std::fs::metadata(&path)
            .map_err(|_| AppError::new("inline_image_read", "无法读取所选图片。"))?;
        if !metadata.is_file() || metadata.len() > mail::MAX_INLINE_IMAGE_BYTES as u64 {
            return Err(AppError::new(
                "inline_image_size",
                "每张正文图片不能超过 5 MiB。",
            ));
        }
        // 文件在检查后仍可能增长，实际读取也限制为上限加一，避免无界分配。
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| AppError::new("inline_image_read", "无法读取图片内容。"))?;
        let mut bytes = Vec::new();
        file.take((mail::MAX_INLINE_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::new("inline_image_read", "无法读取图片内容。"))?;
        let mime = mail::inline_image_mime_type(&bytes)?.to_owned();
        selected.push((bytes, mime));
    }
    register_inline_images(&state, selected)
}

/// 为撤销暂存的图片使用独立缓存上限；页面卸载释放，发送只装载草稿显式引用的句柄。
fn register_inline_images(
    state: &AppState,
    selected: Vec<(Vec<u8>, String)>,
) -> AppResult<Vec<InlineImageView>> {
    let mut stored = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
    let cached: Vec<_> = stored
        .values()
        .filter(|file| file.inline.is_some())
        .collect();
    let total: usize = cached.iter().map(|file| file.bytes.len()).sum::<usize>()
        + selected.iter().map(|(bytes, _)| bytes.len()).sum::<usize>();
    if cached.len() + selected.len() > MAX_CACHED_IMAGES || total > MAX_CACHED_IMAGE_BYTES {
        return Err(AppError::new(
            "inline_image_cache",
            "本次编辑图片缓存已满，请复制保留正文，重新打开程序后再插入图片。",
        ));
    }
    let mut result = Vec::new();
    for (bytes, mime_type) in selected {
        let id = uuid::Uuid::new_v4().to_string();
        let content_id = format!("{id}@inline.local");
        let extension = match mime_type.as_str() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        let name = format!("正文图片-{}.{}", result.len() + 1, extension);
        result.push(InlineImageView {
            id: id.clone(),
            content_id: content_id.clone(),
            name: name.clone(),
            size: bytes.len(),
            mime_type: mime_type.clone(),
            data_base64: STANDARD.encode(&bytes),
        });
        stored.insert(
            id,
            StoredAttachment {
                name,
                bytes,
                inline: Some(mail::InlineImageMetadata {
                    content_id,
                    mime_type,
                }),
            },
        );
    }
    Ok(result)
}

/// 仅释放正文图句柄；普通附件不能通过此命令被误删。不存在的句柄按已释放处理。
#[tauri::command]
pub async fn mail_remove_inline_image(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<()> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let mut stored = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
    if stored.get(&id).is_some_and(|file| file.inline.is_some()) {
        stored.remove(&id);
    }
    Ok(())
}

/// 统一启动前的已确认计划与旧 IPC 预检；只读数据库和计划，不消费句柄或变更任务、设置。
/// 全部候选因成功历史或手动规则被过滤时明确拒绝，不能继续建立零批次的“成功”任务。
/// 调用方必须持有 operation 门闩，确保排除版本核对后不会被本进程其他写命令改变。
pub(crate) fn recipients_for_mail_start(
    state: &AppState,
    database: &Database,
    plan_id: Option<&str>,
    candidate_emails: &[String],
    skip_sent: bool,
) -> AppResult<PreparedMailRecipients> {
    let prepared = if let Some(plan_id) = plan_id {
        // begin_delivery 仍在实际发送前原子检查未决记录，确认计划不能绕过最终数据库约束。
        crate::recipient_commands::prepared_for_send(
            state,
            plan_id,
            candidate_emails,
            skip_sent,
            database.manual_exclusion_revision()?,
        )?
    } else {
        // 旧调用也执行当前规则；排除后才核对未决状态，整批使用一个只读事务而非逐人查询。
        database.prepare_mail_recipients_with_exclusions(candidate_emails, skip_sent)?
    };
    if prepared.recipients.is_empty() {
        return Err(AppError::new(
            "no_eligible_recipients",
            "没有可发送的收件人，请调整名单或过滤条件后重试。",
        ));
    }
    Ok(prepared)
}

/// 全局串行队列中的一封邮件；账号下标固定指向本次确认顺序，不因其他账号失败而改变。
struct AssignedMailBatch {
    account_index: usize,
    /// 从1开始；同轮每个实际参与账号最多一批，顺序沿用预检冻结的账号顺序。
    round_index: usize,
    recipients: Vec<String>,
}

/// 先对实际待发名单连续均分，在各账号范围内部按原有模式分封，再交错为每轮各一批。
/// 输入必须已经全局规范化去重并过滤；零分配账号不产生批次，也不需要解析授权码。
/// 交错只改变全局执行顺序，不重新分配收件人，不拆分失败批次，也不改变账号内部的收件顺序。
fn assigned_mail_batches(
    recipients: &[String],
    account_count: usize,
    batch_size: usize,
) -> AppResult<Vec<AssignedMailBatch>> {
    if batch_size == 0 {
        return Err(AppError::new("mail_batch", "每封人数不能为零。"));
    }
    let ranges = mail::balanced_recipient_ranges(recipients.len(), account_count)?;
    let rounds = ranges
        .iter()
        .map(|range| range.len().div_ceil(batch_size))
        .max()
        .unwrap_or(0);
    let mut batches = Vec::new();
    for round in 0..rounds {
        for (account_index, range) in ranges.iter().enumerate() {
            let start = range.start + round * batch_size;
            if start < range.end {
                let end = (start + batch_size).min(range.end);
                batches.push(AssignedMailBatch {
                    account_index,
                    round_index: round + 1,
                    recipients: recipients[start..end].to_vec(),
                });
            }
        }
    }
    Ok(batches)
}

/// 运行期跨任务共享的账号池；只保存规范化发件邮箱及单调截止，不持有密码或邮件内容。
/// 首次发送前从SQLite恢复一次，之后系统校时不能重算旧期限；取消/终态/目录变化都不清空。
#[derive(Clone, Default)]
pub(crate) struct SenderCooldownState {
    restored: bool,
    ready_at: HashMap<String, tokio::time::Instant>,
    /// 最后一次实际SMTP结果处理完成的身份和时刻；取消或新任务不清除，程序重启不恢复此切换状态。
    last_completion: Option<SmtpCompletion>,
}

/// 不同账号切换间隔只依据真实SMTP尝试，不依据目录ID、预登记或本地收件预留失败。
#[derive(Clone)]
struct SmtpCompletion {
    identity: String,
    completed_at: tokio::time::Instant,
}

/// 将持久UTC与运行期单调时刻成对注入，测试可直接推进时间，不依赖真实等待或更改系统时钟。
#[derive(Clone, Copy)]
struct CooldownClock {
    monotonic: tokio::time::Instant,
    utc_unix_ms: i64,
}

impl CooldownClock {
    fn now() -> Self {
        Self {
            monotonic: tokio::time::Instant::now(),
            utc_unix_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 间隔已在任务入口限制为1—3600秒；饱和加法防止异常系统日期导致整数回绕绕过冷却。
    fn utc_deadline(self, interval: Duration) -> i64 {
        self.utc_unix_ms
            .saturating_add(i64::try_from(interval.as_millis()).unwrap_or(i64::MAX))
    }
}

/// 无活跃SMTP时首次恢复；未完成尝试先在SQLite原子变为固定保守期限，恢复失败不建立新任务。
/// 数据库I/O不持有短Mutex，operation门闩与单任务约束保证本进程没有第二个并发恢复者。
fn restore_sender_cooldowns(
    state: &AppState,
    database: &Database,
    clock: CooldownClock,
) -> AppResult<()> {
    if state
        .sender_cooldowns
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .restored
    {
        return Ok(());
    }
    let records = database.recover_sender_cooldowns()?;
    let mut cooldowns = state
        .sender_cooldowns
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for record in records {
        let remaining = record.ready_at_unix_ms.saturating_sub(clock.utc_unix_ms);
        if remaining > 0 {
            let deadline = clock
                .monotonic
                .checked_add(Duration::from_millis(remaining as u64))
                .ok_or_else(|| {
                    AppError::new(
                        "sender_cooldown_storage",
                        "账号冷却期限无法恢复，请检查系统时间和本机数据后重试。",
                    )
                })?;
            cooldowns
                .ready_at
                .entry(record.identity)
                .and_modify(|old| *old = (*old).max(deadline))
                .or_insert(deadline);
        }
    }
    cooldowns.restored = true;
    Ok(())
}

/// 预检冻结的账号身份与间隔；截止始终存AppState共享池，不随worker、账号ID或目录编辑重建。
/// 同轮按冻结顺序优先取已就绪账号，只有该轮无任何可发账号时才等最早截止，避免A冷却挡住B。
struct AccountRoundTiming {
    identities: Vec<String>,
    interval: Duration,
    /// 本任务选定的进程内账号切换间隔；0关闭，对已生效的持久自身冷却没有修改作用。
    switch_interval: Duration,
}

/// 位置指向remaining数组，供调度器仅移除实际选择的一批；不变更账号分配或账号内部名单。
enum RoundDispatch {
    Ready(usize),
    Waiting(usize, tokio::time::Instant, MailWaitReason),
    Finished,
}

impl AccountRoundTiming {
    /// identities只含实际分到收件人的账号，已规范化的同邮箱共享一池，不受重排或删后重加影响。
    fn new(identities: Vec<String>, interval: Duration, switch_interval: Duration) -> Self {
        Self {
            identities,
            interval,
            switch_interval,
        }
    }

    /// 必须在任何SMTP网络调用前成功持久登记；失败不进入网络、不清除已有内存期限。
    /// pending确保进程退出或完成刷新失败时，重启仍会保守冷却，而不是丢掉交互耗时后提前发送。
    fn begin_attempt(
        &self,
        state: &AppState,
        database: &Database,
        account_index: usize,
        clock: CooldownClock,
    ) -> AppResult<i64> {
        let identity = &self.identities[account_index];
        let persisted =
            database.begin_sender_cooldown_attempt(identity, self.interval.as_secs())?;
        self.merge_memory_deadline(state, identity, clock.monotonic + self.interval);
        Ok(persisted)
    }

    /// 冷却登记可能等待SQLite写锁；返回后必须再查取消，避免用户停止期间尚未联网的批次继续发出。
    /// 取消只清除此预登记的pending，保留原固定截止；没有实际SMTP结果，不能更新last_completion。
    fn stop_before_smtp(
        &self,
        database: &Database,
        account_index: usize,
        registered_deadline: i64,
        cancel: &CancellationToken,
    ) -> AppResult<bool> {
        if !cancel.is_cancelled() {
            return Ok(false);
        }
        database
            .finish_sender_cooldown_attempt(&self.identities[account_index], registered_deadline)?;
        Ok(true)
    }

    /// 结果处理完成后先保留内存期限，再持久清pending；失败时保留内存和磁盘pending且停止队列。
    /// 已生效期限仅可取max，用户缩短新间隔或系统校时都不能缩短旧池；新间隔只用于新尝试。
    fn completed(
        &self,
        state: &AppState,
        database: &Database,
        account_index: usize,
        clock: CooldownClock,
    ) -> AppResult<()> {
        let identity = &self.identities[account_index];
        self.merge_memory_deadline(state, identity, clock.monotonic + self.interval);
        // 只有实际SMTP结果处理路径调用此处。写盘失败仍需保留切换间隔，不能让下一任务马上换邮箱发送。
        state
            .sender_cooldowns
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_completion = Some(SmtpCompletion {
            identity: identity.clone(),
            completed_at: clock.monotonic,
        });
        database.finish_sender_cooldown_attempt(identity, clock.utc_deadline(self.interval))?;
        Ok(())
    }

    /// 内存先取max是写失败后的兜底；单调时钟在本进程是权威，持久UTC仅供下次进程恢复。
    fn merge_memory_deadline(
        &self,
        state: &AppState,
        identity: &str,
        deadline: tokio::time::Instant,
    ) {
        state
            .sender_cooldowns
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ready_at
            .entry(identity.to_owned())
            .and_modify(|old| *old = (*old).max(deadline))
            .or_insert(deadline);
    }

    /// remaining保持轮次分组；只选当前最早未完成轮的批次，保证每轮每账号最多一批且不跨轮抢跑。
    /// 等待时返回该轮最早可发账号，期间不写数据库、不续期；取消后重建仍复用原共享截止。
    fn next_dispatch(
        &self,
        state: &AppState,
        batches: &[AssignedMailBatch],
        remaining: &[usize],
        now: tokio::time::Instant,
    ) -> RoundDispatch {
        let cooldowns = state
            .sender_cooldowns
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.dispatch_with_cooldowns(&cooldowns, batches, remaining, now)
    }

    /// 实际调度和预计完成共用选择规则；仅读传入池，预估使用副本，不能提前登记或延长真实冷却。
    fn dispatch_with_cooldowns(
        &self,
        cooldowns: &SenderCooldownState,
        batches: &[AssignedMailBatch],
        remaining: &[usize],
        now: tokio::time::Instant,
    ) -> RoundDispatch {
        let Some(first) = remaining.first() else {
            return RoundDispatch::Finished;
        };
        let round = batches[*first].round_index;
        let mut earliest: Option<(usize, tokio::time::Instant, MailWaitReason)> = None;
        for (position, index) in remaining
            .iter()
            .enumerate()
            .take_while(|(_, index)| batches[**index].round_index == round)
        {
            let identity = &self.identities[batches[*index].account_index];
            let own_deadline = cooldowns
                .ready_at
                .get(identity)
                .copied()
                .filter(|deadline| *deadline > now);
            // 同一规范化发件邮箱即使来自不同目录ID仍是同身份，只受自身冷却；首封和0开关也无切换限制。
            let switch_deadline = cooldowns
                .last_completion
                .as_ref()
                .filter(|last| last.identity != *identity && !self.switch_interval.is_zero())
                .map(|last| last.completed_at + self.switch_interval)
                .filter(|deadline| *deadline > now);
            let (deadline, reason) = match (own_deadline, switch_deadline) {
                (None, None) => return RoundDispatch::Ready(position),
                (Some(deadline), None) => (deadline, MailWaitReason::AccountCooldown),
                (None, Some(deadline)) => (deadline, MailWaitReason::AccountSwitch),
                (Some(own), Some(switch)) => (own.max(switch), MailWaitReason::Combined),
            };
            if earliest.is_none_or(|(_, old, _)| deadline < old) {
                earliest = Some((position, deadline, reason));
            }
        }
        earliest.map_or(RoundDispatch::Finished, |(position, deadline, reason)| {
            RoundDispatch::Waiting(position, deadline, reason)
        })
    }
}

/// 本次任务已完成批次的耗时样本；包含本地登记与结果处理，不含账号等待。
/// 首批前暂按每批10秒估算；之后用本任务平均耗时（最少1秒），不读取历史账号或任何邮件内容。
#[derive(Default)]
struct BatchDurationEstimate {
    elapsed: Duration,
    samples: u32,
}

impl BatchDurationEstimate {
    fn observe(&mut self, elapsed: Duration) {
        self.elapsed = self.elapsed.saturating_add(elapsed);
        self.samples = self.samples.saturating_add(1);
    }

    fn mean(&self) -> Duration {
        if self.samples == 0 {
            Duration::from_secs(10)
        } else {
            (self.elapsed / self.samples).max(Duration::from_secs(1))
        }
    }
}

impl AccountRoundTiming {
    /// 在冷却副本中模拟剩余批次，返回预计队列处理完的UTC毫秒；不保证收件箱送达或服务端不拒绝。
    /// active是已取出但正在执行的批次下标；其余remaining必须保留原轮次分组，均来自冻结发送队列。
    /// 每轮临时数组最多20个账号，模拟为O(批次×账号数)，不对全部剩余数组反复remove造成平方搬移。
    /// 只短暂复制共享池，模拟全程不持有状态锁、不写SQLite、不发网络请求，也不添加尾批后的等待。
    fn estimated_completion(
        &self,
        state: &AppState,
        batches: &[AssignedMailBatch],
        remaining: &[usize],
        active: Option<usize>,
        duration: Duration,
        clock: CooldownClock,
    ) -> Option<i64> {
        if active.is_none() && remaining.is_empty() {
            return None;
        }
        let pool = state
            .sender_cooldowns
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        self.estimated_completion_with_pool(pool, batches, remaining, active, duration, clock)
    }

    /// 确认前与任务中共用纯模拟入口；传入池由调用方复制，模拟结果绝不合并回真实冷却。
    fn estimated_completion_with_pool(
        &self,
        mut pool: SenderCooldownState,
        batches: &[AssignedMailBatch],
        remaining: &[usize],
        active: Option<usize>,
        duration: Duration,
        clock: CooldownClock,
    ) -> Option<i64> {
        if active.is_none() && remaining.is_empty() {
            return None;
        }
        let mut cursor = clock.monotonic;
        // 执行中的批次已经通过实际调度检查；只估算当前批次剩余处理耗时，不重复等待它自己的冷却。
        if let Some(index) = active {
            cursor = cursor.checked_add(duration)?;
            self.project_completion(&mut pool, batches[index].account_index, cursor)?;
        }
        for round in remaining.chunk_by(|a, b| batches[*a].round_index == batches[*b].round_index) {
            let mut pending = round.to_vec();
            while !pending.is_empty() {
                let position = match self.dispatch_with_cooldowns(&pool, batches, &pending, cursor)
                {
                    RoundDispatch::Ready(position) => position,
                    RoundDispatch::Waiting(position, deadline, _) => {
                        cursor = deadline;
                        position
                    }
                    RoundDispatch::Finished => return None,
                };
                let index = pending.remove(position);
                cursor = cursor.checked_add(duration)?;
                self.project_completion(&mut pool, batches[index].account_index, cursor)?;
            }
        }
        Some(clock.utc_deadline(cursor.duration_since(clock.monotonic)))
    }

    /// 模拟实际结果处理后的自身冷却max与账号切换时刻；不处理持久化或投递结果。
    fn project_completion(
        &self,
        pool: &mut SenderCooldownState,
        account_index: usize,
        completed_at: tokio::time::Instant,
    ) -> Option<()> {
        let identity = &self.identities[account_index];
        let deadline = completed_at.checked_add(self.interval)?;
        pool.ready_at
            .entry(identity.clone())
            .and_modify(|old| *old = (*old).max(deadline))
            .or_insert(deadline);
        pool.last_completion = Some(SmtpCompletion {
            identity: identity.clone(),
            completed_at,
        });
        Some(())
    }
}

/// 只读预检估算：按冻结账号的封数构造空载批次，复用实发轮次调度，不读取收件地址或凭据。
/// 首次发送前仅读SQLite冷却；崩溃pending按“若现在发送则恢复原间隔”投影，不清除或续写标记。
/// 已恢复的进程直接复制单调池，故取消后的剩余等待、切换间隔和同邮箱身份合并均与实发一致。
pub(crate) fn estimate_confirmation_completion(
    state: &AppState,
    accounts: &[(String, usize)],
    interval_seconds: u64,
    switch_seconds: u64,
) -> AppResult<Option<i64>> {
    if !(1..=3600).contains(&interval_seconds) || switch_seconds > 3600 {
        return Err(AppError::new(
            "mail_interval",
            "发送间隔无效，请返回修改后重新确认。",
        ));
    }
    let clock = CooldownClock::now();
    let mut pool = state
        .sender_cooldowns
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if !pool.restored {
        for record in state.database()?.sender_cooldowns()? {
            let pending = record
                .pending_interval_seconds
                .unwrap_or(0)
                .saturating_mul(1000);
            let remaining = record
                .ready_at_unix_ms
                .saturating_sub(clock.utc_unix_ms)
                .max(0) as u64;
            if let Some(deadline) = clock
                .monotonic
                .checked_add(Duration::from_millis(remaining.max(pending)))
            {
                pool.ready_at
                    .entry(record.identity)
                    .and_modify(|old| *old = (*old).max(deadline))
                    .or_insert(deadline);
            } else {
                return Err(AppError::new("mail_estimate", "系统时间超出可估算范围。"));
            }
        }
    }
    let timing = AccountRoundTiming {
        identities: accounts
            .iter()
            .map(|(address, _)| normalize_sender_cooldown_identity(address))
            .collect::<AppResult<Vec<_>>>()?,
        interval: Duration::from_secs(interval_seconds),
        switch_interval: Duration::from_secs(switch_seconds),
    };
    let mut batches = Vec::new();
    for round in 0..accounts.iter().map(|(_, count)| *count).max().unwrap_or(0) {
        for (account_index, (_, count)) in accounts.iter().enumerate() {
            if round < *count {
                batches.push(AssignedMailBatch {
                    account_index,
                    round_index: round + 1,
                    recipients: Vec::new(),
                });
            }
        }
    }
    let remaining = (0..batches.len()).collect::<Vec<_>>();
    Ok(timing.estimated_completion_with_pool(
        pool,
        &batches,
        &remaining,
        None,
        BatchDurationEstimate::default().mean(),
        clock,
    ))
}

/// 预计值是可丢弃展示；迟到worker、取消或终态不可借刷新预计值复活旧任务。
fn set_completion_estimate(state: &AppState, job_id: &str, estimate: Option<i64>) {
    let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
    if view.id == job_id {
        view.estimated_completion_at = if view.status == "running" && view.phase != "stopping" {
            estimate
        } else {
            None
        };
    }
}

/// 等待支持一次性手动跳过，不中断SMTP交互；停止优先于跳过和自然到期，避免多发下一批。
async fn wait_for_round_deadline(
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    skip: &CancellationToken,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = skip.cancelled() => true,
        _ = tokio::time::sleep_until(deadline) => true,
    }
}

/// 经宿主验证并与SMTP发送器一起冻结的公开归属；记录不再依赖可修改的账号目录或页面声明。
/// identity只用于冷却匹配，address/name保留实际From邮箱和当时账号名称，供投递账本保存快照。
struct FrozenSenderOrigin {
    identity: String,
    address: String,
    name: String,
}

impl FrozenSenderOrigin {
    /// 进度中优先显示账号名称并保留邮箱；旧单账号未填写名称时直接显示邮箱。
    fn label(&self) -> String {
        if self.name.is_empty() {
            self.address.clone()
        } else {
            format!("{}（{}）", self.name, self.address)
        }
    }
}

/// 发送器与其真实归属必须作为同一项保存；随机分配只重排此项，避免平行数组错配历史账号。
struct FrozenMailSender {
    sender: MailSender,
    origin: FrozenSenderOrigin,
}

/// 在任务建立前一次性准备全部实际参与账号，专门隔离零人账号与“先发后发现配置错误”的边界。
/// 这里只构造TLS传输和本地MIME，不连接服务器；任何账号失败都整体返回且不预留投递记录。
fn multi_account_senders(
    state: &AppState,
    selection: &PreparedAccountSelection,
    recipients: &[String],
    draft: &MailDraft,
    attachments: &[MailAttachment],
) -> AppResult<Vec<FrozenMailSender>> {
    let recipient = recipients
        .first()
        .ok_or_else(|| AppError::new("no_eligible_recipients", "没有可发送的收件人。"))?;
    // 非空区间始终为冻结账号序列前缀；零人账号即使没有授权码也不会被解析或构造SMTP发送器。
    let active_count = recipients.len().min(selection.ids.len());
    let active_ids = &selection.ids[..active_count];
    let mut resolved =
        crate::sender_account_commands::resolve_selected_accounts(state, active_ids)?;
    if resolved.revision != selection.revision
        || resolved.accounts.len() != active_ids.len()
        || resolved
            .accounts
            .iter()
            .any(|account| !active_ids.contains(&account.id))
    {
        return Err(recipient_commands::accounts_changed());
    }
    let mut senders = Vec::with_capacity(active_count);
    for id in active_ids {
        // helper按目录返回；这里按确认冻结的随机/顺序账号映射，不能把密码与另一个账号下标混用。
        let index = resolved
            .accounts
            .iter()
            .position(|account| &account.id == id)
            .ok_or_else(recipient_commands::accounts_changed)?;
        let account = resolved.accounts.remove(index);
        let sender = MailSender::new(&account.settings)?;
        sender.validate_draft(recipient, &draft.subject, &draft.html, attachments)?;
        senders.push(FrozenMailSender {
            sender,
            origin: FrozenSenderOrigin {
                identity: normalize_sender_cooldown_identity(&account.settings.from_address)?,
                address: account.settings.from_address,
                name: account.name,
            },
        });
    }
    Ok(senders)
}

/// 建立已确认的发送任务；配置验证、收件人去重和历史过滤先完成，然后才连接SMTP。
#[tauri::command]
pub async fn start_mail_job(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    draft: MailDraft,
    settings: MailSettings,
) -> AppResult<MailView> {
    let mut operation = diagnostics::operation("mail_job", "start");
    let result = start_mail_job_inner(app, state, draft, settings).await;
    if let Err(error) = &result {
        // 只记录稳定错误码；草稿、设置和收件内容不能因前置校验失败进入日志。
        diagnostics::event(
            "error",
            "mail_job",
            "start_rejected",
            serde_json::json!({
                "error_code": error.code,
            }),
        );
    }
    operation.finish(if result.is_ok() {
        "started"
    } else {
        "rejected"
    });
    result
}

/// 将日志包围范围与已有启动流程分离；仍在相同 operation 锁内完成校验、计划消费及任务建立。
async fn start_mail_job_inner(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    draft: MailDraft,
    mut settings: MailSettings,
) -> AppResult<MailView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    if draft.interval_seconds < 1 || draft.interval_seconds > 3600 {
        return Err(AppError::new("mail_interval", "每批间隔应为1到3600秒。"));
    }
    validate_account_switch_interval(draft.account_switch_interval_seconds)?;
    let effective_batch_size =
        recipient_commands::confirmed_batch_size(&draft.send_mode, draft.batch_size)?;
    recipient_commands::validate_allocation_mode(&draft.allocation_mode)?;
    if draft.subject.trim().is_empty() {
        return Err(AppError::new("mail_subject", "请填写邮件主题。"));
    }
    if draft.html.len() > mail::MAX_BODY_BYTES {
        return Err(AppError::new("mail_body", "邮件正文不能超过2MiB。"));
    }
    let recipients = mail::parse_recipients(&draft.recipients)?;
    if recipients.is_empty() {
        return Err(AppError::new("mail_recipients", "请填写至少一位收件人。"));
    }
    let candidate_emails: Vec<String> = recipients
        .iter()
        .map(|recipient| recipient.email.clone())
        .collect();
    let database = state.database()?;
    let selected_metadata = draft
        .account_ids
        .as_ref()
        .map(|ids| crate::sender_account_commands::selected_account_metadata(&state, ids))
        .transpose()?;
    let selected_accounts = selected_metadata
        .as_ref()
        .map(|selected| {
            // 新目录路径要求明确携带确认版本；省略版本不能退回使用临时单账号配置。
            if draft.account_revision != Some(selected.revision) {
                return Err(recipient_commands::accounts_changed());
            }
            Ok(PreparedAccountSelection {
                ids: selected
                    .accounts
                    .iter()
                    .map(|account| account.id.clone())
                    .collect(),
                revision: selected.revision,
                send_mode: draft.send_mode.clone(),
                batch_size: draft.batch_size,
                allocation_mode: draft.allocation_mode.clone(),
            })
        })
        .transpose()?;
    if selected_accounts.is_none() && draft.account_revision.is_some() {
        return Err(recipient_commands::accounts_changed());
    }
    let selected_accounts = recipient_commands::validate_prepared_accounts(
        &state,
        draft.recipient_plan_id.as_deref(),
        selected_accounts.as_ref(),
    )?;
    // Freeze the verified group source before any SMTP interaction. A new fetch
    // cannot silently replace an older draft's explicit group-member selection.
    let group_audience = draft
        .group_audience
        .as_ref()
        .map(|audience| {
            database.validate_group_mail(
                &audience.snapshot_id,
                &audience.qq_numbers,
                &candidate_emails,
            )
        })
        .transpose()?;
    let prepared = recipients_for_mail_start(
        &state,
        &database,
        draft.recipient_plan_id.as_deref(),
        &candidate_emails,
        draft.skip_sent,
    )?;
    let skipped_sent = prepared.skipped_sent;
    let manual_excluded = prepared.excluded.len();
    let skipped = skipped_sent + manual_excluded;
    let recipients_to_send = prepared.recipients;
    let mut attachment_ids = HashSet::new();
    let attachments = {
        let stored = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
        let mut files = Vec::new();
        for id in &draft.attachment_ids {
            if !attachment_ids.insert(id) {
                continue;
            }
            let file = stored
                .get(id)
                .filter(|file| file.inline.is_none())
                .ok_or_else(|| AppError::new("attachment_missing", "附件已失效，请重新选择。"))?;
            files.push(MailAttachment {
                name: file.name.clone(),
                bytes: file.bytes.clone(),
                inline: None,
            });
        }
        for id in &draft.inline_image_ids {
            if !attachment_ids.insert(id) {
                continue;
            }
            let file = stored
                .get(id)
                .filter(|file| file.inline.is_some())
                .ok_or_else(|| {
                    AppError::new("inline_image_missing", "正文图片已失效，请移除后重新插入。")
                })?;
            files.push(MailAttachment {
                name: file.name.clone(),
                bytes: file.bytes.clone(),
                inline: file.inline.clone(),
            });
        }
        files
    };
    let account_count = selected_accounts
        .as_ref()
        .map_or(1, |selection| selection.ids.len());
    let batches = assigned_mail_batches(&recipients_to_send, account_count, effective_batch_size)?;
    let senders = if let Some(selection) = selected_accounts.as_ref() {
        multi_account_senders(&state, selection, &recipients_to_send, &draft, &attachments)?
    } else {
        resolve_mail_password(&state, &mut settings)?;
        let sender = MailSender::new(&settings)?;
        sender.validate_draft(
            &recipients_to_send[0],
            &draft.subject,
            &draft.html,
            &attachments,
        )?;
        let public = PublicMailSettings {
            host: settings.host.clone(),
            port: settings.port,
            tls_mode: settings.tls_mode.clone(),
            username: settings.username.clone(),
            from_address: settings.from_address.clone(),
            from_name: settings.from_name.clone(),
        };
        // 保留旧临时单账号设置行为；目录发送绝不覆盖首账号或自动保存客户端临时配置。
        database.set_setting(
            "smtp_public",
            &serde_json::to_string(&public)
                .map_err(|_| AppError::new("settings_error", "无法保存发信设置。"))?,
        )?;
        vec![FrozenMailSender {
            sender,
            origin: FrozenSenderOrigin {
                identity: normalize_sender_cooldown_identity(&settings.from_address)?,
                address: settings.from_address.clone(),
                name: settings.from_name.clone(),
            },
        }]
    };
    // 仅在无其他发送任务时首次加载/修复未完成冷却；失败不消费确认、不创建任务或连接SMTP。
    restore_sender_cooldowns(&state, &database, CooldownClock::now())?;
    let timing = AccountRoundTiming::new(
        senders
            .iter()
            .map(|sender| sender.origin.identity.clone())
            .collect(),
        Duration::from_secs(draft.interval_seconds),
        Duration::from_secs(draft.account_switch_interval_seconds),
    );
    let mut remaining = (0..batches.len()).collect::<Vec<_>>();
    let mut duration_estimate = BatchDurationEstimate::default();
    let estimated_completion_at = timing.estimated_completion(
        &state,
        &batches,
        &remaining,
        None,
        duration_estimate.mean(),
        CooldownClock::now(),
    );
    // 到此所有可修正的草稿和配置校验均已通过，接下来只建立任务。一次性计划随成功启动消费。
    *state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
    let id = uuid::Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    *state.mail_cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel.clone());
    {
        let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
        // 同进程的下一任务沿用版本，避免旧任务迟到的事件覆盖刚启动的新任务。
        let state_revision = view.state_revision;
        *view = MailView {
            id: id.clone(),
            status: "running".into(),
            phase: "sending".into(),
            next_batch_at: None,
            estimated_completion_at,
            wait_reason: None,
            wait_id: None,
            wait_skip: None,
            state_revision,
            total: recipients.len(),
            sent: 0,
            failed: 0,
            skipped,
            skipped_sent,
            manual_excluded,
            current: String::new(),
            message: "正在准备发送…".into(),
            failures: vec![],
            group_audience,
        };
    }
    diagnostics::event(
        "info",
        "mail_job",
        "created",
        serde_json::json!({
            "job_id": id,
            "candidate_count": recipients.len(),
            "eligible_count": recipients_to_send.len(),
            "skipped_count": skipped,
            "skipped_sent_count": skipped_sent,
            "manual_excluded_count": manual_excluded,
            "exclusion_revision": prepared.exclusion_revision,
            "send_mode": draft.send_mode,
            "batch_size": if draft.send_mode == "individual" { 1 } else { draft.batch_size },
            "interval_seconds": draft.interval_seconds,
            "account_switch_interval_seconds": draft.account_switch_interval_seconds,
            "account_switch_scope": "same_process_last_actual_smtp_completion_different_from",
            "interval_scope": "persistent_sender_identity_ready_first_round_fair",
            "continue_on_failure": draft.continue_on_failure,
            "skip_sent": draft.skip_sent,
            "attachment_count": attachments.iter().filter(|file| file.inline.is_none()).count(),
            "inline_image_count": attachments.iter().filter(|file| file.inline.is_some()).count(),
            "selected_account_count": account_count,
            "active_account_count": senders.len(),
            "message_count": batches.len(),
            "allocation_mode": draft.allocation_mode,
        }),
    );
    let initial = publish_mail(&app, &state);
    let shared = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        let job_started = Instant::now();
        let round_count = batches.last().map_or(0, |batch| batch.round_index);
        let mut index = 0;
        while !remaining.is_empty() {
            if cancel.is_cancelled() {
                diagnostics::event(
                    "info",
                    "mail_job",
                    "cancel_observed",
                    serde_json::json!({
                        "job_id": id, "next_batch_index": index + 1,
                    }),
                );
                break;
            }
            set_completion_estimate(
                &shared,
                &id,
                timing.estimated_completion(
                    &shared,
                    &batches,
                    &remaining,
                    None,
                    duration_estimate.mean(),
                    CooldownClock::now(),
                ),
            );
            let now = tokio::time::Instant::now();
            let position = match timing.next_dispatch(&shared, &batches, &remaining, now) {
                RoundDispatch::Finished => break,
                RoundDispatch::Ready(position) => position,
                RoundDispatch::Waiting(position, deadline, reason) => {
                    let assigned = &batches[remaining[position]];
                    let batch = &assigned.recipients;
                    let remaining = deadline.duration_since(now);
                    // 墙钟只展示真正剩余的单调等待，不会因其他账号已耗时而重新计满整个间隔。
                    let display_deadline = chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_add(i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX));
                    let skip = {
                        let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                        let waiting = wait_for_batch_view(
                            &mut view,
                            index + 1,
                            batches.len(),
                            batch.len(),
                            display_deadline,
                            reason,
                        );
                        if waiting {
                            view.current = format!(
                                "第 {}/{} 轮 · {} · {}",
                                assigned.round_index,
                                round_count,
                                senders[assigned.account_index].origin.label(),
                                view.current
                            );
                        }
                        waiting.then(|| view.wait_skip.as_ref().unwrap().clone())
                    };
                    let Some(skip) = skip else {
                        break;
                    };
                    diagnostics::event(
                        "info",
                        "mail_job",
                        "batch_wait_started",
                        serde_json::json!({
                            "job_id":id,"next_batch_index":index + 1,"round_index":assigned.round_index,
                            "interval_seconds":draft.interval_seconds,"remaining_ms":remaining.as_millis(),"next_batch_at":display_deadline,
                            "account_switch_interval_seconds":draft.account_switch_interval_seconds,"wait_reason":reason,
                            "wait_scope":"earliest_remaining_identity_in_current_round",
                        }),
                    );
                    publish_mail(&app, &shared);
                    if !wait_for_round_deadline(deadline, &cancel, &skip).await {
                        break;
                    }
                    // 明确跳过仅放行当前等待指向的一批，后面各批继续使用共享冷却池。
                    // 自然到期则重新扫描当前轮；不清空/缩短持久冷却，取消仍在下一步优先检查。
                    if skip.is_cancelled() {
                        position
                    } else {
                        continue;
                    }
                }
            };
            if cancel.is_cancelled() {
                break;
            }
            let assigned_index = remaining.remove(position);
            let assigned = &batches[assigned_index];
            let batch = &assigned.recipients;
            // sender数组与账号身份均按预检冻结顺序建立；调度只选就绪批次，不改变账号名单或凭据映射。
            let frozen_sender = &senders[assigned.account_index];
            let sender = &frozen_sender.sender;
            let batch_started = Instant::now();
            let context = Some(MailSendContext {
                job_id: &id,
                batch_index: index + 1,
            });
            diagnostics::event(
                "info",
                "mail_job",
                "batch_started",
                serde_json::json!({
                    "job_id": id, "batch_index": index + 1, "batch_count": batches.len(), "recipient_count": batch.len(),
                    "account_index": assigned.account_index + 1, "round_index": assigned.round_index, "round_count": round_count,
                }),
            );
            let sending = {
                let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                let sending = begin_batch_view(&mut view, index + 1, batches.len(), batch.len());
                if sending {
                    view.current = format!(
                        "第 {}/{} 轮 · {} · {}",
                        assigned.round_index,
                        round_count,
                        frozen_sender.origin.label(),
                        view.current
                    );
                }
                sending
            };
            if !sending {
                break;
            }
            set_completion_estimate(
                &shared,
                &id,
                timing.estimated_completion(
                    &shared,
                    &batches,
                    &remaining,
                    Some(assigned_index),
                    duration_estimate.mean(),
                    CooldownClock::now(),
                ),
            );
            publish_mail(&app, &shared);
            let registered_deadline = match timing.begin_attempt(
                &shared,
                &database,
                assigned.account_index,
                CooldownClock::now(),
            ) {
                Ok(deadline) => deadline,
                Err(error) => {
                    let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                    fail_cooldown_storage(&mut view, false);
                    drop(view);
                    diagnostics::event(
                        "error",
                        "mail_job",
                        "cooldown_registration_failed",
                        serde_json::json!({"job_id":id,"batch_index":index + 1,"error_code":error.code,"smtp_attempted":false}),
                    );
                    publish_mail(&app, &shared);
                    break;
                }
            };
            match timing.stop_before_smtp(
                &database,
                assigned.account_index,
                registered_deadline,
                &cancel,
            ) {
                Ok(false) => {}
                Ok(true) => {
                    diagnostics::event(
                        "info",
                        "mail_job",
                        "cancel_before_smtp",
                        serde_json::json!({
                            "job_id":id,"batch_index":index + 1,"smtp_attempted":false,
                        }),
                    );
                    break;
                }
                Err(error) => {
                    let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                    fail_cooldown_storage(&mut view, false);
                    drop(view);
                    diagnostics::event(
                        "error",
                        "mail_job",
                        "cancel_cooldown_completion_failed",
                        serde_json::json!({
                            "job_id":id,"batch_index":index + 1,"error_code":error.code,"smtp_attempted":false,
                        }),
                    );
                    publish_mail(&app, &shared);
                    break;
                }
            }
            let mut smtp_attempted = false;
            // 归属取自与发送器绑定的宿主快照；账号后续改名或删除不能改变本批历史。
            // 预留可能等待SQLite写锁，事务取得锁后及提交前必须重新检查取消，避免取消后仍进入SMTP。
            let result = match database.begin_delivery_with_sender_unless_cancelled(
                &id,
                batch,
                &frozen_sender.origin.address,
                &frozen_sender.origin.name,
                || cancel.is_cancelled(),
            ) {
                Ok(true) => {
                    smtp_attempted = true;
                    // Do not cancel an in-flight DATA operation: acceptance may already
                    // have happened. Stop before the next batch and preserve its outcome.
                    if draft.send_mode == "individual" {
                        sender
                            .send_with_context(
                                &batch[0],
                                &draft.subject,
                                &draft.html,
                                &attachments,
                                context,
                            )
                            .await
                    } else {
                        sender
                            .send_batch_with_context(
                                batch,
                                &draft.subject,
                                &draft.html,
                                &attachments,
                                context,
                            )
                            .await
                    }
                }
                Ok(false) => {
                    // false表示整批预留已回滚，尚未尝试SMTP，不能写入待核实记录或更新最近发送完成时刻。
                    // 仅清除此前的冷却pending，保留原登记期限；存储失败沿用已有阻止继续发送的处理。
                    match timing.stop_before_smtp(
                        &database,
                        assigned.account_index,
                        registered_deadline,
                        &cancel,
                    ) {
                        Ok(_) => diagnostics::event(
                            "info",
                            "mail_job",
                            "cancel_before_smtp",
                            serde_json::json!({
                                "job_id":id,"batch_index":index + 1,"smtp_attempted":false,
                            }),
                        ),
                        Err(error) => {
                            let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                            fail_cooldown_storage(&mut view, false);
                            drop(view);
                            diagnostics::event(
                                "error",
                                "mail_job",
                                "cancel_cooldown_completion_failed",
                                serde_json::json!({
                                    "job_id":id,"batch_index":index + 1,"error_code":error.code,"smtp_attempted":false,
                                }),
                            );
                            publish_mail(&app, &shared);
                        }
                    }
                    break;
                }
                Err(error) => {
                    diagnostics::event(
                        "error",
                        "mail_job",
                        "delivery_reservation_failed",
                        serde_json::json!({
                            "job_id": id, "batch_index": index + 1, "recipient_count": batch.len(), "error_code": error.code,
                        }),
                    );
                    Err(error)
                }
            };
            let result = match result {
                Ok(()) => match database.finish_delivery(&id, batch, true, "") {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        // The server may have accepted DATA even when the local commit
                        // failed. Make this visible for manual resolution when possible;
                        // if storage stays unavailable, restart recovers sending as unknown.
                        diagnostics::event(
                            "error",
                            "mail_job",
                            "accepted_record_commit_failed",
                            serde_json::json!({
                                "job_id": id, "batch_index": index + 1, "recipient_count": batch.len(), "error_code": error.code,
                            }),
                        );
                        if let Err(fallback_error) = database.finish_delivery(&id,batch,false,"SMTP 已接受，但本机保存失败，需人工核实；若列表暂不可用，请重启后核实。") {
                            diagnostics::event("error", "mail_job", "unknown_record_commit_failed", serde_json::json!({
                                "job_id": id, "batch_index": index + 1, "error_code": fallback_error.code,
                            }));
                        }
                        Err(error)
                    }
                },
                Err(error) => {
                    if let Err(commit_error) =
                        database.finish_delivery(&id, batch, false, &error.message)
                    {
                        diagnostics::event(
                            "error",
                            "mail_job",
                            "unknown_record_commit_failed",
                            serde_json::json!({
                                "job_id": id, "batch_index": index + 1, "error_code": commit_error.code,
                            }),
                        );
                    }
                    Err(error)
                }
            };
            let mut continue_after_batch = {
                let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                apply_batch_outcome(&mut view, batch, &result, draft.continue_on_failure)
            };
            // 实际SMTP尝试无论成功或待核实，都从结果处理完成重新起算本邮箱冷却；内存先保留再写盘。
            // 预留收件失败时未进网络，只清pending不额外延长预登记期限，保守保留已经生效的短期登记。
            let cooldown_result = if smtp_attempted {
                timing.completed(
                    &shared,
                    &database,
                    assigned.account_index,
                    CooldownClock::now(),
                )
            } else {
                database
                    .finish_sender_cooldown_attempt(
                        &timing.identities[assigned.account_index],
                        registered_deadline,
                    )
                    .map(|_| ())
            };
            if let Err(error) = cooldown_result {
                let mut view = shared.mail.lock().unwrap_or_else(|p| p.into_inner());
                fail_cooldown_storage(&mut view, smtp_attempted);
                continue_after_batch = false;
                drop(view);
                diagnostics::event(
                    "error",
                    "mail_job",
                    "cooldown_completion_failed",
                    serde_json::json!({"job_id":id,"batch_index":index + 1,"error_code":error.code,"smtp_attempted":smtp_attempted}),
                );
            }
            diagnostics::event(
                if result.is_ok() { "info" } else { "error" },
                "mail_job",
                "batch_finished",
                serde_json::json!({
                    "job_id": id, "batch_index": index + 1, "recipient_count": batch.len(),
                    "account_index": assigned.account_index + 1, "round_index": assigned.round_index,
                    "elapsed_ms": batch_started.elapsed().as_millis(),
                    "outcome": if result.is_ok() { "smtp_accepted_and_recorded" } else { "delivery_unknown" },
                    "error_code": result.as_ref().err().map(|error| error.code.as_str()),
                    "continue_after_batch": continue_after_batch,
                    "cancel_requested": cancel.is_cancelled(),
                }),
            );
            if let Err(error) = update_count(&app, &shared) {
                diagnostics::event(
                    "error",
                    "mail_job",
                    "history_refresh_failed",
                    serde_json::json!({
                        "job_id": id, "batch_index": index + 1, "error_code": error.code,
                    }),
                );
            }
            duration_estimate.observe(batch_started.elapsed());
            set_completion_estimate(
                &shared,
                &id,
                timing.estimated_completion(
                    &shared,
                    &batches,
                    &remaining,
                    None,
                    duration_estimate.mean(),
                    CooldownClock::now(),
                ),
            );
            publish_mail(&app, &shared);
            if !continue_after_batch {
                break;
            }
            index += 1;
        }
        let finished = {
            // 与启动新任务共用门闩：终态、旧令牌清理及最终发布不能被新任务创建插入。
            let _finish_operation = shared.operation.lock().await;
            let Some(finished) = finalize_current_mail(&shared, &id, cancel.is_cancelled()) else {
                return;
            };
            publish_mail(&app, &shared);
            finished
        };
        // 日志写入不持有 UI 状态锁，避免磁盘慢时阻塞状态读取或取消按钮。
        diagnostics::event(
            "info",
            "mail_job",
            "finished",
            serde_json::json!({
                "job_id": id, "status": finished.status, "total": finished.total,
                "sent": finished.sent, "unknown": finished.failed, "skipped": finished.skipped,
                "unattempted": finished.total.saturating_sub(finished.sent + finished.failed + finished.skipped),
                "elapsed_ms": job_started.elapsed().as_millis(),
                "cancel_requested": cancel.is_cancelled(),
            }),
        );
    });
    Ok(initial)
}

/// 仅匹配当前任务/当前等待才消费一次跳过；重复点击或旧响应不得给未来批次留下“通行证”。
fn request_wait_skip(view: &mut MailView, job_id: &str, wait_id: &str) -> AppResult<()> {
    if view.id != job_id
        || view.status != "running"
        || view.phase != "waiting"
        || view.wait_id.as_deref() != Some(wait_id)
    {
        return Err(AppError::new(
            "mail_wait_changed",
            "本次等待已结束或任务已变化，请查看最新发送状态。",
        ));
    }
    let signal = view.wait_skip.as_ref().ok_or_else(|| {
        AppError::new(
            "mail_wait_changed",
            "当前没有可跳过的等待，请查看最新发送状态。",
        )
    })?;
    signal.cancel();
    view.next_batch_at = None;
    view.estimated_completion_at = None;
    view.wait_reason = None;
    view.wait_id = None;
    view.wait_skip = None;
    view.message = "已跳过本次等待，正在准备下一批；后续仍按原间隔发送。".into();
    Ok(())
}

/// 用户主动放行当前待发一批；不改账号分配、发送参数、投递账本或其他账号冷却。
/// job_id/wait_id必须来自当前公开状态；只发布安全任务标识，不记录地址或邮件内容。
#[tauri::command]
pub fn skip_mail_wait(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    job_id: String,
    wait_id: String,
) -> AppResult<MailView> {
    {
        let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
        request_wait_skip(&mut view, &job_id, &wait_id)?;
    }
    diagnostics::event(
        "info",
        "mail_job",
        "batch_wait_skipped",
        serde_json::json!({"job_id":job_id,"wait_id":wait_id,"scope":"one_waiting_batch"}),
    );
    Ok(publish_mail(&app, &state))
}

/// 取消只停止后续批次，当前一封等待明确结果，避免“已收但没记账”的重复投递。
#[tauri::command]
pub fn cancel_mail_job(app: AppHandle, state: State<'_, Arc<AppState>>) -> MailView {
    if let Some(cancel) = state
        .mail_cancel
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        cancel.cancel();
    }
    {
        let mut view = state.mail.lock().unwrap_or_else(|p| p.into_inner());
        stop_mail_view(&mut view);
    }
    let view = publish_mail(&app, &state);
    diagnostics::event(
        "info",
        "mail_job",
        "cancel_requested",
        serde_json::json!({
            "job_id": view.id, "status": view.status,
        }),
    );
    view
}

#[cfg(test)]
mod wait_skip_tests {
    use super::*;

    /// 单次等待使用真实状态转换和信号；不创建发送器，不访问真实账号。
    fn waiting_view() -> MailView {
        let mut view = MailView {
            id: "synthetic-skip".into(),
            status: "running".into(),
            ..MailView::default()
        };
        assert!(wait_for_batch_view(
            &mut view,
            1,
            2,
            1,
            9_000_000,
            MailWaitReason::Combined
        ));
        view
    }

    #[test]
    fn skip_is_scoped_to_current_job_and_wait_and_cannot_be_replayed() {
        let mut view = waiting_view();
        let id = view.wait_id.clone().unwrap();
        let signal = view.wait_skip.clone().unwrap();
        for (job, wait) in [("old-job", id.as_str()), ("synthetic-skip", "old-wait")] {
            assert_eq!(
                request_wait_skip(&mut view, job, wait).unwrap_err().code,
                "mail_wait_changed"
            );
            assert!(!signal.is_cancelled());
        }
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["waitId"], id);
        assert!(json.get("waitSkip").is_none());
        request_wait_skip(&mut view, "synthetic-skip", &id).unwrap();
        assert!(signal.is_cancelled());
        assert!(view.wait_id.is_none());
        assert!(view.next_batch_at.is_none());
        assert_eq!(view.sent, 0);
        assert_eq!(view.failed, 0);
        assert_eq!(
            request_wait_skip(&mut view, "synthetic-skip", &id)
                .unwrap_err()
                .code,
            "mail_wait_changed"
        );
        assert!(wait_for_batch_view(
            &mut view,
            2,
            2,
            1,
            10_000_000,
            MailWaitReason::AccountCooldown
        ));
        assert_ne!(view.wait_id.as_ref().unwrap(), &id);
        assert!(!view.wait_skip.as_ref().unwrap().is_cancelled());
        assert_eq!(
            request_wait_skip(&mut view, "synthetic-skip", &id)
                .unwrap_err()
                .code,
            "mail_wait_changed"
        );
    }

    #[test]
    fn stopping_and_terminal_states_reject_skip_without_reviving_task() {
        for phase in ["sending", "stopping", "idle"] {
            let mut view = waiting_view();
            let id = view.wait_id.clone().unwrap();
            view.phase = phase.into();
            assert!(request_wait_skip(&mut view, "synthetic-skip", &id).is_err());
        }
        for status in ["cancelled", "succeeded", "failed"] {
            let mut view = waiting_view();
            let id = view.wait_id.clone().unwrap();
            view.status = status.into();
            assert!(request_wait_skip(&mut view, "synthetic-skip", &id).is_err());
            assert_eq!(view.status, status);
        }
        let mut view = waiting_view();
        let id = view.wait_id.clone().unwrap();
        stop_mail_view(&mut view);
        assert!(request_wait_skip(&mut view, "synthetic-skip", &id).is_err());
        assert!(view.wait_skip.is_none());
    }

    /// 使用真实异步等待验证唤醒；一秒超时是失败保护，不需要等待配置的900秒。
    #[tokio::test]
    async fn skip_wakes_current_wait_and_cancel_wins_if_both_ready() {
        let mut view = waiting_view();
        let id = view.wait_id.clone().unwrap();
        let signal = view.wait_skip.clone().unwrap();
        let waiter_signal = signal.clone();
        let waiter = tokio::spawn(async move {
            wait_for_round_deadline(
                tokio::time::Instant::now() + Duration::from_secs(900),
                &CancellationToken::new(),
                &waiter_signal,
            )
            .await
        });
        tokio::task::yield_now().await;
        request_wait_skip(&mut view, "synthetic-skip", &id).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(!wait_for_round_deadline(tokio::time::Instant::now(), &cancel, &signal).await);
    }
}

#[cfg(test)]
mod completion_estimate_tests {
    use super::*;

    /// 确认估算与已验证的整队列场景保持一致；重复估算只读内存池，不创建任务或SMTP尝试。
    #[test]
    fn confirmation_estimate_uses_frozen_message_counts_and_never_mutates_pool() {
        let state = AppState::new();
        state.sender_cooldowns.lock().unwrap().restored = true;
        for (counts, expected) in [
            (vec![3], 1830),
            (vec![2, 2], 990),
            (vec![1, 1, 1, 0, 0], 150),
            (vec![23; 5], 20310),
        ] {
            let accounts = counts
                .into_iter()
                .enumerate()
                .map(|(index, count)| (format!("sender-{index}@example.test"), count))
                .collect::<Vec<_>>();
            let before = chrono::Utc::now().timestamp_millis();
            let result = estimate_confirmation_completion(&state, &accounts, 900, 60)
                .unwrap()
                .unwrap();
            assert!((before + expected * 1000
                ..=chrono::Utc::now().timestamp_millis() + expected * 1000)
                .contains(&result));
        }
        assert!(state.sender_cooldowns.lock().unwrap().ready_at.is_empty());
        assert!(state
            .sender_cooldowns
            .lock()
            .unwrap()
            .last_completion
            .is_none());
        assert_eq!(state.mail.lock().unwrap().status, "idle");
        assert_eq!(
            estimate_confirmation_completion(&state, &[], 900, 60).unwrap(),
            None
        );
        for (own, switch) in [(0, 60), (3601, 60), (900, 3601)] {
            assert!(estimate_confirmation_completion(&state, &[], own, switch).is_err());
        }
    }

    /// 取消后保留的单调期限与同邮箱别名必须共同生效；零切换仍不能绕过自身冷却。
    #[test]
    fn confirmation_estimate_preserves_live_cooldown_and_same_sender_identity() {
        let state = AppState::new();
        {
            let mut pool = state.sender_cooldowns.lock().unwrap();
            pool.restored = true;
            pool.ready_at.insert(
                "sender@example.test".into(),
                tokio::time::Instant::now() + Duration::from_secs(500),
            );
        }
        let accounts = vec![
            ("SENDER@example.test".into(), 1),
            ("sender@example.test".into(), 1),
        ];
        let before = chrono::Utc::now().timestamp_millis();
        let estimate = estimate_confirmation_completion(&state, &accounts, 900, 0)
            .unwrap()
            .unwrap();
        assert!((before + 1_419_000..=before + 1_421_000).contains(&estimate));
        assert_eq!(state.sender_cooldowns.lock().unwrap().ready_at.len(), 1);
    }

    /// 重启后的未完成尝试只投影保守900秒，不在查看弹窗时清pending、写新截止或提前恢复共享池。
    #[test]
    fn confirmation_reads_persisted_pending_without_recovery_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("preview.sqlite3")).unwrap();
        database
            .begin_sender_cooldown_attempt("sender@example.test", 900)
            .unwrap();
        let before_records = database.sender_cooldowns().unwrap();
        let state = AppState::new();
        *state.database.lock().unwrap() = Some(database.clone());
        let accounts = vec![("sender@example.test".into(), 1)];
        for _ in 0..2 {
            let before = chrono::Utc::now().timestamp_millis();
            let estimate = estimate_confirmation_completion(&state, &accounts, 1, 0)
                .unwrap()
                .unwrap();
            assert!((before + 909_000..=before + 911_000).contains(&estimate));
        }
        assert_eq!(database.sender_cooldowns().unwrap(), before_records);
        assert!(!state.sender_cooldowns.lock().unwrap().restored);
        assert!(state.sender_cooldowns.lock().unwrap().ready_at.is_empty());
        assert_eq!(database.sent_count().unwrap(), 0);
        assert!(database.unresolved_deliveries().unwrap().is_empty());
    }

    /// 使用固定UTC与注入单调时钟验证整条队列；所有收件人均为合成数据，不连接SMTP或用户数据库。
    fn queue(
        recipients: usize,
        accounts: usize,
        size: usize,
    ) -> (AccountRoundTiming, Vec<AssignedMailBatch>, CooldownClock) {
        let recipients = (0..recipients)
            .map(|i| format!("estimate-{i}@example.test"))
            .collect::<Vec<_>>();
        let timing = AccountRoundTiming::new(
            (0..accounts)
                .map(|i| format!("sender-{i}@example.test"))
                .collect(),
            Duration::from_secs(900),
            Duration::from_secs(60),
        );
        let clock = CooldownClock {
            monotonic: tokio::time::Instant::now(),
            utc_unix_ms: 1_800_000_000_000,
        };
        (
            timing,
            assigned_mail_batches(&recipients, accounts, size).unwrap(),
            clock,
        )
    }

    #[test]
    fn initial_and_observed_batch_duration_exclude_deliberate_cooldowns() {
        let mut sample = BatchDurationEstimate::default();
        assert_eq!(sample.mean(), Duration::from_secs(10));
        sample.observe(Duration::from_secs(4));
        sample.observe(Duration::from_secs(8));
        assert_eq!(sample.mean(), Duration::from_secs(6));
        let mut instant = BatchDurationEstimate::default();
        instant.observe(Duration::ZERO);
        assert_eq!(instant.mean(), Duration::from_secs(1));
    }

    #[test]
    fn full_queue_respects_rounds_both_intervals_and_has_no_tail_wait() {
        // 单账号、双账号、账号多于收件人，以及用户截图的2860人/5账号/每封25人。
        for (recipients, accounts, size, expected_seconds) in [
            (3, 1, 1, 1830),
            (4, 2, 1, 990),
            (3, 5, 1, 150),
            (2860, 5, 25, 20310),
        ] {
            let (timing, batches, clock) = queue(recipients, accounts, size);
            let state = AppState::new();
            let remaining = (0..batches.len()).collect::<Vec<_>>();
            let estimate = timing.estimated_completion(
                &state,
                &batches,
                &remaining,
                None,
                Duration::from_secs(10),
                clock,
            );
            assert_eq!(estimate, Some(clock.utc_unix_ms + expected_seconds * 1000));
            assert!(state.sender_cooldowns.lock().unwrap().ready_at.is_empty());
            assert!(state
                .sender_cooldowns
                .lock()
                .unwrap()
                .last_completion
                .is_none());
        }
    }

    #[test]
    fn ready_first_simulation_uses_existing_deadlines_without_mutating_pool() {
        let (timing, batches, clock) = queue(4, 2, 1);
        let state = AppState::new();
        let own = clock.monotonic + Duration::from_secs(500);
        state
            .sender_cooldowns
            .lock()
            .unwrap()
            .ready_at
            .insert(timing.identities[0].clone(), own);
        // B先发0..10；A500..510；第二轮B910..920，A1410..1420。
        let estimate = timing.estimated_completion(
            &state,
            &batches,
            &[0, 1, 2, 3],
            None,
            Duration::from_secs(10),
            clock,
        );
        assert_eq!(estimate, Some(clock.utc_unix_ms + 1_420_000));
        let pool = state.sender_cooldowns.lock().unwrap();
        assert_eq!(pool.ready_at.len(), 1);
        assert_eq!(pool.ready_at[&timing.identities[0]], own);
        assert!(pool.last_completion.is_none());
    }

    #[test]
    fn active_batch_is_counted_once_and_changed_samples_update_finish() {
        let (timing, batches, clock) = queue(4, 2, 1);
        let state = AppState::new();
        // active已通过真实调度，不应再次等待它在begin_attempt登记的冷却。
        state.sender_cooldowns.lock().unwrap().ready_at.insert(
            timing.identities[0].clone(),
            clock.monotonic + Duration::from_secs(900),
        );
        assert_eq!(
            timing.estimated_completion(
                &state,
                &batches,
                &[1, 2, 3],
                Some(0),
                Duration::from_secs(10),
                clock
            ),
            Some(clock.utc_unix_ms + 990_000)
        );
        assert_eq!(
            timing.estimated_completion(
                &state,
                &batches,
                &[1, 2, 3],
                Some(0),
                Duration::from_secs(20),
                clock
            ),
            Some(clock.utc_unix_ms + 1_020_000)
        );
        assert_eq!(
            timing.estimated_completion(
                &state,
                &batches,
                &[],
                None,
                Duration::from_secs(10),
                clock
            ),
            None
        );
    }

    #[test]
    fn previous_switch_and_same_identity_follow_actual_dispatch_rules() {
        let (mut timing, batches, clock) = queue(2, 2, 1);
        let state = AppState::new();
        state.sender_cooldowns.lock().unwrap().last_completion = Some(SmtpCompletion {
            identity: "previous@example.test".into(),
            completed_at: clock.monotonic,
        });
        assert_eq!(
            timing.estimated_completion(
                &state,
                &batches,
                &[0, 1],
                None,
                Duration::from_secs(10),
                clock
            ),
            Some(clock.utc_unix_ms + 140_000)
        );
        timing.identities[1] = timing.identities[0].clone();
        // 重复身份必须共用自身900秒池，不能当作新的邮箱绕过冷却。
        assert_eq!(
            timing.estimated_completion(
                &state,
                &batches,
                &[0, 1],
                None,
                Duration::from_secs(10),
                clock
            ),
            Some(clock.utc_unix_ms + 980_000)
        );
    }

    #[test]
    fn cancellation_failure_terminal_and_late_worker_cannot_keep_estimate() {
        let state = AppState::new();
        *state.mail.lock().unwrap() = MailView {
            id: "estimate-current".into(),
            status: "running".into(),
            phase: "sending".into(),
            ..MailView::default()
        };
        set_completion_estimate(&state, "old-job", Some(123));
        assert_eq!(state.mail.lock().unwrap().estimated_completion_at, None);
        set_completion_estimate(&state, "estimate-current", Some(123));
        assert_eq!(
            serde_json::to_value(state.mail.lock().unwrap().clone()).unwrap()
                ["estimatedCompletionAt"],
            123
        );
        let view = state.mail.lock().unwrap().clone();
        let mut stopped = view.clone();
        stop_mail_view(&mut stopped);
        assert_eq!(stopped.estimated_completion_at, None);
        *state.mail.lock().unwrap() = stopped;
        set_completion_estimate(&state, "estimate-current", Some(456));
        assert_eq!(state.mail.lock().unwrap().estimated_completion_at, None);
        let mut failed = view.clone();
        apply_batch_outcome(
            &mut failed,
            &["synthetic@example.test".into()],
            &Err(AppError::new("synthetic", "合成失败")),
            false,
        );
        assert_eq!(failed.estimated_completion_at, None);
        let mut storage_failed = view.clone();
        fail_cooldown_storage(&mut storage_failed, true);
        assert_eq!(storage_failed.estimated_completion_at, None);
        for cancelled in [false, true] {
            let mut terminal = view.clone();
            finalize_mail_view(&mut terminal, cancelled);
            assert_eq!(terminal.estimated_completion_at, None);
            *state.mail.lock().unwrap() = terminal;
            set_completion_estimate(&state, "estimate-current", Some(456));
            assert_eq!(state.mail.lock().unwrap().estimated_completion_at, None);
        }
    }
}

#[cfg(test)]
mod countdown_tests {
    use super::*;

    /// 倒计时只操作公开任务状态，使用合成任务，不建立数据库、SMTP 或凭据仓库。
    fn running_view() -> MailView {
        MailView {
            id: "synthetic-countdown".into(),
            status: "running".into(),
            phase: "sending".into(),
            total: 4,
            sent: 2,
            ..MailView::default()
        }
    }

    #[test]
    fn missing_interval_defaults_to_fifteen_minutes_and_explicit_values_remain_valid() {
        let mut value = serde_json::json!({
            "recipients": "recipient@example.test",
            "subject": "合成主题",
            "html": "<p>合成正文</p>",
            "attachmentIds": [],
            "skipSent": true,
        });
        let draft: MailDraft = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(draft.interval_seconds, 900);
        for seconds in [1, 120, 300, 900, 3600] {
            value["intervalSeconds"] = serde_json::json!(seconds);
            let draft: MailDraft = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(draft.interval_seconds, seconds);
        }
    }

    #[test]
    fn account_switch_default_and_explicit_bounds_are_validated_before_side_effects() {
        let mut value = serde_json::json!({"recipients":"member@example.test","subject":"合成主题","html":"<p>合成内容</p>","attachmentIds":[],"skipSent":true});
        let legacy: MailDraft = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(legacy.account_switch_interval_seconds, 60);
        for seconds in [0, 60, 3600] {
            value["accountSwitchIntervalSeconds"] = serde_json::json!(seconds);
            let explicit: MailDraft = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(explicit.account_switch_interval_seconds, seconds);
            assert!(validate_account_switch_interval(seconds).is_ok());
        }
        assert_eq!(
            validate_account_switch_interval(3601).unwrap_err().code,
            "mail_account_switch_interval"
        );
        for invalid in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("60"),
        ] {
            value["accountSwitchIntervalSeconds"] = invalid;
            assert!(serde_json::from_value::<MailDraft>(value.clone()).is_err());
        }
    }

    #[test]
    fn wait_reason_is_camel_case_optional_and_cleared_by_sending_cancel_or_terminal() {
        for (reason, wire) in [
            (MailWaitReason::AccountCooldown, "accountCooldown"),
            (MailWaitReason::AccountSwitch, "accountSwitch"),
            (MailWaitReason::Combined, "combined"),
        ] {
            let mut view = running_view();
            assert!(wait_for_batch_view(
                &mut view,
                2,
                2,
                2,
                1_800_000_300_000,
                reason
            ));
            assert_eq!(serde_json::to_value(&view).unwrap()["waitReason"], wire);
            let mut stopped = view.clone();
            stop_mail_view(&mut stopped);
            assert_eq!(stopped.wait_reason, None);
            let mut terminal = view.clone();
            finalize_mail_view(&mut terminal, false);
            assert_eq!(terminal.wait_reason, None);
            assert!(begin_batch_view(&mut view, 2, 2, 2));
            assert_eq!(view.wait_reason, None);
            assert!(serde_json::to_value(view)
                .unwrap()
                .get("waitReason")
                .is_none());
        }
    }

    #[test]
    fn waiting_points_to_next_batch_and_sending_removes_deadline_without_changing_counts() {
        let mut view = running_view();
        let deadline = 1_800_000_300_000;
        assert!(wait_for_batch_view(
            &mut view,
            2,
            2,
            2,
            deadline,
            MailWaitReason::AccountCooldown
        ));
        assert_eq!(view.phase, "waiting");
        assert_eq!(view.next_batch_at, Some(deadline));
        assert_eq!(view.current, "第 2/2 批，2 位收件人");
        assert_eq!((view.sent, view.failed, view.skipped), (2, 0, 0));
        assert!(begin_batch_view(&mut view, 2, 2, 2));
        assert_eq!(view.phase, "sending");
        assert_eq!(view.next_batch_at, None);
        assert_eq!((view.sent, view.failed, view.skipped), (2, 0, 0));
        assert!(!wait_for_batch_view(
            &mut view,
            3,
            2,
            0,
            deadline,
            MailWaitReason::AccountCooldown
        ));
        assert_eq!(view.phase, "sending");
        assert_eq!(view.next_batch_at, None);
    }

    #[test]
    fn cancellation_immediately_clears_wait_and_cannot_be_overwritten_by_late_transition() {
        let mut view = running_view();
        assert!(wait_for_batch_view(
            &mut view,
            2,
            2,
            2,
            1_800_000_300_000,
            MailWaitReason::AccountCooldown
        ));
        stop_mail_view(&mut view);
        assert_eq!(view.phase, "stopping");
        assert_eq!(view.next_batch_at, None);
        assert!(view.current.is_empty());
        let stopping_message = view.message.clone();
        stop_mail_view(&mut view);
        assert_eq!(view.message, stopping_message);
        assert!(!begin_batch_view(&mut view, 2, 2, 2));
        assert!(!wait_for_batch_view(
            &mut view,
            2,
            2,
            2,
            1_800_000_300_000,
            MailWaitReason::AccountCooldown
        ));
        finalize_mail_view(&mut view, true);
        assert_eq!(view.status, "cancelled");
        assert_eq!(view.phase, "idle");
        assert_eq!(view.next_batch_at, None);
    }

    #[test]
    fn every_terminal_state_clears_countdown_and_preserves_delivery_outcome() {
        for (status, cancelled, expected) in [
            ("running", false, "completed"),
            ("running", true, "cancelled"),
            ("failed", false, "failed"),
        ] {
            let mut view = running_view();
            view.status = status.into();
            view.phase = "waiting".into();
            view.next_batch_at = Some(1_800_000_300_000);
            view.failed = 1;
            finalize_mail_view(&mut view, cancelled);
            assert_eq!(view.status, expected);
            assert_eq!(view.phase, "idle");
            assert_eq!(view.next_batch_at, None);
            assert_eq!((view.sent, view.failed), (2, 1));
            assert!(view.current.is_empty());
        }
    }

    #[test]
    fn published_versions_are_unique_under_concurrency_and_reads_keep_the_version() {
        let state = Arc::new(AppState::new());
        *state.mail.lock().unwrap() = running_view();
        let workers = (0..4)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || {
                    (0..50)
                        .map(|_| mail_snapshot_for_publish(&state).state_revision)
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut revisions = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        revisions.sort_unstable();
        assert_eq!(revisions, (1..=200).collect::<Vec<_>>());
        let first_read = state.mail.lock().unwrap().clone();
        let second_read = state.mail.lock().unwrap().clone();
        assert_eq!(first_read.state_revision, 200);
        assert_eq!(second_read.state_revision, first_read.state_revision);
        let serialized = serde_json::to_value(first_read).unwrap();
        assert_eq!(serialized["phase"], "sending");
        assert_eq!(serialized["nextBatchAt"], serde_json::Value::Null);
        assert_eq!(serialized["stateRevision"], 200);
        assert!(serialized.get("next_batch_at").is_none());
        let default = serde_json::to_value(MailView::default()).unwrap();
        assert_eq!(default["phase"], "idle");
        assert_eq!(default["nextBatchAt"], serde_json::Value::Null);
        assert_eq!(default["stateRevision"], 0);
    }

    #[tokio::test]
    async fn stale_worker_cannot_clear_new_task_token_or_countdown() {
        let state = AppState::new();
        let token = CancellationToken::new();
        *state.mail_cancel.lock().unwrap() = Some(token.clone());
        let mut view = running_view();
        view.id = "new-task".into();
        wait_for_batch_view(
            &mut view,
            2,
            2,
            2,
            1_800_000_300_000,
            MailWaitReason::AccountCooldown,
        );
        *state.mail.lock().unwrap() = view;
        let _operation = state.operation.lock().await;
        assert!(finalize_current_mail(&state, "old-task", false).is_none());
        assert!(state.mail_cancel.lock().unwrap().is_some());
        let current = state.mail.lock().unwrap().clone();
        assert_eq!(current.id, "new-task");
        assert_eq!(current.phase, "waiting");
        assert_eq!(current.next_batch_at, Some(1_800_000_300_000));
        assert!(finalize_current_mail(&state, "new-task", true).is_some());
        assert!(state.mail_cancel.lock().unwrap().is_none());
        let finished = state.mail.lock().unwrap().clone();
        assert_eq!(finished.status, "cancelled");
        assert_eq!(finished.phase, "idle");
        assert_eq!(finished.next_batch_at, None);
    }
}

#[cfg(test)]
mod batch_policy_tests {
    use super::*;
    use member_core::database::Database;
    use std::path::PathBuf;

    /// 只为策略回归保存虚构投递；测试发送结果由内存提供，绝不创建 SMTP 连接。
    struct FakeMailTask {
        database: Database,
        folder: PathBuf,
        view: MailView,
        attempts: Vec<Vec<String>>,
    }

    impl FakeMailTask {
        /// 使用真实投递账本及生产状态决策运行批次，outcomes 每项代表服务器接受与否。
        /// cancel_after 指第几次已尝试批次后取消；None 不取消，尚未尝试的批次不能产生记录。
        fn run(continue_on_failure: bool, outcomes: &[bool], cancel_after: Option<usize>) -> Self {
            let folder =
                std::env::temp_dir().join(format!("qq-mail-policy-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&folder).unwrap();
            let database = Database::open(folder.join("data.sqlite3")).unwrap();
            let mut task = Self {
                database,
                folder,
                view: MailView {
                    status: "running".into(),
                    total: outcomes.len() * 2,
                    ..MailView::default()
                },
                attempts: vec![],
            };
            let cancel = CancellationToken::new();
            for (index, accepted) in outcomes.iter().copied().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                let batch = vec![
                    format!("member-{index}-a@example.test"),
                    format!("member-{index}-b@example.test"),
                ];
                task.database.begin_delivery("fake-job", &batch).unwrap();
                task.attempts.push(batch.clone());
                let result = if accepted {
                    Ok(())
                } else {
                    Err(AppError::new(
                        "mail_delivery_unknown",
                        "发送过于频繁，请稍后再试。请先核实本批投递记录，避免重复发送。",
                    ))
                };
                task.database
                    .finish_delivery("fake-job", &batch, accepted, "虚构测试投递")
                    .unwrap();
                let should_continue =
                    apply_batch_outcome(&mut task.view, &batch, &result, continue_on_failure);
                if cancel_after == Some(task.attempts.len()) {
                    cancel.cancel();
                }
                if !should_continue {
                    break;
                }
            }
            finalize_mail_view(&mut task.view, cancel.is_cancelled());
            task
        }
    }

    impl Drop for FakeMailTask {
        fn drop(&mut self) {
            // 仅清理随机测试目录中的已知文件，断言失败时也不递归操作其他路径。
            for name in ["data.sqlite3", "data.sqlite3-wal", "data.sqlite3-shm"] {
                let _ = std::fs::remove_file(self.folder.join(name));
            }
            let _ = std::fs::remove_dir(&self.folder);
        }
    }

    #[test]
    fn legacy_start_rejects_all_manually_excluded_without_changing_settings_or_task() {
        let task = FakeMailTask::run(false, &[], None);
        let candidates: Vec<String> = vec!["blocked@example.test".into()];
        task.database
            .add_manual_exclusions(&candidates[0], "")
            .unwrap();
        task.database
            .set_setting("smtp_public", "unchanged")
            .unwrap();
        let state = AppState::new();
        assert_eq!(
            recipients_for_mail_start(&state, &task.database, None, &candidates, false)
                .unwrap_err()
                .code,
            "no_eligible_recipients"
        );
        assert_eq!(state.mail.lock().unwrap().status, "idle");
        assert!(state.mail_cancel.lock().unwrap().is_none());
        assert_eq!(
            task.database.setting("smtp_public").unwrap().as_deref(),
            Some("unchanged")
        );
        assert_eq!(task.database.sent_count().unwrap(), 0);
        assert!(task.database.unresolved_deliveries().unwrap().is_empty());
    }

    #[test]
    fn legacy_start_rejects_all_success_history_only_when_skip_sent_is_enabled() {
        let task = FakeMailTask::run(false, &[true], None);
        let candidates = task.attempts[0].clone();
        let state = AppState::new();
        assert_eq!(
            recipients_for_mail_start(&state, &task.database, None, &candidates, true)
                .unwrap_err()
                .code,
            "no_eligible_recipients"
        );
        let allowed =
            recipients_for_mail_start(&state, &task.database, None, &candidates, false).unwrap();
        assert_eq!(allowed.recipients, candidates);
        assert_eq!(state.mail.lock().unwrap().status, "idle");
        assert!(state.mail_cancel.lock().unwrap().is_none());
        assert_eq!(task.database.sent_count().unwrap(), 2);
        assert!(task.database.unresolved_deliveries().unwrap().is_empty());
    }

    #[test]
    fn legacy_draft_defaults_to_stop_without_recipient_plan() {
        let mut value = serde_json::json!({
            "recipients": "recipient@example.test",
            "subject": "测试主题",
            "html": "<p>测试内容</p>",
            "attachmentIds": [],
            "skipSent": true,
            "intervalSeconds": 1
        });
        let legacy: MailDraft = serde_json::from_value(value.clone()).unwrap();
        assert!(!legacy.continue_on_failure);
        assert!(legacy.recipient_plan_id.is_none());

        value["continueOnFailure"] = serde_json::json!(true);
        value["recipientPlanId"] = serde_json::json!("synthetic-plan");
        let selected: MailDraft = serde_json::from_value(value).unwrap();
        assert!(selected.continue_on_failure);
        assert_eq!(
            selected.recipient_plan_id.as_deref(),
            Some("synthetic-plan")
        );
    }

    #[test]
    fn default_failure_stops_and_leaves_later_recipients_unattempted() {
        let task = FakeMailTask::run(false, &[false, true, true], None);
        assert_eq!(task.attempts.len(), 1);
        assert_eq!(task.view.status, "failed");
        assert_eq!((task.view.sent, task.view.failed), (0, 2));
        assert_eq!(task.database.sent_count().unwrap(), 0);
        assert_eq!(task.database.unresolved_deliveries().unwrap().len(), 2);
        assert!(!task
            .database
            .has_unresolved("member-1-a@example.test")
            .unwrap());
        assert!(!task.database.was_sent("member-1-a@example.test").unwrap());
    }

    #[test]
    fn selected_continue_attempts_each_later_batch_once_and_keeps_unknown() {
        let task = FakeMailTask::run(true, &[false, true, true], None);
        assert_eq!(task.attempts.len(), 3);
        assert_eq!(task.view.status, "completed");
        assert_eq!((task.view.sent, task.view.failed), (4, 2));
        assert!(task.view.message.contains("2 人投递待核实"));
        assert_eq!(task.database.sent_count().unwrap(), 4);
        let unresolved = task.database.unresolved_deliveries().unwrap();
        assert_eq!(unresolved.len(), 2);
        assert!(unresolved
            .iter()
            .all(|row| row.email.starts_with("member-0-")));
        assert_eq!(task.view.failures.len(), 2);
        assert!(task.view.failures.iter().all(|failure| {
            failure.recipient.starts_with("member-0-")
                && failure.message
                    == "发送过于频繁，请稍后再试。请先核实本批投递记录，避免重复发送。"
        }));
        // 后批成功不能解除前批未决；即使另起任务，数据库仍阻止自动重复投递同一批。
        assert_eq!(
            task.database
                .begin_delivery("another-job", &task.attempts[0])
                .unwrap_err()
                .code,
            "delivery_unresolved"
        );
        assert!(!task.database.was_sent("member-0-a@example.test").unwrap());
    }

    #[test]
    fn all_failed_with_continue_finishes_queue_with_pending_verification() {
        let task = FakeMailTask::run(true, &[false, false, false], None);
        assert_eq!(task.attempts.len(), 3);
        assert_eq!(task.view.status, "completed");
        assert_eq!((task.view.sent, task.view.failed), (0, 6));
        assert!(task.view.message.contains("6 人投递待核实"));
        assert!(task.view.message.contains("未自动重试"));
        assert_eq!(task.database.unresolved_deliveries().unwrap().len(), 6);
        assert_eq!(task.database.sent_count().unwrap(), 0);
    }

    #[test]
    fn cancellation_after_failure_keeps_unknown_and_stops_unattempted_batches() {
        let task = FakeMailTask::run(true, &[false, true, true], Some(1));
        assert_eq!(task.attempts.len(), 1);
        assert_eq!(task.view.status, "cancelled");
        assert_eq!((task.view.sent, task.view.failed), (0, 2));
        assert!(task.view.message.contains("2 位收件人投递待核实"));
        assert_eq!(task.database.unresolved_deliveries().unwrap().len(), 2);
        assert!(!task
            .database
            .has_unresolved("member-1-a@example.test")
            .unwrap());
        assert!(task.view.current.is_empty());
    }

    #[test]
    fn success_counts_and_history_are_identical_with_both_failure_policies() {
        for continue_on_failure in [false, true] {
            let task = FakeMailTask::run(continue_on_failure, &[true, true], None);
            assert_eq!(task.attempts.len(), 2);
            assert_eq!(task.view.status, "completed");
            assert_eq!((task.view.sent, task.view.failed), (4, 0));
            assert_eq!(task.database.sent_count().unwrap(), 4);
            assert!(task.database.unresolved_deliveries().unwrap().is_empty());
        }
    }
}

#[cfg(test)]
mod multi_account_queue_tests {
    use super::*;

    /// 假发送器记录账号映射，使用真实SQLite共享账本和生产队列/结果策略，绝不建立SMTP连接。
    struct FakeAssignedTask {
        database: Database,
        folder: std::path::PathBuf,
        view: MailView,
        attempts: Vec<(usize, Vec<String>)>,
        eligible: Vec<String>,
    }

    impl FakeAssignedTask {
        /// 候选先有重复、成功历史及手动排除；过滤后八人再分给五个账号，首账号模拟待核实。
        fn run(continue_on_failure: bool, cancel_after: Option<usize>) -> Self {
            let folder = std::env::temp_dir()
                .join(format!("qq-account-queue-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&folder).unwrap();
            let database = Database::open(folder.join("data.sqlite3")).unwrap();
            let raw = (0..10)
                .map(|index| format!("recipient-{index}@example.test"))
                .collect::<Vec<_>>();
            database.begin_delivery("previous", &raw[..1]).unwrap();
            database
                .finish_delivery("previous", &raw[..1], true, "合成成功记录")
                .unwrap();
            database
                .add_manual_exclusions(&raw[1], "合成排除规则")
                .unwrap();
            let input = format!("{};{}", raw.join(";"), raw[2]);
            let candidates = mail::parse_recipients(&input)
                .unwrap()
                .into_iter()
                .map(|item| item.email)
                .collect::<Vec<_>>();
            let prepared = database
                .prepare_mail_recipients_with_exclusions(&candidates, true)
                .unwrap();
            assert_eq!(
                (
                    prepared.recipients.len(),
                    prepared.skipped_sent,
                    prepared.excluded.len()
                ),
                (8, 1, 1)
            );
            let batches = assigned_mail_batches(&prepared.recipients, 5, 2).unwrap();
            let mut task = Self {
                database,
                folder,
                view: MailView {
                    status: "running".into(),
                    total: 10,
                    skipped: 2,
                    skipped_sent: 1,
                    manual_excluded: 1,
                    ..MailView::default()
                },
                attempts: Vec::new(),
                eligible: prepared.recipients,
            };
            let cancel = CancellationToken::new();
            for (index, assigned) in batches.iter().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                let batch = &assigned.recipients;
                task.database
                    .begin_delivery("assigned-fake", batch)
                    .unwrap();
                task.attempts.push((assigned.account_index, batch.clone()));
                let accepted = index != 0;
                task.database
                    .finish_delivery("assigned-fake", batch, accepted, "合成服务器结果")
                    .unwrap();
                let result = if accepted {
                    Ok(())
                } else {
                    Err(AppError::new("mail_delivery_unknown", "合成待核实结果"))
                };
                let next = apply_batch_outcome(&mut task.view, batch, &result, continue_on_failure);
                if cancel_after == Some(task.attempts.len()) {
                    cancel.cancel();
                }
                if !next {
                    break;
                }
            }
            finalize_mail_view(&mut task.view, cancel.is_cancelled());
            task
        }
    }

    impl Drop for FakeAssignedTask {
        fn drop(&mut self) {
            // 只清理随机目录内已知测试文件，不递归触碰用户数据。
            for name in ["data.sqlite3", "data.sqlite3-wal", "data.sqlite3-shm"] {
                let _ = std::fs::remove_file(self.folder.join(name));
            }
            let _ = std::fs::remove_dir(&self.folder);
        }
    }

    #[test]
    fn filter_globally_then_run_accounts_serially_without_reassigning_failures() {
        let task = FakeAssignedTask::run(true, None);
        assert_eq!(
            task.attempts
                .iter()
                .map(|(account, _)| *account)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(
            task.attempts
                .iter()
                .map(|(_, batch)| batch.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 2, 1, 1]
        );
        assert_eq!(
            task.attempts
                .iter()
                .flat_map(|(_, batch)| batch.iter().cloned())
                .collect::<Vec<_>>(),
            task.eligible
        );
        assert_eq!(
            (task.view.sent, task.view.failed, task.view.skipped),
            (6, 2, 2)
        );
        assert_eq!(task.database.sent_count().unwrap(), 7); // 本次六人与一人历史成功使用同一全局账本。
        assert_eq!(task.database.unresolved_deliveries().unwrap().len(), 2);
        // 换账号不能绕过未决保护；后续账号成功同样进入共享成功历史。
        assert_eq!(
            task.database
                .begin_delivery("different-account", &task.attempts[0].1)
                .unwrap_err()
                .code,
            "delivery_unresolved"
        );
        assert!(task
            .database
            .prepare_mail_recipients_with_exclusions(&task.attempts[0].1, false)
            .is_err());
        let skipped = task
            .database
            .prepare_mail_recipients_with_exclusions(&task.attempts[1].1, true)
            .unwrap();
        assert_eq!((skipped.recipients.len(), skipped.skipped_sent), (0, 2));
    }

    #[test]
    fn failure_stop_and_cancel_leave_later_accounts_unattempted() {
        for (continue_on_failure, cancel_after, status) in
            [(false, None, "failed"), (true, Some(1), "cancelled")]
        {
            let task = FakeAssignedTask::run(continue_on_failure, cancel_after);
            assert_eq!(task.attempts.len(), 1);
            assert_eq!(task.attempts[0].0, 0);
            assert_eq!(task.view.status, status);
            assert_eq!(task.database.unresolved_deliveries().unwrap().len(), 2);
            for recipient in &task.eligible[2..] {
                assert!(!task.database.was_sent(recipient).unwrap());
                assert!(!task.database.has_unresolved(recipient).unwrap());
            }
        }
    }

    #[test]
    fn account_ranges_split_internally_and_zero_ranges_never_create_batches() {
        let recipients = (0..10)
            .map(|index| format!("member-{index}@example.test"))
            .collect::<Vec<_>>();
        let batches = assigned_mail_batches(&recipients, 3, 2).unwrap();
        assert_eq!(
            batches
                .iter()
                .map(|batch| (batch.account_index, batch.recipients.len()))
                .collect::<Vec<_>>(),
            vec![(0, 2), (1, 2), (2, 2), (0, 2), (1, 1), (2, 1)]
        );
        let small = assigned_mail_batches(&recipients[..1], 5, 20).unwrap();
        assert_eq!(small.len(), 1);
        assert_eq!(small[0].account_index, 0);
        let individual = assigned_mail_batches(&recipients[..8], 5, 1).unwrap();
        assert_eq!(
            individual
                .iter()
                .map(|batch| batch.account_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 0, 1, 2]
        );
        assert!(assigned_mail_batches(&[], 5, 20).unwrap().is_empty());
        assert!(assigned_mail_batches(&recipients, 0, 20).is_err());
    }
}

#[cfg(test)]
mod account_round_timing_tests {
    use super::*;

    /// 每次假投递保存真实账号、轮次和模拟单调时间，便于核对轮内顺序、时间边界及名单归属。
    struct Attempt {
        account: usize,
        round: usize,
        start: Duration,
        recipients: Vec<String>,
    }

    /// 生产分配、独立间隔、状态决策与真实共享SQLite账本组合运行；唯一替代项为时钟及SMTP结果。
    /// 测试推进注入的单调时刻，不等待真实秒数、不建立任何QQ或SMTP连接，也不读取用户数据。
    struct SimulatedTask {
        database: Database,
        _directory: tempfile::TempDir,
        attempts: Vec<Attempt>,
        waits: Vec<(usize, Duration)>,
        view: MailView,
    }

    impl SimulatedTask {
        /// failed_attempt为从1开始的失败批次；cancel_after为完成几批后取消，cancel_in_wait在首次等待时取消。
        /// 每批耗时来自durations；未提供的后续批次视为耗时1秒，所有结果在记账完成后起算本账号间隔。
        #[allow(clippy::too_many_arguments)]
        fn run(
            recipient_count: usize,
            account_count: usize,
            batch_size: usize,
            durations: &[u64],
            failed_attempt: Option<usize>,
            continue_on_failure: bool,
            cancel_after: Option<usize>,
            cancel_in_wait: bool,
        ) -> Self {
            Self::run_with_intervals(
                recipient_count,
                account_count,
                batch_size,
                durations,
                failed_attempt,
                continue_on_failure,
                cancel_after,
                cancel_in_wait,
                300,
                0,
            )
        }

        /// 两种间隔共同使用生产调度器；旧场景显式switch=0验证兼容，新场景验证默认60与自身冷却并行。
        #[allow(clippy::too_many_arguments)]
        fn run_with_intervals(
            recipient_count: usize,
            account_count: usize,
            batch_size: usize,
            durations: &[u64],
            failed_attempt: Option<usize>,
            continue_on_failure: bool,
            cancel_after: Option<usize>,
            cancel_in_wait: bool,
            own_seconds: u64,
            switch_seconds: u64,
        ) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let database = Database::open(directory.path().join("data.sqlite3")).unwrap();
            let recipients = (0..recipient_count)
                .map(|index| format!("round-{index}@example.test"))
                .collect::<Vec<_>>();
            let batches = assigned_mail_batches(&recipients, account_count, batch_size).unwrap();
            let interval = Duration::from_secs(own_seconds);
            let timing = AccountRoundTiming::new(
                (0..account_count)
                    .map(|index| format!("sender-{index}@example.test"))
                    .collect(),
                interval,
                Duration::from_secs(switch_seconds),
            );
            let state = AppState::new();
            let base = tokio::time::Instant::now();
            let utc_base = chrono::Utc::now().timestamp_millis();
            restore_sender_cooldowns(
                &state,
                &database,
                CooldownClock {
                    monotonic: base,
                    utc_unix_ms: utc_base,
                },
            )
            .unwrap();
            let mut now = base;
            let mut task = Self {
                database,
                _directory: directory,
                attempts: Vec::new(),
                waits: Vec::new(),
                view: MailView {
                    status: "running".into(),
                    total: recipient_count,
                    ..MailView::default()
                },
            };
            let cancel = CancellationToken::new();
            let mut remaining = (0..batches.len()).collect::<Vec<_>>();
            let mut index = 0;
            while !remaining.is_empty() {
                if cancel.is_cancelled() {
                    break;
                }
                let position = match timing.next_dispatch(&state, &batches, &remaining, now) {
                    RoundDispatch::Finished => break,
                    RoundDispatch::Ready(position) => position,
                    RoundDispatch::Waiting(position, deadline, reason) => {
                        let batch = &batches[remaining[position]];
                        task.waits
                            .push((batch.round_index, deadline.duration_since(now)));
                        assert!(wait_for_batch_view(
                            &mut task.view,
                            index + 1,
                            batches.len(),
                            batch.recipients.len(),
                            i64::try_from(deadline.duration_since(base).as_millis()).unwrap(),
                            reason,
                        ));
                        if cancel_in_wait {
                            cancel.cancel();
                            stop_mail_view(&mut task.view);
                            break;
                        }
                        now = deadline;
                        continue;
                    }
                };
                let batch = &batches[remaining.remove(position)];
                assert!(begin_batch_view(
                    &mut task.view,
                    index + 1,
                    batches.len(),
                    batch.recipients.len()
                ));
                timing
                    .begin_attempt(
                        &state,
                        &task.database,
                        batch.account_index,
                        CooldownClock {
                            monotonic: now,
                            utc_unix_ms: utc_base
                                + i64::try_from(now.duration_since(base).as_millis()).unwrap(),
                        },
                    )
                    .unwrap();
                task.database
                    .begin_delivery("round-fake-job", &batch.recipients)
                    .unwrap();
                task.attempts.push(Attempt {
                    account: batch.account_index,
                    round: batch.round_index,
                    start: now.duration_since(base),
                    recipients: batch.recipients.clone(),
                });
                now += Duration::from_secs(durations.get(index).copied().unwrap_or(1));
                let accepted = failed_attempt != Some(index + 1);
                task.database
                    .finish_delivery(
                        "round-fake-job",
                        &batch.recipients,
                        accepted,
                        "合成轮次投递结果",
                    )
                    .unwrap();
                let result = if accepted {
                    Ok(())
                } else {
                    Err(AppError::new("mail_delivery_unknown", "合成待核实结果"))
                };
                let next = apply_batch_outcome(
                    &mut task.view,
                    &batch.recipients,
                    &result,
                    continue_on_failure,
                );
                timing
                    .completed(
                        &state,
                        &task.database,
                        batch.account_index,
                        CooldownClock {
                            monotonic: now,
                            utc_unix_ms: utc_base
                                + i64::try_from(now.duration_since(base).as_millis()).unwrap(),
                        },
                    )
                    .unwrap();
                if cancel_after == Some(task.attempts.len()) {
                    cancel.cancel();
                    stop_mail_view(&mut task.view);
                }
                if !next {
                    break;
                }
                index += 1;
            }
            finalize_mail_view(&mut task.view, cancel.is_cancelled());
            task
        }
    }

    #[test]
    fn single_account_keeps_completion_based_interval_and_never_waits_after_tail() {
        let task = SimulatedTask::run(3, 1, 1, &[10, 20, 5], None, false, None, false);
        assert_eq!(
            task.attempts
                .iter()
                .map(|attempt| attempt.start.as_secs())
                .collect::<Vec<_>>(),
            vec![0, 310, 630]
        );
        assert_eq!(
            task.waits,
            vec![(2, Duration::from_secs(300)), (3, Duration::from_secs(300))]
        );
        assert_eq!(task.database.sent_count().unwrap(), 3);
        assert_eq!(task.view.status, "completed");
        assert_eq!(task.view.next_batch_at, None);
    }

    #[test]
    fn two_accounts_send_immediately_then_each_waits_only_its_own_remaining_interval() {
        let task = SimulatedTask::run(4, 2, 1, &[10, 20, 1, 1], None, false, None, false);
        assert_eq!(
            task.attempts
                .iter()
                .map(|attempt| (attempt.account, attempt.round, attempt.start.as_secs()))
                .collect::<Vec<_>>(),
            vec![(0, 1, 0), (1, 1, 10), (0, 2, 310), (1, 2, 330)]
        );
        assert_eq!(
            task.waits,
            vec![(2, Duration::from_secs(280)), (2, Duration::from_secs(19))]
        );
        // A上批完成10，B上批完成30，各自截止310和330；先发A，B只等自身剩余19秒，不反向阻塞A。
        assert_eq!(task.attempts[2].recipients, vec!["round-1@example.test"]);
        assert_eq!(task.attempts[3].recipients, vec!["round-3@example.test"]);
    }

    #[test]
    fn five_accounts_keep_one_batch_each_per_round_without_reassigning_recipients() {
        let task = SimulatedTask::run(10, 5, 1, &[10, 20, 30, 40, 50], None, false, None, false);
        assert_eq!(
            task.attempts
                .iter()
                .map(|attempt| attempt.account)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 0, 1, 2, 3, 4]
        );
        assert_eq!(
            task.waits,
            vec![
                (2, Duration::from_secs(160)),
                (2, Duration::from_secs(19)),
                (2, Duration::from_secs(29)),
                (2, Duration::from_secs(39)),
                (2, Duration::from_secs(49))
            ]
        );
        assert_eq!(task.attempts[5].start, Duration::from_secs(310));
        for account in 0..5 {
            let assigned = task
                .attempts
                .iter()
                .filter(|attempt| attempt.account == account)
                .flat_map(|attempt| attempt.recipients.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                assigned,
                vec![
                    format!("round-{}@example.test", account * 2),
                    format!("round-{}@example.test", account * 2 + 1)
                ]
            );
        }
        assert!(task.database.unresolved_deliveries().unwrap().is_empty());
    }

    #[test]
    fn switch_interval_one_two_and_five_accounts_use_actual_completion_without_tail_wait() {
        let single =
            SimulatedTask::run_with_intervals(2, 1, 1, &[10, 1], None, false, None, false, 900, 60);
        assert_eq!(
            single
                .attempts
                .iter()
                .map(|attempt| attempt.start.as_secs())
                .collect::<Vec<_>>(),
            vec![0, 910]
        );
        assert_eq!(single.waits, vec![(2, Duration::from_secs(900))]);
        let two = SimulatedTask::run_with_intervals(
            4,
            2,
            1,
            &[10, 20, 1, 1],
            None,
            false,
            None,
            false,
            900,
            60,
        );
        assert_eq!(
            two.attempts
                .iter()
                .map(|attempt| (attempt.account, attempt.start.as_secs()))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 70), (0, 910), (1, 990)]
        );
        // A完成10后切B到70；A再发只等自身910，不是900再加60。末批完成后不再追加一次切换等待。
        assert_eq!(
            two.waits,
            vec![
                (1, Duration::from_secs(60)),
                (2, Duration::from_secs(820)),
                (2, Duration::from_secs(79))
            ]
        );
        let five = SimulatedTask::run_with_intervals(
            5,
            5,
            1,
            &[1, 1, 1, 1, 1],
            None,
            false,
            None,
            false,
            900,
            60,
        );
        assert_eq!(
            five.attempts
                .iter()
                .map(|attempt| (attempt.account, attempt.start.as_secs()))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 61), (2, 122), (3, 183), (4, 244)]
        );
        assert_eq!(five.waits.len(), 4);
        assert_eq!(five.database.sent_count().unwrap(), 5);
        assert_eq!(five.view.next_batch_at, None);
        assert_eq!(five.view.wait_reason, None);
    }

    #[test]
    fn switch_interval_failure_continue_and_cancellation_preserve_unattempted_batches() {
        let continued = SimulatedTask::run_with_intervals(
            2,
            2,
            1,
            &[10, 1],
            Some(1),
            true,
            None,
            false,
            900,
            60,
        );
        assert_eq!(
            continued
                .attempts
                .iter()
                .map(|attempt| attempt.start.as_secs())
                .collect::<Vec<_>>(),
            vec![0, 70]
        );
        assert_eq!((continued.view.sent, continued.view.failed), (1, 1));
        assert_eq!(continued.database.unresolved_deliveries().unwrap().len(), 1);
        let stopped =
            SimulatedTask::run_with_intervals(2, 2, 1, &[10], Some(1), false, None, false, 900, 60);
        assert!(stopped.waits.is_empty());
        assert_eq!(stopped.attempts.len(), 1);
        let cancelled =
            SimulatedTask::run_with_intervals(2, 2, 1, &[10], None, false, None, true, 900, 60);
        assert_eq!(cancelled.attempts.len(), 1);
        assert_eq!(cancelled.view.status, "cancelled");
        assert_eq!(cancelled.view.wait_reason, None);
        assert!(!cancelled.database.was_sent("round-1@example.test").unwrap());
        assert!(!cancelled
            .database
            .has_unresolved("round-1@example.test")
            .unwrap());
        let unused =
            SimulatedTask::run_with_intervals(1, 5, 20, &[10], None, false, None, false, 900, 60);
        assert!(unused.waits.is_empty());
    }

    #[test]
    fn exhausted_accounts_do_not_delay_tail_and_other_work_consumes_remaining_interval() {
        let task = SimulatedTask::run(
            7,
            3,
            1,
            &[10, 20, 100, 10, 20, 100],
            None,
            false,
            None,
            false,
        );
        assert_eq!(
            task.attempts
                .iter()
                .map(|attempt| attempt.account)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 0, 1, 2, 0]
        );
        assert_eq!(
            task.waits,
            vec![
                (2, Duration::from_secs(180)),
                (2, Duration::from_secs(10)),
                (2, Duration::from_secs(80)),
                (3, Duration::from_secs(90))
            ]
        );
        assert_eq!(task.attempts[6].start, Duration::from_secs(620));
        // 第三轮只剩A，B/C已发完，不参与选择；B/C耗时抵扣A等待，不能重等完整300秒。
        let fully_elapsed =
            SimulatedTask::run(6, 5, 1, &[10, 30, 40, 50, 200], None, false, None, false);
        assert!(fully_elapsed.waits.is_empty());
        assert_eq!(fully_elapsed.attempts[5].start, Duration::from_secs(330));
        assert_eq!(fully_elapsed.attempts[5].account, 0);
    }

    #[test]
    fn zero_and_unused_accounts_create_no_wait_and_one_batch_has_no_tail_wait() {
        let empty = assigned_mail_batches(&[], 5, 20).unwrap();
        let timing = AccountRoundTiming::new(
            (0..5)
                .map(|index| format!("sender-{index}@example.test"))
                .collect(),
            Duration::from_secs(300),
            Duration::ZERO,
        );
        assert!(matches!(
            timing.next_dispatch(&AppState::new(), &empty, &[], tokio::time::Instant::now()),
            RoundDispatch::Finished
        ));
        let one = SimulatedTask::run(1, 5, 20, &[10], None, false, None, false);
        assert!(one.waits.is_empty());
        assert_eq!(one.attempts.len(), 1);
        assert_eq!(one.attempts[0].account, 0);
        assert_eq!(one.view.next_batch_at, None);
    }

    #[test]
    fn failure_policy_preserves_round_order_cooldown_and_shared_unknown_protection() {
        let continued = SimulatedTask::run(4, 2, 1, &[10, 20, 1, 1], Some(1), true, None, false);
        assert_eq!(
            continued
                .attempts
                .iter()
                .map(|attempt| attempt.account)
                .collect::<Vec<_>>(),
            vec![0, 1, 0, 1]
        );
        assert_eq!(
            continued.attempts[2].recipients,
            vec!["round-1@example.test"]
        );
        assert_eq!(
            continued.waits,
            vec![(2, Duration::from_secs(280)), (2, Duration::from_secs(19))]
        );
        assert_eq!(
            (
                continued.database.sent_count().unwrap(),
                continued.database.unresolved_deliveries().unwrap().len()
            ),
            (3, 1)
        );
        assert_eq!(
            continued
                .database
                .begin_delivery("other-account", &continued.attempts[0].recipients)
                .unwrap_err()
                .code,
            "delivery_unresolved"
        );
        let stopped = SimulatedTask::run(4, 2, 1, &[10], Some(1), false, None, false);
        assert_eq!(stopped.attempts.len(), 1);
        assert!(stopped.waits.is_empty());
        assert_eq!(stopped.view.status, "failed");
        assert!(!stopped.database.was_sent("round-2@example.test").unwrap());
    }

    #[test]
    fn cancelling_in_round_or_during_round_wait_never_reserves_future_batches() {
        for task in [
            SimulatedTask::run(4, 2, 1, &[10], None, false, Some(1), false),
            SimulatedTask::run(4, 2, 1, &[10, 20], None, false, None, true),
        ] {
            assert_eq!(task.view.status, "cancelled");
            assert_eq!(task.view.next_batch_at, None);
            assert!(task.view.current.is_empty());
            assert!(!task.database.was_sent("round-1@example.test").unwrap());
            assert!(!task
                .database
                .has_unresolved("round-1@example.test")
                .unwrap());
            assert!(task.database.unresolved_deliveries().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn cancelling_actual_wait_wakes_immediately_and_wins_when_deadline_is_ready() {
        let cancel = CancellationToken::new();
        let waiting_cancel = cancel.clone();
        let waiter = tokio::spawn(async move {
            wait_for_round_deadline(
                tokio::time::Instant::now() + Duration::from_secs(300),
                &waiting_cancel,
                &CancellationToken::new(),
            )
            .await
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(!waiter.await.unwrap());
        assert!(
            !wait_for_round_deadline(
                tokio::time::Instant::now(),
                &cancel,
                &CancellationToken::new()
            )
            .await
        );
        assert!(
            wait_for_round_deadline(
                tokio::time::Instant::now(),
                &CancellationToken::new(),
                &CancellationToken::new()
            )
            .await
        );
    }
}

#[cfg(test)]
mod persistent_cooldown_tests {
    use super::*;

    /// 独立临时SQLite和AppState；时间由成对注入的单调/UTC时刻提供，不连接真实邮箱或读取业务数据。
    struct Fixture {
        directory: tempfile::TempDir,
        database: Database,
        state: AppState,
        base: CooldownClock,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let database = Database::open(directory.path().join("data.sqlite3")).unwrap();
            let state = AppState::new();
            let base = CooldownClock::now();
            restore_sender_cooldowns(&state, &database, base).unwrap();
            Self {
                directory,
                database,
                state,
                base,
            }
        }

        /// 只推进合成时钟，不休眠；便于精确断言取消重试时剩余多少，而不是重新等完整间隔。
        fn at(&self, seconds: u64) -> CooldownClock {
            CooldownClock {
                monotonic: self.base.monotonic + Duration::from_secs(seconds),
                utc_unix_ms: self.base.utc_unix_ms + i64::try_from(seconds * 1000).unwrap(),
            }
        }

        fn timing(&self, identities: &[&str], interval: u64) -> AccountRoundTiming {
            self.timing_with_switch(identities, interval, 0)
        }

        /// 自身持久期限与本任务切换间隔分开注入，用同一生产调度器检查max及重建/重启语义。
        fn timing_with_switch(
            &self,
            identities: &[&str],
            interval: u64,
            switch_seconds: u64,
        ) -> AccountRoundTiming {
            AccountRoundTiming::new(
                identities
                    .iter()
                    .map(|identity| normalize_sender_cooldown_identity(identity).unwrap())
                    .collect(),
                Duration::from_secs(interval),
                Duration::from_secs(switch_seconds),
            )
        }

        /// 使用真实前/后登记和共享投递账本，只有SMTP服务器结果被合成布尔值替代。
        fn attempt(
            &self,
            timing: &AccountRoundTiming,
            account: usize,
            start: u64,
            finish: u64,
            accepted: bool,
            recipient: &str,
        ) {
            timing
                .begin_attempt(&self.state, &self.database, account, self.at(start))
                .unwrap();
            let recipients = vec![recipient.to_owned()];
            self.database
                .begin_delivery("persistent-fake", &recipients)
                .unwrap();
            self.database
                .finish_delivery("persistent-fake", &recipients, accepted, "合成跨任务投递")
                .unwrap();
            timing
                .completed(&self.state, &self.database, account, self.at(finish))
                .unwrap();
        }

        /// 暂时把本测试的SQLite文件位置变为目录来触发真实存储故障；不修改或删除任何用户路径。
        /// 只移动随机目录中的单文件，恢复后仍能核对故障前的pending和已发送记录。
        fn block_database(&self) {
            std::fs::rename(
                self.database.path(),
                self.directory.path().join("saved.sqlite3"),
            )
            .unwrap();
            std::fs::create_dir(self.database.path()).unwrap();
        }

        fn unblock_database(&self) {
            std::fs::remove_dir(self.database.path()).unwrap();
            std::fs::rename(
                self.directory.path().join("saved.sqlite3"),
                self.database.path(),
            )
            .unwrap();
        }
    }

    fn batch_queue(count: usize, accounts: usize) -> Vec<AssignedMailBatch> {
        let recipients = (0..count)
            .map(|index| format!("pool-{index}@example.test"))
            .collect::<Vec<_>>();
        assigned_mail_batches(&recipients, accounts, 1).unwrap()
    }

    fn waiting_deadline(dispatch: RoundDispatch) -> tokio::time::Instant {
        match dispatch {
            RoundDispatch::Waiting(_, deadline, _) => deadline,
            _ => panic!("合成账号应仍处于冷却"),
        }
    }

    /// 跳过只唤醒一批，不修改持久冷却；强制发送结束后仍登记新的自身期限，其他账号保持原值。
    #[test]
    fn skipping_one_wait_preserves_persistent_pool_and_future_cooldowns() {
        let fixture = Fixture::new();
        let timing = fixture.timing(&["sender-a@example.test", "sender-b@example.test"], 900);
        fixture.attempt(&timing, 0, 0, 10, true, "skip-first@example.test");
        fixture.attempt(&timing, 1, 10, 20, true, "skip-second@example.test");
        let queue = batch_queue(2, 2);
        let saved = fixture
            .database
            .setting("smtp_sender_cooldowns_v1")
            .unwrap();
        let before = waiting_deadline(timing.next_dispatch(
            &fixture.state,
            &queue,
            &[0],
            fixture.at(30).monotonic,
        ));
        let other = waiting_deadline(timing.next_dispatch(
            &fixture.state,
            &queue,
            &[1],
            fixture.at(30).monotonic,
        ));
        let mut view = MailView {
            id: "skip-pool".into(),
            status: "running".into(),
            ..MailView::default()
        };
        assert!(wait_for_batch_view(
            &mut view,
            1,
            2,
            1,
            fixture.at(910).utc_unix_ms,
            MailWaitReason::AccountCooldown
        ));
        let id = view.wait_id.clone().unwrap();
        request_wait_skip(&mut view, "skip-pool", &id).unwrap();
        assert_eq!(
            fixture
                .database
                .setting("smtp_sender_cooldowns_v1")
                .unwrap(),
            saved
        );
        assert_eq!(
            waiting_deadline(timing.next_dispatch(
                &fixture.state,
                &queue,
                &[0],
                fixture.at(30).monotonic
            )),
            before
        );
        fixture.attempt(&timing, 0, 30, 40, true, "skip-forced@example.test");
        assert_eq!(
            waiting_deadline(timing.next_dispatch(
                &fixture.state,
                &queue,
                &[0],
                fixture.at(40).monotonic
            )),
            fixture.at(940).monotonic
        );
        assert_eq!(
            waiting_deadline(timing.next_dispatch(
                &fixture.state,
                &queue,
                &[1],
                fixture.at(40).monotonic
            )),
            other
        );
    }

    #[test]
    fn switch_gap_uses_current_task_input_survives_cancel_and_is_not_restored_after_restart() {
        let fixture = Fixture::new();
        let first = fixture.timing_with_switch(&["sender-a@example.test"], 900, 60);
        fixture.attempt(&first, 0, 0, 10, true, "switch-first@example.test");
        let saved = fixture
            .database
            .setting("smtp_sender_cooldowns_v1")
            .unwrap();
        let queue = batch_queue(1, 1);
        for switch in [0, 60, 120, 3600] {
            let next = fixture.timing_with_switch(&["sender-b@example.test"], 900, switch);
            let dispatch =
                next.next_dispatch(&fixture.state, &queue, &[0], fixture.at(20).monotonic);
            if switch == 0 {
                assert!(matches!(dispatch, RoundDispatch::Ready(0)));
            } else {
                assert!(matches!(dispatch,
                    RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountSwitch)
                        if deadline == fixture.at(10 + switch).monotonic
                ));
            }
        }
        let mut view = MailView {
            status: "running".into(),
            ..MailView::default()
        };
        assert!(wait_for_batch_view(
            &mut view,
            1,
            1,
            1,
            fixture.at(70).utc_unix_ms,
            MailWaitReason::AccountSwitch
        ));
        stop_mail_view(&mut view);
        finalize_mail_view(&mut view, true);
        let rebuilt = fixture.timing_with_switch(&["sender-b@example.test"], 900, 60);
        assert!(
            matches!(rebuilt.next_dispatch(&fixture.state, &queue, &[0], fixture.at(40).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountSwitch) if deadline == fixture.at(70).monotonic
            )
        );
        assert_eq!(
            fixture
                .database
                .setting("smtp_sender_cooldowns_v1")
                .unwrap(),
            saved
        );
        assert_eq!(
            fixture
                .state
                .sender_cooldowns
                .lock()
                .unwrap()
                .last_completion
                .as_ref()
                .unwrap()
                .completed_at,
            fixture.at(10).monotonic
        );

        // 新进程只恢复SQLite的自身冷却；不凭持久历史猜测进程内的最后一次SMTP完成顺序。
        let restarted = AppState::new();
        restore_sender_cooldowns(&restarted, &fixture.database, fixture.at(40)).unwrap();
        assert!(matches!(
            rebuilt.next_dispatch(&restarted, &queue, &[0], fixture.at(40).monotonic),
            RoundDispatch::Ready(0)
        ));
        assert!(
            matches!(first.next_dispatch(&restarted, &queue, &[0], fixture.at(40).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountCooldown) if deadline == fixture.at(910).monotonic
            )
        );
        assert!(restarted
            .sender_cooldowns
            .lock()
            .unwrap()
            .last_completion
            .is_none());
    }

    #[test]
    fn effective_deadline_is_maximum_and_same_normalized_identity_has_no_switch_gap() {
        let fixture = Fixture::new();
        let timing = fixture.timing_with_switch(
            &["sender-a@example.test", "sender-b@example.test"],
            900,
            60,
        );
        fixture.attempt(&timing, 0, 0, 10, true, "max-a@example.test");
        fixture.attempt(&timing, 1, 70, 80, true, "max-b@example.test");
        let queue = batch_queue(1, 1);
        let a = fixture.timing_with_switch(&["sender-a@example.test"], 900, 60);
        assert!(
            matches!(a.next_dispatch(&fixture.state, &queue, &[0], fixture.at(100).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::Combined) if deadline == fixture.at(910).monotonic
            )
        );
        assert!(
            matches!(a.next_dispatch(&fixture.state, &queue, &[0], fixture.at(140).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountCooldown) if deadline == fixture.at(910).monotonic
            )
        );
        let b_alias = fixture.timing_with_switch(&["  SENDER-B@EXAMPLE.TEST  "], 900, 3600);
        assert!(
            matches!(b_alias.next_dispatch(&fixture.state, &queue, &[0], fixture.at(100).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountCooldown) if deadline == fixture.at(980).monotonic
            )
        );
        let long_switch = fixture.timing_with_switch(&["sender-a@example.test"], 900, 1200);
        assert!(
            matches!(long_switch.next_dispatch(&fixture.state, &queue, &[0], fixture.at(100).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::Combined) if deadline == fixture.at(1280).monotonic
            )
        );
        assert!(matches!(
            a.next_dispatch(&fixture.state, &queue, &[0], fixture.at(910).monotonic),
            RoundDispatch::Ready(0)
        ));
    }

    #[test]
    fn earliest_effective_deadline_keeps_round_order_and_directory_aliases_do_not_add_a_gap() {
        let fixture = Fixture::new();
        let initial = fixture.timing_with_switch(&["sender-a@example.test"], 900, 60);
        fixture.attempt(&initial, 0, 0, 10, true, "earliest-a@example.test");
        let timing = fixture.timing_with_switch(
            &["sender-a@example.test", "sender-b@example.test"],
            900,
            60,
        );
        let queue = batch_queue(4, 2);
        assert!(
            matches!(timing.next_dispatch(&fixture.state, &queue, &[0,1,2,3], fixture.at(20).monotonic),
                RoundDispatch::Waiting(1, deadline, MailWaitReason::AccountSwitch) if deadline == fixture.at(70).monotonic
            )
        );
        assert!(matches!(
            timing.next_dispatch(
                &fixture.state,
                &queue,
                &[0, 1, 2, 3],
                fixture.at(70).monotonic
            ),
            RoundDispatch::Ready(1)
        ));
        // B在第一轮完成后不能越过仍未就绪的A，提前发送第二轮的B。
        assert!(
            matches!(timing.next_dispatch(&fixture.state, &queue, &[0,2,3], fixture.at(70).monotonic),
                RoundDispatch::Waiting(0, deadline, MailWaitReason::AccountCooldown) if deadline == fixture.at(910).monotonic
            )
        );
        let aliases = fixture.timing_with_switch(
            &["SENDER-A@EXAMPLE.TEST", " sender-a@example.test "],
            900,
            3600,
        );
        assert!(matches!(
            aliases.next_dispatch(
                &fixture.state,
                &queue,
                &[0, 1, 2, 3],
                fixture.at(910).monotonic
            ),
            RoundDispatch::Ready(0)
        ));
        assert!(matches!(
            aliases.next_dispatch(
                &fixture.state,
                &queue,
                &[1, 2, 3],
                fixture.at(910).monotonic
            ),
            RoundDispatch::Ready(0)
        ));
        let fresh = AppState::new();
        assert!(matches!(
            timing.next_dispatch(&fresh, &queue, &[0, 1, 2, 3], fixture.at(0).monotonic),
            RoundDispatch::Ready(0)
        ));
    }

    #[test]
    fn cancel_rebuild_keeps_remaining_nine_hundred_seconds_and_wait_cancel_never_renews() {
        let fixture = Fixture::new();
        let first = fixture.timing(&["sender-a@example.test"], 900);
        fixture.attempt(&first, 0, 0, 10, true, "first@example.test");
        let queue = batch_queue(1, 1);
        let saved = fixture
            .database
            .setting("smtp_sender_cooldowns_v1")
            .unwrap();
        let second = fixture.timing(&["sender-a@example.test"], 900);
        let deadline = waiting_deadline(second.next_dispatch(
            &fixture.state,
            &queue,
            &[0],
            fixture.at(100).monotonic,
        ));
        assert_eq!(
            deadline.duration_since(fixture.at(100).monotonic),
            Duration::from_secs(810)
        );
        let mut view = MailView {
            status: "running".into(),
            ..MailView::default()
        };
        assert!(wait_for_batch_view(
            &mut view,
            1,
            1,
            1,
            fixture.at(910).utc_unix_ms,
            MailWaitReason::AccountCooldown,
        ));
        stop_mail_view(&mut view);
        finalize_mail_view(&mut view, true);
        let third = fixture.timing(&["sender-a@example.test"], 900);
        assert_eq!(
            waiting_deadline(third.next_dispatch(
                &fixture.state,
                &queue,
                &[0],
                fixture.at(400).monotonic
            )),
            fixture.at(910).monotonic
        );
        assert_eq!(
            fixture
                .database
                .setting("smtp_sender_cooldowns_v1")
                .unwrap(),
            saved
        );
        assert!(matches!(
            third.next_dispatch(&fixture.state, &queue, &[0], fixture.at(910).monotonic),
            RoundDispatch::Ready(0)
        ));
    }

    #[test]
    fn ready_account_b_bypasses_cancelled_account_a_and_remains_independent_in_later_round() {
        let fixture = Fixture::new();
        let initial = fixture.timing(&["sender-a@example.test"], 900);
        fixture.attempt(&initial, 0, 0, 10, false, "unknown@example.test");
        let next = fixture.timing(&["sender-a@example.test", "sender-b@example.test"], 900);
        let queue = batch_queue(4, 2);
        assert!(matches!(
            next.next_dispatch(
                &fixture.state,
                &queue,
                &[0, 1, 2, 3],
                fixture.at(100).monotonic
            ),
            RoundDispatch::Ready(1)
        ));
        fixture.attempt(&next, 1, 100, 101, true, "from-b@example.test");
        assert_eq!(
            waiting_deadline(next.next_dispatch(
                &fixture.state,
                &queue,
                &[0, 2, 3],
                fixture.at(101).monotonic
            )),
            fixture.at(910).monotonic
        );
        fixture.attempt(&next, 0, 910, 911, true, "new-a@example.test");
        let dispatch =
            next.next_dispatch(&fixture.state, &queue, &[2, 3], fixture.at(911).monotonic);
        assert!(
            matches!(dispatch,RoundDispatch::Waiting(1,deadline,_) if deadline==fixture.at(1001).monotonic)
        );
        assert!(matches!(
            next.next_dispatch(&fixture.state, &queue, &[2, 3], fixture.at(1001).monotonic),
            RoundDispatch::Ready(1)
        ));
        assert_eq!(fixture.database.unresolved_deliveries().unwrap().len(), 1);
        assert_eq!(fixture.database.sent_count().unwrap(), 2);
    }

    #[test]
    fn identity_is_stable_across_directory_id_password_config_case_and_interval_changes() {
        let fixture = Fixture::new();
        let old = fixture.timing(&["Sender@EXAMPLE.TEST"], 900);
        fixture.attempt(&old, 0, 0, 10, true, "identity@example.test");
        // 新目录ID/服务器/密码不参与键，重新解析同一From邮箱仍命中同池；明确换From才是另一身份。
        let rebuilt = fixture.timing(&[" sender@example.test "], 1);
        let longer = fixture.timing(&["sender@example.test"], 3600);
        let queue = batch_queue(1, 1);
        for timing in [&rebuilt, &longer] {
            assert_eq!(
                waiting_deadline(timing.next_dispatch(
                    &fixture.state,
                    &queue,
                    &[0],
                    fixture.at(100).monotonic
                )),
                fixture.at(910).monotonic
            );
        }
        assert!(matches!(
            fixture.timing(&["other@example.test"], 900).next_dispatch(
                &fixture.state,
                &queue,
                &[0],
                fixture.at(100).monotonic
            ),
            RoundDispatch::Ready(0)
        ));
        // 降低间隔也不能改写旧截止；运行期间UTC跳变不会重读并重算共享单调期限。
        rebuilt.merge_memory_deadline(
            &fixture.state,
            &rebuilt.identities[0],
            fixture.at(101).monotonic,
        );
        restore_sender_cooldowns(
            &fixture.state,
            &fixture.database,
            CooldownClock {
                monotonic: fixture.at(100).monotonic,
                utc_unix_ms: fixture.at(100).utc_unix_ms + 86_400_000,
            },
        )
        .unwrap();
        assert_eq!(
            waiting_deadline(rebuilt.next_dispatch(
                &fixture.state,
                &queue,
                &[0],
                fixture.at(100).monotonic
            )),
            fixture.at(910).monotonic
        );
    }

    #[test]
    fn new_process_restores_only_remaining_time_without_renewing_completed_cooldown() {
        let fixture = Fixture::new();
        let first = fixture.timing(&["restart@example.test"], 900);
        fixture.attempt(&first, 0, 0, 10, true, "restart-recipient@example.test");
        let saved = fixture
            .database
            .setting("smtp_sender_cooldowns_v1")
            .unwrap();
        let restarted = AppState::new();
        let restart_clock = CooldownClock {
            monotonic: fixture.base.monotonic + Duration::from_secs(10_000),
            utc_unix_ms: fixture.at(400).utc_unix_ms,
        };
        restore_sender_cooldowns(&restarted, &fixture.database, restart_clock).unwrap();
        let queue = batch_queue(1, 1);
        let deadline = waiting_deadline(first.next_dispatch(
            &restarted,
            &queue,
            &[0],
            restart_clock.monotonic,
        ));
        assert_eq!(
            deadline.duration_since(restart_clock.monotonic),
            Duration::from_secs(510)
        );
        assert_eq!(
            fixture
                .database
                .setting("smtp_sender_cooldowns_v1")
                .unwrap(),
            saved
        );
    }

    #[test]
    fn cancel_after_registration_clears_pending_without_network_delivery_or_new_completion() {
        let fixture = Fixture::new();
        let first = fixture.timing_with_switch(&["first@example.test"], 900, 60);
        fixture.attempt(&first, 0, 0, 10, true, "before-cancel@example.test");
        let next = fixture.timing_with_switch(&["unattempted@example.test"], 900, 60);
        let registered = next
            .begin_attempt(&fixture.state, &fixture.database, 0, fixture.at(70))
            .unwrap();
        let cancel = CancellationToken::new();
        assert!(!next
            .stop_before_smtp(&fixture.database, 0, registered, &cancel)
            .unwrap());
        assert_eq!(
            fixture
                .database
                .sender_cooldowns()
                .unwrap()
                .iter()
                .find(|row| row.identity == next.identities[0])
                .unwrap()
                .pending_interval_seconds,
            Some(900)
        );
        cancel.cancel();
        assert!(next
            .stop_before_smtp(&fixture.database, 0, registered, &cancel)
            .unwrap());
        let stored = fixture.database.sender_cooldowns().unwrap();
        let unattempted = stored
            .iter()
            .find(|row| row.identity == next.identities[0])
            .unwrap();
        assert_eq!(unattempted.ready_at_unix_ms, registered);
        assert_eq!(unattempted.pending_interval_seconds, None);
        let cooldowns = fixture.state.sender_cooldowns.lock().unwrap();
        assert_eq!(
            cooldowns.ready_at[&next.identities[0]],
            fixture.at(970).monotonic
        );
        let last = cooldowns.last_completion.as_ref().unwrap();
        assert_eq!(last.identity, first.identities[0]);
        assert_eq!(last.completed_at, fixture.at(10).monotonic);
        assert_eq!(fixture.database.sent_count().unwrap(), 1);
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
    }

    /// 模拟预留事务提交前才收到取消；宿主必须把false作为停止信号，不能当作投递失败落待核实记录。
    /// 使用真实SQLite回滚及冷却清理，合成SMTP计数器确保取消分支不进入网络，也不刷新切换计时。
    #[test]
    fn cancel_during_delivery_reservation_rolls_back_and_clears_pending_without_completion() {
        let fixture = Fixture::new();
        let first = fixture.timing_with_switch(&["first@example.test"], 900, 60);
        fixture.attempt(
            &first,
            0,
            0,
            10,
            true,
            "before-reservation-cancel@example.test",
        );
        let next = fixture.timing_with_switch(&["unattempted@example.test"], 900, 60);
        let registered = next
            .begin_attempt(&fixture.state, &fixture.database, 0, fixture.at(70))
            .unwrap();
        let cancel = CancellationToken::new();
        assert!(!next
            .stop_before_smtp(&fixture.database, 0, registered, &cancel)
            .unwrap());
        let checks = std::cell::Cell::new(0);
        let recipients = vec!["cancelled-reservation@example.test".to_owned()];
        let reserved = fixture
            .database
            .begin_delivery_with_sender_unless_cancelled(
                "cancelled-reservation",
                &recipients,
                "unattempted@example.test",
                "未发送账号",
                || {
                    checks.set(checks.get() + 1);
                    // 第一次取得写锁时尚未取消，第二次提交前取消，覆盖已有预留行需要整体回滚的路径。
                    if checks.get() == 2 {
                        cancel.cancel();
                    }
                    cancel.is_cancelled()
                },
            )
            .unwrap();
        let mut network_attempts = 0;
        if reserved {
            network_attempts += 1;
        } else {
            assert!(next
                .stop_before_smtp(&fixture.database, 0, registered, &cancel)
                .unwrap());
        }
        assert!(!reserved);
        assert_eq!(checks.get(), 2);
        assert_eq!(network_attempts, 0);
        let stored = fixture.database.sender_cooldowns().unwrap();
        let unattempted = stored
            .iter()
            .find(|row| row.identity == next.identities[0])
            .unwrap();
        assert_eq!(unattempted.ready_at_unix_ms, registered);
        assert_eq!(unattempted.pending_interval_seconds, None);
        let cooldowns = fixture.state.sender_cooldowns.lock().unwrap();
        assert_eq!(
            cooldowns.ready_at[&next.identities[0]],
            fixture.at(970).monotonic
        );
        let last = cooldowns.last_completion.as_ref().unwrap();
        assert_eq!(last.identity, first.identities[0]);
        assert_eq!(last.completed_at, fixture.at(10).monotonic);
        assert_eq!(fixture.database.sent_count().unwrap(), 1);
        assert!(!fixture.database.was_sent(&recipients[0]).unwrap());
        assert!(!fixture.database.has_unresolved(&recipients[0]).unwrap());
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
    }

    #[test]
    fn registration_failure_is_zero_network_no_delivery_record_and_no_new_memory_pool() {
        let fixture = Fixture::new();
        let timing = fixture.timing(&["unattempted@example.test"], 900);
        fixture.block_database();
        let mut network_attempts = 0;
        if timing
            .begin_attempt(&fixture.state, &fixture.database, 0, fixture.at(0))
            .is_ok()
        {
            network_attempts += 1;
        }
        assert_eq!(network_attempts, 0);
        let mut view = MailView {
            status: "running".into(),
            total: 1,
            ..MailView::default()
        };
        fail_cooldown_storage(&mut view, false);
        assert_eq!((view.sent, view.failed), (0, 0));
        assert_eq!(view.status, "failed");
        assert!(fixture
            .state
            .sender_cooldowns
            .lock()
            .unwrap()
            .ready_at
            .is_empty());
        assert!(fixture
            .state
            .sender_cooldowns
            .lock()
            .unwrap()
            .last_completion
            .is_none());
        fixture.unblock_database();
        assert_eq!(fixture.database.sent_count().unwrap(), 0);
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
        assert!(fixture.database.sender_cooldowns().unwrap().is_empty());
    }

    #[test]
    fn accepted_delivery_survives_cooldown_finish_failure_and_pending_recovers_once() {
        let fixture = Fixture::new();
        let timing = fixture.timing(&["accepted@example.test"], 900);
        timing
            .begin_attempt(&fixture.state, &fixture.database, 0, CooldownClock::now())
            .unwrap();
        let recipients = vec!["accepted-recipient@example.test".into()];
        fixture
            .database
            .begin_delivery("accepted-fake", &recipients)
            .unwrap();
        fixture
            .database
            .finish_delivery("accepted-fake", &recipients, true, "")
            .unwrap();
        let mut view = MailView {
            status: "running".into(),
            total: 1,
            ..MailView::default()
        };
        assert!(apply_batch_outcome(&mut view, &recipients, &Ok(()), false));
        fixture.block_database();
        let completion = CooldownClock::now();
        assert!(timing
            .completed(&fixture.state, &fixture.database, 0, completion)
            .is_err());
        fail_cooldown_storage(&mut view, true);
        assert_eq!((view.sent, view.failed), (1, 0));
        assert_eq!(view.status, "failed");
        assert_eq!(
            fixture.state.sender_cooldowns.lock().unwrap().ready_at[&timing.identities[0]],
            completion.monotonic + Duration::from_secs(900)
        );
        assert_eq!(
            fixture
                .state
                .sender_cooldowns
                .lock()
                .unwrap()
                .last_completion
                .as_ref()
                .unwrap()
                .completed_at,
            completion.monotonic
        );
        fixture.unblock_database();
        assert_eq!(fixture.database.sent_count().unwrap(), 1);
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(
            fixture.database.sender_cooldowns().unwrap()[0].pending_interval_seconds,
            Some(900)
        );
        let restarted = AppState::new();
        restore_sender_cooldowns(&restarted, &fixture.database, CooldownClock::now()).unwrap();
        assert_eq!(
            fixture.database.sender_cooldowns().unwrap()[0].pending_interval_seconds,
            None
        );
        let recovered = fixture
            .database
            .setting("smtp_sender_cooldowns_v1")
            .unwrap();
        let next_restart = AppState::new();
        restore_sender_cooldowns(&next_restart, &fixture.database, CooldownClock::now()).unwrap();
        assert_eq!(
            fixture
                .database
                .setting("smtp_sender_cooldowns_v1")
                .unwrap(),
            recovered
        );
    }

    #[test]
    fn corrupt_cooldown_read_fails_closed_and_local_reservation_failure_retains_only_registration()
    {
        let fixture = Fixture::new();
        let timing = fixture.timing(&["local-failure@example.test"], 900);
        fixture
            .database
            .add_manual_exclusions("blocked@example.test", "")
            .unwrap();
        let deadline = timing
            .begin_attempt(&fixture.state, &fixture.database, 0, fixture.at(0))
            .unwrap();
        assert!(fixture
            .database
            .begin_delivery("not-sent", &["blocked@example.test".into()])
            .is_err());
        // 该批已登记真实投递准备；后续本地预留失败保守保留一次登记，不从失败时刻再加900秒。
        fixture
            .database
            .finish_sender_cooldown_attempt(&timing.identities[0], deadline)
            .unwrap();
        assert_eq!(
            fixture.state.sender_cooldowns.lock().unwrap().ready_at[&timing.identities[0]],
            fixture.at(900).monotonic
        );
        assert!(fixture
            .state
            .sender_cooldowns
            .lock()
            .unwrap()
            .last_completion
            .is_none());
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(
            fixture.database.sender_cooldowns().unwrap()[0].pending_interval_seconds,
            None
        );
        fixture
            .database
            .set_setting("smtp_sender_cooldowns_v1", "{invalid")
            .unwrap();
        let restarted = AppState::new();
        assert!(
            restore_sender_cooldowns(&restarted, &fixture.database, CooldownClock::now()).is_err()
        );
        assert!(!restarted.sender_cooldowns.lock().unwrap().restored);
        assert_eq!(restarted.mail.lock().unwrap().status, "idle");
    }
}

#[cfg(all(test, windows))]
mod credential_tests {
    use super::*;
    use member_core::database::Database;

    #[test]
    fn only_nonzero_assigned_accounts_need_credentials_and_all_validate_before_delivery() {
        let folder = tempfile::tempdir().unwrap();
        let state = AppState::new();
        state
            .initialize_database(Database::open(folder.path().join("data.sqlite3")).unwrap())
            .unwrap();
        let settings = MailSettings {
            host: "smtp.example.test".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: "sender@example.test".into(),
            password: "synthetic-multi-account-secret".into(),
            from_address: "sender@example.test".into(),
            from_name: "合成账号".into(),
        };
        let saved = crate::sender_account_commands::save_legacy_account(&state, &settings).unwrap();
        let good_id = saved.accounts[0].id.clone();
        let database = state.database().unwrap();
        for index in 0..4 {
            let catalog = database.sender_accounts().unwrap();
            let mut public =
                member_core::sender_accounts::SenderPublicSettings::from_mail(&settings);
            public.username = format!("missing-{index}@example.test");
            public.from_address = public.username.clone();
            database
                .save_sender_account(
                    catalog.revision,
                    &uuid::Uuid::new_v4().simple().to_string(),
                    &format!("未参与{index}"),
                    &public,
                    None,
                )
                .unwrap();
        }
        let catalog = database.sender_accounts().unwrap();
        let mut selection = PreparedAccountSelection {
            ids: catalog
                .accounts
                .iter()
                .map(|account| account.id.clone())
                .collect(),
            revision: catalog.revision,
            send_mode: "bcc".into(),
            batch_size: 20,
            allocation_mode: "random".into(),
        };
        let draft:MailDraft=serde_json::from_value(serde_json::json!({"recipients":"member@example.test","subject":"合成主题","html":"<p>合成正文</p>","attachmentIds":[],"skipSent":true})).unwrap();
        let recipients = vec!["member@example.test".into()];
        let senders = multi_account_senders(&state, &selection, &recipients, &draft, &[]).unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0].origin.identity, "sender@example.test");
        assert_eq!(senders[0].origin.address, "sender@example.test");
        assert_eq!(senders[0].origin.name, "默认发件账号");
        assert_eq!(selection.ids[0], good_id);
        // 第二位分到真实收件人时缺凭据，整个准备失败；首账号也没有任何已发送/未决记录。
        let two = vec!["member@example.test".into(), "second@example.test".into()];
        assert!(
            matches!(multi_account_senders(&state,&selection,&two,&draft,&[]),Err(error) if error.code=="missing_smtp_credentials")
        );
        // 随机冻结的首账号是缺凭据账号时，不能借用目录首账号的有效凭据。
        selection.ids.swap(0, 1);
        assert!(
            matches!(multi_account_senders(&state,&selection,&recipients,&draft,&[]),Err(error) if error.code=="missing_smtp_credentials")
        );
        assert_eq!(database.sent_count().unwrap(), 0);
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(state.mail.lock().unwrap().status, "idle");
        assert!(state.mail_cancel.lock().unwrap().is_none());
    }

    /// 真实宿主解析DPAPI合成账号，只把SMTP结果替换为本机账本提交；不发送邮件或读取业务目录。
    #[test]
    fn frozen_sender_origin_follows_ordered_and_random_assignment_and_survives_catalog_changes() {
        use member_core::sender_accounts::{SenderCredentialReference, SenderPublicSettings};

        let folder = tempfile::tempdir().unwrap();
        let state = AppState::new();
        state
            .initialize_database(Database::open(folder.path().join("data.sqlite3")).unwrap())
            .unwrap();
        let database = state.database().unwrap();
        let mut ids = Vec::new();
        for index in 0..2 {
            let settings = MailSettings {
                host: "smtp.example.test".into(),
                port: 465,
                tls_mode: "tls".into(),
                username: format!("authentication-{index}@example.test"),
                password: format!("synthetic-origin-secret-{index}"),
                from_address: format!("sender-{index}@example.test"),
                from_name: format!("SMTP显示姓名{index}"),
            };
            let id = uuid::Uuid::new_v4().simple().to_string();
            let catalog = database.sender_accounts().unwrap();
            let catalog = database
                .save_sender_account(
                    catalog.revision,
                    &id,
                    &format!("目录账号{index}"),
                    &SenderPublicSettings::from_mail(&settings),
                    Some(SenderCredentialReference::Named {
                        key: uuid::Uuid::new_v4().simple().to_string(),
                    }),
                )
                .unwrap();
            let account = catalog
                .accounts
                .iter()
                .find(|account| account.id == id)
                .unwrap();
            account
                .credential_store(folder.path())
                .unwrap()
                .unwrap()
                .save(&account.credential_binding(), &settings.password)
                .unwrap();
            ids.push(id);
        }
        let revision = database.sender_accounts().unwrap().revision;
        let draft: MailDraft = serde_json::from_value(serde_json::json!({
            "recipients":"placeholder@example.test", "subject":"合成主题", "html":"<p>合成正文</p>",
            "attachmentIds":[], "skipSent":true,
        }))
        .unwrap();
        for (mode, frozen_ids, expected_accounts) in [
            ("ordered", ids.clone(), [0, 1]),
            ("random", vec![ids[1].clone(), ids[0].clone()], [1, 0]),
        ] {
            let selection = PreparedAccountSelection {
                ids: frozen_ids,
                revision,
                send_mode: "individual".into(),
                batch_size: 1,
                allocation_mode: mode.into(),
            };
            let recipients = vec![
                format!("{mode}-accepted@example.test"),
                format!("{mode}-unknown@example.test"),
            ];
            let senders =
                multi_account_senders(&state, &selection, &recipients, &draft, &[]).unwrap();
            let batches = assigned_mail_batches(&recipients, 2, 1).unwrap();
            for batch in &batches {
                let origin = &senders[batch.account_index].origin;
                let account = expected_accounts[batch.account_index];
                assert_eq!(origin.address, format!("sender-{account}@example.test"));
                assert_eq!(origin.identity, origin.address);
                // 记录所选目录名称和实际From，不误用SMTP登录名或From显示姓名。
                assert_eq!(origin.name, format!("目录账号{account}"));
                database
                    .begin_delivery_with_sender(
                        mode,
                        &batch.recipients,
                        &origin.address,
                        &origin.name,
                    )
                    .unwrap();
                database
                    .finish_delivery(
                        mode,
                        &batch.recipients,
                        batch.account_index == 0,
                        "合成归属结果",
                    )
                    .unwrap();
            }
        }
        let sent = database.sent_records().unwrap();
        let unknown = database.unresolved_deliveries().unwrap();
        for (mode, accepted_account, unknown_account) in [("ordered", 0, 1), ("random", 1, 0)] {
            let accepted = sent
                .iter()
                .find(|row| row.email == format!("{mode}-accepted@example.test"))
                .unwrap();
            assert_eq!(
                accepted.sender_address,
                format!("sender-{accepted_account}@example.test")
            );
            assert_eq!(accepted.sender_name, format!("目录账号{accepted_account}"));
            let unresolved = unknown
                .iter()
                .find(|row| row.email == format!("{mode}-unknown@example.test"))
                .unwrap();
            assert_eq!(
                unresolved.sender_address,
                format!("sender-{unknown_account}@example.test")
            );
            assert_eq!(unresolved.sender_name, format!("目录账号{unknown_account}"));
        }
        let catalog = database.sender_accounts().unwrap();
        let first = catalog
            .accounts
            .iter()
            .find(|account| account.id == ids[0])
            .unwrap();
        let renamed = database
            .save_sender_account(
                catalog.revision,
                &ids[0],
                "后改账号名称",
                &first.settings,
                None,
            )
            .unwrap();
        database
            .remove_sender_account(renamed.revision, &ids[1])
            .unwrap();
        assert!(database
            .sent_records()
            .unwrap()
            .iter()
            .all(|row| row.sender_name.starts_with("目录账号")));
        assert!(database
            .unresolved_deliveries()
            .unwrap()
            .iter()
            .all(|row| row.sender_name.starts_with("目录账号")));
        assert_eq!(database.sent_count().unwrap(), 2);
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 2);
    }

    /// 使用随机临时目录及虚构邮箱，不依赖真实账户，不连接 SMTP。
    #[test]
    fn saved_password_stays_private_and_only_matching_settings_can_reuse_it() {
        let folder = std::env::temp_dir().join(format!("qq-smtp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let state = AppState::new();
        state
            .initialize_database(Database::open(folder.join("data.sqlite3")).unwrap())
            .unwrap();
        let public = PublicMailSettings {
            host: "smtp.example.test".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: "sender@example.test".into(),
            from_address: "sender@example.test".into(),
            from_name: "凭据测试".into(),
        };
        let secret = "synthetic-host-regression-password";
        let store = state.smtp_credential_store().unwrap();
        let binding = SmtpCredentialBinding::from_settings(&public.with_password(String::new()));
        store.save(&binding, secret).unwrap();
        state
            .database()
            .unwrap()
            .set_setting("smtp_public", &serde_json::to_string(&public).unwrap())
            .unwrap();
        let response = settings_view(&state, read_public_mail_settings(&state).unwrap());
        assert!(response.password_saved);
        assert_eq!(response.password_status, "saved");
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["host"], public.host);
        assert_eq!(json["passwordSaved"], true);
        assert!(json.get("password").is_none());
        assert!(!json.to_string().contains(secret));
        assert!(!state
            .database()
            .unwrap()
            .setting("smtp_public")
            .unwrap()
            .unwrap()
            .contains(secret));

        let mut sending = public.with_password(String::new());
        resolve_mail_password(&state, &mut sending).unwrap();
        assert_eq!(sending.password, secret);
        let mut changed = public.clone();
        changed.username = "other@example.test".into();
        let response = settings_view(&state, changed.clone());
        assert!(!response.password_saved);
        assert_eq!(response.password_status, "mismatch");
        assert_eq!(
            resolve_mail_password(&state, &mut changed.with_password(String::new()))
                .unwrap_err()
                .code,
            "smtp_password_mismatch"
        );
        // 用户临时输入的授权码可以覆盖本次认证，但不得更新磁盘中已保存的授权码。
        let mut one_time = changed.with_password("synthetic-one-time-only".into());
        resolve_mail_password(&state, &mut one_time).unwrap();
        let mut again = public.with_password(String::new());
        resolve_mail_password(&state, &mut again).unwrap();
        assert_eq!(again.password, secret);

        std::fs::write(folder.join("smtp-credential.bin"), b"corrupt").unwrap();
        let unavailable = settings_view(&state, public.clone());
        assert!(!unavailable.password_saved);
        assert_eq!(unavailable.password_status, "unavailable");
        assert_eq!(unavailable.settings.host, public.host);
        assert_eq!(
            resolve_mail_password(&state, &mut public.with_password(String::new()))
                .unwrap_err()
                .code,
            "smtp_password_unavailable"
        );
        store.clear().unwrap();
        assert_eq!(
            settings_view(&state, public.clone()).password_status,
            "missing"
        );
        assert_eq!(
            resolve_mail_password(&state, &mut public.with_password(String::new()))
                .unwrap_err()
                .code,
            "missing_smtp_credentials"
        );
        drop(state);
        // 只删除本测试已知的文件，不做递归删除，避免扩大清理范围。
        for name in ["data.sqlite3", "data.sqlite3-wal", "data.sqlite3-shm"] {
            let path = folder.join(name);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        std::fs::remove_dir(folder).unwrap();
    }
}
