//! 本机结构化诊断日志：有界轮转、即时刷新和统一脱敏。作者：ahui。
//! 只记录调用方明确选取的诊断字段，不订阅 SMTP 协议或 HTTP 请求体；日志失败不得改变业务结果。

use chrono::Local;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::hash_map::RandomState,
    fs::{File, OpenOptions},
    hash::BuildHasher,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::Instant,
};

/// 当前文件与四份历史文件合计约25MiB；单条上限64KiB，不允许异常字段撑大磁盘。
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const BACKUP_COUNT: usize = 4;
static LOGGER: OnceLock<Mutex<LogState>> = OnceLock::new();
static CORRELATION_KEY: OnceLock<RandomState> = OnceLock::new();
static OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 只暴露日志是否可用及固定说明，不向页面回显底层路径或操作系统错误正文。
#[derive(Clone, Serialize)]
pub struct LogStatus {
    pub available: bool,
    pub message: String,
}

struct LogState {
    directory: PathBuf,
    writer: Option<RotatingWriter>,
    failure: Option<io::ErrorKind>,
    retry_at: Option<Instant>,
}

/// 初始化仅接收宿主决定的固定目录。失败后可由后续事件重试写入，不影响登录、保存和投递。
/// 未初始化时事件为空操作，便于核心库独立测试；应用应在打开业务数据库之前调用。
pub fn initialize(directory: &Path) -> io::Result<()> {
    let writer = RotatingWriter::new(directory.to_path_buf(), MAX_FILE_BYTES);
    let result = writer
        .as_ref()
        .map(|_| ())
        .map_err(|error| io::Error::from(error.kind()));
    let initial = LogState {
        directory: directory.to_path_buf(),
        writer: writer.ok(),
        failure: result.as_ref().err().map(io::Error::kind),
        retry_at: None,
    };
    let _ = LOGGER.set(Mutex::new(initial));
    result
}

/// 返回当前可用性；失败提示不包含凭据或用户路径，调用本函数不会创建或更改日志文件。
pub fn status() -> LogStatus {
    let available = LOGGER.get().is_some_and(|logger| {
        let state = logger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.writer.is_some() && state.failure.is_none()
    });
    LogStatus {
        available,
        message: if available {
            String::new()
        } else {
            "诊断日志暂时无法写入，请检查磁盘空间和日志目录权限。".into()
        },
    }
}

/// 生成仅在当前进程稳定的关联标识。隐藏随机键不落盘，不能据此跨会话建立QQ/邮箱名单。
/// 参数可为邮箱、QQ号或模板标识；返回值仅用于同一日志会话的关联，不能作为业务身份。
pub fn correlate(value: &str) -> String {
    format!(
        "ref_{:016x}",
        CORRELATION_KEY
            .get_or_init(RandomState::new)
            .hash_one(value)
    )
}

/// 清理服务端错误摘要。调用方必须传入已知授权码、认证身份和当前收件人等额外敏感值。
/// 先隐藏精确秘密，再遮盖邮箱、凭据键值、URL、本机路径和长令牌；输出单行且至多4096字。
/// 本函数不是任意邮件内容的匿名化器，主题、正文、Cookie、请求参数仍禁止传入。
pub fn sanitize(text: &str, secrets: &[&str]) -> String {
    let mut clean = text.chars().take(16 * 1024).collect::<String>();
    let mut secrets = secrets
        .iter()
        .filter(|value| !value.is_empty())
        .copied()
        .collect::<Vec<_>>();
    secrets.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    for secret in secrets {
        clean = clean.replace(secret, "[redacted]");
    }
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| vec![
        (Regex::new(r#"(?i)\b(?:https?|smtps?|file)://[^\s<>\"']+"#).unwrap(), "[url]"),
        (Regex::new(r#"(?i)\b(?:authorization|proxy-authorization|set-cookie|cookie|password|passwd|token|secret|skey|p_skey|uin|qrsig|auth)\s*[:=]\s*[^\r\n;]+"#).unwrap(), "[credential]"),
        (Regex::new(r#"[\p{L}\p{N}.!#$%&'*+/=?^_`{|}~-]+@[\p{L}\p{N}](?:[\p{L}\p{N}.-]*[\p{L}\p{N}])?"#).unwrap(), "[email]"),
        (Regex::new(r#"(?i)\b[a-z]:[\\/][^\r\n\"<>|]*"#).unwrap(), "[path]"),
        (Regex::new(r#"\b[A-Za-z0-9+/=]{24,}\b"#).unwrap(), "[token]"),
        (Regex::new(r#"\b[0-9]{7,}\b"#).unwrap(), "[number]"),
    ]);
    for (pattern, replacement) in patterns {
        clean = pattern.replace_all(&clean, *replacement).into_owned();
    }
    clean
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(4096)
        .collect()
}

/// 追加一条UTF-8 JSONL事件。level/component/name必须是源码固定标识，fields只放最少必要诊断数据。
/// 每条包含带时区时间、进程号和事件名；所有字符串再次防御性脱敏，写入后立即flush。
/// I/O错误只更新日志状态，不panic、不递归记录自身错误，也不把日志失败变为邮件失败。
pub fn event(level: &str, component: &str, name: &str, fields: Value) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let record = json!({
        "timestamp": Local::now().to_rfc3339(), "pid": std::process::id(),"session":correlate("process-session"),
        "level": level.to_ascii_uppercase(), "component": component, "event": name,
        "fields": clean_value(fields, 0),
    });
    let Ok(mut bytes) = serde_json::to_vec(&record) else {
        return;
    };
    if bytes.len() > MAX_RECORD_BYTES {
        bytes = serde_json::to_vec(&json!({
            "timestamp": Local::now().to_rfc3339(), "pid": std::process::id(),
            "level":"WARN", "component":"diagnostics", "event":"record_too_large",
            "fields":{"original_component":component,"original_event":name}
        }))
        .unwrap_or_default();
    }
    bytes.push(b'\n');
    let mut state = logger
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 启动目录暂不可写时最多每10秒重试一次；恢复后自动继续，不要求中断邮件或重启。
    if state.writer.is_none()
        && state
            .retry_at
            .is_none_or(|instant| instant.elapsed().as_secs() >= 10)
    {
        state.retry_at = Some(Instant::now());
        match RotatingWriter::new(state.directory.clone(), MAX_FILE_BYTES) {
            Ok(writer) => {
                state.writer = Some(writer);
                state.failure = None;
            }
            Err(error) => {
                state.failure = Some(error.kind());
            }
        }
    }
    if let Some(writer) = state.writer.as_mut() {
        state.failure = writer.write(&bytes).err().map(|error| error.kind());
    }
}

/// panic钩子使用try_lock，防止日志模块自身或持锁线程异常时二次死锁；不记录panic载荷或用户数据。
pub fn record_panic(location: Option<(&str, u32)>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let Ok(mut state) = logger.try_lock() else {
        return;
    };
    let Some(writer) = state.writer.as_mut() else {
        return;
    };
    let (file, line) = location.unwrap_or(("unknown", 0));
    // file由编译器提供，取最后一级源码文件名，避免开发机绝对目录泄漏。
    let source = file.rsplit(['/', '\\']).next().unwrap_or("unknown");
    let record = json!({"timestamp":Local::now().to_rfc3339(),"pid":std::process::id(),"level":"ERROR","component":"lifecycle","event":"panic","fields":{"source":source,"line":line}});
    if let Ok(mut bytes) = serde_json::to_vec(&record) {
        bytes.push(b'\n');
        state.failure = writer.write(&bytes).err().map(|error| error.kind());
    }
}

/// 限制嵌套和字段数量，避免错误报告把完整名单、正文或循环调试数据倾倒到日志。
fn clean_value(value: Value, depth: usize) -> Value {
    if depth > 6 {
        return Value::String("[depth limited]".into());
    }
    match value {
        Value::String(text) => Value::String(sanitize(&text, &[])),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(32)
                .map(|item| clean_value(item, depth + 1))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .take(48)
                .map(|(key, value)| {
                    let lowered = key.to_ascii_lowercase();
                    let sensitive = [
                        "password",
                        "cookie",
                        "authorization",
                        "body",
                        "html",
                        "subject",
                        "recipients",
                        "username",
                        "from_address",
                        "attachment_name",
                    ];
                    let value = if sensitive.iter().any(|secret| lowered == *secret) {
                        json!("[redacted]")
                    } else {
                        clean_value(value, depth + 1)
                    };
                    (sanitize(&key, &[]), value)
                })
                .collect(),
        ),
        value => value,
    }
}

/// 操作范围在提前返回/异常展开时也保留结束标记；finish明确区分成功、失败或取消。
pub struct Operation {
    component: &'static str,
    name: &'static str,
    id: u64,
    started: Instant,
    outcome: &'static str,
}

/// 创建一对可关联的操作开始/结束记录，不捕获参数、返回数据或异常正文。
pub fn operation(component: &'static str, name: &'static str) -> Operation {
    let id = OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    event(
        "INFO",
        component,
        "operation_start",
        json!({"operation":name,"operation_id":id}),
    );
    Operation {
        component,
        name,
        id,
        started: Instant::now(),
        outcome: "ended",
    }
}

impl Operation {
    /// outcome只能使用源码固定值，例如success/error/cancelled；不允许放入用户输入或错误正文。
    pub fn finish(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        event(
            "INFO",
            self.component,
            "operation_end",
            json!({
                "operation":self.name,"operation_id":self.id,"outcome":self.outcome,
                "duration_ms":self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            }),
        );
    }
}

struct RotatingWriter {
    directory: PathBuf,
    file: Option<File>,
    bytes: u64,
    limit: u64,
}

impl RotatingWriter {
    fn new(directory: PathBuf, limit: u64) -> io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        let (file, bytes) = Self::open_current(&directory)?;
        Ok(Self {
            directory,
            file: Some(file),
            bytes,
            limit,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.file.is_none() {
            let (file, size) = Self::open_current(&self.directory)?;
            self.bytes = size;
            self.file = Some(file);
        }
        if self.bytes + bytes.len() as u64 > self.limit {
            self.rotate()?;
            let (file, size) = Self::open_current(&self.directory)?;
            self.bytes = size;
            self.file = Some(file);
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file unavailable"))?;
        if let Err(error) = file.write_all(bytes).and_then(|_| file.flush()) {
            // 磁盘满可能只写入半条；关闭并在下次写入前修复末尾，不能把新JSON接在残片后。
            self.file.take();
            return Err(error);
        }
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    /// 重开时移除进程中断/磁盘写满留下的尾部半行，只处理当前日志，不动数据库或完整历史。
    fn open_current(directory: &Path) -> io::Result<(File, u64)> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("app.jsonl"))?;
        let mut size = file.metadata()?.len();
        if size > 0 {
            let length = size.min(MAX_RECORD_BYTES as u64 + 1) as usize;
            file.seek(SeekFrom::End(-(length as i64)))?;
            let mut tail = vec![0; length];
            file.read_exact(&mut tail)?;
            if tail.last() != Some(&b'\n') {
                size = tail
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| size - length as u64 + index as u64 + 1)
                    .unwrap_or(0);
                file.set_len(size)?;
            }
        }
        // Windows的仅追加句柄没有截断权限；使用可写句柄修复后定位末尾，进程内写入统一受Mutex保护。
        file.seek(SeekFrom::End(0))?;
        Ok((file, size))
    }

    /// 四个备份槽循环使用，按修改时间选最旧文件；不先删除/移动历史，避免当前文件被占用时反复丢历史。
    /// 标准库rename在Windows/Unix均替换目标文件；任何失败保留原当前文件，恢复后可安全重试。
    fn rotate(&mut self) -> io::Result<()> {
        self.file.take();
        let mut slots = Vec::new();
        for index in 1..=BACKUP_COUNT {
            let path = self.directory.join(format!("app.{index}.jsonl"));
            match path.metadata() {
                Ok(metadata) if metadata.is_file() => {
                    slots.push((path, Some(metadata.modified()?)))
                }
                Ok(_) => return Err(io::Error::other("log backup is not a regular file")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => slots.push((path, None)),
                Err(error) => return Err(error),
            }
        }
        slots.sort_by_key(|(_, modified)| *modified);
        let current = self.directory.join("app.jsonl");
        std::fs::rename(current, &slots[0].0)?;
        self.bytes = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实SMTP状态与扩展码须保留，邮件、精确秘密、注入换行和长令牌不应进入诊断。
    #[test]
    fn sanitization_keeps_status_but_removes_sensitive_material_and_injection() {
        let text = "550 5.7.1 quota for sample@example.test secret-value\r\nCookie: skey=hidden; https://host.test/?token=hidden 123456789 abcdefghijklmnopqrstuvwxyz";
        let clean = sanitize(text, &["secret-value"]);
        assert!(clean.contains("550 5.7.1 quota"));
        for secret in [
            "sample@example.test",
            "secret-value",
            "hidden",
            "123456789",
            "abcdefghijklmnopqrstuvwxyz",
            "\r",
            "\n",
        ] {
            assert!(!clean.contains(secret), "{clean}");
        }
        assert_eq!(
            sanitize("中文提示：连接失败。", &[]),
            "中文提示：连接失败。"
        );
    }

    #[test]
    fn correlation_is_stable_inside_process_and_does_not_contain_input() {
        assert_eq!(
            correlate("sample@example.test"),
            correlate("sample@example.test")
        );
        assert_ne!(
            correlate("sample@example.test"),
            correlate("different@example.test")
        );
        assert!(!correlate("sample@example.test").contains("sample"));
    }

    /// 用小阈值真实轮转，验证重开续写、最多五个文件、每行完整JSON以及UTF-8中文。
    #[test]
    fn rotation_is_bounded_and_reopen_appends_complete_json_lines() {
        let folder = tempfile::tempdir().unwrap();
        let mut writer = RotatingWriter::new(folder.path().to_path_buf(), 120).unwrap();
        for index in 0..30 {
            writer
                .write(format!("{{\"event\":\"中文记录\",\"index\":{index}}}\n").as_bytes())
                .unwrap();
        }
        drop(writer);
        let mut writer = RotatingWriter::new(folder.path().to_path_buf(), 120).unwrap();
        writer.write(b"{\"event\":\"reopened\"}\n").unwrap();
        drop(writer);
        let files = std::fs::read_dir(folder.path())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 5);
        for file in files {
            let text = std::fs::read_to_string(file.path()).unwrap();
            assert!(!text.starts_with('\u{feff}'));
            for line in text.lines() {
                serde_json::from_str::<Value>(line).unwrap();
            }
        }
        assert!(std::fs::read_to_string(folder.path().join("app.jsonl"))
            .unwrap()
            .contains("reopened"));
    }

    /// 文件不可写仅返回I/O失败；恢复权限/目标后同一写入器可以继续，业务错误不应被替换。
    #[test]
    fn failed_rotation_is_recoverable_and_keeps_existing_record() {
        let folder = tempfile::tempdir().unwrap();
        let mut writer = RotatingWriter::new(folder.path().to_path_buf(), 20).unwrap();
        writer.write(b"{\"first\":true}\n").unwrap();
        std::fs::create_dir(folder.path().join("app.4.jsonl")).unwrap();
        assert!(writer.write(b"{\"second\":true}\n").is_err());
        assert!(std::fs::read_to_string(folder.path().join("app.jsonl"))
            .unwrap()
            .contains("first"));
        std::fs::remove_dir(folder.path().join("app.4.jsonl")).unwrap();
        writer.write(b"{\"second\":true}\n").unwrap();
        assert!(std::fs::read_to_string(folder.path().join("app.jsonl"))
            .unwrap()
            .contains("second"));
    }

    #[test]
    fn structured_fields_are_bounded_and_sensitive_keys_are_suppressed() {
        let clean = clean_value(
            json!({"password":"short","html":"private body","items":vec!["user@example.test";50],"code":"smtp_failure"}),
            0,
        );
        assert_eq!(clean["password"], "[redacted]");
        assert_eq!(clean["html"], "[redacted]");
        assert_eq!(clean["items"].as_array().unwrap().len(), 32);
        assert!(!clean.to_string().contains("user@example.test"));
        assert_eq!(clean["code"], "smtp_failure");
    }

    /// 多后台线程共用同一锁，写入不能交错，所有完整事件都应可解析且不丢计数。
    #[test]
    fn concurrent_writes_preserve_complete_records() {
        let folder = tempfile::tempdir().unwrap();
        let writer = std::sync::Arc::new(Mutex::new(
            RotatingWriter::new(folder.path().to_path_buf(), MAX_FILE_BYTES).unwrap(),
        ));
        let workers = (0..8)
            .map(|thread| {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    for index in 0..50 {
                        writer
                            .lock()
                            .unwrap()
                            .write(
                                format!("{{\"thread\":{thread},\"index\":{index}}}\n").as_bytes(),
                            )
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        let text = std::fs::read_to_string(folder.path().join("app.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 400);
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }

    #[test]
    fn restart_discards_only_unfinished_tail_and_preserves_valid_records() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join("app.jsonl"),
            b"{\"complete\":true}\n{\"interrupted\":",
        )
        .unwrap();
        let mut writer = RotatingWriter::new(folder.path().to_path_buf(), 120).unwrap();
        writer.write(b"{\"recovered\":true}\n").unwrap();
        let text = std::fs::read_to_string(folder.path().join("app.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
        assert!(!text.contains("interrupted"));
    }

    /// 查看器未共享删除时Windows会拒绝轮转；重试不能先删备份，释放占用后应正常恢复。
    #[cfg(windows)]
    #[test]
    fn locked_current_log_does_not_delete_any_backup_on_failed_retries() {
        use std::os::windows::fs::OpenOptionsExt;
        let folder = tempfile::tempdir().unwrap();
        let mut writer = RotatingWriter::new(folder.path().to_path_buf(), 20).unwrap();
        writer.write(b"{\"first\":true}\n").unwrap();
        writer.file.take();
        for index in 1..=BACKUP_COUNT {
            std::fs::write(
                folder.path().join(format!("app.{index}.jsonl")),
                format!("backup-{index}"),
            )
            .unwrap();
        }
        let occupied = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(folder.path().join("app.jsonl"))
            .unwrap();
        for _ in 0..3 {
            assert!(writer.write(b"{\"second\":true}\n").is_err());
            for index in 1..=BACKUP_COUNT {
                assert_eq!(
                    std::fs::read_to_string(folder.path().join(format!("app.{index}.jsonl")))
                        .unwrap(),
                    format!("backup-{index}")
                );
            }
        }
        drop(occupied);
        writer.write(b"{\"second\":true}\n").unwrap();
        assert!(std::fs::read_to_string(folder.path().join("app.jsonl"))
            .unwrap()
            .contains("second"));
    }
}
