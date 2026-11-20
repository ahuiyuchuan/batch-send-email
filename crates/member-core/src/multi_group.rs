//! 多目标群的固定组合键和原始名单合并；过滤仍由既有verify管线统一执行。作者：ahui。
//! 合并前逐群核验，首个群的成员展示资料优先，好友关系冲突降为unknown；绝不累加参考人数。

use crate::models::*;
use std::collections::{BTreeSet, HashMap};

/// 组合键只由去重、排序后的群ID构成，选择/搜索顺序不同仍读取同一账号的同组结果。
/// 单群返回原ID，兼容旧缓存；重复ID先去重，空集合、非法ID或去重后超过200项拒绝。
pub fn selection_key(ids: &[String]) -> AppResult<String> {
    let ids = ids.iter().collect::<BTreeSet<_>>();
    if ids.is_empty()
        || ids.len() > 200
        || ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(AppError::new(
            "group_selection",
            "请选择1到200个当前账号的群聊。",
        ));
    }
    if ids.len() == 1 {
        return Ok((*ids.first().unwrap()).clone());
    }
    Ok(format!(
        "multi:{}",
        ids.into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(",")
    ))
}

/// 合并同任务的未筛选目标；任一群为部分/待核验时合并结果保持该状态，不能以重合抵消缺失。
/// source_count为合并后的合法唯一QQ数，duplicate_count同时包含群内重复和跨群重合次数。
pub fn merge_targets(mut targets: Vec<Snapshot>, id: String) -> AppResult<Snapshot> {
    if targets.len() == 1 {
        return Ok(targets.remove(0));
    }
    let first = targets
        .first()
        .ok_or_else(|| AppError::new("group_selection", "请至少选择一个群聊。"))?;
    let account = first.account.clone();
    let key = selection_key(
        &targets
            .iter()
            .map(|target| target.group.id.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut merged = first.clone();
    merged.id = id;
    merged.group = Group {
        id: key,
        name: format!("{}个群聊", targets.len()),
        member_count: None,
        role: String::new(),
    };
    merged.members.clear();
    merged.source_groups.clear();
    merged.duplicate_count = 0;
    merged.invalid_count = 0;
    merged.warnings.clear();
    merged.reported_count = None;
    merged.status = "matched".into();
    let mut indexes = HashMap::new();
    let mut seen_groups = BTreeSet::new();
    for target in targets {
        if target.account != account
            || !seen_groups.insert(target.group.id.clone())
            || !target.source_groups.is_empty()
            || !target.filtered_members.is_empty()
            || target.exclude_friends
            || target.exclude_sent
        {
            return Err(AppError::new(
                "group_merge_input",
                "只能合并同一账号本次获取的原始群名单。",
            ));
        }
        if target.status == "partial" {
            merged.status = "partial".into();
        } else if target.status != "matched" && merged.status == "matched" {
            merged.status = "unverified".into();
        }
        merged.duplicate_count += target.duplicate_count;
        merged.invalid_count += target.invalid_count;
        merged.warnings.extend(
            target
                .warnings
                .into_iter()
                .map(|warning| format!("{}：{warning}", target.group.name)),
        );
        merged.source_groups.push(SnapshotSourceGroup {
            group: target.group,
            qq_numbers: target
                .members
                .iter()
                .map(|member| member.qq_number.clone())
                .collect(),
            reported_count: target.reported_count,
            status: target.status,
        });
        for member in target.members {
            if let Some(&index) = indexes.get(&member.qq_number) {
                let old: &mut Member = &mut merged.members[index];
                if old.friend_relation != member.friend_relation {
                    old.friend_relation = FriendRelation::Unknown;
                }
                merged.duplicate_count += 1;
            } else {
                indexes.insert(member.qq_number.clone(), merged.members.len());
                merged.members.push(member);
            }
        }
    }
    merged.source_count = merged.members.len();
    merged.friend_unknown_count = merged
        .members
        .iter()
        .filter(|member| member.friend_relation == FriendRelation::Unknown)
        .count();
    merged.message = format!(
        "已获取{}个群，合并去重后{}人；完整性按各群分别核验。",
        merged.source_groups.len(),
        merged.source_count
    );
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify;
    fn target(group: &str, qqs: &[&str], reported: usize) -> Snapshot {
        verify::create_snapshot(
            group.into(),
            Account {
                qq_number: "100100".into(),
                nickname: "合成账号".into(),
                source: "web".into(),
            },
            Group {
                id: group.into(),
                name: format!("合成群{group}"),
                member_count: Some(reported),
                role: "member".into(),
            },
            FetchResult {
                members: qqs
                    .iter()
                    .map(|qq| Member {
                        qq_number: qq.to_string(),
                        nickname: "测试".into(),
                        card: group.into(),
                        friend_relation: FriendRelation::NotFriend,
                    })
                    .collect(),
                reported_count: Some(reported),
                freshness_confirmed: true,
                truncated: false,
                warnings: vec![],
            },
            "2026-11-17T10:00:00+08:00".into(),
            "2026-11-17T10:00:01+08:00".into(),
        )
    }
    #[test]
    fn stable_keys_validate_bounds_and_do_not_depend_on_selection_order() {
        assert_eq!(
            selection_key(&["900002".into(), "900001".into(), "900002".into()]).unwrap(),
            "multi:900001,900002"
        );
        assert_eq!(selection_key(&["900001".into()]).unwrap(), "900001");
        for ids in [
            vec![],
            vec!["bad".into()],
            (0..201).map(|i| i.to_string()).collect(),
        ] {
            assert!(selection_key(&ids).is_err());
        }
    }
    #[test]
    fn merged_unique_counts_sources_filters_and_sqlite_scope_stay_consistent() {
        use crate::export::{default_filename, write_export, ExportFormat};
        use std::io::{Cursor, Read};

        let merged = merge_targets(
            vec![
                target("900001", &["100101", "100102"], 2),
                target("900002", &["100102", "100103"], 2),
            ],
            "merged".into(),
        )
        .unwrap();
        assert_eq!(
            (
                merged.source_count,
                merged.duplicate_count,
                merged.reported_count,
                merged.status.as_str()
            ),
            (3, 1, None, "matched")
        );
        assert_eq!(merged.source_groups[1].qq_numbers, ["100102", "100103"]);
        let filtered =
            verify::apply_exclusions(merged, vec![target("900003", &["100102"], 1)]).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database =
            crate::database::Database::open(directory.path().join("multi.sqlite3")).unwrap();
        database.save_snapshot(&filtered).unwrap();
        let stored = database
            .latest_group_snapshot("100100", "web", "multi:900001,900002")
            .unwrap()
            .unwrap();
        assert_eq!(stored.members.len(), 2);
        assert_eq!(stored.source_groups.len(), 2);
        assert!(database
            .validate_group_mail(
                "merged",
                &["100101".into(), "100103".into()],
                &["100101@qq.com".into(), "100103@qq.com".into()]
            )
            .is_ok());
        assert!(database
            .validate_group_mail(
                "merged",
                &["100101".into()],
                &["100101@qq.com".into(), "100102@qq.com".into()]
            )
            .is_err());
        let catalog = database.saved_group_catalog(&stored.account).unwrap();
        assert_eq!(catalog.len(), 2);
        assert!(catalog.iter().all(|g| !g.id.starts_with("multi:")));
        // 检查真实导出内容而非仅IPC：不导出重复/已过滤QQ，来源元数据不暴露内部组合键。
        let txt = directory.path().join("merged.txt");
        write_export(&stored, &txt, ExportFormat::Txt).unwrap();
        assert_eq!(std::fs::read(txt).unwrap(), b"100101\r\n100103\r\n");
        let csv = directory.path().join("merged.csv");
        write_export(&stored, &csv, ExportFormat::Csv).unwrap();
        let csv_bytes = std::fs::read(csv).unwrap();
        let mut reader = csv::Reader::from_reader(&csv_bytes[3..]);
        assert_eq!(reader.records().count(), 2);
        let xlsx = directory.path().join("merged.xlsx");
        write_export(&stored, &xlsx, ExportFormat::Xlsx).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(std::fs::read(xlsx).unwrap())).unwrap();
        let mut strings = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut strings)
            .unwrap();
        assert!(strings.contains("来源群"));
        assert!(strings.contains("合成群900001"));
        assert!(strings.contains("合成群900002"));
        assert!(!strings.contains("multi:900001,900002"));
        assert!(default_filename(&stored, ExportFormat::Xlsx).contains("多群合并"));
    }
    #[test]
    fn one_incomplete_source_cannot_be_masked_by_other_group_overlap() {
        let merged = merge_targets(
            vec![
                target("900001", &["100101"], 2),
                target("900002", &["100101", "100102"], 2),
            ],
            "m".into(),
        )
        .unwrap();
        assert_eq!(merged.status, "partial");
        assert_eq!(merged.source_count, 2);
        assert_eq!(merged.reported_count, None);
        assert!(verify::apply_exclusions(merged, vec![target("900002", &["100101"], 1)]).is_err());
    }
}
