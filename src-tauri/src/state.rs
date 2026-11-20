//! 单进程会话与任务生命周期。作者：ahui。
//! Mutex 只保护短时状态更新，网络等待和原生文件对话框都不能占用该锁。

use crate::mail_commands::{MailView, SenderCooldownState, StoredAttachment};
use member_core::database::Database;
#[cfg(test)]
use member_core::smtp_credential_store::SmtpCredentialStore;
use member_core::{models::*, providers::WebProvider};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// 界面只接收这一份公开状态，内部 Cookie 和读取模块不会被序列化。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub version: String,
    pub source: Option<String>,
    pub account: Option<Account>,
    pub groups: Vec<Group>,
    pub snapshot: Option<Snapshot>,
    /// 当前全局手动排除版本；同一原始快照可按此版本重新派生，不占用新的历史版本。
    pub exclusion_revision: u64,
    pub task: TaskView,
    pub runtime: String,
    pub history_count: usize,
    pub database_path: String,
    pub session: SessionView,
}

/// 仅描述当前进程的登录状态，不包含凭据、本地期限或恢复状态。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// signed_out / active / expired；active 只代表本次已核验，服务端仍可随时使登录失效。
    pub status: String,
    pub message: String,
}

impl SessionView {
    /// status 由宿主固定状态转换提供；message 为不含 Cookie 的公开提示，不表示保存期限。
    pub fn new(status: &str, message: &str) -> Self {
        Self {
            status: status.into(),
            message: message.into(),
        }
    }
}

impl Default for SessionView {
    fn default() -> Self {
        Self::new("signed_out", "请扫码登录，关闭程序后需重新扫码。")
    }
}

/// 获取任务反馈与名单核验分别表示；completed 不代表保证全量。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub status: String,
    pub message: String,
}

impl Default for TaskView {
    fn default() -> Self {
        Self {
            id: String::new(),
            status: "idle".into(),
            message: "连接账号后选择群聊。".into(),
        }
    }
}

/// 只在宿主保存的凭据读取模块和可替换任务令牌。
pub struct Inner {
    pub web: Option<Arc<WebProvider>>,
    /// 二维码使用独立会话；本次 QQ 身份及群目录验证通过后，可直接替换为任意已授权扫码账号。
    pub pending_web: Option<Arc<WebProvider>>,
    pub view: AppView,
    pub cancel: Option<CancellationToken>,
}

/// 跨命令共享状态；operation 防止登录、退出和刷新相互覆盖。
pub struct AppState {
    pub inner: Mutex<Inner>,
    pub operation: tokio::sync::Mutex<()>,
    pub database: Mutex<Option<Database>>,
    pub mail: Mutex<MailView>,
    pub mail_cancel: Mutex<Option<CancellationToken>>,
    /// 发件邮箱身份的跨任务单调冷却；取消、退出QQ、任务终态和目录变更均不得清空。
    /// SQLite负责重启恢复，运行期以此状态为准，不能因新建worker或系统校时重算已生效期限。
    pub(crate) sender_cooldowns: Mutex<SenderCooldownState>,
    pub attachments: Mutex<HashMap<String, StoredAttachment>>,
    /// 发送确认使用的短期过滤结果；历史写入立即失效，不与模板/持久草稿混用。
    pub(crate) prepared_recipients: Mutex<Option<crate::recipient_commands::PreparedRecipients>>,
    /// 原生文件导入的唯一短期预览；完整资源不返回页面，成功、取消或五分钟超时即释放。
    pub(crate) pending_template_import:
        Mutex<Option<Arc<crate::template_transfer_commands::PendingTemplateImport>>>,
}

impl AppState {
    /// 创建未连接的真实空状态，不装入演示成员数据。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                web: None,
                pending_web: None,
                cancel: None,
                view: AppView {
                    version: env!("CARGO_PKG_VERSION").into(),
                    source: None,
                    account: None,
                    groups: vec![],
                    snapshot: None,
                    exclusion_revision: 0,
                    task: TaskView::default(),
                    runtime: "Tauri 2 · Windows WebView2".into(),
                    history_count: 0,
                    database_path: String::new(),
                    session: SessionView::default(),
                },
            }),
            operation: tokio::sync::Mutex::new(()),
            database: Mutex::new(None),
            mail: Mutex::new(MailView::default()),
            mail_cancel: Mutex::new(None),
            sender_cooldowns: Mutex::new(SenderCooldownState::default()),
            attachments: Mutex::new(HashMap::new()),
            prepared_recipients: Mutex::new(None),
            pending_template_import: Mutex::new(None),
        }
    }

    /// 复制可公开状态，调用者不得在持有内部锁时发事件。
    pub fn view(&self) -> AppView {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .view
            .clone()
    }

    /// 返回本次内存中的网页连接；历史快照不能充当登录凭据。
    pub fn provider(&self) -> AppResult<Arc<dyn MemberProvider>> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .web
            .clone()
            .map(|provider| provider as Arc<dyn MemberProvider>)
            .ok_or_else(|| AppError::new("not_connected", "请先扫码登录。"))
    }

    /// 任务运行时禁止维护账号、名单或发件资源，避免跨账号回写或改变已确认的任务范围。
    pub fn ensure_idle(&self) -> AppResult<()> {
        if self.view().task.status == "running" {
            return Err(AppError::new(
                "task_running",
                "正在获取成员，请先取消或等待完成。",
            ));
        }
        if self.mail.lock().unwrap_or_else(|p| p.into_inner()).status == "running" {
            return Err(AppError::new(
                "mail_running",
                "邮件正在发送，请先停止发送任务。",
            ));
        }
        Ok(())
    }

    /// 关闭窗口时取消后台工作并清空内存凭据；QQ Cookie 不保存到磁盘。
    pub fn cancel_all(&self) {
        self.pending_template_import
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(cancel) = self
            .mail_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            cancel.cancel();
        }
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = inner.cancel.take() {
            cancel.cancel();
        }
        inner.web = None;
        inner.pending_web = None;
        inner.view.account = None;
        inner.view.groups.clear();
        inner.view.snapshot = None;
        inner.view.source = None;
        inner.view.session = SessionView::default();
    }

    /// 启动时加载历史数据并清理旧版登录文件；每次进程启动均未登录，历史快照不是登录凭据。
    pub fn initialize_database(&self, database: Database) -> AppResult<()> {
        let count = database.sent_count()?;
        let snapshot = database
            .latest_snapshot()?
            .map(|snapshot| database.apply_manual_exclusions(&snapshot))
            .transpose()?;
        let exclusion_revision = database.manual_exclusions()?.revision;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.view.database_path = database.path().to_string_lossy().into_owned();
        inner.view.history_count = count;
        inner.view.snapshot = snapshot;
        inner.view.exclusion_revision = exclusion_revision;
        crate::session_commands::clear_legacy_login(
            &database.path().with_file_name("qq-session.bin"),
        );
        *self.database.lock().unwrap_or_else(|p| p.into_inner()) = Some(database);
        Ok(())
    }

    /// 复制轻量数据库句柄，每次操作使用独立短连接。
    pub fn database(&self) -> AppResult<Database> {
        self.database
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| AppError::new("database_unavailable", "本机数据库尚未就绪。"))
    }

    /// 仅供旧格式兼容回归测试建立固定独立密文；生产代码由账号目录解析各账号文件引用。
    #[cfg(test)]
    pub fn smtp_credential_store(&self) -> AppResult<SmtpCredentialStore> {
        Ok(SmtpCredentialStore::new(
            self.database()?
                .path()
                .with_file_name("smtp-credential.bin"),
        ))
    }
}
