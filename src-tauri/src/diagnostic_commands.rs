//! 桌面诊断的最小IPC边界。作者：ahui。
//! 报告仅允许预定义命令名、状态和数字指标；日志预览只读固定文件，不接受用户指定路径。

use member_core::{diagnostics, models::*};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use tauri::Manager;

/// 日志预览读取的字节与完整行上限；仅限制本次显示，不裁剪或删除磁盘上的日志。
const MAX_RECENT_LOG_BYTES: u64 = 256 * 1024;
const MAX_RECENT_LOG_LINES: usize = 200;

/// 原有脱敏日志的只读片段；content 按文件写入顺序保留完整 JSONL，页面须以纯文本显示。
#[derive(Serialize)]
pub struct RecentLogs {
    content: String,
    /// 达到预览上限或丢弃不完整/损坏行时为 true；更早文件仍可通过现有目录入口查看。
    truncated: bool,
}

/// 前端仅回报操作指标，不把调用参数、正文、凭据或自由异常消息传给日志模块。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendDiagnostic {
    command: String,
    outcome: String,
    duration_ms: u64,
    error_code: Option<String>,
}

/// 浏览器未捕获异常只提供固定类型；错误文本、堆栈URL及DOM信息可能携带正文，禁止接收。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendFault {
    kind: String,
    error_type: Option<String>,
    source: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
}

/// 日志成功与否不使用业务任务门闩，发送期间也能只读查看状态。
#[tauri::command]
pub fn get_log_status() -> diagnostics::LogStatus {
    diagnostics::status()
}

/// 只读当前应用固定的活动日志，不接收页面路径，也不争用发送、登录或读取任务的业务门闩。
///
/// 文件 I/O 放到阻塞线程；读取不会初始化日志模块、创建目录、写日志、修复半行或触发轮转。
/// 不存在的文件返回空片段，其他故障只返回固定的可重试错误，不包含磁盘路径或系统异常正文。
#[tauri::command]
pub async fn get_recent_logs(app: tauri::AppHandle) -> AppResult<RecentLogs> {
    let path = app
        .path()
        .app_local_data_dir()
        .map_err(|_| log_read_error())?
        .join("logs")
        .join("app.jsonl");
    tokio::task::spawn_blocking(move || read_recent_logs(&path))
        .await
        .map_err(|_| log_read_error())?
}

/// 从打开时的文件长度取有上限的尾部快照，随后追加的数据留给用户下一次刷新。
///
/// UTF-8 只在找到完整换行边界后解码，避免从中文中间截取产生乱码；超出窗口时保守丢弃首片段。
/// 末尾尚未换行的 JSON 即使暂时可解析也不展示，防止读取到正在追加的半条事件。
/// 每条完整行须为 JSON 对象；损坏行跳过并标记片段不完整，但不修改原文件。
/// path 仅供宿主固定调用和隔离测试，IPC 命令不允许用户传入该参数。
fn read_recent_logs(path: &Path) -> AppResult<RecentLogs> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecentLogs {
                content: String::new(),
                truncated: false,
            });
        }
        Err(_) => return Err(log_read_error()),
    };
    let metadata = file.metadata().map_err(|_| log_read_error())?;
    if !metadata.is_file() {
        return Err(log_read_error());
    }
    let length = metadata.len();
    let start = length.saturating_sub(MAX_RECENT_LOG_BYTES);
    let expected_bytes = length - start;
    file.seek(SeekFrom::Start(start))
        .map_err(|_| log_read_error())?;
    let mut bytes = Vec::with_capacity(expected_bytes as usize);
    file.take(expected_bytes)
        .read_to_end(&mut bytes)
        .map_err(|_| log_read_error())?;
    let mut truncated = start > 0 || bytes.len() as u64 != expected_bytes;
    let mut offset = 0;
    if start > 0 {
        // 不额外读取第 256 KiB 之外的字节来猜测边界；首片段可能从 JSON/中文中间开始，直接跳过。
        offset = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1);
    }
    let mut lines = VecDeque::with_capacity(MAX_RECENT_LOG_LINES);
    for (index, byte) in bytes.iter().enumerate().skip(offset) {
        if *byte != b'\n' {
            continue;
        }
        let raw = &bytes[offset..index];
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let valid = std::str::from_utf8(raw).ok().filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|value| value.is_object())
        });
        if let Some(line) = valid {
            if lines.len() == MAX_RECENT_LOG_LINES {
                lines.pop_front();
                truncated = true;
            }
            lines.push_back(line);
        } else {
            truncated = true;
        }
        offset = index + 1;
    }
    if offset < bytes.len() {
        truncated = true;
    }
    let content = lines.into_iter().collect::<Vec<_>>().join("\n");
    Ok(RecentLogs { content, truncated })
}

/// AppError::new 会自动记录事件；这里直接构造安全错误，确保“查看日志”本身始终只读。
fn log_read_error() -> AppError {
    AppError {
        code: "log_read_error".into(),
        message: "日志暂时无法读取，请检查访问权限后刷新重试。".into(),
    }
}

/// 每个日志输入都用白名单检查；无效报告直接忽略，不制造错误循环或改变原业务结果。
#[tauri::command]
pub fn report_frontend_diagnostic(input: FrontendDiagnostic) -> diagnostics::LogStatus {
    if valid_command(&input.command)
        && matches!(input.outcome.as_str(), "started" | "succeeded" | "failed")
    {
        // errorCode只保留短源码标识；不能通过错误消息伪装成编码字段写入日志。
        let code = input.error_code.filter(|value| {
            value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        });
        diagnostics::event(
            if input.outcome == "failed" {
                "WARN"
            } else {
                "INFO"
            },
            "renderer",
            "command",
            json!({
                "command":input.command,"outcome":input.outcome,"duration_ms":input.duration_ms.min(86_400_000),"error_code":code,
            }),
        );
    }
    diagnostics::status()
}

/// 未捕获前端故障的固定类别足够确认故障来源；具体业务入口另有命令/任务日志关联。
#[tauri::command]
pub fn report_frontend_fault(input: FrontendFault) -> diagnostics::LogStatus {
    if matches!(input.kind.as_str(), "error" | "unhandledrejection") {
        let error_type = input.error_type.filter(|value| {
            matches!(
                value.as_str(),
                "Error"
                    | "TypeError"
                    | "ReferenceError"
                    | "SyntaxError"
                    | "RangeError"
                    | "URIError"
                    | "EvalError"
                    | "unknown"
            )
        });
        let source = input.source.filter(|value| {
            value.len() <= 100
                && value.ends_with(".js")
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        });
        diagnostics::event(
            "ERROR",
            "renderer",
            "unhandled_fault",
            json!({"kind":input.kind,"error_type":error_type,"source":source,"line":input.line,"column":input.column}),
        );
    }
    diagnostics::status()
}

/// 仅打开应用固定日志目录，不能借此启动任意程序或访问页面传入的路径。
/// Explorer打开不持有业务门闩，因此不影响正在发送或获取的任务；失败向页面提供重试提示。
#[tauri::command]
pub fn open_log_directory(app: tauri::AppHandle) -> AppResult<()> {
    let directory = app
        .path()
        .app_local_data_dir()
        .map_err(|_| AppError::new("log_directory", "无法定位日志目录，请重启程序后重试。"))?
        .join("logs");
    std::fs::create_dir_all(&directory).map_err(|error| {
        diagnostics::event(
            "ERROR",
            "diagnostics",
            "open_directory_failed",
            json!({"io_kind":format!("{:?}",error.kind())}),
        );
        AppError::new(
            "log_directory",
            "无法打开日志目录，请检查磁盘空间和访问权限。",
        )
    })?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(&directory)
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|error| {
                diagnostics::event(
                    "ERROR",
                    "diagnostics",
                    "explorer_failed",
                    json!({"io_kind":format!("{:?}",error.kind())}),
                );
                AppError::new("log_directory", "无法打开日志目录，请稍后重试。")
            })?;
        diagnostics::event("INFO", "diagnostics", "directory_opened", json!({}));
        Ok(())
    }
    #[cfg(not(windows))]
    Err(AppError::new(
        "log_directory",
        "请在Windows桌面程序中打开日志目录。",
    ))
}

/// 与宿主登记的业务命令保持一致；诊断命令自身不记录，避免递归和无界日志增长。
fn valid_command(command: &str) -> bool {
    matches!(
        command,
        "get_app_state"
            | "get_update_status"
            | "check_updates"
            | "open_external_url"
            | "get_manual_exclusions"
            | "add_manual_exclusions"
            | "remove_manual_exclusions"
            | "get_snapshot_view"
            | "restore_filtered_members"
            | "get_group_snapshot"
            | "start_web_login"
            | "poll_web_login"
            | "cancel_web_login"
            | "refresh_groups"
            | "fetch_members"
            | "cancel_fetch"
            | "logout"
            | "export_members"
            | "get_sent_history"
            | "add_sent_records"
            | "import_sent_records"
            | "remove_sent_record"
            | "resolve_mail_delivery"
            | "resolve_mail_deliveries"
            | "get_mail_state"
            | "get_member_mail_status"
            | "get_mail_settings"
            | "save_mail_settings"
            | "clear_mail_password"
            | "mail_pick_attachments"
            | "mail_remove_attachment"
            | "mail_add_inline_image"
            | "mail_pick_inline_images"
            | "mail_remove_inline_image"
            | "mail_template_list"
            | "mail_template_preview"
            | "mail_template_save"
            | "mail_template_rename"
            | "mail_template_duplicate"
            | "mail_template_delete"
            | "mail_template_apply"
            | "mail_clear_draft_content"
            | "mail_template_export"
            | "mail_template_import_preview"
            | "mail_template_import_commit"
            | "mail_template_import_cancel"
            | "start_mail_job"
            | "prepare_mail_recipients"
            | "skip_mail_wait"
            | "cancel_mail_job"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只创建随机临时目录与合成日志；Drop 只移除已知文件，不触及应用真实日志或凭据。
    struct LogFixture {
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl LogFixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("qq-log-preview-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("app.jsonl");
            Self { directory, path }
        }

        fn write(&self, bytes: impl AsRef<[u8]>) {
            std::fs::write(&self.path, bytes).unwrap();
        }
    }

    impl Drop for LogFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }

    #[test]
    fn only_registered_business_commands_are_reportable() {
        assert!(valid_command("start_mail_job"));
        for command in [
            "mail_template_export",
            "mail_template_import_preview",
            "mail_template_import_commit",
            "mail_template_import_cancel",
            "get_update_status",
            "check_updates",
            "open_external_url",
        ] {
            assert!(valid_command(command));
        }
        for invalid in [
            "report_frontend_diagnostic",
            "get_log_status",
            "get_recent_logs",
            "open_log_directory",
            "user@example.test",
            "custom_command",
        ] {
            assert!(!valid_command(invalid));
        }
    }

    #[test]
    fn invalid_reports_do_not_reject_original_operation_or_initialize_logging() {
        let _ = report_frontend_diagnostic(FrontendDiagnostic {
            command: "user@example.test".into(),
            outcome: "failed".into(),
            duration_ms: u64::MAX,
            error_code: Some("private message".into()),
        });
        let _ = report_frontend_fault(FrontendFault {
            kind: "secret text".into(),
            error_type: None,
            source: None,
            line: None,
            column: None,
        });
    }

    #[test]
    fn recent_logs_missing_and_empty_file_return_empty_without_creating_or_writing() {
        let fixture = LogFixture::new();
        let missing = read_recent_logs(&fixture.path).unwrap();
        assert!(missing.content.is_empty());
        assert!(!missing.truncated);
        assert!(!fixture.path.exists());
        fixture.write([]);
        let empty = read_recent_logs(&fixture.path).unwrap();
        assert!(empty.content.is_empty());
        assert!(!empty.truncated);
        assert_eq!(std::fs::metadata(&fixture.path).unwrap().len(), 0);
    }

    #[test]
    fn recent_logs_keep_last_two_hundred_complete_lines_in_original_order_and_preserve_file() {
        let fixture = LogFixture::new();
        let original = (0..220)
            .map(|index| format!("{{\"index\":{index},\"message\":\"合成事件\"}}\n"))
            .collect::<String>();
        fixture.write(&original);
        let modified = std::fs::metadata(&fixture.path)
            .unwrap()
            .modified()
            .unwrap();
        let result = read_recent_logs(&fixture.path).unwrap();
        assert!(result.truncated);
        let lines = result.content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), MAX_RECENT_LOG_LINES);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["index"],
            20
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[199]).unwrap()["index"],
            219
        );
        assert_eq!(std::fs::read(&fixture.path).unwrap(), original.as_bytes());
        assert_eq!(
            std::fs::metadata(&fixture.path)
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }

    #[test]
    fn recent_logs_discard_utf8_cut_prefix_and_keep_complete_chinese_and_emoji() {
        let fixture = LogFixture::new();
        let prefix = format!("{{\"message\":\"{}\"}}\n", "汉".repeat(100_000));
        let original = (0..3)
            .map(|padding| {
                format!(
                    "{prefix}{{\"message\":\"中文边界{}\"}}\n{{\"message\":\"😀完整\"}}\n",
                    "x".repeat(padding)
                )
            })
            .find(|value| {
                let start = value.len() - MAX_RECENT_LOG_BYTES as usize;
                value.as_bytes()[start] & 0xc0 == 0x80
            })
            .unwrap();
        let start = original.len() - MAX_RECENT_LOG_BYTES as usize;
        assert!(std::str::from_utf8(&original.as_bytes()[start..]).is_err());
        fixture.write(&original);
        let result = read_recent_logs(&fixture.path).unwrap();
        assert!(result.truncated);
        assert_eq!(result.content.lines().count(), 2);
        assert!(result.content.contains("中文边界"));
        assert!(result.content.contains("😀完整"));
        assert!(!result.content.contains('\u{fffd}'));
        assert_eq!(std::fs::read(&fixture.path).unwrap(), original.as_bytes());
    }

    #[test]
    fn recent_logs_ignore_partial_tail_invalid_json_and_non_object_lines_without_repairing() {
        let fixture = LogFixture::new();
        let mut original =
            b"{\"index\":1}\r\nnot-json\n[]\n{\"index\":2}\n{\"message\":\"".to_vec();
        // 模拟中文刚写入一个字节、尚未补完 UTF-8 与 JSON 的时刻，读取不能修复或替换它。
        original.push(0xe4);
        fixture.write(&original);
        let result = read_recent_logs(&fixture.path).unwrap();
        assert_eq!(result.content, "{\"index\":1}\n{\"index\":2}");
        assert!(result.truncated);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), original);

        fixture.write(b"{\"index\":3}");
        let complete_json_without_newline = read_recent_logs(&fixture.path).unwrap();
        assert!(complete_json_without_newline.content.is_empty());
        assert!(complete_json_without_newline.truncated);
    }

    #[test]
    fn recent_logs_short_valid_snapshot_is_not_truncated_and_long_single_line_is_bounded() {
        let fixture = LogFixture::new();
        fixture.write("{\"message\":\"只读合成\"}\n");
        let valid = read_recent_logs(&fixture.path).unwrap();
        assert_eq!(valid.content, "{\"message\":\"只读合成\"}");
        assert!(!valid.truncated);
        let serialized = serde_json::to_value(valid).unwrap();
        assert!(serialized["content"].is_string());
        assert_eq!(serialized["truncated"], false);

        fixture.write(format!(
            "{{\"message\":\"{}\"}}\n",
            "x".repeat(MAX_RECENT_LOG_BYTES as usize + 1)
        ));
        let oversized = read_recent_logs(&fixture.path).unwrap();
        assert!(oversized.content.is_empty());
        assert!(oversized.truncated);
    }

    #[test]
    fn recent_logs_io_errors_use_fixed_retry_message_without_path_or_log_write() {
        let fixture = LogFixture::new();
        let before = std::fs::read_dir(&fixture.directory).unwrap().count();
        let error = read_recent_logs(&fixture.directory).err().unwrap();
        assert_eq!(error.code, "log_read_error");
        assert_eq!(
            error.message,
            "日志暂时无法读取，请检查访问权限后刷新重试。"
        );
        assert!(!error
            .message
            .contains(&fixture.directory.to_string_lossy().to_string()));
        assert_eq!(
            std::fs::read_dir(&fixture.directory).unwrap().count(),
            before
        );
        assert!(!fixture.path.exists());
    }
}
