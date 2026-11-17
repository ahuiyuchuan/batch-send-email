//! 本机 SQLite 持久化：名单、过滤结果、明确发送记录、邮件模板和无凭据配置。作者：ahui。
//! 所有 SQL 使用参数绑定；SMTP 密码、QQ Cookie 永不进入数据库。

use crate::{
    diagnostics,
    models::{AppError, AppResult, Snapshot},
};
use chrono::Local;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub use crate::manual_exclusions::{
    ManualExclusionMutation, ManualExclusionRecord, ManualExclusionView, PreparedMailRecipients,
};

/// 本机历史按收件人全局去重，不因群或提取账号不同而重复发送。
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SentRecord {
    pub id: i64,
    pub qq_number: String,
    pub email: String,
    pub sent_at: String,
    /// smtp_accepted、manual_verified、manual 或 imported；人工核实不等同于 SMTP 接受证明。
    pub origin: String,
    pub note: String,
    /// 最近一次成功/人工核实已发送所用的发件邮箱快照；旧版、导入和独立人工标记为空，不猜测账号。
    #[serde(rename = "senderAddress")]
    pub sender_address: String,
    /// 发送前冻结的账号名称；账号后续改名或删除不改写历史，空值表示旧记录未保存归属。
    #[serde(rename = "senderName")]
    pub sender_name: String,
}

/// 等待用户人工核实的投递；只返回 unknown，正在发送的 sending 不能被人工改判。
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedDelivery {
    /// 本机投递记录标识，处理操作必须使用此标识而不是邮箱批量覆盖。
    pub id: i64,
    pub email: String,
    /// 带时区的 RFC 3339 时间，表示最近一次确认结果不明的时间。
    pub updated_at: String,
    /// 后台提供的无凭据说明，不包含 SMTP 原始响应或授权码。
    pub message: String,
    /// 本次尝试开始前保存的发件邮箱，重启恢复及人工核实时继续沿用；旧记录为空。
    #[serde(rename = "senderAddress")]
    pub sender_address: String,
    /// 本次尝试开始时的账号名称，不通过现有目录回填，避免误归属到已改名或重建账号。
    #[serde(rename = "senderName")]
    pub sender_name: String,
}

/// 导入结果，非法行不会导致合法行丢失，重复行单独统计。
#[derive(Default, Serialize, Debug)]
pub struct ImportOutcome {
    pub added: usize,
    pub skipped: usize,
    pub invalid: usize,
}

/// 已由本机固定快照核对的群收件范围；群名和数量只能取自数据库与校验后的选择。
/// 这是本次任务的来源摘要，不是 QQ 邮箱已开通或邮件已送达的证明。
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GroupMailAudience {
    pub snapshot_id: String,
    pub group_id: String,
    pub group_name: String,
    /// 去重后的群成员候选数，不包含额外填写的外部地址，也未扣除发送时历史跳过项。
    pub member_count: usize,
}

/// 成员当前本机发信状态；未决优先于历史成功，available 仅表示本机没有阻止依据。
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MemberMailStatus {
    pub qq_number: String,
    /// available、sent 或 unresolved；不将“无历史”解释为未开通邮箱或从未在外部发送。
    pub status: String,
    /// sent 时保留 smtp_accepted/manual_verified/manual/imported，其他状态为空字符串。
    pub origin: String,
}

/// 每次操作使用短连接，后台不在持有 UI 状态锁时执行数据库或文件 I/O。
#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}

/// 统一屏蔽底层路径、SQL 参数及模板内容；数据库模块和模板事务共用此错误边界。
pub(crate) fn db_error(error: rusqlite::Error) -> AppError {
    // SQLite 的说明字符串可能含 SQL、路径或参数，只记录驱动的固定分类和数值扩展码。
    let fields = match &error {
        rusqlite::Error::SqliteFailure(code, _) => serde_json::json!({
            "kind": "sqlite_failure", "code": format!("{:?}", code.code),
            "extended_code": code.extended_code,
        }),
        rusqlite::Error::ToSqlConversionFailure(source)
        | rusqlite::Error::FromSqlConversionFailure(_, _, source) => {
            if let Some(io) = source.downcast_ref::<std::io::Error>() {
                serde_json::json!({"kind": "conversion_io", "io_kind": format!("{:?}", io.kind()), "os_code": io.raw_os_error()})
            } else {
                serde_json::json!({"kind": "conversion"})
            }
        }
        _ => serde_json::json!({"kind": "sqlite_other"}),
    };
    diagnostics::event("error", "database", "driver_failure", fields);
    AppError::new(
        "database_error",
        "本机数据库读写失败，请检查磁盘空间和数据目录权限。",
    )
}

impl Database {
    /// 打开或建立版本 5 数据库；发件归属与已有结构同事务升级，旧记录保持未知，不据当前账号猜测。
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut diagnostic = diagnostics::operation("database", "open");
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| {
                    diagnostics::event("error", "database", "directory_failure", serde_json::json!({"io_kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}));
                    AppError::new("data_directory", "无法创建本机数据目录。")
                })?;
        }
        let database = Self { path };
        let mut connection = database.connection()?;
        // 先只读检查，避免旧程序在拒绝新版数据库之前更改其 journal_mode。
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        if version > 5 {
            return Err(AppError::new(
                "database_version",
                "数据库由更新版本创建，请使用对应版本程序，避免覆盖数据。",
            ));
        }
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(db_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let version: i64 = transaction
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        // 拿到迁移写锁后再次检查，防止初次读取后另一个程序已经升级数据库。
        if version > 5 {
            return Err(AppError::new(
                "database_version",
                "数据库由更新版本创建，请使用对应版本程序，避免覆盖数据。",
            ));
        }
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS sent_history (
                id INTEGER PRIMARY KEY,
                recipient_key TEXT NOT NULL UNIQUE,
                qq_number TEXT NOT NULL DEFAULT '',
                email TEXT NOT NULL,
                sent_at TEXT NOT NULL,
                origin TEXT NOT NULL,
                note TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS ix_sent_qq ON sent_history(qq_number);
             CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                finished_at TEXT NOT NULL,
                payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mail_delivery (
                id INTEGER PRIMARY KEY,
                job_id TEXT NOT NULL,
                recipient TEXT NOT NULL,
                state TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                message TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS ix_mail_job ON mail_delivery(job_id);
             CREATE INDEX IF NOT EXISTS ix_mail_recipient_state ON mail_delivery(recipient,state);
             -- 模板只保存用户明确保存的主题、正文与资源副本，不包含收件人、发送配置或凭据。
             CREATE TABLE IF NOT EXISTS mail_templates (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 80),
                subject TEXT NOT NULL CHECK(length(subject)<=200),
                html TEXT NOT NULL CHECK(length(CAST(html AS BLOB))<=2097152),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS ix_mail_template_updated ON mail_templates(updated_at,id);
             -- 资源以模板为所有者；复制时产生独立行，删除模板只级联其自身资源。
             CREATE TABLE IF NOT EXISTS mail_template_resources (
                template_id TEXT NOT NULL REFERENCES mail_templates(id) ON DELETE CASCADE,
                resource_id TEXT NOT NULL,
                position INTEGER NOT NULL CHECK(position>=0),
                name TEXT NOT NULL,
                mime_type TEXT,
                content_id TEXT,
                bytes BLOB NOT NULL CHECK(length(bytes)<=18874368),
                PRIMARY KEY(template_id,resource_id),
                UNIQUE(template_id,position),
                UNIQUE(template_id,content_id),
                CHECK((mime_type IS NULL AND content_id IS NULL) OR (mime_type IS NOT NULL AND content_id IS NOT NULL))
             );
             -- 兼容早期记录的域名大小写，仅域名归一化，保留可能区分大小写的本地部分。
             UPDATE mail_delivery
                SET recipient=substr(recipient,1,instr(recipient,'@')) || lower(substr(recipient,instr(recipient,'@')+1))
                WHERE instr(recipient,'@')>0
                  AND substr(recipient,instr(recipient,'@')+1)<>lower(substr(recipient,instr(recipient,'@')+1));
             -- 手动排除独立于已发送历史，按规范完整邮箱全局生效；记录不意味着已投递。
             CREATE TABLE IF NOT EXISTS manual_exclusions (
                id INTEGER PRIMARY KEY,
                recipient_key TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL,
                qq_number TEXT NOT NULL DEFAULT '',
                note TEXT NOT NULL DEFAULT '' CHECK(length(note)<=500),
                created_at TEXT NOT NULL
             );
             -- 单行版本与增删同事务提交；仅实际改变记录时递增，用于拒绝过期确认。
             CREATE TABLE IF NOT EXISTS manual_exclusion_state (
                id INTEGER PRIMARY KEY CHECK(id=1),
                revision INTEGER NOT NULL CHECK(revision>=0)
             );
             INSERT OR IGNORE INTO manual_exclusion_state(id,revision) VALUES(1,0);"
        ).map_err(db_error)?;
        // 旧快照按真实完成时间回填每群最新结果；失败时新增表、数据与版本号一起回滚，不能半升级。
        crate::group_cache::initialize(&transaction, version)?;
        if version < 5 {
            // 归属是不可变尝试快照，空串只表示历史未保存；新增列、数据和版本号必须一并回滚。
            // 新库先建立兼容旧结构再执行本迁移，使0—4版本与新建库采用相同列定义。
            transaction
                .execute_batch(
                    "ALTER TABLE sent_history ADD COLUMN sender_address TEXT NOT NULL DEFAULT '';
                 ALTER TABLE sent_history ADD COLUMN sender_name TEXT NOT NULL DEFAULT '';
                 ALTER TABLE mail_delivery ADD COLUMN sender_address TEXT NOT NULL DEFAULT '';
                 ALTER TABLE mail_delivery ADD COLUMN sender_name TEXT NOT NULL DEFAULT '';",
                )
                .map_err(db_error)?;
        }
        transaction
            .pragma_update(None, "user_version", 5)
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        // 启动恢复只应由唯一实例执行。进程可能在 SMTP 接受之后、落库之前退出，
        // 因此遗留 sending 必须转为待核实，不能当成未发送而直接重试。
        let recovered = connection.execute(
            "UPDATE mail_delivery SET state='unknown',updated_at=?1,message='上次进程中断，投递结果需人工核实' WHERE state='sending'",
            [Local::now().to_rfc3339()]
        ).map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "opened",
            serde_json::json!({"schema_version": 5, "previous_schema_version": version, "recovered_unknown": recovered}),
        );
        diagnostic.finish("success");
        Ok(database)
    }

    /// 每个短连接都启用外键，保证模板删除级联和资源写入约束在后续操作中持续生效。
    pub(crate) fn connection(&self) -> AppResult<Connection> {
        let connection = Connection::open(&self.path).map_err(db_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(db_error)?;
        Ok(connection)
    }

    /// 显示本机实际数据位置，路径只返回本机界面，不写入诊断输出。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 同一事务保存不可变名单与过滤明细；新获取/沿最新版本恢复时同步更新群最新结果和元信息。
    /// 获取及恢复版本共保留最近 20 次；每群最新结果额外保留，从旧版派生不覆盖更新的获取结果。
    pub fn save_snapshot(&self, snapshot: &Snapshot) -> AppResult<()> {
        let mut diagnostic = diagnostics::operation("database", "save_snapshot");
        let payload = serde_json::to_string(snapshot)
            .map_err(|_| AppError::new("snapshot_encode", "无法保存本次结果，请先导出文件。"))?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO snapshots(id,finished_at,payload) VALUES(?1,?2,?3)",
                // 排序列表示版本完成时间；JSON 中的原获取时间保持不变，调整时间独立可追溯。
                params![
                    snapshot.id,
                    snapshot
                        .restoration
                        .as_ref()
                        .map(|value| value.adjusted_at.as_str())
                        .unwrap_or(&snapshot.finished_at),
                    payload
                ],
            )
            .map_err(db_error)?;
        // 从旧邮件版本恢复成员不能覆盖同群已经重新获取的更新结果；只有沿用最新版本才推进群缓存。
        let update_latest = match &snapshot.restoration {
            None => true,
            Some(value) => transaction.query_row(
                "SELECT snapshot_id=?1 FROM latest_group_results WHERE source=?2 AND account_qq=?3 AND group_id=?4",
                params![value.parent_snapshot_id, snapshot.account.source, snapshot.account.qq_number, snapshot.group.id],
                |row| row.get::<_, bool>(0),
            ).optional().map_err(db_error)?.unwrap_or(true),
        };
        if update_latest {
            crate::group_cache::save_latest_result(&transaction, snapshot, &payload)?;
        }
        // RFC3339 字符串可能具有不同时区偏移，按真实时间排序才能在跨时区或回拨时保留最新结果。
        transaction.execute("DELETE FROM snapshots WHERE id NOT IN (SELECT id FROM snapshots ORDER BY julianday(finished_at) DESC, id DESC LIMIT 20)", []).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "snapshot_committed",
            serde_json::json!({"snapshot": diagnostics::correlate(&snapshot.id), "members": snapshot.members.len(), "filtered": snapshot.filtered_members.len(), "payload_bytes": payload.len()}),
        );
        diagnostic.finish("committed");
        Ok(())
    }

    /// 恢复上次结果，但不恢复其登录凭据或连接，页面必须标注历史来源和时间。
    pub fn latest_snapshot(&self) -> AppResult<Option<Snapshot>> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT payload FROM snapshots ORDER BY julianday(finished_at) DESC, id DESC LIMIT 1")
            .map_err(db_error)?;
        let mut rows = query.query([]).map_err(db_error)?;
        if let Some(row) = rows.next().map_err(db_error)? {
            let text: String = row.get(0).map_err(db_error)?;
            let snapshot = serde_json::from_str(&text).map_err(|_| {
                AppError::new(
                    "snapshot_decode",
                    "上次结果格式无法读取，请重新获取；原始数据库未被清空。",
                )
            })?;
            Ok(Some(snapshot))
        } else {
            Ok(None)
        }
    }

    /// 获取指定账号、群的最近一次已保存结果；无结果返回 None，不访问 QQ。
    ///
    /// 优先读取每群独立最新缓存；缺失时兼容读取最近 20 份历史，避免直接写入旧历史的测试/修复数据失效。
    /// 不依赖 SQLite JSON 扩展。不同账号的同名群或同群结果不可串用，不恢复任何登录凭据。
    pub fn latest_group_snapshot(
        &self,
        account_id: &str,
        source: &str,
        group_id: &str,
    ) -> AppResult<Option<Snapshot>> {
        let connection = self.connection()?;
        if let Some(snapshot) =
            crate::group_cache::read_latest_result(&connection, account_id, source, group_id)?
        {
            return Ok(Some(snapshot));
        }
        let mut query = connection.prepare(
            "SELECT payload FROM snapshots ORDER BY julianday(finished_at) DESC, id DESC LIMIT 20"
        ).map_err(db_error)?;
        let mut rows = query.query([]).map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            let payload: String = row.get(0).map_err(db_error)?;
            let snapshot: Snapshot = serde_json::from_str(&payload).map_err(|_| {
                AppError::new(
                    "snapshot_decode",
                    "已保存名单无法读取，请重新获取；原始数据库未被清空。",
                )
            })?;
            if snapshot.account.qq_number == account_id
                && snapshot.account.source == source
                && snapshot.group.id == group_id
            {
                return Ok(Some(snapshot));
            }
        }
        Ok(None)
    }

    /// 按固定标识读取历史或每群最新名单；只有两种保留范围都不存在才返回 None。
    /// 只读取本机名单，不恢复登录状态；调用者应要求用户重新确认范围，不能改用最新群猜测。
    pub fn snapshot_by_id(&self, id: &str) -> AppResult<Option<Snapshot>> {
        let payload: Option<String> = self
            .connection()?
            .query_row("SELECT payload FROM snapshots WHERE id=?1 UNION ALL SELECT payload FROM latest_group_results WHERE snapshot_id=?1 LIMIT 1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_error)?;
        payload
            .map(|text| {
                serde_json::from_str(&text).map_err(|_| {
                    AppError::new(
                        "snapshot_decode",
                        "本封邮件绑定的名单无法读取，请回到成员页重新获取并确认收件人。",
                    )
                })
            })
            .transpose()
    }

    /// 核对本封邮件绑定的群成员，防止页面旧选择或手动补充把已排除成员重新加回。
    /// qq_numbers 是用户在该快照最终名单中明确选定的数字 QQ，1—10000 个，重复项只计一次。
    /// recipients 必须是邮件解析器已验证的完整地址；全部选中成员的 QQ 邮箱必须存在，
    /// 而快照目标群中未选或已过滤成员的 QQ 邮箱不得出现。其他外部地址允许作为明确补充。
    /// 本方法只做本机校验，不发信、不修改快照或历史；失败时不能悄悄回退到无群来源发送。
    pub fn validate_group_mail(
        &self,
        snapshot_id: &str,
        qq_numbers: &[String],
        recipients: &[String],
    ) -> AppResult<GroupMailAudience> {
        if snapshot_id.is_empty()
            || snapshot_id.len() > 128
            || qq_numbers.is_empty()
            || qq_numbers.len() > crate::mail::MAX_RECIPIENTS
            || qq_numbers.iter().any(|qq| !is_numeric_qq(qq))
        {
            return Err(AppError::new(
                "group_mail_selection",
                "请从群成员最终名单中选择 1 到 10000 位有效成员。",
            ));
        }
        let snapshot = self.snapshot_by_id(snapshot_id)?.ok_or_else(|| {
            AppError::new(
                "group_mail_snapshot_missing",
                "本封邮件绑定的名单已不在本机保留范围，请回到成员页重新确认收件人。",
            )
        })?;
        // 邮件始终消费已保存的最终名单；过滤不完整或关系矛盾时不能绕过成员页继续发送。
        crate::verify::validate_friend_filter(&snapshot)?;
        let eligible: HashSet<&str> = snapshot
            .members
            .iter()
            .map(|member| member.qq_number.as_str())
            .collect();
        let selected: HashSet<&str> = qq_numbers.iter().map(String::as_str).collect();
        if selected.iter().any(|qq| !eligible.contains(qq)) {
            return Err(AppError::new(
                "group_mail_selection",
                "选中的成员不在本次最终名单中，可能已被过滤，请返回成员页重新选择。",
            ));
        }
        let recipients: HashSet<String> = recipients
            .iter()
            .map(|email| recipient_key(email))
            .collect();
        if selected
            .iter()
            .any(|qq| !recipients.contains(&format!("{qq}@qq.com")))
        {
            return Err(AppError::new(
                "group_mail_recipient_missing",
                "群成员选择与收件地址不一致，请回到成员页重新确认。",
            ));
        }
        let source_qq = snapshot
            .members
            .iter()
            .chain(snapshot.filtered_members.iter().map(|entry| &entry.member));
        if source_qq.into_iter().any(|member| {
            !selected.contains(member.qq_number.as_str())
                && recipients.contains(&format!("{}@qq.com", member.qq_number))
        }) {
            return Err(AppError::new("group_mail_excluded_recipient", "补充地址包含本群未选择或已过滤的成员，请在成员页调整选择，不能通过补充地址重新加入。"));
        }
        Ok(GroupMailAudience {
            snapshot_id: snapshot.id,
            group_id: snapshot.group.id,
            group_name: snapshot.group.name,
            member_count: selected.len(),
        })
    }

    /// 批量读取成员发信状态，使用全库历史而非界面最近 10000 条记录。
    /// 每批 400 位成员、800 个绑定参数，兼容 SQLite 参数上限；避免每人单独打开连接查询。
    /// 请求最多 10000 项且必须为数字 QQ；返回按首次输入顺序去重，空请求返回空集合。
    /// 分批查询共用一个只读事务，避免一次刷新各批次读取到不同的数据库提交点。
    /// sending/unknown 一律返回 unresolved，已有成功记录也不能盖住本次未决投递。
    pub fn member_mail_status(&self, qq_numbers: &[String]) -> AppResult<Vec<MemberMailStatus>> {
        if qq_numbers.len() > crate::mail::MAX_RECIPIENTS
            || qq_numbers.iter().any(|qq| !is_numeric_qq(qq))
        {
            return Err(AppError::new(
                "member_mail_status_input",
                "一次最多查询 10000 个有效数字 QQ 号。",
            ));
        }
        let mut seen = HashSet::new();
        let unique: Vec<&String> = qq_numbers
            .iter()
            .filter(|qq| seen.insert(qq.as_str()))
            .collect();
        if unique.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let mut statuses = HashMap::new();
        for chunk in unique.chunks(400) {
            let placeholders = std::iter::repeat_n("(?,?)", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            // Only fixed SQL syntax and generated placeholders are interpolated.
            // QQ values and derived mailboxes always use bound parameters.
            let sql = format!(
                "WITH requested(qq,email) AS (VALUES {placeholders})
                 SELECT r.qq,
                    CASE WHEN COUNT(p.id)>0 THEN 'unresolved'
                         WHEN h.id IS NOT NULL THEN 'sent' ELSE 'available' END,
                    CASE WHEN COUNT(p.id)=0 AND h.id IS NOT NULL THEN h.origin ELSE '' END
                 FROM requested r
                 LEFT JOIN sent_history h ON h.recipient_key=r.email
                 LEFT JOIN mail_delivery p ON p.recipient=r.email AND p.state IN ('sending','unknown')
                 GROUP BY r.qq,h.id,h.origin"
            );
            let parameters: Vec<String> = chunk
                .iter()
                .flat_map(|qq| [(*qq).clone(), format!("{qq}@qq.com")])
                .collect();
            let mut query = transaction.prepare(&sql).map_err(db_error)?;
            let rows = query
                .query_map(params_from_iter(parameters), |row| {
                    Ok(MemberMailStatus {
                        qq_number: row.get(0)?,
                        status: row.get(1)?,
                        origin: row.get(2)?,
                    })
                })
                .map_err(db_error)?;
            for row in rows {
                let status = row.map_err(db_error)?;
                statuses.insert(status.qq_number.clone(), status);
            }
        }
        transaction.commit().map_err(db_error)?;
        unique
            .into_iter()
            .map(|qq| {
                statuses.remove(qq).ok_or_else(|| {
                    AppError::new(
                        "member_mail_status_missing",
                        "成员发信状态读取不完整，请刷新后重试。",
                    )
                })
            })
            .collect()
    }

    /// 返回最近记录，当前版本界面最多载入 10000 条，total 仍为真实总量。
    pub fn sent_records(&self) -> AppResult<Vec<SentRecord>> {
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT id,qq_number,email,sent_at,origin,note,sender_address,sender_name FROM sent_history ORDER BY id DESC LIMIT 10000").map_err(db_error)?;
        let rows = query
            .query_map([], |row| {
                Ok(SentRecord {
                    id: row.get(0)?,
                    qq_number: row.get(1)?,
                    email: row.get(2)?,
                    sent_at: row.get(3)?,
                    origin: row.get(4)?,
                    note: row.get(5)?,
                    sender_address: row.get(6)?,
                    sender_name: row.get(7)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    /// 全库 QQ 集合用于名单过滤，不能只使用界面分页的历史记录。
    pub fn sent_qq_numbers(&self) -> AppResult<HashSet<String>> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT DISTINCT qq_number FROM sent_history WHERE qq_number<>''")
            .map_err(db_error)?;
        let rows = query.query_map([], |row| row.get(0)).map_err(db_error)?;
        rows.collect::<Result<HashSet<String>, _>>()
            .map_err(db_error)
    }

    pub fn sent_count(&self) -> AppResult<usize> {
        self.connection()?
            .query_row("SELECT COUNT(*) FROM sent_history", [], |row| row.get(0))
            .map_err(db_error)
    }

    /// 按邮箱判断已发送，QQ 数字地址的大小写域名会归一化。
    pub fn was_sent(&self, email: &str) -> AppResult<bool> {
        let key = recipient_key(email);
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sent_history WHERE recipient_key=?1)",
                [key],
                |row| row.get(0),
            )
            .map_err(db_error)
    }

    /// 检查全库是否有正在发送或结果不明的投递；即使关闭已发送过滤也必须阻止重发。
    /// 域名大小写归一化，本地部分保留大小写；这里只查询，不改变任何投递状态。
    pub fn has_unresolved(&self, email: &str) -> AppResult<bool> {
        self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM mail_delivery WHERE recipient=?1 AND state IN ('sending','unknown'))",
            [recipient_key(email)],
            |row| row.get(0),
        ).map_err(db_error)
    }

    /// 在同一个只读事务中准备本次收件人，确认弹窗和实际投递共用这份过滤结果。
    ///
    /// 输入必须是邮件解析器验证的完整地址，最多10000项；域名归一化并去重后保持输入顺序。
    /// 兼容旧调用方：每批400项查询全库状态，先跳过手动排除，再检查其余地址的未决投递。
    /// 返回实际待发地址和成功历史/手动排除的合计跳过人数；需要分类明细时使用扩展接口。
    pub fn prepare_mail_recipients(
        &self,
        recipients: &[String],
        skip_sent: bool,
    ) -> AppResult<(Vec<String>, usize)> {
        let prepared = self.prepare_mail_recipients_with_exclusions(recipients, skip_sent)?;
        Ok((
            prepared.recipients,
            prepared.skipped_sent + prepared.excluded.len(),
        ))
    }

    /// 同一数据库读快照先排除手动地址，再检查未决和可选成功历史；不把排除记录记成已发送。
    /// excluded 为规范地址，用于确认明细；exclusion_revision 由宿主绑定确认令牌并在启动复核。
    pub fn prepare_mail_recipients_with_exclusions(
        &self,
        recipients: &[String],
        skip_sent: bool,
    ) -> AppResult<PreparedMailRecipients> {
        let mut diagnostic = diagnostics::operation("database", "prepare_recipients");
        if recipients.is_empty() || recipients.len() > crate::mail::MAX_RECIPIENTS {
            return Err(AppError::new("mail_recipients", "请选择1到10000位收件人。"));
        }
        let mut seen = HashSet::new();
        let addresses: Vec<String> = recipients
            .iter()
            .map(|email| recipient_key(email))
            .filter(|email| seen.insert(email.clone()))
            .collect();
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let exclusion_revision = crate::manual_exclusions::read_revision(&transaction)?;
        let mut states = HashMap::new();
        for chunk in addresses.chunks(400) {
            let values = (1..=chunk.len())
                .map(|index| format!("(?{index})"))
                .collect::<Vec<_>>()
                .join(",");
            // VALUES 只拼接程序生成的占位符；邮箱始终经参数绑定，不参与SQL文本构造。
            let sql = format!("WITH requested(email) AS (VALUES {values})
                SELECT r.email,
                    EXISTS(SELECT 1 FROM sent_history h WHERE h.recipient_key=r.email),
                    EXISTS(SELECT 1 FROM mail_delivery d WHERE d.recipient=r.email AND d.state IN ('sending','unknown')),
                    EXISTS(SELECT 1 FROM manual_exclusions m WHERE m.recipient_key=r.email)
                FROM requested r");
            let mut query = transaction.prepare(&sql).map_err(db_error)?;
            let rows = query
                .query_map(params_from_iter(chunk), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })
                .map_err(db_error)?;
            for row in rows {
                let (email, sent, unresolved, excluded) = row.map_err(db_error)?;
                states.insert(email, (sent, unresolved, excluded));
            }
        }
        transaction.commit().map_err(db_error)?;
        if states
            .values()
            .any(|(_, unresolved, excluded)| *unresolved && !excluded)
        {
            diagnostics::event(
                "warn",
                "database",
                "preparation_blocked",
                serde_json::json!({"candidates": addresses.len(), "reason": "unresolved_delivery"}),
            );
            return Err(AppError::new(
                "delivery_unresolved",
                "收件人中存在上次投递结果不明的记录，请在本机历史中先核实，再开始发送。",
            ));
        }
        let original_count = addresses.len();
        let excluded = addresses
            .iter()
            .filter(|email| states.get(*email).is_some_and(|(_, _, excluded)| *excluded))
            .cloned()
            .collect::<Vec<_>>();
        let eligible = addresses
            .into_iter()
            .filter(|email| !states.get(email).is_some_and(|(_, _, excluded)| *excluded))
            .filter(|email| !skip_sent || !states.get(email).is_some_and(|(sent, _, _)| *sent))
            .collect::<Vec<_>>();
        let skipped = original_count - eligible.len() - excluded.len();
        diagnostics::event(
            "info",
            "database",
            "recipients_prepared",
            serde_json::json!({"candidates": original_count, "actual": eligible.len(), "skipped": skipped, "skip_sent": skip_sent, "query_chunks": original_count.div_ceil(400)}),
        );
        diagnostic.finish("success");
        Ok(PreparedMailRecipients {
            recipients: eligible,
            skipped_sent: skipped,
            excluded,
            exclusion_revision,
        })
    }

    /// 返回全部可人工核实的记录；不分页遗漏未决收件人，不暴露正在发送的可变状态。
    pub fn unresolved_deliveries(&self) -> AppResult<Vec<UnresolvedDelivery>> {
        let connection = self.connection()?;
        let mut query = connection.prepare(
            "SELECT id,recipient,updated_at,message,sender_address,sender_name FROM mail_delivery WHERE state='unknown' ORDER BY id DESC"
        ).map_err(db_error)?;
        let rows = query
            .query_map([], |row| {
                Ok(UnresolvedDelivery {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    updated_at: row.get(2)?,
                    message: row.get(3)?,
                    sender_address: row.get(4)?,
                    sender_name: row.get(5)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    /// 根据用户实际核实结果处理一条 unknown，不允许改判发送中、已解决或不存在的记录。
    /// accepted=true 只代表人工确认已发送，来源记为 manual_verified，不伪造 SMTP 证明。
    /// accepted=false 标为 not_sent；不删除其他已发送历史，防止误清除以前的成功投递。
    /// 读取、状态转换及成功记录共用写事务，重复点击或其他连接竞争不能产生半完成状态。
    pub fn resolve_delivery(&self, id: i64, accepted: bool) -> AppResult<()> {
        // 旧单条入口将非正数视为不存在，保留既有调用方识别的错误码。
        if id <= 0 {
            return Err(AppError::new(
                "delivery_missing",
                "这条投递记录已不存在，请刷新记录。",
            ));
        }
        self.resolve_deliveries(&[id], accepted).map(|_| ())
    }

    /// 按用户核实结果原子处理一批 unknown；输入长度须为 1—10000，正整数标识去重后只处理一次。
    ///
    /// 立即写事务先核对全部记录存在且仍为 unknown，任一不存在、已核实或发送中会拒绝整批。
    /// accepted=true 写 manual_verified 历史；false 只解除本次未决，不删除以前成功历史。
    /// 全部状态、历史同事务提交，验证或中途写入失败均回滚；返回实际处理的去重记录数，不发邮件。
    pub fn resolve_deliveries(&self, ids: &[i64], accepted: bool) -> AppResult<usize> {
        let mut diagnostic = diagnostics::operation("database", "resolve_deliveries");
        diagnostics::event(
            "info",
            "database",
            "resolution_requested",
            serde_json::json!({"input_count": ids.len(), "accepted": accepted}),
        );
        if ids.is_empty()
            || ids.len() > crate::mail::MAX_RECIPIENTS
            || ids.iter().any(|id| *id <= 0)
        {
            return Err(AppError::new(
                "delivery_selection",
                "请选择 1 到 10000 条有效待核实记录。",
            ));
        }
        let mut seen = HashSet::new();
        let ids: Vec<i64> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let mut records = Vec::with_capacity(ids.len());
        {
            // 复用绑定语句而不拼接 ID 列表；写锁贯穿核验与提交，竞争者只能看到完整前后状态。
            let mut query = transaction
                .prepare("SELECT recipient,state,sender_address,sender_name FROM mail_delivery WHERE id=?1")
                .map_err(db_error)?;
            for id in &ids {
                let record: Option<(String, String, String, String)> = query
                    .query_row([id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .optional()
                    .map_err(db_error)?;
                let Some((recipient, state, sender_address, sender_name)) = record else {
                    return Err(AppError::new(
                        "delivery_missing",
                        "所选投递记录已不存在，请刷新记录后重新选择；本批未修改。",
                    ));
                };
                if state != "unknown" {
                    return Err(AppError::new(
                        "delivery_not_resolvable",
                        "只能处理结果不明的投递，所选记录包含发送中或已核实项；本批未修改。",
                    ));
                }
                let verified = if accepted {
                    Some(normalize_recipient(&recipient).ok_or_else(|| {
                        AppError::new(
                            "delivery_recipient",
                            "所选投递包含无效收件地址，无法记录人工核实结果；本批未修改。",
                        )
                    })?)
                } else {
                    None
                };
                records.push((*id, verified, sender_address, sender_name));
            }
        }
        let now = Local::now().to_rfc3339();
        for (id, verified, sender_address, sender_name) in records {
            let updated = transaction.execute(
            "UPDATE mail_delivery SET state=?1,updated_at=?2,message=?3 WHERE id=?4 AND state='unknown'",
            params![if accepted { "manual_verified" } else { "not_sent" },now,
                if accepted { "用户人工核实已发送" } else { "用户人工核实未发送，允许重新发送" },id],
        ).map_err(db_error)?;
            if updated != 1 {
                return Err(AppError::new(
                    "delivery_not_resolvable",
                    "投递状态已变化，请刷新记录后重试；本批未修改。",
                ));
            }
            if let Some((qq, email)) = verified {
                transaction.execute(
                "INSERT INTO sent_history(recipient_key,qq_number,email,sent_at,origin,note,sender_address,sender_name)
                 VALUES(?1,?2,?3,?4,'manual_verified','用户人工核实已发送，不代表程序收到 SMTP 接受证明',?5,?6)
                 ON CONFLICT(recipient_key) DO UPDATE SET sent_at=excluded.sent_at,origin=excluded.origin,note=excluded.note,sender_address=excluded.sender_address,sender_name=excluded.sender_name",
                params![recipient_key(&email),qq,email,now,sender_address,sender_name],
            ).map_err(db_error)?;
            }
        }
        transaction.commit().map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "resolution_committed",
            serde_json::json!({"count": ids.len(), "accepted": accepted}),
        );
        diagnostic.finish("committed");
        Ok(ids.len())
    }

    /// 导入一行一个 QQ/邮箱，或带表头的 CSV；不从非 QQ 邮箱猜测 QQ 号。
    pub fn import_sent(&self, text: &str, origin: &str) -> AppResult<ImportOutcome> {
        let mut diagnostic = diagnostics::operation("database", "import_sent");
        if text.len() > 4 * 1024 * 1024 {
            return Err(AppError::new(
                "import_too_large",
                "导入文本不能超过 4 MiB，请分批导入。",
            ));
        }
        let text = text.trim_start_matches('\u{feff}');
        let mut outcome = ImportOutcome::default();
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes());
        for record in reader.records() {
            let record = match record {
                Ok(value) => value,
                Err(_) => {
                    outcome.invalid += 1;
                    continue;
                }
            };
            if record.iter().all(|field| field.trim().is_empty()) {
                continue;
            }
            if record.iter().any(|field| {
                matches!(
                    field.trim(),
                    "QQ号" | "QQ 号" | "qq_number" | "qqNumber" | "邮箱" | "email"
                )
            }) {
                continue;
            }
            let candidate = record
                .iter()
                .find_map(|field| normalize_recipient(field.trim()));
            let Some((qq_number, email)) = candidate else {
                outcome.invalid += 1;
                continue;
            };
            let changed = transaction.execute(
                "INSERT OR IGNORE INTO sent_history(recipient_key,qq_number,email,sent_at,origin,note) VALUES(?1,?2,?3,?4,?5,'用户明确标记为已发送')",
                params![recipient_key(&email), qq_number, email, Local::now().to_rfc3339(), origin]
            ).map_err(db_error)?;
            if changed == 0 {
                outcome.skipped += 1;
            } else {
                outcome.added += 1;
            }
        }
        transaction.commit().map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "history_import_committed",
            serde_json::json!({"added": outcome.added, "skipped": outcome.skipped, "invalid": outcome.invalid}),
        );
        diagnostic.finish("committed");
        Ok(outcome)
    }

    /// 删除一条人工或实际记录；删除后该收件人会重新进入可发送名单。
    pub fn remove_sent(&self, id: i64) -> AppResult<()> {
        self.connection()?
            .execute("DELETE FROM sent_history WHERE id=?1", [id])
            .map_err(db_error)?;
        Ok(())
    }

    /// 保存无凭据设置；宿主只传公开 SMTP 字段，不允许传入密码。
    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.connection()?
            .execute(
                "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,?2)",
                params![key, value],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> AppResult<Option<String>> {
        self.connection()?
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_error)
    }

    /// 在发往 SMTP 前为整批收件人原子记录 sending；任一地址被手动排除或未决则整批拒绝。
    /// IMMEDIATE 在检查前取得写锁，避免两条连接均查无未决后同时插入并重复投递。
    /// 已解决记录不会阻止用户明确发起的新投递；是否跳过成功历史由上层选项控制。
    pub fn begin_delivery(&self, job: &str, recipients: &[String]) -> AppResult<()> {
        self.begin_delivery_with_sender(job, recipients, "", "")
    }

    /// 在整批sending记录中冻结经宿主验证的发件邮箱及账号名称，不能由页面单独声明历史归属。
    /// sender_address须为合法裸邮箱（只规范域名大小写），sender_name最多256字且无控制字符。
    /// 两者同时为空仅供旧入口兼容，邮箱为空时名称也必须为空；不从当前目录回填未知旧数据。
    /// 账号改名/删除不会改历史；后续accepted和人工核实已发送都从本记录复制原始归属。
    pub fn begin_delivery_with_sender(
        &self,
        job: &str,
        recipients: &[String],
        sender_address: &str,
        sender_name: &str,
    ) -> AppResult<()> {
        self.begin_delivery_with_sender_unless_cancelled(
            job,
            recipients,
            sender_address,
            sender_name,
            || false,
        )
        .map(|_| ())
    }

    /// 在整批登记事务中检查取消，避免等待SQLite写锁期间停止的批次在解锁后仍发往SMTP。
    /// 输入、发件归属校验和共享排除规则与`begin_delivery_with_sender`一致；不修改凭据或成功历史。
    /// `is_cancelled`须为快速、无数据库操作的只读检查：拿到IMMEDIATE写锁后及commit前各调用一次，
    /// 闭包不得阻塞、重入数据库或产生副作用；调用方可读取原子标记，不需要依赖特定异步运行时。
    /// 返回true表示整批sending已提交，调用方方可进入SMTP；false表示观察到取消并回滚整个事务，
    /// 不留下sending/unknown或部分批次。提交后的取消由宿主的进行中SMTP结果处理规则负责。
    /// 非法发件归属、手动排除、已有未决投递和SQL错误沿用原错误码，均不能据此启动SMTP。
    pub fn begin_delivery_with_sender_unless_cancelled(
        &self,
        job: &str,
        recipients: &[String],
        sender_address: &str,
        sender_name: &str,
        is_cancelled: impl Fn() -> bool,
    ) -> AppResult<bool> {
        let (sender_address, sender_name) = validate_delivery_sender(sender_address, sender_name)?;
        let mut diagnostic = diagnostics::operation("database", "begin_delivery");
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        // 取得写锁可能已等待busy_timeout；先复查取消，不为已经停止的批次建立待核实记录。
        if is_cancelled() {
            diagnostic.finish("cancelled");
            return Ok(false);
        }
        let now = Local::now().to_rfc3339();
        // 手工/快捷/旧调用同样不能绕过全局排除；整批先检查，任何命中都不产生 sending/unknown。
        for recipient in recipients {
            let excluded: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM manual_exclusions WHERE recipient_key=?1)",
                    [recipient_key(recipient)],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if excluded {
                return Err(AppError::new(
                    "manual_excluded",
                    "收件人已加入手动排除，请重新确认发送名单。",
                ));
            }
        }
        for recipient in recipients {
            let email = recipient_key(recipient);
            let unresolved: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM mail_delivery WHERE recipient=?1 AND state IN ('sending','unknown'))",
                [&email], |row| row.get(0),
            ).map_err(db_error)?;
            if unresolved {
                return Err(AppError::new(
                    "delivery_unresolved",
                    "收件人存在正在发送或结果不明的投递，请先在已发送记录中核实，避免重复发送。",
                ));
            }
            transaction.execute("INSERT INTO mail_delivery(job_id,recipient,state,updated_at,sender_address,sender_name) VALUES(?1,?2,'sending',?3,?4,?5)", params![job,email,now,sender_address,sender_name]).map_err(db_error)?;
        }
        // 规则检查和整批插入期间也可能收到取消；提交前最后检查，drop事务会回滚已插入的每一行。
        if is_cancelled() {
            diagnostic.finish("cancelled");
            return Ok(false);
        }
        transaction.commit().map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "delivery_reserved",
            serde_json::json!({"job": diagnostics::correlate(job), "count": recipients.len()}),
        );
        diagnostic.finish("committed");
        Ok(true)
    }

    /// 只有 SMTP 明确接受才写 sent_history；批次投递状态、原发件归属和成功记录同事务提交。
    /// 同收件人再次成功更新最近一次归属，不影响全局收件人去重；失败仅保留本次未决归属。
    pub fn finish_delivery(
        &self,
        job: &str,
        recipients: &[String],
        accepted: bool,
        message: &str,
    ) -> AppResult<()> {
        let mut diagnostic = diagnostics::operation("database", "finish_delivery");
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let now = Local::now().to_rfc3339();
        for recipient in recipients {
            let recipient = recipient_key(recipient);
            let sender: Option<(String,String)> = transaction.query_row(
                "SELECT sender_address,sender_name FROM mail_delivery WHERE job_id=?1 AND recipient=?2 AND state='sending'",
                params![job,recipient], |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional().map_err(db_error)?;
            let updated = transaction.execute("UPDATE mail_delivery SET state=?1,updated_at=?2,message=?3 WHERE job_id=?4 AND recipient=?5 AND state='sending'", params![if accepted {"accepted"} else {"unknown"},now,message,job,recipient]).map_err(db_error)?;
            if updated != 1 {
                return Err(AppError::new(
                    "delivery_state",
                    "投递状态已变化，未写入成功记录，请先核实本批实际结果。",
                ));
            }
            if accepted {
                let (sender_address, sender_name) = sender.ok_or_else(|| {
                    AppError::new(
                        "delivery_state",
                        "投递归属无法确认，未写入成功记录，请先核实本批实际结果。",
                    )
                })?;
                let qq = normalize_recipient(&recipient)
                    .map(|pair| pair.0)
                    .unwrap_or_default();
                transaction.execute(
                    "INSERT INTO sent_history(recipient_key,qq_number,email,sent_at,origin,note,sender_address,sender_name) VALUES(?1,?2,?3,?4,'smtp_accepted','邮件服务器已接受，不等于收件箱送达',?5,?6) ON CONFLICT(recipient_key) DO UPDATE SET sent_at=excluded.sent_at,origin=excluded.origin,note=excluded.note,sender_address=excluded.sender_address,sender_name=excluded.sender_name",
                    params![recipient_key(&recipient),qq,recipient,now,sender_address,sender_name]
                ).map_err(db_error)?;
            }
        }
        transaction.commit().map_err(db_error)?;
        diagnostics::event(
            "info",
            "database",
            "delivery_result_committed",
            serde_json::json!({"job": diagnostics::correlate(job), "count": recipients.len(), "accepted": accepted}),
        );
        diagnostic.finish("committed");
        Ok(())
    }
}

/// 归属只保存宿主已验证的公开快照；统一拒绝控制字符及超长输入，防止日志或界面混入伪造行。
/// 空邮箱+空名称用于旧记录兼容，合法邮箱可没有显示名称；邮箱本地部分保持真实发送时的大小写。
fn validate_delivery_sender(address: &str, name: &str) -> AppResult<(String, String)> {
    let error = || {
        AppError::new(
            "delivery_sender",
            "发件账号归属无效，本批未登记，请检查账号配置后重试。",
        )
    };
    if address.chars().any(char::is_control)
        || name.chars().any(char::is_control)
        || address.len() > 512
        || name.chars().count() > 256
    {
        return Err(error());
    }
    let address = address.trim();
    let name = name.trim();
    if address.is_empty() {
        return if name.is_empty() {
            Ok((String::new(), String::new()))
        } else {
            Err(error())
        };
    }
    let parsed: lettre::Address = address.parse().map_err(|_| error())?;
    Ok((
        format!("{}@{}", parsed.user(), parsed.domain().to_ascii_lowercase()),
        name.into(),
    ))
}

/// QQ 号使用非零开头的纯数字字符串，不能把邮箱、UID 或显示名称当成成员标识。
fn is_numeric_qq(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn recipient_key(email: &str) -> String {
    if let Some((local, domain)) = email.rsplit_once('@') {
        format!("{}@{}", local, domain.to_ascii_lowercase())
    } else {
        email.into()
    }
}

pub(crate) fn normalize_recipient(input: &str) -> Option<(String, String)> {
    let input = input.trim().trim_matches('"');
    let numeric = |value: &str| {
        !value.is_empty()
            && value.len() <= 20
            && value.bytes().all(|c| c.is_ascii_digit())
            && !value.starts_with('0')
    };
    if numeric(input) {
        return Some((input.into(), format!("{input}@qq.com")));
    }
    let address: lettre::Address = input.parse().ok()?;
    let email = address.to_string();
    let (local, domain) = email.rsplit_once('@')?;
    let qq = if domain.eq_ignore_ascii_case("qq.com") && numeric(local) {
        local.to_string()
    } else {
        String::new()
    };
    Some((qq, recipient_key(&email)))
}

#[cfg(test)]
mod sender_provenance_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;

    fn fixture() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("provenance.sqlite3")).unwrap();
        (directory, database)
    }

    fn remove_provenance_columns(database: &Database) {
        database
            .connection()
            .unwrap()
            .execute_batch(
                "ALTER TABLE sent_history DROP COLUMN sender_address;
             ALTER TABLE sent_history DROP COLUMN sender_name;
             ALTER TABLE mail_delivery DROP COLUMN sender_address;
             ALTER TABLE mail_delivery DROP COLUMN sender_name;
             PRAGMA user_version=4;",
            )
            .unwrap();
    }

    /// 真实SQLite写锁阻塞登记时取消；解锁后必须回滚且不产生sending、unknown或成功记录。
    /// 两个线程只操作临时库与原子标记；有界等待确认未提前返回，不连接任何SMTP服务器。
    #[test]
    fn cancellable_registration_observes_cancellation_during_sqlite_write_lock_wait() {
        let (_directory, database) = fixture();
        let mut blocker = database.connection().unwrap();
        let write_lock = blocker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let checks = Arc::new(AtomicUsize::new(0));
        let worker_database = database.clone();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_checks = Arc::clone(&checks);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = worker_database.begin_delivery_with_sender_unless_cancelled(
                "cancelled-while-locked",
                &["recipient@example.test".into()],
                "sender@example.test",
                "合成账号",
                || {
                    worker_checks.fetch_add(1, Ordering::SeqCst);
                    worker_cancelled.load(Ordering::SeqCst)
                },
            );
            finished_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(checks.load(Ordering::SeqCst), 0);
        cancelled.store(true, Ordering::SeqCst);
        write_lock.commit().unwrap();
        assert!(!finished_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap());
        worker.join().unwrap();
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        let count: i64 = database
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mail_delivery", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 第二次检查模拟插入整批后、commit前刚收到取消；已插入行须整笔回滚且不改此前成功历史。
    /// 检查计数仅为测试注入确定的时序，生产闭包只读取宿主取消标记。
    #[test]
    fn cancellable_registration_rolls_back_whole_batch_when_cancelled_before_commit() {
        let (_directory, database) = fixture();
        database
            .import_sent("preserved@example.test", "manual")
            .unwrap();
        let checks = AtomicUsize::new(0);
        let reserved = database
            .begin_delivery_with_sender_unless_cancelled(
                "cancelled-before-commit",
                &["first@example.test".into(), "second@example.test".into()],
                "sender@example.test",
                "合成账号",
                || checks.fetch_add(1, Ordering::SeqCst) > 0,
            )
            .unwrap();
        assert!(!reserved);
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        let count: i64 = database
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mail_delivery", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        let records = database.sent_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].email, "preserved@example.test");
    }

    /// 未取消时两次检查都通过，归属与原兼容入口一致；成功提交后才可调用SMTP并落成功历史。
    #[test]
    fn cancellable_registration_commits_sender_snapshot_when_not_cancelled() {
        let (_directory, database) = fixture();
        let checks = AtomicUsize::new(0);
        let recipients = vec!["recipient@example.test".into()];
        assert!(database
            .begin_delivery_with_sender_unless_cancelled(
                "not-cancelled",
                &recipients,
                " Sender@EXAMPLE.test ",
                " 合成账号 ",
                || {
                    checks.fetch_add(1, Ordering::SeqCst);
                    false
                },
            )
            .unwrap());
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        let detail: (String, String, String) = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT state,sender_address,sender_name FROM mail_delivery WHERE job_id='not-cancelled'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            detail,
            (
                "sending".into(),
                "Sender@example.test".into(),
                "合成账号".into()
            )
        );
        database
            .finish_delivery("not-cancelled", &recipients, true, "")
            .unwrap();
        let records = database.sent_records().unwrap();
        assert_eq!(records[0].sender_address, "Sender@example.test");
        assert_eq!(records[0].sender_name, "合成账号");
        assert!(database.unresolved_deliveries().unwrap().is_empty());
    }

    /// v4历史和待核实没有可靠发件信息，迁移必须保持空值；v5重开保留内容而不根据当前目录猜测。
    #[test]
    fn schema_four_migration_keeps_legacy_sender_unknown_and_five_reopens() {
        let (directory, database) = fixture();
        remove_provenance_columns(&database);
        database.connection().unwrap().execute_batch(
            "INSERT INTO sent_history(recipient_key,qq_number,email,sent_at,origin,note) VALUES('legacy@example.test','','legacy@example.test','2026-11-09T00:00:00Z','smtp_accepted','synthetic');
             INSERT INTO mail_delivery(job_id,recipient,state,updated_at,message) VALUES('legacy-job','unknown@example.test','unknown','2026-11-09T00:00:00Z','synthetic');"
        ).unwrap();
        drop(database);
        let upgraded = Database::open(directory.path().join("provenance.sqlite3")).unwrap();
        let records = upgraded.sent_records().unwrap();
        assert_eq!(
            (
                records[0].sender_address.as_str(),
                records[0].sender_name.as_str()
            ),
            ("", "")
        );
        let unresolved = upgraded.unresolved_deliveries().unwrap();
        assert_eq!(
            (
                unresolved[0].sender_address.as_str(),
                unresolved[0].sender_name.as_str()
            ),
            ("", "")
        );
        let serialized = serde_json::to_value(&records[0]).unwrap();
        assert_eq!(serialized["senderAddress"], "");
        assert_eq!(serialized["senderName"], "");
        assert!(serialized.get("sender_address").is_none());
        assert_eq!(
            upgraded
                .connection()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        drop(upgraded);
        let reopened = Database::open(directory.path().join("provenance.sqlite3")).unwrap();
        assert_eq!(
            reopened.sent_records().unwrap()[0].email,
            "legacy@example.test"
        );
        assert_eq!(
            reopened.unresolved_deliveries().unwrap()[0].email,
            "unknown@example.test"
        );
    }

    /// 第四个ALTER失败时前三个新增列和版本必须回滚，保留原始v4结构与数据以供修复/备份恢复。
    #[test]
    fn schema_five_failure_rolls_back_all_new_columns_and_version() {
        let (directory, database) = fixture();
        remove_provenance_columns(&database);
        database.connection().unwrap().execute_batch("ALTER TABLE mail_delivery ADD COLUMN sender_name TEXT NOT NULL DEFAULT ''; INSERT INTO settings VALUES('preserved','yes');").unwrap();
        drop(database);
        assert!(Database::open(directory.path().join("provenance.sqlite3")).is_err());
        let connection = Connection::open(directory.path().join("provenance.sqlite3")).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
        let columns = |table: &str| {
            let mut query = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            query
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(!columns("sent_history")
            .iter()
            .any(|column| column.starts_with("sender_")));
        assert!(!columns("mail_delivery")
            .iter()
            .any(|column| column == "sender_address"));
        assert!(columns("mail_delivery")
            .iter()
            .any(|column| column == "sender_name"));
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key='preserved'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "yes"
        );
    }

    /// 归属在尝试开始时冻结；旧目录改名/删除不影响，重发成功只更新同一全局去重行的最近归属。
    #[test]
    fn accepted_delivery_freezes_sender_and_latest_success_replaces_only_provenance() {
        let (_directory, database) = fixture();
        let recipients = vec!["recipient@example.test".into()];
        database
            .begin_delivery_with_sender("first", &recipients, " Sender@EXAMPLE.test ", " 原账号 ")
            .unwrap();
        database
            .set_setting(
                "smtp_sender_accounts_v1",
                "synthetic renamed or deleted directory",
            )
            .unwrap();
        database
            .finish_delivery("first", &recipients, true, "")
            .unwrap();
        let first = database.sent_records().unwrap();
        assert_eq!(
            (
                first[0].sender_address.as_str(),
                first[0].sender_name.as_str()
            ),
            ("Sender@example.test", "原账号")
        );
        let id = first[0].id;
        database
            .begin_delivery_with_sender("second", &recipients, "other@example.test", "另一个账号")
            .unwrap();
        database
            .finish_delivery("second", &recipients, true, "")
            .unwrap();
        let second = database.sent_records().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, id);
        assert_eq!(
            (
                second[0].sender_address.as_str(),
                second[0].sender_name.as_str()
            ),
            ("other@example.test", "另一个账号")
        );
        assert!(database
            .prepare_mail_recipients_with_exclusions(&recipients, true)
            .unwrap()
            .recipients
            .is_empty());
    }

    /// unknown展示其尝试账号；人工确认已发送复制原快照，未发送只解除该尝试且保留以前成功归属。
    #[test]
    fn unknown_resolution_copies_original_sender_only_for_confirmed_sent() {
        let (_directory, database) = fixture();
        let recipient = vec!["recipient@example.test".into()];
        database
            .begin_delivery_with_sender("unknown-one", &recipient, "first@example.test", "原始名称")
            .unwrap();
        database
            .finish_delivery("unknown-one", &recipient, false, "合成待核实")
            .unwrap();
        let unresolved = database.unresolved_deliveries().unwrap();
        assert_eq!(
            (
                unresolved[0].sender_address.as_str(),
                unresolved[0].sender_name.as_str()
            ),
            ("first@example.test", "原始名称")
        );
        let serialized = serde_json::to_value(&unresolved[0]).unwrap();
        assert_eq!(serialized["senderAddress"], "first@example.test");
        database.resolve_delivery(unresolved[0].id, true).unwrap();
        let saved = database.sent_records().unwrap();
        assert_eq!(saved[0].origin, "manual_verified");
        assert_eq!(
            (
                saved[0].sender_address.as_str(),
                saved[0].sender_name.as_str()
            ),
            ("first@example.test", "原始名称")
        );
        database
            .begin_delivery_with_sender(
                "unknown-two",
                &recipient,
                "second@example.test",
                "第二账号",
            )
            .unwrap();
        database
            .finish_delivery("unknown-two", &recipient, false, "合成待核实")
            .unwrap();
        database
            .resolve_delivery(database.unresolved_deliveries().unwrap()[0].id, false)
            .unwrap();
        assert_eq!(
            database.sent_records().unwrap()[0].sender_address,
            "first@example.test"
        );
        let detail: (String, String) = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT sender_address,sender_name FROM mail_delivery WHERE job_id='unknown-two'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(detail, ("second@example.test".into(), "第二账号".into()));
    }

    /// 归属批次遵守共享排除且与sending/accepted事务一并提交，任一行失败不得残留半批或错误归属。
    #[test]
    fn provenance_batches_rollback_reservation_finish_and_manual_resolution() {
        let (_directory, database) = fixture();
        let recipients: Vec<String> =
            vec!["first@example.test".into(), "second@example.test".into()];
        database
            .add_manual_exclusions(&recipients[1], "合成排除")
            .unwrap();
        assert_eq!(
            database
                .begin_delivery_with_sender("excluded", &recipients, "sender@example.test", "账号")
                .unwrap_err()
                .code,
            "manual_excluded"
        );
        assert!(!database.has_unresolved(&recipients[0]).unwrap());
        let exclusion_id = database.manual_exclusions().unwrap().records[0].id;
        database.remove_manual_exclusions(&[exclusion_id]).unwrap();
        database
            .begin_delivery_with_sender("atomic", &recipients, "sender@example.test", "账号")
            .unwrap();
        database.connection().unwrap().execute_batch("CREATE TRIGGER reject_sender_history BEFORE INSERT ON sent_history WHEN NEW.email='second@example.test' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert!(database
            .finish_delivery("atomic", &recipients, true, "")
            .is_err());
        assert_eq!(database.sent_count().unwrap(), 0);
        let count:i64=database.connection().unwrap().query_row("SELECT COUNT(*) FROM mail_delivery WHERE state='sending' AND sender_address='sender@example.test'",[],|row|row.get(0)).unwrap();
        assert_eq!(count, 2);
        database
            .finish_delivery("atomic", &recipients, false, "合成待核实")
            .unwrap();
        let ids = database
            .unresolved_deliveries()
            .unwrap()
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        assert!(database.resolve_deliveries(&ids, true).is_err());
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 2);
        assert_eq!(database.sent_count().unwrap(), 0);
        database
            .connection()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_sender_history;")
            .unwrap();
        database.resolve_deliveries(&ids, true).unwrap();
        assert!(database
            .sent_records()
            .unwrap()
            .iter()
            .all(|record| record.sender_address == "sender@example.test"
                && record.sender_name == "账号"));
    }

    /// 无归属的旧接口、导入和独立人工标记保持空值；非法归属在写入前拒绝，不创建任何投递行。
    #[test]
    fn legacy_manual_import_remain_unknown_and_invalid_sender_never_reserves() {
        let (_directory, database) = fixture();
        database
            .import_sent("manual@example.test", "manual")
            .unwrap();
        database
            .import_sent("imported@example.test", "imported")
            .unwrap();
        let legacy = vec!["legacy@example.test".into()];
        database.begin_delivery("legacy", &legacy).unwrap();
        database
            .finish_delivery("legacy", &legacy, true, "")
            .unwrap();
        assert!(database
            .sent_records()
            .unwrap()
            .iter()
            .all(|record| record.sender_address.is_empty() && record.sender_name.is_empty()));
        for (address, name) in [
            ("", "name"),
            ("bad-address", "name"),
            ("sender@example.test\n", "name"),
            ("sender@example.test", "bad\nname"),
        ] {
            assert_eq!(
                database
                    .begin_delivery_with_sender(
                        "invalid",
                        &["new@example.test".into()],
                        address,
                        name
                    )
                    .unwrap_err()
                    .code,
                "delivery_sender"
            );
        }
        assert!(!database.has_unresolved("new@example.test").unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Account, FilteredMember, FriendRelation, Group, Member};

    /// 合成名单中 100101/100102 为最终成员，100103 已过滤；不使用真实账号或生产数据。
    fn group_mail_snapshot(id: &str, group_id: &str, finished_at: &str) -> Snapshot {
        let member = |qq: &str| Member {
            qq_number: qq.into(),
            nickname: "模拟成员".into(),
            card: String::new(),
            friend_relation: FriendRelation::Unknown,
        };
        Snapshot {
            id: id.into(),
            account: Account {
                qq_number: "100100".into(),
                nickname: "模拟账号".into(),
                source: "web".into(),
            },
            group: Group {
                id: group_id.into(),
                name: format!("模拟群 {group_id}"),
                member_count: Some(3),
                role: "member".into(),
            },
            started_at: finished_at.into(),
            finished_at: finished_at.into(),
            members: vec![member("100101"), member("100102")],
            source_count: 3,
            source_groups: vec![],
            excluded_count: 1,
            exclusions: vec![],
            exclude_friends: false,
            friend_filtered_count: 0,
            friend_unknown_count: 3,
            friend_filter_status: "not_requested".into(),
            sent_filtered_count: 0,
            manual_filtered_count: 0,
            exclusion_revision: 0,
            restoration: None,
            filtered_members: vec![FilteredMember {
                member: member("100103"),
                reason: "group".into(),
                detail: "模拟排除群".into(),
            }],
            exclude_sent: true,
            reported_count: Some(3),
            status: "matched".into(),
            message: String::new(),
            duplicate_count: 0,
            invalid_count: 0,
            warnings: vec![],
        }
    }

    /// 未知历史来源字符串原样往返，历史名单读取不恢复任何登录状态。
    #[test]
    fn historical_snapshot_source_remains_readable() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let mut snapshot = group_mail_snapshot("history", "900001", "2026-11-05T10:00:00+00:00");
        snapshot.account.source = "historical".into();
        database.save_snapshot(&snapshot).unwrap();
        let restored = database.snapshot_by_id("history").unwrap().unwrap();
        assert_eq!(restored.account.source, "historical");
        assert_eq!(restored.members.len(), snapshot.members.len());
        assert_eq!(
            restored.filtered_members.len(),
            snapshot.filtered_members.len()
        );
        assert_eq!(database.latest_snapshot().unwrap().unwrap().id, "history");
    }

    /// 新字段保存在已有 JSON 内；旧快照仍可读取，且不会自动开启好友或手动排除。
    #[test]
    fn legacy_snapshot_defaults_friend_metadata_without_database_migration() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = group_mail_snapshot("legacy", "900001", "2026-11-05T10:00:00+00:00");
        let mut payload = serde_json::to_value(&original).unwrap();
        let object = payload.as_object_mut().unwrap();
        for key in [
            "excludeFriends",
            "friendFilteredCount",
            "friendUnknownCount",
            "friendFilterStatus",
            "manualFilteredCount",
            "exclusionRevision",
        ] {
            object.remove(key);
        }
        for member in object.get_mut("members").unwrap().as_array_mut().unwrap() {
            member.as_object_mut().unwrap().remove("friendRelation");
        }
        for item in object
            .get_mut("filteredMembers")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            item.get_mut("member")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("friendRelation");
        }
        database
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO snapshots(id,finished_at,payload) VALUES(?1,?2,?3)",
                params![original.id, original.finished_at, payload.to_string()],
            )
            .unwrap();
        let restored = database.snapshot_by_id("legacy").unwrap().unwrap();
        assert!(!restored.exclude_friends);
        assert_eq!(restored.friend_filter_status, "not_requested");
        assert_eq!(restored.friend_filtered_count, 0);
        assert_eq!(restored.manual_filtered_count, 0);
        assert_eq!(restored.exclusion_revision, 0);
        assert_eq!(restored.friend_unknown_count, 0);
        assert!(restored
            .members
            .iter()
            .all(|member| member.friend_relation == FriendRelation::Unknown));
        assert_eq!(
            restored.filtered_members[0].member.friend_relation,
            FriendRelation::Unknown
        );
        let future: Member = serde_json::from_value(serde_json::json!({
            "qqNumber":"100101", "nickname":"模拟成员", "card":"", "friendRelation":"future_status"
        }))
        .unwrap();
        assert_eq!(future.friend_relation, FriendRelation::Unknown);
        assert!(database
            .validate_group_mail("legacy", &["100101".into()], &["100101@qq.com".into()])
            .is_ok());
    }

    /// 按群恢复必须同时匹配账号与来源，按带时区时间选最新版本；不能串用另一群/账号结果。
    #[test]
    fn group_snapshot_lookup_keeps_account_source_group_and_version_isolated() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        database
            .save_snapshot(&group_mail_snapshot(
                "older",
                "900001",
                "2026-11-05T10:00:00+00:00",
            ))
            .unwrap();
        database
            .save_snapshot(&group_mail_snapshot(
                "newer",
                "900001",
                "2026-11-05T06:00:00-05:00",
            ))
            .unwrap();
        database
            .save_snapshot(&group_mail_snapshot(
                "other-group",
                "900002",
                "2026-11-05T12:00:00+00:00",
            ))
            .unwrap();
        let mut other_account =
            group_mail_snapshot("other-account", "900001", "2026-11-05T12:01:00+00:00");
        other_account.account.qq_number = "100200".into();
        database.save_snapshot(&other_account).unwrap();
        let mut historical =
            group_mail_snapshot("history-source", "900001", "2026-11-05T12:02:00+00:00");
        historical.account.source = "historical".into();
        database.save_snapshot(&historical).unwrap();
        assert_eq!(
            database
                .latest_group_snapshot("100100", "web", "900001")
                .unwrap()
                .unwrap()
                .id,
            "newer"
        );
        assert_eq!(
            database
                .latest_group_snapshot("100200", "web", "900001")
                .unwrap()
                .unwrap()
                .id,
            "other-account"
        );
        assert_eq!(
            database
                .latest_group_snapshot("100100", "historical", "900001")
                .unwrap()
                .unwrap()
                .id,
            "history-source"
        );
        assert!(database
            .latest_group_snapshot("100100", "web", "missing")
            .unwrap()
            .is_none());
        assert!(database
            .latest_group_snapshot("missing", "web", "900001")
            .unwrap()
            .is_none());
    }

    /// 最近20份历史的淘汰和全局启动恢复仍按真实时间；每群最新则按成功提交顺序更新，时钟回拨不阻止刷新。
    #[test]
    fn snapshot_retention_and_latest_use_absolute_time_across_offsets() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        for minute in 0..20 {
            database
                .save_snapshot(&group_mail_snapshot(
                    &format!("kept-{minute}"),
                    "900001",
                    &format!("2026-11-05T10:{minute:02}:00+00:00"),
                ))
                .unwrap();
        }
        // 文本看起来是当天 23 点，但 UTC 实为 09 点，应立即淘汰，保留原先 20 份。
        database
            .save_snapshot(&group_mail_snapshot(
                "timezone-old",
                "900001",
                "2026-11-05T23:00:00+14:00",
            ))
            .unwrap();
        assert_eq!(
            database
                .connection()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM snapshots WHERE id='timezone-old'",
                    [],
                    |row| row.get::<_, usize>(0)
                )
                .unwrap(),
            0
        );
        // 该版本虽不属于按时间保留的20份历史，却是该群最后成功获取结果，仍须支持固定版本读取。
        assert_eq!(
            database.snapshot_by_id("timezone-old").unwrap().unwrap().id,
            "timezone-old"
        );
        assert!(database.snapshot_by_id("kept-0").unwrap().is_some());
        // 文本只有 06 点，UTC 却是 14 点，必须成为最新且保留，淘汰真正最早的 10:00。
        database
            .save_snapshot(&group_mail_snapshot(
                "timezone-new",
                "900001",
                "2026-11-05T06:00:00-08:00",
            ))
            .unwrap();
        assert_eq!(
            database.latest_snapshot().unwrap().unwrap().id,
            "timezone-new"
        );
        assert!(database.snapshot_by_id("kept-0").unwrap().is_none());
        assert!(database.snapshot_by_id("kept-1").unwrap().is_some());
    }

    /// 草稿绑定旧固定结果而非最新群；重复选择只计一次，外部补充地址不冒充群成员。
    #[test]
    fn group_mail_uses_saved_snapshot_and_deduplicated_explicit_selection() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = group_mail_snapshot("older", "900001", "2026-11-05T10:00:00+00:00");
        database.save_snapshot(&original).unwrap();
        database
            .save_snapshot(&group_mail_snapshot(
                "newer",
                "900002",
                "2026-11-05T10:01:00+00:00",
            ))
            .unwrap();
        let audience = database
            .validate_group_mail(
                "older",
                &["100101".into(), "100101".into()],
                &["100101@QQ.COM".into(), "external@example.test".into()],
            )
            .unwrap();
        assert_eq!(audience.snapshot_id, "older");
        assert_eq!(audience.group_id, "900001");
        assert_eq!(audience.group_name, original.group.name);
        assert_eq!(audience.member_count, 1);
        assert_eq!(database.latest_snapshot().unwrap().unwrap().id, "newer");
        assert_eq!(
            database.snapshot_by_id("older").unwrap().unwrap().id,
            "older"
        );
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 前端不得通过旧号、已过滤号、漏收地址或手动补充绕过本封明确选择的群成员范围。
    #[test]
    fn group_mail_rejects_missing_snapshot_and_out_of_scope_recipients() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        database
            .save_snapshot(&group_mail_snapshot(
                "scope",
                "900001",
                "2026-11-05T10:00:00+00:00",
            ))
            .unwrap();
        let selected = ["100101".into()];
        let addresses = ["100101@qq.com".into()];
        assert_eq!(
            database
                .validate_group_mail("missing", &selected, &addresses)
                .unwrap_err()
                .code,
            "group_mail_snapshot_missing"
        );
        for invalid in [
            vec![],
            vec!["100103".into()],
            vec!["999999".into()],
            vec!["0100101".into()],
        ] {
            assert_eq!(
                database
                    .validate_group_mail("scope", &invalid, &addresses)
                    .unwrap_err()
                    .code,
                "group_mail_selection"
            );
        }
        assert_eq!(
            database
                .validate_group_mail("scope", &selected, &["external@example.test".into()])
                .unwrap_err()
                .code,
            "group_mail_recipient_missing"
        );
        for bypass in ["100102@QQ.COM", "100103@qq.com"] {
            assert_eq!(
                database
                    .validate_group_mail(
                        "scope",
                        &selected,
                        &["100101@qq.com".into(), bypass.into()]
                    )
                    .unwrap_err()
                    .code,
                "group_mail_excluded_recipient"
            );
        }
        assert!(database
            .validate_group_mail(
                "scope",
                &["100101".into(), "100102".into()],
                &[
                    "100101@qq.com".into(),
                    "100102@qq.com".into(),
                    "external@example.test".into()
                ]
            )
            .is_ok());
    }

    /// 发信消费固定快照，拒绝未完成的好友过滤，也不能通过补充地址把已排除好友加回。
    #[test]
    fn group_mail_requires_completed_friend_filter_and_rejects_friend_bypass() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let mut raw = group_mail_snapshot("friends", "900001", "2026-11-05T10:00:00+00:00");
        raw.exclude_sent = false;
        for member in &mut raw.members {
            member.friend_relation = if member.qq_number == "100101" {
                FriendRelation::Friend
            } else {
                FriendRelation::NotFriend
            };
        }
        raw.filtered_members[0].member.friend_relation = FriendRelation::NotFriend;
        let account = raw.account.clone();
        let completed = crate::verify::apply_friend_filter(raw, &account).unwrap();
        database.save_snapshot(&completed).unwrap();
        let restored = database.snapshot_by_id("friends").unwrap().unwrap();
        assert_eq!(restored.friend_filtered_count, 1);
        assert_eq!(restored.friend_filter_status, "completed");
        assert!(database
            .validate_group_mail("friends", &["100102".into()], &["100102@qq.com".into()])
            .is_ok());
        assert_eq!(
            database
                .validate_group_mail(
                    "friends",
                    &["100102".into()],
                    &["100102@qq.com".into(), "100101@qq.com".into()]
                )
                .unwrap_err()
                .code,
            "group_mail_excluded_recipient"
        );
        let mut incomplete = completed.clone();
        incomplete.friend_filter_status = "not_requested".into();
        database.save_snapshot(&incomplete).unwrap();
        assert_eq!(
            database
                .validate_group_mail("friends", &["100102".into()], &["100102@qq.com".into()])
                .unwrap_err()
                .code,
            "friend_filter_incomplete"
        );
        let mut inconsistent = completed;
        inconsistent.members[0].friend_relation = FriendRelation::Friend;
        database.save_snapshot(&inconsistent).unwrap();
        assert_eq!(
            database
                .validate_group_mail("friends", &["100102".into()], &["100102@qq.com".into()])
                .unwrap_err()
                .code,
            "friend_filter_incomplete"
        );
    }

    /// 超过 400 位时跨多个有界参数批次查询；输出保留首次顺序，历史使用全库且未决优先。
    #[test]
    fn member_status_uses_batched_full_history_and_preserves_unique_order() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let mut numbers: Vec<String> = (100000..101001).map(|qq| qq.to_string()).collect();
        database
            .import_sent(
                "100850@qq.com\n100400@qq.com\n100999@other.test",
                "imported",
            )
            .unwrap();
        database
            .begin_delivery("uncertain", &["100400@qq.com".into()])
            .unwrap();
        database
            .finish_delivery(
                "uncertain",
                &["100400@qq.com".into()],
                false,
                "模拟不明结果",
            )
            .unwrap();
        numbers.push("100400".into());
        let statuses = database.member_mail_status(&numbers).unwrap();
        assert_eq!(statuses.len(), 1001);
        for (status, qq) in statuses.iter().zip(numbers.iter()) {
            assert_eq!(&status.qq_number, qq);
        }
        assert_eq!(statuses[850].status, "sent");
        assert_eq!(statuses[850].origin, "imported");
        assert_eq!(statuses[400].status, "unresolved");
        assert!(statuses[400].origin.is_empty());
        assert_eq!(statuses[999].status, "available");
        assert!(database.member_mail_status(&[]).unwrap().is_empty());
        assert_eq!(
            database.member_mail_status(&["0".into()]).unwrap_err().code,
            "member_mail_status_input"
        );
        assert_eq!(
            database
                .member_mail_status(&vec!["100000".into(); 10_001])
                .unwrap_err()
                .code,
            "member_mail_status_input"
        );
    }

    /// 人工核实改变的是本机记录；解除一次未决不能抹去以前成功历史，也不伪造新成功。
    #[test]
    fn member_status_refreshes_after_resolution_without_losing_prior_success() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let address = ["100200@qq.com".into()];
        database.import_sent("100200@qq.com", "manual").unwrap();
        database.begin_delivery("uncertain", &address).unwrap();
        assert_eq!(
            database.member_mail_status(&["100200".into()]).unwrap()[0].status,
            "unresolved"
        );
        database
            .finish_delivery("uncertain", &address, false, "模拟不明结果")
            .unwrap();
        database
            .resolve_delivery(database.unresolved_deliveries().unwrap()[0].id, false)
            .unwrap();
        let status = &database.member_mail_status(&["100200".into()]).unwrap()[0];
        assert_eq!(status.status, "sent");
        assert_eq!(status.origin, "manual");
        database
            .begin_delivery("new", &["100201@qq.com".into()])
            .unwrap();
        database
            .finish_delivery("new", &["100201@qq.com".into()], false, "模拟不明结果")
            .unwrap();
        database
            .resolve_delivery(database.unresolved_deliveries().unwrap()[0].id, false)
            .unwrap();
        assert_eq!(
            database.member_mail_status(&["100201".into()]).unwrap()[0].status,
            "available"
        );
    }

    #[test]
    fn imported_history_is_global_and_deduplicated_without_guessing_other_domains() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("中文.sqlite3")).unwrap();
        let result = database
            .import_sent(
                "QQ号,昵称\n123456,甲\n123456@qq.com,乙\n123456@other.test,丙\n无效\n",
                "imported",
            )
            .unwrap();
        assert_eq!((result.added, result.skipped, result.invalid), (2, 1, 1));
        assert_eq!(database.sent_count().unwrap(), 2);
        assert_eq!(
            database.sent_qq_numbers().unwrap(),
            HashSet::from(["123456".into()])
        );
        assert!(database.was_sent("123456@QQ.COM").unwrap());
        let id = database.sent_records().unwrap()[0].id;
        database.remove_sent(id).unwrap();
        assert_eq!(database.sent_count().unwrap(), 1);
    }

    #[test]
    fn only_accepted_delivery_enters_sent_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let unknown_recipient = vec!["100001@qq.com".into()];
        database.begin_delivery("job1", &unknown_recipient).unwrap();
        database
            .finish_delivery("job1", &unknown_recipient, false, "结果待核实")
            .unwrap();
        assert_eq!(database.sent_count().unwrap(), 0);
        let recipient = vec!["987654@qq.com".into()];
        database.begin_delivery("job2", &recipient).unwrap();
        database
            .finish_delivery("job2", &recipient, true, "")
            .unwrap();
        assert_eq!(database.sent_count().unwrap(), 1);
        assert_eq!(database.sent_records().unwrap()[0].origin, "smtp_accepted");
    }

    /// 进程中断后的 unknown 仍然拦截重发，且同批其他地址的预留必须一并回滚。
    #[test]
    fn restart_recovers_unknown_and_blocks_whole_batch_without_partial_reservations() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("data.sqlite3");
        let database = Database::open(&path).unwrap();
        database
            .begin_delivery("interrupted", &["100002@QQ.COM".into()])
            .unwrap();
        assert!(database.has_unresolved("100002@qq.com").unwrap());
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        drop(database);

        let database = Database::open(&path).unwrap();
        let unresolved = database.unresolved_deliveries().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].email, "100002@qq.com");
        assert!(!unresolved[0].updated_at.is_empty());
        assert!(database.has_unresolved("100002@QQ.COM").unwrap());
        assert!(!database.was_sent("100002@qq.com").unwrap());
        let error = database
            .begin_delivery("retry", &["100003@qq.com".into(), "100002@QQ.COM".into()])
            .unwrap_err();
        assert_eq!(error.code, "delivery_unresolved");
        assert!(!database.has_unresolved("100003@qq.com").unwrap());
    }

    /// 人工确认写独立来源而非 SMTP 证明；默认历史可跳过，显式重发仍可重新建立投递。
    #[test]
    fn verified_acceptance_writes_manual_origin_and_allows_explicit_new_delivery() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let recipients = ["100004@qq.com".into()];
        database.begin_delivery("first", &recipients).unwrap();
        database
            .finish_delivery("first", &recipients, false, "结果待核实")
            .unwrap();
        let id = database.unresolved_deliveries().unwrap()[0].id;

        database.resolve_delivery(id, true).unwrap();
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        assert!(!database.has_unresolved("100004@QQ.COM").unwrap());
        assert!(database.was_sent("100004@qq.com").unwrap());
        let records = database.sent_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].origin, "manual_verified");
        assert_eq!(records[0].qq_number, "100004");
        let error = database.resolve_delivery(id, false).unwrap_err();
        assert_eq!(error.code, "delivery_not_resolvable");
        database
            .begin_delivery("explicit-resend", &recipients)
            .unwrap();
        assert!(database.has_unresolved("100004@qq.com").unwrap());
    }

    /// 人工确认未发送不会产生成功历史，解除未决后可安全建立新任务。
    #[test]
    fn confirmed_not_sent_allows_retry_without_success_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let recipients = ["User@EXAMPLE.TEST".into()];
        database.begin_delivery("first", &recipients).unwrap();
        database
            .finish_delivery("first", &recipients, false, "结果待核实")
            .unwrap();
        let id = database.unresolved_deliveries().unwrap()[0].id;

        database.resolve_delivery(id, false).unwrap();
        assert!(!database.has_unresolved("User@example.test").unwrap());
        assert!(!database.was_sent("User@example.test").unwrap());
        assert_eq!(database.sent_count().unwrap(), 0);
        let state: String = database
            .connection()
            .unwrap()
            .query_row("SELECT state FROM mail_delivery WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(state, "not_sent");
        database.begin_delivery("retry", &recipients).unwrap();
        assert!(database.has_unresolved("User@EXAMPLE.TEST").unwrap());
        assert!(!database.has_unresolved("user@example.test").unwrap());
    }

    /// 正在等待 SMTP 的状态不可人工改判；不存在的标识也不能建立伪造成功记录。
    #[test]
    fn only_unknown_delivery_can_be_resolved() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        database
            .begin_delivery("running", &["100005@qq.com".into()])
            .unwrap();
        let id: i64 = database
            .connection()
            .unwrap()
            .query_row("SELECT id FROM mail_delivery", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            database.resolve_delivery(id, true).unwrap_err().code,
            "delivery_not_resolvable"
        );
        assert_eq!(
            database.resolve_delivery(id + 1, true).unwrap_err().code,
            "delivery_missing"
        );
        assert!(database.has_unresolved("100005@qq.com").unwrap());
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 原有成功记录与本次投递独立；确认本次未发送不能抹掉以前的成功历史。
    #[test]
    fn not_sent_resolution_preserves_previous_success_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let recipients = ["100006@qq.com".into()];
        database.begin_delivery("accepted", &recipients).unwrap();
        database
            .finish_delivery("accepted", &recipients, true, "")
            .unwrap();
        database.begin_delivery("resend", &recipients).unwrap();
        database
            .finish_delivery("resend", &recipients, false, "结果待核实")
            .unwrap();
        database
            .resolve_delivery(database.unresolved_deliveries().unwrap()[0].id, false)
            .unwrap();
        assert!(database.was_sent("100006@qq.com").unwrap());
        assert_eq!(database.sent_records().unwrap()[0].origin, "smtp_accepted");
        assert!(!database.has_unresolved("100006@qq.com").unwrap());
    }

    /// 准备跨查询分块的大名单时保持去重顺序，已发送只计一次；历史更新后重新准备必须反映新人数。
    #[test]
    fn recipient_preparation_filters_once_across_chunks_and_refreshes_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let mut recipients = (0..450)
            .map(|index| format!("prepared-{index}@example.test"))
            .collect::<Vec<_>>();
        recipients.push("prepared-0@EXAMPLE.TEST".into());
        database
            .import_sent(&recipients[..130].join("\n"), "imported")
            .unwrap();
        let (eligible, skipped) = database.prepare_mail_recipients(&recipients, true).unwrap();
        assert_eq!(skipped, 130);
        assert_eq!(eligible, recipients[130..450]);
        let (unfiltered, skipped) = database
            .prepare_mail_recipients(&recipients, false)
            .unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(unfiltered, recipients[..450]);
        database
            .import_sent(&recipients[130..132].join("\n"), "imported")
            .unwrap();
        let (eligible, skipped) = database.prepare_mail_recipients(&recipients, true).unwrap();
        assert_eq!(skipped, 132);
        assert_eq!(eligible, recipients[132..450]);
        database
            .import_sent(&recipients[..450].join("\n"), "imported")
            .unwrap();
        assert_eq!(
            database.prepare_mail_recipients(&recipients, true).unwrap(),
            (vec![], 450)
        );
    }

    /// 已成功的地址也可能有后续未决重发；任何开关都不能让成功历史遮住未核实记录。
    #[test]
    fn recipient_preparation_blocks_unknown_and_sending_until_manual_resolution() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let recipients = vec!["prepared-blocked@example.test".into()];
        database
            .import_sent(&recipients.join("\n"), "imported")
            .unwrap();
        database
            .begin_delivery("preparation-blocked", &recipients)
            .unwrap();
        for skip_sent in [false, true] {
            assert_eq!(
                database
                    .prepare_mail_recipients(&recipients, skip_sent)
                    .unwrap_err()
                    .code,
                "delivery_unresolved"
            );
        }
        database
            .finish_delivery("preparation-blocked", &recipients, false, "合成待核实投递")
            .unwrap();
        for skip_sent in [false, true] {
            assert_eq!(
                database
                    .prepare_mail_recipients(&recipients, skip_sent)
                    .unwrap_err()
                    .code,
                "delivery_unresolved"
            );
        }
        let id = database.unresolved_deliveries().unwrap()[0].id;
        database.resolve_delivery(id, false).unwrap();
        assert_eq!(
            database.prepare_mail_recipients(&recipients, true).unwrap(),
            (vec![], 1)
        );
        assert_eq!(
            database
                .prepare_mail_recipients(&recipients, false)
                .unwrap(),
            (recipients, 0)
        );
    }

    /// 空输入和超过产品上限的输入提前拒绝，避免生成无意义确认或无界SQL工作量。
    #[test]
    fn recipient_preparation_rejects_empty_and_oversized_inputs() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        for recipients in [
            vec![],
            vec!["limit@example.test".into(); crate::mail::MAX_RECIPIENTS + 1],
        ] {
            assert_eq!(
                database
                    .prepare_mail_recipients(&recipients, true)
                    .unwrap_err()
                    .code,
                "mail_recipients"
            );
        }
    }

    /// 批量核实使用合成投递记录，不建立 SMTP 连接；输入顺序保持，便于验证后续条目失败的回滚。
    fn batch_resolution_fixture(database: &Database, job: &str, count: usize) -> Vec<i64> {
        let recipients = (0..count)
            .map(|index| format!("batch-{job}-{index}@example.test"))
            .collect::<Vec<_>>();
        database.begin_delivery(job, &recipients).unwrap();
        database
            .finish_delivery(job, &recipients, false, "合成待核实投递")
            .unwrap();
        let connection = database.connection().unwrap();
        let mut query = connection
            .prepare("SELECT id FROM mail_delivery WHERE job_id=?1 ORDER BY id")
            .unwrap();
        query
            .query_map([job], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// 去重标识只计一次；确认已发送记人工来源，确认未发送则仅解除阻止，均不自动投递。
    #[test]
    fn batch_resolution_commits_true_false_and_deduplicates_ids() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let accepted = batch_resolution_fixture(&database, "accepted", 3);
        assert_eq!(
            database
                .resolve_deliveries(&[accepted[0], accepted[1], accepted[0], accepted[2]], true)
                .unwrap(),
            3
        );
        assert_eq!(database.sent_count().unwrap(), 3);
        assert!(database
            .sent_records()
            .unwrap()
            .iter()
            .all(|record| record.origin == "manual_verified"));
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        let rejected = batch_resolution_fixture(&database, "rejected", 2);
        assert_eq!(database.resolve_deliveries(&rejected, false).unwrap(), 2);
        assert_eq!(database.sent_count().unwrap(), 3);
        assert!(database.unresolved_deliveries().unwrap().is_empty());
        assert!(!database
            .has_unresolved("batch-rejected-0@example.test")
            .unwrap());
        assert!(!database.was_sent("batch-rejected-0@example.test").unwrap());
        let state: String = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT state FROM mail_delivery WHERE id=?1",
                [rejected[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "not_sent");
    }

    /// 原始输入在去重前限制长度，重复上万个标识不能绕过上限；旧单条错误码仍保留。
    #[test]
    fn batch_resolution_rejects_empty_nonpositive_and_oversized_selection() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        for invalid in [
            vec![],
            vec![0],
            vec![-1],
            vec![1, 0],
            vec![1; crate::mail::MAX_RECIPIENTS + 1],
        ] {
            assert_eq!(
                database
                    .resolve_deliveries(&invalid, true)
                    .unwrap_err()
                    .code,
                "delivery_selection"
            );
        }
        assert_eq!(
            database.resolve_delivery(0, true).unwrap_err().code,
            "delivery_missing"
        );
        assert_eq!(
            database.resolve_delivery(-1, false).unwrap_err().code,
            "delivery_missing"
        );
        let ids = batch_resolution_fixture(&database, "limit", 1);
        assert_eq!(
            database
                .resolve_deliveries(&vec![ids[0]; crate::mail::MAX_RECIPIENTS], false)
                .unwrap(),
            1
        );
    }

    /// 任一不存在、已核实或发送中的选择拒绝整批；先前合法的条目仍为 unknown，不写成功历史。
    #[test]
    fn batch_resolution_rolls_back_missing_stale_and_sending_selection() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let ids = batch_resolution_fixture(&database, "stale", 3);
        assert_eq!(
            database
                .resolve_deliveries(&[ids[0], i64::MAX], true)
                .unwrap_err()
                .code,
            "delivery_missing"
        );
        database.resolve_delivery(ids[2], false).unwrap();
        assert_eq!(
            database
                .resolve_deliveries(&[ids[0], ids[2]], true)
                .unwrap_err()
                .code,
            "delivery_not_resolvable"
        );
        database
            .begin_delivery("running", &["batch-running@example.test".into()])
            .unwrap();
        let running: i64 = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT id FROM mail_delivery WHERE job_id='running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            database
                .resolve_deliveries(&[ids[0], running], false)
                .unwrap_err()
                .code,
            "delivery_not_resolvable"
        );
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 2);
        assert!(database
            .has_unresolved("batch-stale-0@example.test")
            .unwrap());
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 后半段状态更新或历史写入触发器失败时，前半段已执行的状态和历史也必须共同回滚。
    #[test]
    fn batch_resolution_trigger_failure_rolls_back_state_and_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let ids = batch_resolution_fixture(&database, "trigger", 3);
        let connection = database.connection().unwrap();
        connection.execute_batch("CREATE TRIGGER fail_resolution_state BEFORE UPDATE ON mail_delivery WHEN NEW.recipient='batch-trigger-1@example.test' AND NEW.state='manual_verified' BEGIN SELECT RAISE(ABORT,'synthetic state failure'); END;").unwrap();
        assert_eq!(
            database.resolve_deliveries(&ids, true).unwrap_err().code,
            "database_error"
        );
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 3);
        assert_eq!(database.sent_count().unwrap(), 0);
        connection.execute_batch("DROP TRIGGER fail_resolution_state; CREATE TRIGGER fail_resolution_history BEFORE INSERT ON sent_history WHEN NEW.email='batch-trigger-1@example.test' BEGIN SELECT RAISE(ABORT,'synthetic history failure'); END;").unwrap();
        assert_eq!(
            database.resolve_deliveries(&ids, true).unwrap_err().code,
            "database_error"
        );
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 3);
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 批量确认本次未发送不能删除任一旧成功记录，避免误清理历史后造成重复投递。
    #[test]
    fn batch_resolution_not_sent_preserves_previous_successes() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        database
            .import_sent(
                "batch-prior-0@example.test\nbatch-prior-1@example.test",
                "imported",
            )
            .unwrap();
        let before = database.sent_records().unwrap();
        let ids = batch_resolution_fixture(&database, "prior", 2);
        assert_eq!(database.resolve_deliveries(&ids, false).unwrap(), 2);
        let after = database.sent_records().unwrap();
        assert_eq!(before.len(), after.len());
        assert!(after.iter().all(|record| record.origin == "imported"));
        assert!(database.was_sent("batch-prior-0@example.test").unwrap());
        assert!(database.was_sent("batch-prior-1@example.test").unwrap());
        assert!(database.unresolved_deliveries().unwrap().is_empty());
    }

    /// 两个连接竞争同批核实时只允许一个提交；另一方看到已核实状态，不能混合处理或重复记录。
    #[test]
    fn batch_resolution_concurrent_connections_commit_only_once() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let ids = batch_resolution_fixture(&database, "race", 3);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers = [true, false]
            .into_iter()
            .map(|accepted| {
                let database = database.clone();
                let ids = ids.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (accepted, database.resolve_deliveries(&ids, accepted))
                })
            })
            .collect::<Vec<_>>();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        let winning_accepted = outcomes
            .iter()
            .find(|(_, result)| result.is_ok())
            .unwrap()
            .0;
        assert_eq!(
            database.sent_count().unwrap(),
            if winning_accepted { 3 } else { 0 }
        );
        let error = outcomes
            .into_iter()
            .find_map(|(_, result)| result.err())
            .unwrap();
        assert_eq!(error.code, "delivery_not_resolvable");
        assert!(database.unresolved_deliveries().unwrap().is_empty());
    }

    /// 独立连接同时争抢同一收件人时只准一个任务预留，另一方必须看到已提交的未决状态。
    #[test]
    fn concurrent_connections_cannot_reserve_the_same_recipient_twice() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|index| {
                let database = database.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    database.begin_delivery(&format!("job-{index}"), &["100007@qq.com".into()])
                })
            })
            .collect();
        let outcomes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        let rejected = outcomes.into_iter().find_map(Result::err).unwrap();
        assert_eq!(rejected.code, "delivery_unresolved");
    }
}
