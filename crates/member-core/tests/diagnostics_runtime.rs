//! 隔离进程验证实际日志入口，不连接QQ或SMTP、不读取用户目录。作者：ahui。

use member_core::{diagnostics, models::AppError};
use serde_json::{json, Value};

/// 独立测试二进制拥有自己的全局日志器，真实验证初始化、结构化事件、兜底错误及panic位置。
/// 临时目录中的日志与应用目录完全分离；断言禁止凭据/正文/地址进入可分享的诊断文件。
#[test]
fn actual_sink_writes_redacted_utf8_events_without_changing_business_errors() {
    let folder = tempfile::tempdir().unwrap();
    diagnostics::initialize(folder.path()).unwrap();
    assert!(diagnostics::status().available);
    {
        let mut operation = diagnostics::operation("integration", "synthetic_operation");
        diagnostics::event(
            "ERROR",
            "smtp",
            "synthetic_failure",
            json!({
                "status":"550", "extended":"5.7.1",
                "detail":diagnostics::sanitize("550 5.7.1 sample@example.test sample-auth-code", &["sample-auth-code"]),
                "password":"short-sensitive", "html":"private mail body",
            }),
        );
        operation.finish("error");
    }
    let error = AppError::new("synthetic_failure", "用户可见的中文说明");
    assert_eq!(error.code, "synthetic_failure");
    assert_eq!(error.message, "用户可见的中文说明");
    diagnostics::record_panic(Some(("C:\\private\\source.rs", 42)));
    let text = std::fs::read_to_string(folder.path().join("app.jsonl")).unwrap();
    for secret in [
        "sample@example.test",
        "sample-auth-code",
        "short-sensitive",
        "private mail body",
        "C:\\private",
        "用户可见的中文说明",
    ] {
        assert!(!text.contains(secret), "unexpected sensitive text");
    }
    let records = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 5);
    assert!(records.iter().all(|record| record["timestamp"]
        .as_str()
        .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())));
    assert_eq!(records[1]["fields"]["status"], "550");
    assert_eq!(records[1]["fields"]["extended"], "5.7.1");
    assert_eq!(records[2]["fields"]["outcome"], "error");
    assert_eq!(records[4]["fields"]["source"], "source.rs");
}
