//! QQ 登录仅保留在当前进程内存；服务端拒绝后要求重新扫码。作者：ahui。
//! 不保存、读取或恢复 Cookie；升级时仅删除旧版本固定名称的登录文件，不影响 SMTP 或 SQLite。

use crate::state::{AppState, SessionView};
use member_core::{diagnostics, models::AppError};
use std::path::Path;

/// 启动时清理旧版 QQ 会话文件。只删除传入的固定文件，不解密、不递归，也不阻止应用启动。
/// 删除失败时记录不含路径/凭据的错误，下一次启动重试；旧文件无论是否删除均不会再被读取。
/// 调用方只能传应用数据目录中的 qq-session.bin，不能接收前端路径。
pub fn clear_legacy_login(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            diagnostics::event(
                "warn",
                "session",
                "legacy_file_cleanup_failed",
                serde_json::json!({"credentials_ignored": true}),
            );
        }
    }
}

/// 仅明确的认证/身份错误撤下当前连接；网络超时、权限不足或业务错误不能冒充登录过期。
pub fn authentication_failed(error: &AppError) -> bool {
    matches!(
        error.code.as_str(),
        "login_expired" | "unauthorized" | "login_required" | "account_changed"
    )
}

/// 认证失效时清空内存身份、目录和当前工作快照，保留 SQLite 历史及独立邮件草稿。
/// 调用方须持有 operation 门闩，继续向用户报告原始失败并发布新状态；不自动重试登录。
pub fn handle_authentication_failure(state: &AppState, error: &AppError) {
    let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
    inner.web = None;
    inner.view.account = None;
    inner.view.groups.clear();
    inner.view.snapshot = None;
    inner.view.source = None;
    inner.view.session = SessionView::new("expired", "QQ 登录已失效，请重新扫码登录。");
    diagnostics::event(
        "info",
        "session",
        "scan_required",
        serde_json::json!({"error_code":error.code}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use member_core::{
        database::Database,
        models::{Account, Group},
        providers::WebProvider,
    };
    use std::sync::Arc;

    #[test]
    fn authentication_failures_clear_identity_without_touching_history_or_smtp() {
        let state = AppState::new();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.web = Some(Arc::new(WebProvider::new().unwrap()));
            inner.view.account = Some(Account {
                qq_number: "990001".into(),
                nickname: "合成账号".into(),
                source: "web".into(),
            });
            inner.view.groups = vec![Group {
                id: "990002".into(),
                name: "合成群".into(),
                member_count: Some(1),
                role: "member".into(),
            }];
            inner.view.source = Some("web".into());
            inner.view.history_count = 42;
        }
        handle_authentication_failure(&state, &AppError::new("login_expired", "合成拒绝"));
        let view = state.view();
        assert!(view.account.is_none() && view.groups.is_empty() && view.snapshot.is_none());
        assert!(state.provider().is_err());
        assert_eq!(view.history_count, 42);
        assert_eq!(view.session.status, "expired");
        let public = serde_json::to_value(view.session).unwrap();
        assert!(public.get("expiresAt").is_none());
        assert!(public.get("persistenceWarning").is_none());
    }

    #[test]
    fn only_explicit_authentication_errors_require_a_new_scan() {
        for code in [
            "login_expired",
            "login_required",
            "unauthorized",
            "account_changed",
        ] {
            assert!(authentication_failed(&AppError::new(code, "合成认证错误")));
        }
        for code in [
            "timeout",
            "connection_failed",
            "group_forbidden",
            "rate_limited",
            "database_error",
        ] {
            assert!(!authentication_failed(&AppError::new(code, "合成业务错误")));
        }
    }

    #[test]
    fn startup_discards_only_legacy_login_and_always_starts_signed_out() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("qq-session.bin");
        let smtp = directory.path().join("smtp-credential.bin");
        std::fs::write(&legacy, b"synthetic-legacy-placeholder").unwrap();
        std::fs::write(&smtp, b"synthetic-smtp-placeholder").unwrap();
        let database = Database::open(directory.path().join("data.sqlite3")).unwrap();
        let state = AppState::new();
        state.initialize_database(database).unwrap();
        assert!(!legacy.exists());
        assert_eq!(std::fs::read(&smtp).unwrap(), b"synthetic-smtp-placeholder");
        assert!(state.view().account.is_none());
        assert_eq!(state.view().session.status, "signed_out");
        clear_legacy_login(&legacy);
        // 同名目录不会递归删除；清理失败也不阻断正常扫码或数据库加载。
        std::fs::create_dir(&legacy).unwrap();
        clear_legacy_login(&legacy);
        assert!(legacy.is_dir());
    }
}
