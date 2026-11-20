//! 已过滤成员的本名单恢复。作者：ahui。
//! 只接受已保存快照中的号码，由核心生成新版本；不删除全局规则、不修改旧版、不访问 QQ/SMTP。

use crate::{
    database::Database,
    models::{AppError, AppResult, FriendRelation, Snapshot, SnapshotRestoration},
    verify,
};
use chrono::Local;
use std::collections::{HashMap, HashSet};

/// 恢复记录必须对应同一成员，允许之后被当前手动排除再次遮蔽；未知好友关系仍不能放行。
/// 此校验不把恢复记录计入目标人数，因为它是审计副本，而不是第二份成员。
pub(crate) fn validate_restoration(snapshot: &Snapshot) -> AppResult<()> {
    let Some(restoration) = &snapshot.restoration else {
        return Ok(());
    };
    // 按号码建立索引，使上万成员的整批恢复仍为线性检查，避免为每份审计记录重复遍历名单。
    let available = snapshot
        .members
        .iter()
        .chain(
            snapshot
                .filtered_members
                .iter()
                .filter(|entry| entry.reason == "manual")
                .map(|entry| &entry.member),
        )
        .map(|member| (member.qq_number.as_str(), member))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let invalid = restoration.members.iter().any(|item| {
        !matches!(item.reason.as_str(), "group" | "friend" | "sent")
            || !seen.insert(&item.member.qq_number)
            || (snapshot.exclude_friends && item.member.friend_relation == FriendRelation::Unknown)
            || (item.reason == "friend" && item.member.friend_relation != FriendRelation::Friend)
            || available.get(item.member.qq_number.as_str()).copied() != Some(&item.member)
    });
    if restoration.parent_snapshot_id.is_empty()
        || restoration.parent_snapshot_id == snapshot.id
        || restoration.members.is_empty()
        || chrono::DateTime::parse_from_rfc3339(&restoration.adjusted_at).is_err()
        || invalid
    {
        return Err(restoration_error(
            "名单恢复记录不一致，请重新获取名单后重试。",
        ));
    }
    Ok(())
}

impl Database {
    /// 按固定快照恢复指定过滤成员，成功返回叠加当前全局手动排除的新版本。
    ///
    /// snapshot_id 是已保存版本；qq_numbers 为 1..=10000 个数字号码，整批校验失败时不保存。
    /// expected_revision 是页面看到的手动排除版本，变化时拒绝旧确认；调用方须持有宿主操作锁。
    /// 原因 group/friend/sent 可恢复，manual 必须通过全局排除管理解除；发送时仍独立检查历史和未决投递。
    /// SQLite 保留原版和新版本；保存失败不返回乐观成功，也不改变当前群的最新记录。
    pub fn restore_filtered_members(
        &self,
        snapshot_id: &str,
        qq_numbers: &[String],
        expected_revision: u64,
    ) -> AppResult<Snapshot> {
        if snapshot_id.is_empty()
            || snapshot_id.len() > 128
            || qq_numbers.is_empty()
            || qq_numbers.len() > 10_000
            || qq_numbers
                .iter()
                .any(|qq| qq.is_empty() || qq.len() > 20 || !qq.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err(restoration_error("请选择需要恢复的已过滤成员。"));
        }
        let raw = self
            .snapshot_by_id(snapshot_id)?
            .ok_or_else(|| restoration_error("此版本已不在本机保留记录中，请重新获取名单。"))?;
        let current = self.apply_manual_exclusions(&raw)?;
        if current.exclusion_revision != expected_revision {
            return Err(restoration_error(
                "手动排除已更新，请刷新名单后重新确认恢复。",
            ));
        }
        verify::validate_friend_filter(&current)?;
        let chosen = qq_numbers
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let entries = raw
            .filtered_members
            .iter()
            .filter(|item| chosen.contains(item.member.qq_number.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if entries.len() != chosen.len()
            || entries
                .iter()
                .any(|item| !matches!(item.reason.as_str(), "group" | "friend" | "sent"))
        {
            return Err(restoration_error(
                "所选成员已不在此版本的可恢复列表中；手动排除请到排除名单中移除。",
            ));
        }
        let mut restored = raw.clone();
        restored.id = uuid::Uuid::new_v4().to_string();
        let mut audit = restored
            .restoration
            .take()
            .map(|value| value.members)
            .unwrap_or_default();
        for entry in entries {
            // 每人只有一个首次命中的过滤原因；累计审计副本不参与人数，防止恢复造成重复收件人。
            match entry.reason.as_str() {
                "group" => {
                    restored.excluded_count = restored
                        .excluded_count
                        .checked_sub(1)
                        .ok_or_else(|| restoration_error("群过滤计数不一致，请重新获取。"))?
                }
                "friend" => {
                    restored.friend_filtered_count =
                        restored
                            .friend_filtered_count
                            .checked_sub(1)
                            .ok_or_else(|| restoration_error("好友过滤计数不一致，请重新获取。"))?
                }
                "sent" => {
                    restored.sent_filtered_count = restored
                        .sent_filtered_count
                        .checked_sub(1)
                        .ok_or_else(|| restoration_error("已发送过滤计数不一致，请重新获取。"))?
                }
                _ => unreachable!("已校验过滤原因"),
            }
            restored.members.push(entry.member.clone());
            audit.push(entry);
        }
        restored
            .filtered_members
            .retain(|item| !chosen.contains(item.member.qq_number.as_str()));
        restored.members.sort_by(|a, b| {
            a.qq_number
                .len()
                .cmp(&b.qq_number.len())
                .then_with(|| a.qq_number.cmp(&b.qq_number))
        });
        restored.restoration = Some(SnapshotRestoration {
            parent_snapshot_id: raw.id,
            adjusted_at: Local::now().to_rfc3339(),
            members: audit,
        });
        verify::validate_friend_filter(&restored)?;
        let overlay = self.apply_manual_exclusions(&restored)?;
        // 只有完整派生和校验成功后才持久化；不能把手动叠加层误存成永久解除规则。
        self.save_snapshot(&restored)?;
        Ok(overlay)
    }
}

fn restoration_error(message: &str) -> AppError {
    AppError::new("snapshot_restoration", message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Account, FetchResult, Group, Member};

    /// 四位合成成员分别命中群重合（同时为好友）、好友、已发送和未过滤路径。
    /// 测试只用临时 SQLite，不读应用目录、不访问 QQ 或 SMTP。
    fn fixture() -> Snapshot {
        let account = Account {
            qq_number: "990000".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        };
        let make = |id: &str, numbers: &[u64]| {
            verify::create_snapshot(
                id.into(),
                account.clone(),
                Group {
                    id: id.into(),
                    name: "合成群".into(),
                    member_count: Some(numbers.len()),
                    role: "member".into(),
                },
                FetchResult {
                    members: numbers
                        .iter()
                        .map(|qq| Member {
                            qq_number: qq.to_string(),
                            nickname: "合成成员".into(),
                            card: String::new(),
                            friend_relation: if *qq <= 990102 {
                                FriendRelation::Friend
                            } else {
                                FriendRelation::NotFriend
                            },
                        })
                        .collect(),
                    reported_count: Some(numbers.len()),
                    freshness_confirmed: true,
                    truncated: false,
                    warnings: vec![],
                },
                "2026-11-09T00:00:00Z".into(),
                "2026-11-09T00:01:00Z".into(),
            )
        };
        let snapshot = verify::apply_exclusions(
            make("target", &[990101, 990102, 990103, 990104]),
            vec![make("exclude", &[990101])],
        )
        .unwrap();
        let snapshot = verify::apply_friend_filter(snapshot, &account).unwrap();
        verify::apply_sent_filter(snapshot, &HashSet::from(["990103".into()]))
    }

    #[test]
    fn restoration_is_immutable_persistent_and_backend_group_scope_uses_new_version() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("data.sqlite3")).unwrap();
        let original = fixture();
        db.save_snapshot(&original).unwrap();
        let qq = "990101".to_string();
        let email = "990101@qq.com".to_string();
        let restored = db
            .restore_filtered_members(&original.id, std::slice::from_ref(&qq), 0)
            .unwrap();
        assert_ne!(restored.id, original.id);
        assert_eq!(restored.finished_at, original.finished_at);
        assert_eq!(restored.excluded_count, 0);
        assert_eq!(restored.exclusions[0].removed_count, 1); // 原获取证据保持原值。
        assert_eq!(restored.members.len(), 2);
        assert_eq!(
            restored.restoration.as_ref().unwrap().members[0].reason,
            "group"
        );
        verify::validate_friend_filter(&restored).unwrap();
        assert!(db
            .validate_group_mail(
                &original.id,
                std::slice::from_ref(&qq),
                std::slice::from_ref(&email)
            )
            .is_err());
        db.validate_group_mail(&restored.id, &[qq], &[email])
            .unwrap();
        assert_eq!(
            db.snapshot_by_id(&original.id)
                .unwrap()
                .unwrap()
                .members
                .len(),
            1
        );
        assert_eq!(db.latest_snapshot().unwrap().unwrap().id, restored.id);
        let reopened = Database::open(dir.path().join("data.sqlite3")).unwrap();
        assert_eq!(
            reopened
                .latest_group_snapshot("990000", "web", "target")
                .unwrap()
                .unwrap()
                .id,
            restored.id
        );
        let path = dir.path().join("restored.xlsx");
        crate::export::write_export(&restored, &path, crate::export::ExportFormat::Xlsx).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut xml = String::new();
        use std::io::Read;
        zip.by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        assert!(xml.contains("本名单恢复人数"));
        assert!(xml.contains("已恢复成员"));
    }

    #[test]
    fn restore_friend_and_sent_keeps_global_exclusion_history_and_unresolved_independent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&fixture()).unwrap();
        db.import_sent("990103@qq.com", "manual").unwrap();
        db.add_manual_exclusions("990102@qq.com", "").unwrap();
        let restored = db
            .restore_filtered_members("target", &["990102".into(), "990103".into()], 1)
            .unwrap();
        assert_eq!(restored.manual_filtered_count, 1);
        assert_eq!(restored.friend_filtered_count, 0);
        assert_eq!(restored.sent_filtered_count, 0);
        verify::validate_friend_filter(&restored).unwrap();
        let prepared = db
            .prepare_mail_recipients_with_exclusions(
                &[
                    "990102@qq.com".into(),
                    "990103@qq.com".into(),
                    "990104@qq.com".into(),
                ],
                true,
            )
            .unwrap();
        assert_eq!(prepared.recipients, ["990104@qq.com"]);
        assert_eq!(prepared.skipped_sent, 1);
        assert_eq!(prepared.excluded, ["990102@qq.com"]);
        let ids = db
            .manual_exclusions()
            .unwrap()
            .records
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        db.remove_manual_exclusions(&ids).unwrap();
        let overlay = db.apply_manual_exclusions(&restored).unwrap();
        assert_eq!(overlay.members.len(), 3);
        verify::validate_friend_filter(&overlay).unwrap();
        db.begin_delivery("synthetic", &["990102@qq.com".into()])
            .unwrap();
        assert!(db
            .prepare_mail_recipients_with_exclusions(&["990102@qq.com".into()], false)
            .is_err());
        assert_eq!(db.sent_count().unwrap(), 1);
    }

    #[test]
    fn stale_manual_invalid_and_partial_requests_do_not_save_or_change_original() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&fixture()).unwrap();
        db.add_manual_exclusions("990104", "").unwrap();
        for (id, qqs, revision) in [
            ("target", vec!["990101".into()], 0),
            ("target", vec!["990104".into()], 1),
            ("target", vec!["990101".into(), "missing".into()], 1),
            ("target", vec!["990101".into(), "990999".into()], 1),
            ("target", vec![], 1),
            ("missing", vec!["990101".into()], 1),
        ] {
            assert!(db.restore_filtered_members(id, &qqs, revision).is_err());
        }
        assert_eq!(db.latest_snapshot().unwrap().unwrap().id, "target");
        assert_eq!(
            db.snapshot_by_id("target")
                .unwrap()
                .unwrap()
                .filtered_members
                .len(),
            3
        );
    }

    #[test]
    fn save_failure_rolls_back_and_historical_branch_preserves_newer_group() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("data.sqlite3")).unwrap();
        db.save_snapshot(&fixture()).unwrap();
        db.connection().unwrap().execute_batch("CREATE TRIGGER reject_restore BEFORE INSERT ON snapshots WHEN NEW.id!='target' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(db
            .restore_filtered_members("target", &["990101".into()], 0)
            .is_err());
        assert_eq!(
            db.latest_group_snapshot("990000", "web", "target")
                .unwrap()
                .unwrap()
                .id,
            "target"
        );
        db.connection()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_restore")
            .unwrap();
        let mut newer = fixture();
        newer.id = "newer".into();
        newer.finished_at = "2026-11-09T00:02:00Z".into();
        db.save_snapshot(&newer).unwrap();
        let branch = db
            .restore_filtered_members("target", &["990101".into()], 0)
            .unwrap();
        assert_eq!(
            db.latest_group_snapshot("990000", "web", "target")
                .unwrap()
                .unwrap()
                .id,
            "newer"
        );
        let again = db
            .restore_filtered_members(&branch.id, &["990102".into()], 0)
            .unwrap();
        assert_eq!(again.restoration.as_ref().unwrap().members.len(), 2);
        verify::validate_friend_filter(&again).unwrap();
        assert_eq!(
            db.latest_group_snapshot("990000", "web", "target")
                .unwrap()
                .unwrap()
                .id,
            "newer"
        );
    }

    #[test]
    fn invalid_restoration_cannot_bypass_friend_validation_and_old_json_defaults_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("data.sqlite3")).unwrap();
        let original = fixture();
        db.save_snapshot(&original).unwrap();
        let restored = db
            .restore_filtered_members("target", &["990101".into()], 0)
            .unwrap();
        let mut invalid = restored.clone();
        invalid.restoration.as_mut().unwrap().members[0]
            .member
            .friend_relation = FriendRelation::Unknown;
        assert!(verify::validate_friend_filter(&invalid).is_err());
        let mut invalid = restored.clone();
        invalid.restoration = None;
        assert!(verify::validate_friend_filter(&invalid).is_err());
        let mut invalid = restored.clone();
        let item = invalid.restoration.as_ref().unwrap().members[0].clone();
        invalid.restoration.as_mut().unwrap().members.push(item);
        assert!(verify::validate_friend_filter(&invalid).is_err());
        let old = serde_json::to_value(original).unwrap();
        assert!(old.get("restoration").is_none());
        assert!(serde_json::from_value::<Snapshot>(old)
            .unwrap()
            .restoration
            .is_none());
    }
}
