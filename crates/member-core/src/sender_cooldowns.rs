//! 跨任务、跨重启的 SMTP 发件身份冷却记录。作者：ahui。
//! 冷却按规范化 From 邮箱保存，账号排序、授权码更新、删除重建或旧接口不能清除已生效等待。
//! 本模块只保存公开邮箱、UTC 记录/截止时间及未完成尝试的间隔，不保存凭据或收件人。
//! 宿主运行时使用单调时钟；SQLite 仅负责恢复剩余时间和崩溃后的保守屏障，不自行发送邮件。

use crate::{
    database::{db_error, Database},
    models::{AppError, AppResult},
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const STORAGE_KEY: &str = "smtp_sender_cooldowns_v1";
const MAX_RECORDS: usize = 10_000;
const MAX_STORAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IDENTITY_BYTES: usize = 512;

/// 一个公开发件身份的有效屏障，不对应目录 ID，也不表示 SMTP 接受或收件箱已送达。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SenderCooldown {
    /// 已解析、去除首尾空白且 ASCII 大小写合并的裸 From 邮箱；不可写入登录名或显示名称。
    pub identity: String,
    /// 最近一次延长屏障或登记尝试的 UTC Unix 毫秒，用于交接和识别系统时钟回拨。
    pub recorded_at_unix_ms: i64,
    /// 不得早于 recorded_at；恢复时取截止时间减当前 UTC，不因调低新任务间隔而缩短。
    pub ready_at_unix_ms: i64,
    /// Some 表示网络尝试可能开始但结果后的屏障尚未提交；恢复时须补足该秒数并持久清除此标记。
    /// 正常等待/取消不产生此标记；即使旧截止已过期，也不能按过期记录删除未完成尝试。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_interval_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredCooldowns {
    version: u32,
    entries: Vec<SenderCooldown>,
}

/// 将合法裸 From 邮箱转成冷却键，不推断认证账号、QQ 别名或邮箱服务商关系。
///
/// 冷却刻意合并 ASCII 本地部分大小写，宁可让大小写不同的发件身份共用等待，也不允许
/// 通过修改大小写绕过既有屏障；这不是收件地址去重规则，不改变发送历史的本地部分语义。
/// 输入最多 512 字节，不接受显示名称、分隔列表或控制字符；返回值仅含规范化公开邮箱。
pub fn normalize_sender_cooldown_identity(address: &str) -> AppResult<String> {
    if address.len() > MAX_IDENTITY_BYTES || address.chars().any(char::is_control) {
        return Err(identity_error());
    }
    let address = address.trim();
    if address.is_empty() {
        return Err(identity_error());
    }
    let parsed: lettre::Address = address.parse().map_err(|_| identity_error())?;
    Ok(format!("{}@{}", parsed.user(), parsed.domain()).to_ascii_lowercase())
}

impl Database {
    /// 只读全部已验证记录；缺少键是首次使用，不写空标记，不清理过期项或改变任何目录版本。
    /// 调用方只恢复 ready_at 仍在未来的等待；pending 必须先经 recover_sender_cooldowns 固定恢复。
    /// 格式或存储失败必须阻止新 SMTP 尝试，不能把错误解释成没有冷却记录。
    pub fn sender_cooldowns(&self) -> AppResult<Vec<SenderCooldown>> {
        let connection = self.connection().map_err(|_| storage_error())?;
        Ok(read_state(&connection)?.entries)
    }

    /// 无活跃 SMTP 时首次装载持久记录，保守恢复中断尝试并只提交一次固定截止时间。
    ///
    /// pending 表示上一进程可能在任意时刻崩溃，因此从本次恢复时刻补足其原间隔；与旧截止取
    /// 最大值后清除标记。再次重启只恢复剩余时间，不再重新延长；写失败整批回滚并禁止发送。
    /// 正常取消等待没有 pending，调用此方法不会延长或重写其记录。时钟回拨时保留较晚旧截止。
    pub fn recover_sender_cooldowns(&self) -> AppResult<Vec<SenderCooldown>> {
        self.recover_sender_cooldowns_at(Utc::now().timestamp_millis())
    }

    /// 在真正开始网络尝试之前持久登记可能发生的副作用；成功提交之后宿主才能调用 SMTP。
    /// interval_seconds 必须为 1..=3600 秒，真实发送成功、失败或 unknown 均需调用 finish 刷新。
    /// 宿主须先完成首次恢复，并串行化同一发件身份的网络生命周期，不能在旧尝试仍进行时重入。
    /// 此标记只能由明确尝试产生，取消尚未开始的任务或等待不得调用本方法。
    pub fn begin_sender_cooldown_attempt(
        &self,
        identity: &str,
        interval_seconds: u64,
    ) -> AppResult<i64> {
        self.begin_sender_cooldown_attempt_at(
            identity,
            interval_seconds,
            Utc::now().timestamp_millis(),
        )
    }

    /// 在本批 SMTP 结果和本机记账处理后刷新屏障，并清除已提交的尝试标记。
    /// ready_at_unix_ms 是宿主按完成时刻加本次间隔生成的 UTC Unix 毫秒，不能早于当前 UTC。
    /// 与已有截止时间取最大值，不因失败、旧接口或调低间隔而缩短；事务失败保留 pending 供恢复。
    pub fn finish_sender_cooldown_attempt(
        &self,
        identity: &str,
        ready_at_unix_ms: i64,
    ) -> AppResult<i64> {
        self.merge_sender_cooldown_at(
            identity,
            ready_at_unix_ms,
            Utc::now().timestamp_millis(),
            Mutation::Finish,
        )
    }

    /// 为已有公开身份保守合并一个截止时间，返回实际保存的最大值，不清除 pending 标记。
    /// 本方法用于宿主同步已有屏障，不能替代网络尝试前后的 begin/finish 崩溃保护协议。
    pub fn record_sender_cooldown(&self, identity: &str, ready_at_unix_ms: i64) -> AppResult<i64> {
        self.merge_sender_cooldown_at(
            identity,
            ready_at_unix_ms,
            Utc::now().timestamp_millis(),
            Mutation::Record,
        )
    }

    fn begin_sender_cooldown_attempt_at(
        &self,
        identity: &str,
        interval_seconds: u64,
        now: i64,
    ) -> AppResult<i64> {
        validate_interval(interval_seconds)?;
        let ready_at = now
            .checked_add(i64::try_from(interval_seconds * 1000).map_err(|_| time_error())?)
            .ok_or_else(time_error)?;
        self.merge_sender_cooldown_at(identity, ready_at, now, Mutation::Begin(interval_seconds))
    }

    fn merge_sender_cooldown_at(
        &self,
        identity: &str,
        ready_at: i64,
        now: i64,
        mutation: Mutation,
    ) -> AppResult<i64> {
        let identity = normalize_sender_cooldown_identity(identity)?;
        validate_time(now, ready_at)?;
        let mut connection = self.connection().map_err(|_| storage_error())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let mut state = read_state(&transaction)?;
        // 未完成尝试即使过了初始截止也不能被丢弃；其网络持续时长未知，必须保留恢复屏障。
        state.entries.retain(|entry| {
            entry.identity == identity
                || entry.pending_interval_seconds.is_some()
                || entry.ready_at_unix_ms > now
        });
        let position = state
            .entries
            .iter()
            .position(|entry| entry.identity == identity);
        let entry = if let Some(position) = position {
            &mut state.entries[position]
        } else {
            if state.entries.len() >= MAX_RECORDS {
                return Err(storage_error());
            }
            state.entries.push(SenderCooldown {
                identity,
                recorded_at_unix_ms: now,
                ready_at_unix_ms: ready_at,
                pending_interval_seconds: None,
            });
            state.entries.last_mut().ok_or_else(storage_error)?
        };
        if ready_at > entry.ready_at_unix_ms {
            entry.ready_at_unix_ms = ready_at;
            entry.recorded_at_unix_ms = now;
        }
        match mutation {
            Mutation::Record => {}
            Mutation::Begin(interval) => {
                entry.recorded_at_unix_ms = now;
                // 重复登记也不缩短尚未完成尝试的恢复间隔，防止较短的新配置覆盖旧屏障。
                entry.pending_interval_seconds =
                    Some(entry.pending_interval_seconds.unwrap_or(0).max(interval));
            }
            Mutation::Finish => {
                entry.pending_interval_seconds = None;
            }
        }
        let result = entry.ready_at_unix_ms;
        write_state(&transaction, &mut state)?;
        transaction.commit().map_err(sql_error)?;
        Ok(result)
    }

    fn recover_sender_cooldowns_at(&self, now: i64) -> AppResult<Vec<SenderCooldown>> {
        validate_timestamp(now)?;
        let mut connection = self.connection().map_err(|_| storage_error())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let mut state = read_state(&transaction)?;
        let mut changed = false;
        for entry in &mut state.entries {
            if let Some(interval) = entry.pending_interval_seconds.take() {
                let recovered = now
                    .checked_add(i64::try_from(interval * 1000).map_err(|_| storage_error())?)
                    .ok_or_else(storage_error)?;
                entry.ready_at_unix_ms = entry.ready_at_unix_ms.max(recovered);
                entry.recorded_at_unix_ms = now;
                changed = true;
            }
        }
        if changed {
            write_state(&transaction, &mut state)?;
        }
        transaction.commit().map_err(sql_error)?;
        Ok(state.entries)
    }
}

enum Mutation {
    Record,
    Begin(u64),
    Finish,
}

/// 单行 settings 保存一整份小型目录，立即事务负责并发合并；SQL参数从不直接拼接邮箱或JSON。
fn read_state(connection: &Connection) -> AppResult<StoredCooldowns> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [STORAGE_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(stored) = stored else {
        return Ok(StoredCooldowns {
            version: 1,
            entries: Vec::new(),
        });
    };
    if stored.len() > MAX_STORAGE_BYTES {
        return Err(storage_error());
    }
    let state: StoredCooldowns = serde_json::from_str(&stored).map_err(|_| storage_error())?;
    validate_state(&state)?;
    Ok(state)
}

fn write_state(connection: &Connection, state: &mut StoredCooldowns) -> AppResult<()> {
    state
        .entries
        .sort_by(|left, right| left.identity.cmp(&right.identity));
    validate_state(state)?;
    let stored = serde_json::to_string(state).map_err(|_| storage_error())?;
    if stored.len() > MAX_STORAGE_BYTES {
        return Err(storage_error());
    }
    connection.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![STORAGE_KEY, stored],
    ).map_err(sql_error)?;
    Ok(())
}

fn validate_state(state: &StoredCooldowns) -> AppResult<()> {
    if state.version != 1 || state.entries.len() > MAX_RECORDS {
        return Err(storage_error());
    }
    let mut identities = HashSet::new();
    for entry in &state.entries {
        if normalize_sender_cooldown_identity(&entry.identity).map_err(|_| storage_error())?
            != entry.identity
            || !identities.insert(&entry.identity)
        {
            return Err(storage_error());
        }
        validate_time(entry.recorded_at_unix_ms, entry.ready_at_unix_ms)
            .map_err(|_| storage_error())?;
        if let Some(interval) = entry.pending_interval_seconds {
            validate_interval(interval).map_err(|_| storage_error())?;
        }
    }
    Ok(())
}

fn validate_timestamp(value: i64) -> AppResult<()> {
    if value < 0 || chrono::DateTime::<Utc>::from_timestamp_millis(value).is_none() {
        return Err(time_error());
    }
    Ok(())
}

fn validate_time(recorded_at: i64, ready_at: i64) -> AppResult<()> {
    validate_timestamp(recorded_at)?;
    validate_timestamp(ready_at)?;
    if ready_at < recorded_at {
        return Err(time_error());
    }
    Ok(())
}

fn validate_interval(seconds: u64) -> AppResult<()> {
    if !(1..=3600).contains(&seconds) {
        return Err(time_error());
    }
    Ok(())
}

fn sql_error(error: rusqlite::Error) -> AppError {
    // 驱动诊断只记录固定分类；页面只接收冷却专属错误，不能在失败时绕过发送屏障。
    let _ = db_error(error);
    storage_error()
}

fn identity_error() -> AppError {
    AppError::new(
        "sender_cooldown_identity",
        "发件邮箱无法用于发送间隔校验，请检查账号配置。",
    )
}

fn time_error() -> AppError {
    AppError::new(
        "sender_cooldown_time",
        "发送间隔或系统时间无效，已阻止发送，请检查后重试。",
    )
}

fn storage_error() -> AppError {
    AppError::new(
        "sender_cooldown_storage",
        "无法可靠读取或保存发件账号冷却记录，已阻止后续发送。请检查本机数据目录后重试。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("cooldown.sqlite3")).unwrap();
        (directory, database)
    }

    fn record(database: &Database, identity: &str, now: i64, ready: i64) -> i64 {
        database
            .merge_sender_cooldown_at(identity, ready, now, Mutation::Record)
            .unwrap()
    }

    /// 冷却键只由裸发件邮箱组成，大小写变化不能绕过；不猜测不同别名是否属于同一账号。
    #[test]
    fn normalized_sender_identity_merges_ascii_case_and_rejects_non_addresses() {
        assert_eq!(
            normalize_sender_cooldown_identity(" Sender@EXAMPLE.Test ").unwrap(),
            "sender@example.test"
        );
        assert_ne!(
            normalize_sender_cooldown_identity("alias@example.test").unwrap(),
            normalize_sender_cooldown_identity("sender@example.test").unwrap()
        );
        for value in [
            "",
            " ",
            "Display <sender@example.test>",
            "first@example.test;second@example.test",
            "sender@example.test\n",
            "not-an-email",
        ] {
            assert_eq!(
                normalize_sender_cooldown_identity(value).unwrap_err().code,
                "sender_cooldown_identity"
            );
        }
        assert!(normalize_sender_cooldown_identity(&"a".repeat(MAX_IDENTITY_BYTES + 1)).is_err());
    }

    /// 未发送账号只读时不产生记录；重开恢复同一截止而不是重计间隔，且不动目录版本或投递历史。
    #[test]
    fn restart_keeps_remaining_deadline_independent_identities_and_other_settings() {
        let (directory, database) = fixture();
        assert!(database.sender_cooldowns().unwrap().is_empty());
        assert!(database.setting(STORAGE_KEY).unwrap().is_none());
        database
            .set_setting("smtp_sender_accounts_v1", "synthetic-unchanged-revision")
            .unwrap();
        record(&database, "Sender@EXAMPLE.test", 1_000, 901_000);
        record(&database, "other@example.test", 2_000, 102_000);
        let before = database.sender_cooldowns().unwrap();
        drop(database);
        let reopened = Database::open(directory.path().join("cooldown.sqlite3")).unwrap();
        assert_eq!(
            reopened.recover_sender_cooldowns_at(51_000).unwrap(),
            before
        );
        assert_eq!(
            before
                .iter()
                .find(|entry| entry.identity == "sender@example.test")
                .unwrap()
                .ready_at_unix_ms
                - 51_000,
            850_000
        );
        assert_eq!(
            reopened
                .setting("smtp_sender_accounts_v1")
                .unwrap()
                .unwrap(),
            "synthetic-unchanged-revision"
        );
        assert_eq!(reopened.sent_count().unwrap(), 0);
        assert!(reopened.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(
            reopened
                .connection()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            5
        );
    }

    /// 调低下一任务间隔、旧时间写入或时钟回拨都不能缩短已有截止；不同账号独立合并。
    #[test]
    fn merge_never_shortens_existing_deadline_and_clock_rollback_is_conservative() {
        let (_directory, database) = fixture();
        assert_eq!(
            record(&database, "sender@example.test", 1_000, 901_000),
            901_000
        );
        assert_eq!(
            record(&database, "SENDER@example.test", 11_000, 21_000),
            901_000
        );
        assert_eq!(
            record(&database, "sender@example.test", 500, 5_500),
            901_000
        );
        assert_eq!(
            record(&database, "other@example.test", 12_000, 22_000),
            22_000
        );
        assert_eq!(
            record(&database, "sender@example.test", 101_000, 1_001_000),
            1_001_000
        );
        let entries = database.sender_cooldowns().unwrap();
        let first = entries
            .iter()
            .find(|entry| entry.identity == "sender@example.test")
            .unwrap();
        assert_eq!(
            (first.recorded_at_unix_ms, first.ready_at_unix_ms),
            (101_000, 1_001_000)
        );
        assert_eq!(first.ready_at_unix_ms - 500, 1_000_500);
    }

    /// 多个短连接并发合并同一身份必须保留最大截止，不能出现后提交的旧值覆盖较新屏障。
    #[test]
    fn concurrent_transactions_keep_the_maximum_deadline() {
        let (_directory, database) = fixture();
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|index| {
                let database = database.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let proposed = 2_000 + index * 100;
                    assert!(record(&database, "sender@example.test", 1_000, proposed) >= proposed);
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        let entries = database.sender_cooldowns().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ready_at_unix_ms, 2_700);
    }

    /// 达到有界目录上限时拒绝新身份，不能驱逐未到期或pending记录；仅已过期安全项可腾出位置。
    #[test]
    fn bounded_storage_never_evicts_live_or_pending_accounts() {
        let (_directory, database) = fixture();
        let mut state = StoredCooldowns {
            version: 1,
            entries: (0..MAX_RECORDS)
                .map(|index| SenderCooldown {
                    identity: format!("sender-{index}@example.test"),
                    recorded_at_unix_ms: 1_000,
                    ready_at_unix_ms: 901_000,
                    pending_interval_seconds: None,
                })
                .collect(),
        };
        write_state(&database.connection().unwrap(), &mut state).unwrap();
        let before = database.setting(STORAGE_KEY).unwrap().unwrap();
        assert_eq!(
            database
                .merge_sender_cooldown_at("new@example.test", 902_000, 2_000, Mutation::Record)
                .unwrap_err()
                .code,
            "sender_cooldown_storage"
        );
        assert_eq!(database.setting(STORAGE_KEY).unwrap().unwrap(), before);
        state.entries[0].ready_at_unix_ms = 1_500;
        state.entries[1].ready_at_unix_ms = 1_500;
        state.entries[1].pending_interval_seconds = Some(1);
        let expired = state.entries[0].identity.clone();
        let pending = state.entries[1].identity.clone();
        write_state(&database.connection().unwrap(), &mut state).unwrap();
        record(&database, "new@example.test", 2_000, 902_000);
        let after = database.sender_cooldowns().unwrap();
        assert_eq!(after.len(), MAX_RECORDS);
        assert!(after.iter().all(|entry| entry.identity != expired));
        assert_eq!(
            after
                .iter()
                .find(|entry| entry.identity == pending)
                .unwrap()
                .pending_interval_seconds,
            Some(1)
        );
        assert!(after
            .iter()
            .any(|entry| entry.identity == "new@example.test"));
    }

    /// 只有明确过期且无未完成尝试的旁路记录可清理；不删未来记录，也不因目录移除而删除屏障。
    #[test]
    fn write_cleanup_removes_only_expired_nonpending_records() {
        let (_directory, database) = fixture();
        record(&database, "expired@example.test", 100, 1_000);
        database
            .begin_sender_cooldown_attempt_at("pending@example.test", 1, 100)
            .unwrap();
        record(&database, "future@example.test", 200, 100_000);
        record(&database, "new@example.test", 2_000, 5_000);
        let entries = database.sender_cooldowns().unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries
            .iter()
            .all(|entry| entry.identity != "expired@example.test"));
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.identity == "pending@example.test")
                .unwrap()
                .pending_interval_seconds,
            Some(1)
        );
        assert!(entries
            .iter()
            .any(|entry| entry.identity == "future@example.test"));
        let stored = database.setting(STORAGE_KEY).unwrap().unwrap();
        // 只读及正常恢复均不清空过期项或重写已生效截止，避免取消等待被延长。
        assert_eq!(database.sender_cooldowns().unwrap(), entries);
        database.recover_sender_cooldowns_at(3_000).unwrap();
        assert_ne!(database.setting(STORAGE_KEY).unwrap().unwrap(), stored);
        let recovered = database.setting(STORAGE_KEY).unwrap().unwrap();
        database.recover_sender_cooldowns_at(4_000).unwrap();
        assert_eq!(database.setting(STORAGE_KEY).unwrap().unwrap(), recovered);
    }

    /// 长SMTP/进程中断使结果后的屏障未保存时，重启补足一次原间隔；后续重启只恢复剩余时间。
    #[test]
    fn pending_attempt_recovers_once_after_restart_without_infinite_extension() {
        let (directory, database) = fixture();
        database
            .begin_sender_cooldown_attempt_at("sender@example.test", 900, 1_000)
            .unwrap();
        assert_eq!(
            database.sender_cooldowns().unwrap()[0].pending_interval_seconds,
            Some(900)
        );
        drop(database);
        let reopened = Database::open(directory.path().join("cooldown.sqlite3")).unwrap();
        let recovered = reopened.recover_sender_cooldowns_at(2_000_000).unwrap();
        assert_eq!(
            (
                recovered[0].recorded_at_unix_ms,
                recovered[0].ready_at_unix_ms
            ),
            (2_000_000, 2_900_000)
        );
        assert_eq!(recovered[0].pending_interval_seconds, None);
        assert_eq!(
            reopened.recover_sender_cooldowns_at(2_001_000).unwrap(),
            recovered
        );
        // 回拨时保留更晚已生效屏障，不把恢复时刻+间隔强行当作较短的新截止。
        reopened
            .begin_sender_cooldown_attempt_at("sender@example.test", 1, 1_000)
            .unwrap();
        assert_eq!(
            reopened.recover_sender_cooldowns_at(2_000).unwrap()[0].ready_at_unix_ms,
            2_900_000
        );
    }

    /// 正常完成与unknown结果采用相同finish协议；普通合并不擅自清除崩溃保护标记。
    #[test]
    fn finished_attempt_extends_deadline_and_clears_pending_only_when_committed() {
        let (_directory, database) = fixture();
        database
            .begin_sender_cooldown_attempt_at("sender@example.test", 900, 1_000)
            .unwrap();
        record(&database, "sender@example.test", 2_000, 902_000);
        assert_eq!(
            database.sender_cooldowns().unwrap()[0].pending_interval_seconds,
            Some(900)
        );
        assert_eq!(
            database
                .merge_sender_cooldown_at("sender@example.test", 950_000, 50_000, Mutation::Finish)
                .unwrap(),
            950_000
        );
        let entry = &database.sender_cooldowns().unwrap()[0];
        assert_eq!(entry.pending_interval_seconds, None);
        assert_eq!(
            (entry.recorded_at_unix_ms, entry.ready_at_unix_ms),
            (50_000, 950_000)
        );
        let stored = database.setting(STORAGE_KEY).unwrap().unwrap();
        database.recover_sender_cooldowns_at(100_000).unwrap();
        assert_eq!(database.setting(STORAGE_KEY).unwrap().unwrap(), stored);
    }

    /// 真正SQLite失败不得提交部分合并、清理或清pending；原始记录保留供恢复，不能宣称已安全发送。
    #[test]
    fn sql_failure_rolls_back_merge_cleanup_finish_and_recovery() {
        let (_directory, database) = fixture();
        record(&database, "expired@example.test", 100, 1_000);
        database
            .begin_sender_cooldown_attempt_at("sender@example.test", 900, 100)
            .unwrap();
        let before = database.setting(STORAGE_KEY).unwrap().unwrap();
        database.connection().unwrap().execute_batch("CREATE TRIGGER reject_cooldown BEFORE UPDATE ON settings WHEN OLD.key='smtp_sender_cooldowns_v1' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert_eq!(
            database
                .merge_sender_cooldown_at("new@example.test", 50_000, 5_000, Mutation::Record)
                .unwrap_err()
                .code,
            "sender_cooldown_storage"
        );
        assert!(database
            .merge_sender_cooldown_at("sender@example.test", 950_000, 50_000, Mutation::Finish)
            .is_err());
        assert!(database.recover_sender_cooldowns_at(1_000_000).is_err());
        assert_eq!(database.setting(STORAGE_KEY).unwrap().unwrap(), before);
        assert_eq!(
            database
                .sender_cooldowns()
                .unwrap()
                .iter()
                .find(|entry| entry.identity == "sender@example.test")
                .unwrap()
                .pending_interval_seconds,
            Some(900)
        );
        assert!(database
            .sender_cooldowns()
            .unwrap()
            .iter()
            .any(|entry| entry.identity == "expired@example.test"));
        let (_empty_directory, empty) = fixture();
        empty.connection().unwrap().execute_batch("CREATE TRIGGER reject_initial_cooldown BEFORE INSERT ON settings WHEN NEW.key='smtp_sender_cooldowns_v1' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert_eq!(
            empty
                .begin_sender_cooldown_attempt_at("sender@example.test", 900, 1_000)
                .unwrap_err()
                .code,
            "sender_cooldown_storage"
        );
        assert!(empty.setting(STORAGE_KEY).unwrap().is_none());
    }

    /// 损坏或带未声明字段的数据不得静默丢弃；读取、写入和恢复都fail closed并保留原始SQLite证据。
    #[test]
    fn malformed_storage_never_resets_protection_or_repairs_user_data() {
        let (_directory, database) = fixture();
        let valid = SenderCooldown {
            identity: "sender@example.test".into(),
            recorded_at_unix_ms: 1_000,
            ready_at_unix_ms: 901_000,
            pending_interval_seconds: None,
        };
        let duplicate = serde_json::to_string(&StoredCooldowns {
            version: 1,
            entries: vec![valid.clone(), valid],
        })
        .unwrap();
        for stored in [
            "{invalid".to_owned(),
            r#"{"version":2,"entries":[]}"#.to_owned(),
            r#"{"version":1,"entries":[],"password":"forbidden"}"#.to_owned(),
            r#"{"version":1,"entries":[{"identity":"SENDER@example.test","recordedAtUnixMs":1000,"readyAtUnixMs":2000}]}"#.to_owned(),
            r#"{"version":1,"entries":[{"identity":"sender@example.test","recordedAtUnixMs":2000,"readyAtUnixMs":1000}]}"#.to_owned(),
            r#"{"version":1,"entries":[{"identity":"sender@example.test","recordedAtUnixMs":1000,"readyAtUnixMs":2000,"pendingIntervalSeconds":0}]}"#.to_owned(),
            duplicate,
        ] {
            database.set_setting(STORAGE_KEY,&stored).unwrap();
            assert_eq!(database.sender_cooldowns().unwrap_err().code,"sender_cooldown_storage");
            assert!(database.recover_sender_cooldowns_at(1_000).is_err());
            assert!(database.begin_sender_cooldown_attempt_at("new@example.test",900,1_000).is_err());
            assert_eq!(database.setting(STORAGE_KEY).unwrap().unwrap(),stored);
        }
    }

    /// 无效时间及间隔在访问持久目录前拒绝，且不会意外创建首次尝试或空冷却池。
    #[test]
    fn invalid_intervals_and_timestamps_do_not_create_records() {
        let (_directory, database) = fixture();
        for seconds in [0, 3_601, u64::MAX] {
            assert_eq!(
                database
                    .begin_sender_cooldown_attempt_at("sender@example.test", seconds, 1_000)
                    .unwrap_err()
                    .code,
                "sender_cooldown_time"
            );
        }
        for (now, ready) in [(-1, 1_000), (1_000, 999), (1_000, i64::MAX)] {
            assert!(database
                .merge_sender_cooldown_at("sender@example.test", ready, now, Mutation::Record)
                .is_err());
        }
        assert!(database.setting(STORAGE_KEY).unwrap().is_none());
    }
}
