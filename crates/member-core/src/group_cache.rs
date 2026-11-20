//! 按来源、账号与群隔离的本机群目录和每群最新名单。作者：ahui。
//! 群目录只保存 Group 元信息；成员仍使用既有 Snapshot JSON，不另存好友全表、Cookie 或重复成员行。
//! 最近 20 次历史用于回看版本，每群一份最新结果独立于历史淘汰；所有更新由宿主成功获取后显式调用。

use crate::{
    database::{db_error, Database},
    models::{Account, AppError, AppResult, Group, Snapshot},
};
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

/// 在 Database::open 已持有的迁移事务内创建 v4 结构并回填；不能单独提交版本或部分数据。
/// 回填只依赖旧库尚存的快照，按真实完成时间及标识取每群最新，不伪造此前已淘汰的群或成员。
pub(crate) fn initialize(connection: &Connection, previous_version: i64) -> AppResult<()> {
    connection
        .execute_batch(
            "-- 群目录的复合主键是隔离边界，完整刷新只替换同来源同账号的列表。
         CREATE TABLE IF NOT EXISTS account_group_catalog (
            source TEXT NOT NULL,
            account_qq TEXT NOT NULL,
            group_id TEXT NOT NULL,
            position INTEGER NOT NULL CHECK(position>=0),
            payload TEXT NOT NULL,
            fetched_at TEXT NOT NULL,
            PRIMARY KEY(source,account_qq,group_id)
         );
         -- 每个隔离群只保留一份最新原始快照；不引用会被最近20次淘汰的历史行。
         CREATE TABLE IF NOT EXISTS latest_group_results (
            source TEXT NOT NULL,
            account_qq TEXT NOT NULL,
            group_id TEXT NOT NULL,
            snapshot_id TEXT NOT NULL UNIQUE,
            payload TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            PRIMARY KEY(source,account_qq,group_id)
         );",
        )
        .map_err(db_error)?;
    if previous_version >= 4 {
        return Ok(());
    }
    let mut statement = connection
        .prepare("SELECT payload FROM snapshots ORDER BY julianday(finished_at),id")
        .map_err(db_error)?;
    let mut rows = statement.query([]).map_err(db_error)?;
    while let Some(row) = rows.next().map_err(db_error)? {
        let payload: String = row.get(0).map_err(db_error)?;
        let snapshot: Snapshot =
            serde_json::from_str(&payload).map_err(|_| cache_decode_error())?;
        save_latest_result(connection, &snapshot, &payload)?;
    }
    Ok(())
}

impl Database {
    /// 原子替换已核验账号的完整群目录，顺序沿用服务端列表；空列表表示该账号当前没有可见群。
    /// 只修改同 source/qq_number 的目录，不删除其他账号目录或任何已保存成员结果；失败保留原目录。
    /// 重复/空群标识拒绝整次更新，不把部分可用项拼成“完整群列表”；不恢复登录或联网。
    pub fn save_group_catalog(&self, account: &Account, groups: &[Group]) -> AppResult<()> {
        validate_identity(account)?;
        let mut ids = HashSet::new();
        if groups
            .iter()
            .any(|group| group.id.trim().is_empty() || !ids.insert(&group.id))
        {
            return Err(AppError::new(
                "group_catalog_invalid",
                "群列表标识重复或缺失，本次列表未保存，请刷新后重试。",
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM account_group_catalog WHERE source=?1 AND account_qq=?2",
                params![account.source, account.qq_number],
            )
            .map_err(db_error)?;
        let now = Local::now().to_rfc3339();
        for (position, group) in groups.iter().enumerate() {
            let payload = serde_json::to_string(group).map_err(|_| cache_decode_error())?;
            transaction.execute("INSERT INTO account_group_catalog(source,account_qq,group_id,position,payload,fetched_at) VALUES(?1,?2,?3,?4,?5,?6)",params![account.source,account.qq_number,group.id,position,payload,now]).map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)
    }

    /// 读取指定账号上次已保存的群元信息，无记录返回空数组；这些历史值不代表当前登录或实时权限。
    /// 不改变顺序、不读取成员 BLOB/JSON，不在失败时退回其他账号或用最新快照猜测登录身份。
    pub fn saved_group_catalog(&self, account: &Account) -> AppResult<Vec<Group>> {
        validate_identity(account)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT group_id,payload FROM account_group_catalog WHERE source=?1 AND account_qq=?2 ORDER BY position,group_id").map_err(db_error)?;
        let mut rows = statement
            .query(params![account.source, account.qq_number])
            .map_err(db_error)?;
        let mut groups = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            let id: String = row.get(0).map_err(db_error)?;
            let payload: String = row.get(1).map_err(db_error)?;
            let group: Group = serde_json::from_str(&payload).map_err(|_| cache_decode_error())?;
            if group.id != id {
                return Err(cache_decode_error());
            }
            groups.push(group);
        }
        Ok(groups)
    }
}

/// 与历史快照同一写事务保存最新结果；服务端人数存在时更新目录人数，绝不使用过滤后人数代替。
/// 实时获取按宿主已核对的成功提交顺序更新，不用本机时间或随机标识决定新旧；时钟回退也能替换成员。
/// 旧库迁移已先按完成时间/标识升序遍历，最后提交的就是能从旧历史证明的最新版本。
/// 最新结果更新失败时，调用者必须回滚历史及目录，不能只让其中一个保存成功。
pub(crate) fn save_latest_result(
    connection: &Connection,
    snapshot: &Snapshot,
    payload: &str,
) -> AppResult<()> {
    validate_identity(&snapshot.account)?;
    if snapshot.group.id.trim().is_empty() {
        return Err(cache_decode_error());
    }
    connection.execute(
        "INSERT INTO latest_group_results(source,account_qq,group_id,snapshot_id,payload,finished_at) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(source,account_qq,group_id) DO UPDATE SET snapshot_id=excluded.snapshot_id,payload=excluded.payload,finished_at=excluded.finished_at",
        params![snapshot.account.source,snapshot.account.qq_number,snapshot.group.id,snapshot.id,payload,snapshot.finished_at]
    ).map_err(db_error)?;
    // 组合键只进入名单缓存，不能伪装成QQ群写入真实目录；各来源群人数仍在同事务内更新。
    let source_groups = if snapshot.source_groups.is_empty() {
        vec![snapshot.group.clone()]
    } else {
        snapshot
            .source_groups
            .iter()
            .map(|source| {
                let mut group = source.group.clone();
                group.member_count = source.reported_count.or(group.member_count);
                group
            })
            .collect()
    };
    for mut group in source_groups {
        if let Some(reported_count) = snapshot
            .reported_count
            .filter(|_| snapshot.source_groups.is_empty())
        {
            group.member_count = Some(reported_count);
        }
        let group_payload = serde_json::to_string(&group).map_err(|_| cache_decode_error())?;
        // 已存在的目录顺序保持；只有未列入目录的历史群才追加到该账号目录末尾。
        connection.execute(
        "INSERT INTO account_group_catalog(source,account_qq,group_id,position,payload,fetched_at)
         VALUES(?1,?2,?3,(SELECT coalesce(max(position)+1,0) FROM account_group_catalog WHERE source=?1 AND account_qq=?2),?4,?5)
         ON CONFLICT(source,account_qq,group_id) DO UPDATE SET payload=excluded.payload,fetched_at=excluded.fetched_at",
        params![snapshot.account.source,snapshot.account.qq_number,group.id,group_payload,snapshot.finished_at]
    ).map_err(db_error)?;
    }
    Ok(())
}

/// 复合键定位后仍核对 JSON 内账号、来源、群与快照标识，防止损坏数据突破账号隔离边界。
pub(crate) fn read_latest_result(
    connection: &Connection,
    account: &str,
    source: &str,
    group: &str,
) -> AppResult<Option<Snapshot>> {
    let row: Option<(String,String)> = connection.query_row("SELECT snapshot_id,payload FROM latest_group_results WHERE source=?1 AND account_qq=?2 AND group_id=?3",params![source,account,group],|row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(db_error)?;
    row.map(|(id, payload)| {
        let snapshot: Snapshot =
            serde_json::from_str(&payload).map_err(|_| cache_decode_error())?;
        if snapshot.id != id
            || snapshot.account.qq_number != account
            || snapshot.account.source != source
            || snapshot.group.id != group
        {
            return Err(cache_decode_error());
        }
        Ok(snapshot)
    })
    .transpose()
}

fn validate_identity(account: &Account) -> AppResult<()> {
    // 兼容旧快照未知来源字符串，不能把它重写为 web；空隔离键本身不足以归属到任何账号。
    if account.source.trim().is_empty() || account.qq_number.trim().is_empty() {
        return Err(AppError::new(
            "group_catalog_invalid",
            "账号或来源信息缺失，无法安全保存群列表。",
        ));
    }
    Ok(())
}

fn cache_decode_error() -> AppError {
    AppError::new(
        "snapshot_decode",
        "已保存的群信息或名单无法读取，请重新获取；原始数据库未被清空。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        export::{write_export, ExportFormat},
        models::{FetchResult, FriendRelation, Member},
        verify::create_snapshot,
    };

    fn account() -> Account {
        Account {
            qq_number: "120001".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        }
    }
    fn group(id: &str) -> Group {
        Group {
            id: id.into(),
            name: format!("合成群{id}"),
            member_count: Some(2),
            role: "member".into(),
        }
    }
    fn snapshot(id: &str, group_id: &str, minute: usize) -> Snapshot {
        let time = format!("2026-11-06T10:{minute:02}:00+08:00");
        create_snapshot(
            id.into(),
            account(),
            group(group_id),
            FetchResult {
                members: ["100101", "100102"]
                    .iter()
                    .map(|number| Member {
                        qq_number: (*number).into(),
                        nickname: "合成成员".into(),
                        card: String::new(),
                        friend_relation: FriendRelation::Unknown,
                    })
                    .collect(),
                reported_count: Some(2),
                freshness_confirmed: true,
                truncated: false,
                warnings: vec![],
            },
            time.clone(),
            time,
        )
    }

    /// 跨 25 个群后，早期群的最新结果仍可按群/固定版本读取、导出和校验群邮件，不受20次历史淘汰影响。
    #[test]
    fn each_groups_latest_result_survives_global_history_retention() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("data.sqlite3");
        let db = Database::open(&path).unwrap();
        for index in 0..25 {
            db.save_snapshot(&snapshot(
                &format!("snapshot-{index}"),
                &format!("900{index:03}"),
                index,
            ))
            .unwrap();
        }
        let connection = db.connection().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM snapshots", [], |row| row
                    .get::<_, usize>(0))
                .unwrap(),
            20
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM latest_group_results", [], |row| row
                    .get::<_, usize>(
                    0
                ))
                .unwrap(),
            25
        );
        drop(connection);
        drop(db);
        let db = Database::open(&path).unwrap();
        let restored = db
            .latest_group_snapshot("120001", "web", "900000")
            .unwrap()
            .unwrap();
        assert_eq!(restored.id, "snapshot-0");
        assert_eq!(
            db.snapshot_by_id("snapshot-0")
                .unwrap()
                .unwrap()
                .members
                .len(),
            2
        );
        assert_eq!(
            db.validate_group_mail("snapshot-0", &["100101".into()], &["100101@qq.com".into()])
                .unwrap()
                .member_count,
            1
        );
        let export_path = folder.path().join("cached.txt");
        write_export(&restored, &export_path, ExportFormat::Txt).unwrap();
        assert_eq!(
            std::fs::read_to_string(&export_path).unwrap(),
            "100101\r\n100102\r\n"
        );
        assert_eq!(db.latest_snapshot().unwrap().unwrap().id, "snapshot-24");
    }

    /// 同群成功更新完全替换成员，不追加已离开成员；partial状态和未知来源按原值保存，各账号互不串用。
    #[test]
    fn successful_updates_replace_members_and_preserve_account_source_status() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let first = snapshot("first", "900001", 0);
        db.save_snapshot(&first).unwrap();
        let mut updated = snapshot("updated", "900001", 1);
        updated.members.remove(0);
        updated.source_count = 1;
        updated.reported_count = Some(3);
        updated.status = "partial".into();
        updated.warnings = vec!["合成部分结果".into()];
        db.save_snapshot(&updated).unwrap();
        let mut other = snapshot("other-account", "900001", 2);
        other.account.qq_number = "120002".into();
        db.save_snapshot(&other).unwrap();
        let mut legacy = snapshot("other-source", "900001", 3);
        legacy.account.source = "historical".into();
        db.save_snapshot(&legacy).unwrap();
        let restored = db
            .latest_group_snapshot("120001", "web", "900001")
            .unwrap()
            .unwrap();
        assert_eq!(restored.id, "updated");
        assert_eq!(restored.members.len(), 1);
        assert_eq!(restored.members[0].qq_number, "100102");
        assert_eq!(restored.status, "partial");
        assert_eq!(restored.warnings, updated.warnings);
        assert_eq!(
            db.saved_group_catalog(&account()).unwrap()[0].member_count,
            Some(3)
        );
        assert_eq!(
            db.latest_group_snapshot("120002", "web", "900001")
                .unwrap()
                .unwrap()
                .id,
            "other-account"
        );
        assert_eq!(
            db.latest_group_snapshot("120001", "historical", "900001")
                .unwrap()
                .unwrap()
                .id,
            "other-source"
        );
        assert!(db
            .latest_group_snapshot("absent", "web", "900001")
            .unwrap()
            .is_none());
    }

    /// 宿主已串行核对任务代次，成功提交才是实时更新边界；同刻随机ID较小或系统回拨都不能留下旧成员。
    #[test]
    fn successful_commit_order_wins_over_equal_timestamp_and_clock_rollback() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&snapshot("zzz-initial", "900001", 10))
            .unwrap();
        let mut equal_time = snapshot("aaa-next", "900001", 10);
        equal_time.members.remove(0);
        equal_time.source_count = 1;
        equal_time.reported_count = Some(1);
        db.save_snapshot(&equal_time).unwrap();
        let restored = db
            .latest_group_snapshot("120001", "web", "900001")
            .unwrap()
            .unwrap();
        assert_eq!(restored.id, "aaa-next");
        assert_eq!(restored.members.len(), 1);
        let mut clock_back = snapshot("clock-back", "900001", 2);
        clock_back.members[0].nickname = "校时后新资料".into();
        db.save_snapshot(&clock_back).unwrap();
        let restored = db
            .latest_group_snapshot("120001", "web", "900001")
            .unwrap()
            .unwrap();
        assert_eq!(restored.id, "clock-back");
        assert_eq!(restored.members[0].nickname, "校时后新资料");
        assert_eq!(
            db.saved_group_catalog(&account()).unwrap()[0].member_count,
            Some(2)
        );
        // 全局历史恢复继续使用原来的时间排序，独立于每群最新提交的定义。
        assert_eq!(db.latest_snapshot().unwrap().unwrap().id, "zzz-initial");
    }

    /// 目录完整刷新可移除已退出的群，却保留该群历史结果；空群列表和未知人数不被猜成旧值。
    #[test]
    fn catalog_replacement_is_scoped_ordered_and_does_not_delete_results() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&snapshot("saved", "900001", 0)).unwrap();
        let mut other = account();
        other.qq_number = "120002".into();
        db.save_group_catalog(&other, &[group("other")]).unwrap();
        let mut unknown = group("900002");
        unknown.member_count = None;
        db.save_group_catalog(&account(), &[unknown.clone(), group("900003")])
            .unwrap();
        let groups = db.saved_group_catalog(&account()).unwrap();
        assert_eq!(
            groups
                .iter()
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            vec!["900002", "900003"]
        );
        assert_eq!(groups[0].member_count, None);
        assert_eq!(db.saved_group_catalog(&other).unwrap()[0].id, "other");
        assert!(db.snapshot_by_id("saved").unwrap().is_some());
        assert!(db
            .save_group_catalog(&account(), &[unknown.clone(), unknown])
            .is_err());
        assert_eq!(db.saved_group_catalog(&account()).unwrap().len(), 2);
        db.save_group_catalog(&account(), &[]).unwrap();
        assert!(db.saved_group_catalog(&account()).unwrap().is_empty());
        assert_eq!(db.saved_group_catalog(&other).unwrap().len(), 1);
        assert!(db
            .latest_group_snapshot("120001", "web", "900001")
            .unwrap()
            .is_some());
    }

    /// 在真实SQLite失败点验证历史、新群最新值与目录同时回滚；失败不能成为下次点击群时的最新结果。
    #[test]
    fn snapshot_and_catalog_failures_roll_back_all_writes() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&snapshot("original", "900001", 0))
            .unwrap();
        db.connection().unwrap().execute_batch("CREATE TRIGGER reject_latest BEFORE UPDATE ON latest_group_results WHEN NEW.snapshot_id='reject-latest' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END; CREATE TRIGGER reject_catalog BEFORE UPDATE ON account_group_catalog WHEN NEW.fetched_at='2026-11-06T10:02:00+08:00' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        for (id, minute) in [("reject-latest", 1), ("reject-catalog", 2)] {
            assert!(db.save_snapshot(&snapshot(id, "900001", minute)).is_err());
            assert!(db.snapshot_by_id(id).unwrap().is_none());
            assert_eq!(
                db.latest_group_snapshot("120001", "web", "900001")
                    .unwrap()
                    .unwrap()
                    .id,
                "original"
            );
        }
        db.connection().unwrap().execute_batch("CREATE TRIGGER reject_group_insert BEFORE INSERT ON account_group_catalog WHEN NEW.group_id='fail' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert!(db
            .save_group_catalog(&account(), &[group("new"), group("fail")])
            .is_err());
        assert_eq!(db.saved_group_catalog(&account()).unwrap()[0].id, "900001");
    }

    /// v3仅从尚存快照回填每账号/来源/群最新，不清空历史、模板、配置或手动排除状态。
    #[test]
    fn schema_three_backfill_preserves_data_and_selects_latest_per_identity() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("old.sqlite3");
        let db = Database::open(&path).unwrap();
        db.save_snapshot(&snapshot("old", "900001", 0)).unwrap();
        db.save_snapshot(&snapshot("new", "900001", 1)).unwrap();
        let mut other = snapshot("other", "900001", 2);
        other.account.qq_number = "120002".into();
        db.save_snapshot(&other).unwrap();
        db.save_snapshot(&snapshot("second-group", "900002", 3))
            .unwrap();
        let template = db
            .save_mail_template("保留模板", "保留主题", "", &[], None)
            .unwrap();
        db.connection().unwrap().execute_batch("INSERT INTO settings(key,value) VALUES('preserved','yes'); DROP TABLE account_group_catalog; DROP TABLE latest_group_results; ALTER TABLE sent_history DROP COLUMN sender_address; ALTER TABLE sent_history DROP COLUMN sender_name; ALTER TABLE mail_delivery DROP COLUMN sender_address; ALTER TABLE mail_delivery DROP COLUMN sender_name; PRAGMA user_version=3;").unwrap();
        drop(db);
        let upgraded = Database::open(&path).unwrap();
        assert_eq!(
            upgraded
                .connection()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            upgraded
                .latest_group_snapshot("120001", "web", "900001")
                .unwrap()
                .unwrap()
                .id,
            "new"
        );
        assert_eq!(
            upgraded
                .latest_group_snapshot("120002", "web", "900001")
                .unwrap()
                .unwrap()
                .id,
            "other"
        );
        assert_eq!(upgraded.saved_group_catalog(&account()).unwrap().len(), 2);
        assert_eq!(
            upgraded.setting("preserved").unwrap().as_deref(),
            Some("yes")
        );
        assert_eq!(
            upgraded
                .mail_template(&template.id)
                .unwrap()
                .unwrap()
                .subject,
            "保留主题"
        );
        assert!(upgraded.snapshot_by_id("old").unwrap().is_some());
        assert!(upgraded
            .latest_group_snapshot("120001", "web", "never-saved")
            .unwrap()
            .is_none());
    }

    /// 损坏旧JSON导致迁移失败时保留v3及原始记录，新增表也回滚，避免假装成功或自动清库。
    #[test]
    fn schema_three_corruption_rolls_back_new_tables_and_version() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("bad.sqlite3");
        let db = Database::open(&path).unwrap();
        db.connection().unwrap().execute_batch("DROP TABLE account_group_catalog; DROP TABLE latest_group_results; INSERT INTO snapshots(id,finished_at,payload) VALUES('bad','2026-11-06T10:00:00+08:00','{invalid'); PRAGMA user_version=3;").unwrap();
        drop(db);
        assert_eq!(Database::open(&path).err().unwrap().code, "snapshot_decode");
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('latest_group_results','account_group_catalog')",[],|row|row.get::<_,usize>(0)).unwrap(),0);
        assert_eq!(
            connection
                .query_row("SELECT payload FROM snapshots WHERE id='bad'", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "{invalid"
        );
    }
}
