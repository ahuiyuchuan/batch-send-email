//! 本机全局手动排除；仅记录“不向该完整邮箱发信”的用户选择，不伪造已发送状态。作者：ahui。
//! 增删与版本同事务提交；名单叠加只生成内存副本，不重写 QQ 原始快照或读取网络。

use crate::{
    database::{db_error, normalize_recipient, recipient_key, Database},
    models::{AppError, AppResult, FilteredMember, Snapshot},
};
use chrono::Local;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Serialize;
use std::collections::HashSet;

/// 独立排除记录；qq_number 只对数字 QQ 邮箱有效，其他品牌不猜测关联账号。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualExclusionRecord {
    pub id: i64,
    pub email: String,
    pub qq_number: String,
    pub note: String,
    pub created_at: String,
}

/// 同一只读事务内得到的列表与版本；空列表仍有版本，不以记录数充当版本。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualExclusionView {
    pub revision: u64,
    pub records: Vec<ManualExclusionRecord>,
}

/// 批次输入结果；非法项明确计数，合法项可以添加，调用方在 invalid>0 时保留原输入供修正。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualExclusionMutation {
    pub view: ManualExclusionView,
    pub added: usize,
    pub skipped: usize,
    pub invalid: usize,
    pub removed: usize,
}

/// 确认与实际投递共用的后端过滤结果；排除地址独立计数，绝不写入已发送历史。
#[derive(Debug)]
pub struct PreparedMailRecipients {
    pub recipients: Vec<String>,
    pub skipped_sent: usize,
    pub excluded: Vec<String>,
    pub exclusion_revision: u64,
}

/// 读取单行版本；SQLite 有符号整数范围内递增，损坏或缺失状态拒绝而不猜成零。
pub(crate) fn read_revision(connection: &Connection) -> AppResult<u64> {
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM manual_exclusion_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    u64::try_from(revision).map_err(|_| {
        AppError::new(
            "manual_exclusion_state",
            "手动排除版本无法读取，请重新启动程序后重试。",
        )
    })
}

/// 只在持有写事务且确实改变排除内容后调用，版本溢出会使整个变更回滚。
fn advance_revision(connection: &Connection) -> AppResult<()> {
    let changed = connection.execute(
        "UPDATE manual_exclusion_state SET revision=revision+1 WHERE id=1 AND revision<9223372036854775807", [],
    ).map_err(db_error)?;
    if changed != 1 {
        return Err(AppError::new(
            "manual_exclusion_state",
            "手动排除版本无法更新，本次修改未保存。",
        ));
    }
    Ok(())
}

/// 调用方提供同一读/写事务，保证返回列表和版本属于同一个提交点。
fn read_view(connection: &Connection) -> AppResult<ManualExclusionView> {
    let revision = read_revision(connection)?;
    let mut query = connection
        .prepare(
            "SELECT id,email,qq_number,note,created_at FROM manual_exclusions ORDER BY id DESC",
        )
        .map_err(db_error)?;
    let records = query
        .query_map([], |row| {
            Ok(ManualExclusionRecord {
                id: row.get(0)?,
                email: row.get(1)?,
                qq_number: row.get(2)?,
                note: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(ManualExclusionView { revision, records })
}

impl Database {
    /// 仅读取当前手动排除版本，供发送启动核对确认令牌；不加载完整名单或修改任何状态。
    /// 返回版本对应已提交的数据库状态；是否与后续投递形成原子校验仍由投递写事务保证。
    pub fn manual_exclusion_revision(&self) -> AppResult<u64> {
        read_revision(&self.connection()?)
    }

    /// 读取本机所有账号/群/手工邮件共用的排除记录；不触发 QQ、SMTP 或历史更新。
    pub fn manual_exclusions(&self) -> AppResult<ManualExclusionView> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let view = read_view(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(view)
    }

    /// 批量添加完整邮箱或裸数字 QQ，兼容换行/逗号/分号；每次最多10000非空项和512KiB文本。
    /// 重复项保留原备注与时间，计入 skipped，不递增版本；备注至多500字符且不含控制字符。
    /// 合法项与版本原子保存，非法项计入 invalid 返回，失败时全部数据库变更回滚。
    pub fn add_manual_exclusions(
        &self,
        text: &str,
        note: &str,
    ) -> AppResult<ManualExclusionMutation> {
        if text.len() > 512 * 1024
            || note.chars().count() > 500
            || note.chars().any(char::is_control)
        {
            return Err(AppError::new(
                "manual_exclusion_input",
                "排除内容最多512KiB，备注最多500字且不能包含控制字符。",
            ));
        }
        let inputs = text
            .split(['\n', '\r', ',', ';', '，', '；'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if inputs.len() > crate::mail::MAX_RECIPIENTS {
            return Err(AppError::new(
                "manual_exclusion_input",
                "每次最多添加10000个排除地址，请分批处理。",
            ));
        }
        let mut parsed = Vec::new();
        let mut seen = HashSet::new();
        let mut skipped = 0;
        let mut invalid = 0;
        for input in inputs {
            if let Some((qq_number, email)) = normalize_recipient(input) {
                if seen.insert(email.clone()) {
                    parsed.push((qq_number, email));
                } else {
                    skipped += 1;
                }
            } else {
                invalid += 1;
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = Local::now().to_rfc3339();
        let mut added = 0;
        for (qq, email) in parsed {
            let count = transaction.execute(
                "INSERT OR IGNORE INTO manual_exclusions(recipient_key,email,qq_number,note,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![recipient_key(&email), email, qq, note.trim(), now],
            ).map_err(db_error)?;
            added += count;
            skipped += usize::from(count == 0);
        }
        if added > 0 {
            advance_revision(&transaction)?;
        }
        let view = read_view(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(ManualExclusionMutation {
            view,
            added,
            skipped,
            invalid,
            removed: 0,
        })
    }

    /// 原子移除明确选择的本机记录，重复ID仅处理一次；包含失效ID时整批拒绝，不能部分删除。
    /// 空选择不改版本；非法/超过10000项拒绝。移除不解除好友、群重合、已发送或未决投递限制。
    pub fn remove_manual_exclusions(&self, ids: &[i64]) -> AppResult<ManualExclusionMutation> {
        if ids.len() > crate::mail::MAX_RECIPIENTS || ids.iter().any(|id| *id <= 0) {
            return Err(AppError::new(
                "manual_exclusion_input",
                "请选择最多10000条有效的手动排除记录。",
            ));
        }
        let unique = ids.iter().copied().collect::<HashSet<_>>();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        for id in &unique {
            if transaction
                .execute("DELETE FROM manual_exclusions WHERE id=?1", [id])
                .map_err(db_error)?
                != 1
            {
                return Err(AppError::new(
                    "manual_exclusion_changed",
                    "部分排除记录已变化，本次未移除任何记录，请刷新后重试。",
                ));
            }
        }
        if !unique.is_empty() {
            advance_revision(&transaction)?;
        }
        let view = read_view(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(ManualExclusionMutation {
            view,
            added: 0,
            skipped: ids.len() - unique.len(),
            invalid: 0,
            removed: unique.len(),
        })
    }

    /// 从保存的合格成员叠加当前全局排除，不联网、不保存新快照、不改变原获取时间/参考人数。
    /// 已有manual层先还原；撤销排除只恢复原本合格的人，绝不解除group/friend/sent的旧过滤。
    pub fn apply_manual_exclusions(&self, snapshot: &Snapshot) -> AppResult<Snapshot> {
        let view = self.manual_exclusions()?;
        let excluded = view
            .records
            .iter()
            .map(|record| record.email.as_str())
            .collect::<HashSet<_>>();
        let mut result = snapshot.clone();
        crate::verify::validate_manual_filter(&result)?;
        let mut retained_filters = Vec::new();
        for item in std::mem::take(&mut result.filtered_members) {
            if item.reason == "manual" {
                result.members.push(item.member);
            } else {
                retained_filters.push(item);
            }
        }
        result.filtered_members = retained_filters;
        result.manual_filtered_count = 0;
        let mut retained_members = Vec::new();
        for member in std::mem::take(&mut result.members) {
            if excluded.contains(format!("{}@qq.com", member.qq_number).as_str()) {
                result.filtered_members.push(FilteredMember {
                    member,
                    reason: "manual".into(),
                    detail: "本机手动排除".into(),
                });
                result.manual_filtered_count += 1;
            } else {
                retained_members.push(member);
            }
        }
        result.members = retained_members;
        result.members.sort_by(|a, b| {
            a.qq_number
                .len()
                .cmp(&b.qq_number.len())
                .then_with(|| a.qq_number.cmp(&b.qq_number))
        });
        result.filtered_members.sort_by(|a, b| {
            a.member
                .qq_number
                .len()
                .cmp(&b.member.qq_number.len())
                .then_with(|| a.member.qq_number.cmp(&b.member.qq_number))
        });
        result.exclusion_revision = view.revision;
        crate::verify::validate_manual_filter(&result)?;
        crate::verify::validate_friend_filter(&result)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Account, FetchResult, FriendRelation, Group, Member},
        verify,
    };

    /// 仅使用随机临时 SQLite 与合成号码；原始名单包含两位合格成员和一位已由排除群移除者。
    fn snapshot() -> Snapshot {
        let account = Account {
            qq_number: "990000".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        };
        let make = |group_id: &str, numbers: &[&str]| {
            verify::create_snapshot(
                group_id.into(),
                account.clone(),
                Group {
                    id: group_id.into(),
                    name: "合成群".into(),
                    member_count: Some(numbers.len()),
                    role: "member".into(),
                },
                FetchResult {
                    members: numbers
                        .iter()
                        .map(|qq| Member {
                            qq_number: (*qq).into(),
                            nickname: "合成成员".into(),
                            card: String::new(),
                            friend_relation: FriendRelation::NotFriend,
                        })
                        .collect(),
                    reported_count: Some(numbers.len()),
                    freshness_confirmed: true,
                    truncated: false,
                    warnings: vec![],
                },
                "2026-11-06T00:00:00Z".into(),
                "2026-11-06T00:01:00Z".into(),
            )
        };
        verify::apply_exclusions(
            make("990001", &["990101", "990102", "990103"]),
            vec![make("990002", &["990103"])],
        )
        .unwrap()
    }

    #[test]
    fn global_crud_normalizes_deduplicates_reports_invalid_and_changes_revision_only_on_real_changes(
    ) {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        assert_eq!(database.manual_exclusions().unwrap().revision, 0);
        assert_eq!(database.manual_exclusion_revision().unwrap(), 0);
        let added = database
            .add_manual_exclusions(
                "990101\n990101@QQ.COM\nName@EXAMPLE.test;broken\n",
                "合成备注",
            )
            .unwrap();
        assert_eq!((added.added, added.skipped, added.invalid), (2, 1, 1));
        assert_eq!(added.view.revision, 1);
        assert_eq!(database.manual_exclusion_revision().unwrap(), 1);
        assert_eq!(database.sent_count().unwrap(), 0);
        let qq = added
            .view
            .records
            .iter()
            .find(|record| record.qq_number == "990101")
            .unwrap();
        assert_eq!(qq.email, "990101@qq.com");
        assert_eq!(qq.note, "合成备注");
        assert!(added
            .view
            .records
            .iter()
            .any(|record| record.email == "Name@example.test" && record.qq_number.is_empty()));
        let duplicate = database
            .add_manual_exclusions("990101@qq.com", "不能覆盖备注")
            .unwrap();
        assert_eq!(
            (duplicate.added, duplicate.skipped, duplicate.view.revision),
            (0, 1, 1)
        );
        assert_eq!(
            duplicate
                .view
                .records
                .iter()
                .find(|record| record.id == qq.id)
                .unwrap()
                .note,
            "合成备注"
        );
        let bad = database.add_manual_exclusions("bad", "").unwrap();
        assert_eq!((bad.invalid, bad.view.revision), (1, 1));
        let removed = database.remove_manual_exclusions(&[qq.id, qq.id]).unwrap();
        assert_eq!(
            (removed.removed, removed.skipped, removed.view.revision),
            (1, 1, 2)
        );
        assert_eq!(
            database
                .remove_manual_exclusions(&[])
                .unwrap()
                .view
                .revision,
            2
        );
        assert_eq!(
            serde_json::to_value(removed.view).unwrap()["records"][0]["email"],
            "Name@example.test"
        );
    }

    #[test]
    fn mutations_rollback_records_and_revision_on_stale_id_sql_failure_and_revision_overflow() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let first = database
            .add_manual_exclusions("first@example.test", "")
            .unwrap();
        let id = first.view.records[0].id;
        assert!(database.remove_manual_exclusions(&[id, i64::MAX]).is_err());
        assert_eq!(database.manual_exclusions().unwrap().records.len(), 1);
        assert_eq!(database.manual_exclusions().unwrap().revision, 1);
        database.connection().unwrap().execute_batch("CREATE TRIGGER fail_manual_insert BEFORE INSERT ON manual_exclusions WHEN NEW.email='stop@example.test' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(database
            .add_manual_exclusions("second@example.test;stop@example.test", "")
            .is_err());
        assert_eq!(database.manual_exclusions().unwrap().records.len(), 1);
        assert_eq!(database.manual_exclusions().unwrap().revision, 1);
        database
            .connection()
            .unwrap()
            .execute(
                "UPDATE manual_exclusion_state SET revision=9223372036854775807",
                [],
            )
            .unwrap();
        assert!(database
            .add_manual_exclusions("third@example.test", "")
            .is_err());
        assert_eq!(database.manual_exclusions().unwrap().records.len(), 1);
        assert_eq!(
            database.manual_exclusions().unwrap().revision,
            i64::MAX as u64
        );
    }

    #[test]
    fn input_limits_are_checked_before_any_change() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        for (text, note) in [
            ("a@example.test".into(), "x".repeat(501)),
            ("a@example.test".into(), "bad\nnote".into()),
            ("x".repeat(512 * 1024 + 1), String::new()),
            ("a@example.test\n".repeat(10_001), String::new()),
        ] {
            assert!(database.add_manual_exclusions(&text, &note).is_err());
        }
        assert!(database.remove_manual_exclusions(&[0]).is_err());
        assert!(database.remove_manual_exclusions(&vec![1; 10_001]).is_err());
        assert_eq!(database.manual_exclusions().unwrap().revision, 0);
        assert!(database.manual_exclusions().unwrap().records.is_empty());
    }

    #[test]
    fn overlay_restores_only_original_eligible_members_and_does_not_persist_or_consume_snapshot_versions(
    ) {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = snapshot();
        database.save_snapshot(&original).unwrap();
        database
            .add_manual_exclusions("990101;990103;outside@example.test", "")
            .unwrap();
        let overlay = database.apply_manual_exclusions(&original).unwrap();
        assert_eq!(
            (
                overlay.source_count,
                overlay.excluded_count,
                overlay.manual_filtered_count,
                overlay.members.len()
            ),
            (3, 1, 1, 1)
        );
        assert_eq!(overlay.id, original.id);
        assert_eq!(overlay.finished_at, original.finished_at);
        assert_eq!(overlay.message, original.message);
        assert_eq!(
            overlay
                .filtered_members
                .iter()
                .filter(|item| item.reason == "manual")
                .count(),
            1
        );
        let repeated = database.apply_manual_exclusions(&overlay).unwrap();
        assert_eq!(repeated.manual_filtered_count, 1);
        let ids = database
            .manual_exclusions()
            .unwrap()
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        database.remove_manual_exclusions(&ids).unwrap();
        let restored = database.apply_manual_exclusions(&overlay).unwrap();
        assert_eq!(restored.members, original.members);
        assert_eq!(restored.filtered_members.len(), 1);
        assert_eq!(restored.manual_filtered_count, 0);
        assert!(restored
            .members
            .iter()
            .all(|member| member.qq_number != "990103"));
        assert_eq!(
            database
                .snapshot_by_id(&original.id)
                .unwrap()
                .unwrap()
                .manual_filtered_count,
            0
        );
        assert_eq!(
            database
                .connection()
                .unwrap()
                .query_row("SELECT count(*) FROM snapshots", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn all_excluded_overlay_is_valid_and_exports_manual_reason_and_metadata() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = verify::apply_friend_filter(snapshot(), &snapshot().account).unwrap();
        database.add_manual_exclusions("990101;990102", "").unwrap();
        let overlay = database.apply_manual_exclusions(&original).unwrap();
        assert!(overlay.members.is_empty());
        assert_eq!(overlay.manual_filtered_count, 2);
        verify::validate_friend_filter(&overlay).unwrap();
        let path = folder.path().join("manual.xlsx");
        crate::export::write_export(&overlay, &path, crate::export::ExportFormat::Xlsx).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut xml = String::new();
        use std::io::Read;
        zip.by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        assert!(xml.contains("手动排除"));
        assert!(xml.contains("手动排除人数"));
        assert!(xml.contains("手动排除版本"));
        let mut corrupted = overlay;
        corrupted.manual_filtered_count = 1;
        assert!(verify::validate_friend_filter(&corrupted).is_err());
    }

    #[test]
    fn preparation_excludes_before_unresolved_and_delivery_rechecks_atomically_without_creating_unknown(
    ) {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let blocked = "blocked@example.test".to_string();
        let valid = "valid@example.test".to_string();
        database
            .begin_delivery("old", std::slice::from_ref(&blocked))
            .unwrap();
        database
            .finish_delivery("old", std::slice::from_ref(&blocked), false, "合成未决")
            .unwrap();
        database.add_manual_exclusions(&blocked, "").unwrap();
        let prepared = database
            .prepare_mail_recipients_with_exclusions(&[blocked.clone(), valid.clone()], true)
            .unwrap();
        assert_eq!(prepared.recipients, vec![valid.clone()]);
        assert_eq!(prepared.excluded, vec![blocked.clone()]);
        assert_eq!(prepared.skipped_sent, 0);
        assert_eq!(prepared.exclusion_revision, 1);
        assert_eq!(
            database
                .prepare_mail_recipients(&[blocked.clone(), valid.clone()], false)
                .unwrap(),
            (vec![valid.clone()], 1)
        );
        assert_eq!(
            database
                .begin_delivery("quick", &[valid.clone(), blocked.clone()])
                .unwrap_err()
                .code,
            "manual_excluded"
        );
        assert!(!database.has_unresolved(&valid).unwrap());
        assert_eq!(database.unresolved_deliveries().unwrap().len(), 1);
        database.add_manual_exclusions(&valid, "").unwrap();
        let all = database
            .prepare_mail_recipients_with_exclusions(&[blocked.clone(), valid], true)
            .unwrap();
        assert!(all.recipients.is_empty());
        assert_eq!(all.excluded.len(), 2);
        let records = database.manual_exclusions().unwrap().records;
        let id = records
            .iter()
            .find(|record| record.email == blocked)
            .unwrap()
            .id;
        database.remove_manual_exclusions(&[id]).unwrap();
        assert_eq!(
            database
                .prepare_mail_recipients_with_exclusions(&[blocked], false)
                .unwrap_err()
                .code,
            "delivery_unresolved"
        );
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    /// 跨 SQL 分块仍保留输入顺序；同时命中历史与手动排除者只计手动，不猜测其他品牌邮箱。
    #[test]
    fn preparation_keeps_chunk_order_and_does_not_double_count_overlapping_history() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        database
            .import_sent("991001@qq.com\n991401@qq.com", "manual")
            .unwrap();
        database
            .add_manual_exclusions("991001@qq.com;991800@qq.com;991002@other.test", "")
            .unwrap();
        let candidates = (991000..=991800)
            .map(|number| format!("{number}@qq.com"))
            .chain(["991001@QQ.COM".into()])
            .collect::<Vec<_>>();
        let prepared = database
            .prepare_mail_recipients_with_exclusions(&candidates, true)
            .unwrap();
        assert_eq!(prepared.recipients.len(), 798);
        assert_eq!(prepared.skipped_sent, 1);
        assert_eq!(prepared.excluded, ["991001@qq.com", "991800@qq.com"]);
        assert_eq!(prepared.recipients[0], "991000@qq.com");
        assert_eq!(prepared.recipients[1], "991002@qq.com");
        assert_eq!(prepared.recipients.last().unwrap(), "991799@qq.com");
        let without_history = database
            .prepare_mail_recipients_with_exclusions(&candidates, false)
            .unwrap();
        assert_eq!(without_history.recipients.len(), 799);
        assert_eq!(without_history.skipped_sent, 0);
        assert_eq!(without_history.excluded.len(), 2);
    }

    /// 撤销手动排除只恢复本层候选；此前因发送历史移除的人不会重新进入成员名单。
    #[test]
    fn overlay_preserves_sent_priority_when_exclusions_are_removed() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = verify::apply_sent_filter(snapshot(), &HashSet::from(["990101".into()]));
        database.add_manual_exclusions("990101;990102", "").unwrap();
        let overlay = database.apply_manual_exclusions(&original).unwrap();
        assert_eq!(overlay.sent_filtered_count, 1);
        assert_eq!(overlay.manual_filtered_count, 1);
        assert!(overlay.members.is_empty());
        let ids = database
            .manual_exclusions()
            .unwrap()
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        database.remove_manual_exclusions(&ids).unwrap();
        let restored = database.apply_manual_exclusions(&overlay).unwrap();
        assert_eq!(restored.members, original.members);
        assert_eq!(restored.sent_filtered_count, 1);
        assert_eq!(restored.manual_filtered_count, 0);
        assert!(restored
            .filtered_members
            .iter()
            .any(|item| item.reason == "sent" && item.member.qq_number == "990101"));
    }

    #[test]
    fn schema_two_upgrade_preserves_data_and_failed_upgrade_rolls_back_tables_and_version() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("old.sqlite3");
        let database = Database::open(&path).unwrap();
        database.connection().unwrap().execute_batch("DROP TABLE manual_exclusions; DROP TABLE manual_exclusion_state; INSERT INTO settings VALUES('preserved','yes'); ALTER TABLE sent_history DROP COLUMN sender_address; ALTER TABLE sent_history DROP COLUMN sender_name; ALTER TABLE mail_delivery DROP COLUMN sender_address; ALTER TABLE mail_delivery DROP COLUMN sender_name; PRAGMA user_version=2;").unwrap();
        let upgraded = Database::open(&path).unwrap();
        assert_eq!(
            upgraded.setting("preserved").unwrap().as_deref(),
            Some("yes")
        );
        assert_eq!(upgraded.manual_exclusions().unwrap().revision, 0);
        assert_eq!(
            upgraded
                .connection()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        let invalid = folder.path().join("invalid.sqlite3");
        let connection = Connection::open(&invalid).unwrap();
        connection.execute_batch("CREATE TABLE preserved(value TEXT); INSERT INTO preserved VALUES('keep'); CREATE VIEW manual_exclusion_state AS SELECT 1 AS id,0 AS revision; PRAGMA user_version=2;").unwrap();
        drop(connection);
        assert!(Database::open(&invalid).is_err());
        let connection = Connection::open(&invalid).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='manual_exclusions'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(
            connection
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }
}
