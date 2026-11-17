//! 本次结果的清洗与人数核验。作者：ahui。
//! 核验只说明已知人数条件，不承诺获取期间群成员没有等量进出。

use crate::models::{
    Account, AppError, AppResult, ExclusionSummary, FetchResult, FilteredMember, FriendList,
    FriendRelation, Group, Member, Snapshot,
};
use std::collections::{HashMap, HashSet};

/// 创建绑定账号、群和时间的不可变结果，供预览和导出共同使用。
///
/// `id` 是调用方生成的任务标识；时间使用含偏移的 RFC 3339 字符串。
/// 成员只按数字 QQ 号去重，昵称和名片保留第一条原文；好友关系冲突时改为未知，避免错误排除。
/// 群列表人数只可作参考，不能代替成员响应缺失的实时总人数证明数量一致。
pub fn create_snapshot(
    id: String,
    account: Account,
    group: Group,
    result: FetchResult,
    started_at: String,
    finished_at: String,
) -> Snapshot {
    let mut seen = HashMap::<String, usize>::new();
    let mut members = Vec::<Member>::with_capacity(result.members.len());
    let mut duplicate_count = 0;
    let mut invalid_count = 0;

    // 只规范化号码的数字编码。不得从昵称、UID 或其他字段推断缺失 QQ 号。
    for mut member in result.members {
        let Some(number) = normalize_number(&member.qq_number) else {
            invalid_count += 1;
            continue;
        };
        if let Some(index) = seen.get(&number) {
            duplicate_count += 1;
            if members[*index].friend_relation != member.friend_relation {
                members[*index].friend_relation = FriendRelation::Unknown;
            }
            continue;
        }
        seen.insert(number.clone(), members.len());
        member.qq_number = number;
        members.push(member);
    }
    members.sort_by(|left, right| {
        left.qq_number
            .len()
            .cmp(&right.qq_number.len())
            .then_with(|| left.qq_number.cmp(&right.qq_number))
    });

    let count = members.len();
    let friend_unknown_count = members
        .iter()
        .filter(|member| member.friend_relation == FriendRelation::Unknown)
        .count();
    let reported_count = result.reported_count.or(group.member_count);
    let count_conflict = matches!(
        (result.reported_count, group.member_count),
        (Some(current), Some(previous)) if current != previous
    );
    let mut warnings = result.warnings;
    if result.reported_count.is_none() && group.member_count.is_some() {
        warnings
            .push("参考人数来自群列表，成员响应未提供独立总人数，不能据此确认名单完整。".into());
    }
    if count_conflict {
        warnings.push(
            "群列表人数与本次成员响应人数不同，获取期间可能发生成员变化，请重新核验。".into(),
        );
    }
    if !result.freshness_confirmed {
        warnings.push("来源无法确认本次名单已完全刷新，可能包含缓存结果。".into());
    }
    if duplicate_count > 0 {
        warnings.push(format!("本次已去掉 {duplicate_count} 条重复 QQ 号记录。"));
    }
    if invalid_count > 0 {
        warnings.push(format!(
            "本次有 {invalid_count} 条记录缺少有效数字 QQ 号，已从名单中排除。"
        ));
    }

    let (status, message) = if count == 0 {
        if result.truncated || invalid_count > 0 || reported_count.is_some_and(|total| total > 0) {
            (
                "partial",
                "未取得可导出的有效成员，当前结果不完整，请重新扫码登录后获取，并检查当前账号权限。".into(),
            )
        } else {
            (
                "unverified",
                "没有取得有效成员，不能据此认定群里没有成员，请检查登录、权限和连接。".into(),
            )
        }
    } else if result.truncated || invalid_count > 0 {
        (
            "partial",
            format!("已获取 {count} 人，但读取存在中断或无效号码，名单不完整。"),
        )
    } else if count_conflict {
        (
            "unverified",
            format!("已获取 {count} 人，参考人数发生变化，完整性待核验。"),
        )
    } else if reported_count.is_some_and(|total| count < total) {
        let total = reported_count.unwrap_or_default();
        (
            "partial",
            format!(
                "已获取 {count} / {total} 人，与参考人数相差 {} 人。",
                total - count
            ),
        )
    } else if result.reported_count.is_none() {
        (
            "unverified",
            format!("已获取 {count} 人，缺少本次成员响应的独立总人数，完整性待核验。"),
        )
    } else if !result.freshness_confirmed {
        (
            "unverified",
            format!("已获取 {count} 人，名单刷新状态无法确认，完整性待核验。"),
        )
    } else if reported_count.is_some_and(|total| count > total) {
        (
            "unverified",
            format!("已获取 {count} 人，超过参考人数，可能存在人数变化，请重新核验。"),
        )
    } else {
        (
            "matched",
            format!("数量一致：{count} / {count} 人。名单以本次获取时为准。"),
        )
    };

    Snapshot {
        id,
        account,
        group,
        source_groups: vec![],
        started_at,
        finished_at,
        source_count: count,
        excluded_count: 0,
        exclusions: vec![],
        exclude_friends: false,
        friend_filtered_count: 0,
        friend_unknown_count,
        friend_filter_status: "not_requested".into(),
        sent_filtered_count: 0,
        manual_filtered_count: 0,
        exclusion_revision: 0,
        filtered_members: vec![],
        restoration: None,
        exclude_sent: false,
        members,
        reported_count,
        status: status.into(),
        message,
        duplicate_count,
        invalid_count,
        warnings,
    }
}

/// 将本次账号的已校验好友集合绑定到原始目标群；只保存本群关系，不把完整好友名单带入快照。
/// 必须先完成原始人数核验且尚未筛选；覆盖未验证的fr标记，之后仍按群→好友→历史顺序过滤。
/// 这里信任提供方已经检查好友响应契约，另校验账号/来源与所有数字标识，防止跨账号或损坏集合混用。
pub fn bind_friend_list(mut target: Snapshot, friends: &FriendList) -> AppResult<Snapshot> {
    if target.account.qq_number != friends.account.qq_number
        || target.account.source != friends.account.source
    {
        return Err(AppError::new(
            "friend_account_mismatch",
            "好友列表与当前 QQ 账号不一致，请重新获取名单。",
        ));
    }
    if target.exclude_friends
        || target.exclude_sent
        || !target.exclusions.is_empty()
        || target.excluded_count != 0
        || target.sent_filtered_count != 0
        || target.friend_filtered_count != 0
        || !target.filtered_members.is_empty()
        || target.friend_filter_status != "not_requested"
        || target.source_count == 0
        || target.source_count != target.members.len()
    {
        return Err(AppError::new(
            "invalid_friend_filter_state",
            "请重新获取目标群后再排除好友，不能更改已筛选名单的好友关系。",
        ));
    }
    if friends
        .qq_numbers
        .iter()
        .any(|number| normalize_number(number).as_deref() != Some(number.as_str()))
    {
        return Err(AppError::new(
            "friend_list_invalid",
            "QQ 好友列表包含无法识别的号码，本次名单未更新。",
        ));
    }
    for member in &mut target.members {
        member.friend_relation = if friends.qq_numbers.contains(&member.qq_number) {
            FriendRelation::Friend
        } else {
            FriendRelation::NotFriend
        };
    }
    target.friend_unknown_count = 0;
    Ok(target)
}

/// 从目标群中移除同时出现在一个或多个排除群里的 QQ 号，不按昵称或名片匹配。
///
/// 所有快照必须来自同一账号、同一来源且是未经过排除的原始群结果；目标群不能排除自身。
/// 排除群按输入顺序去重，摘要只记录每个群新移除的人数，跨群重合不重复计数。
/// 排除名单未核验时不能宣称已排除全部重合成员；明确不完整的目标状态仍保留。
pub fn apply_exclusions(mut target: Snapshot, exclusions: Vec<Snapshot>) -> AppResult<Snapshot> {
    if exclusions.is_empty() {
        return Ok(target);
    }
    if target.source_count == 0 {
        return Err(AppError::new(
            "empty_target",
            "目标群未取得有效名单，无法执行排除。",
        ));
    }
    if target.excluded_count != 0
        || !target.exclusions.is_empty()
        || target.exclude_friends
        || target.friend_filtered_count != 0
        || target.friend_filter_status != "not_requested"
        || target.sent_filtered_count != 0
        || !target.filtered_members.is_empty()
        || target.source_count != target.members.len()
    {
        return Err(AppError::new(
            "invalid_exclusion_state",
            "请重新获取目标群后再执行排除，不能重复处理已筛选结果。",
        ));
    }

    let mut seen_groups = HashSet::new();
    let mut remaining = target
        .members
        .iter()
        .map(|member| member.qq_number.clone())
        .collect::<HashSet<_>>();
    let original_members = target
        .members
        .iter()
        .map(|member| (member.qq_number.clone(), member.clone()))
        .collect::<HashMap<_, _>>();
    let mut unverified_exclusion = false;
    for exclusion in exclusions {
        if exclusion.account.qq_number != target.account.qq_number
            || exclusion.account.source != target.account.source
        {
            return Err(AppError::new(
                "exclusion_account_mismatch",
                "排除群必须与目标群使用同一个 QQ 账号和获取方式。",
            ));
        }
        if exclusion.group.id == target.group.id
            || target
                .source_groups
                .iter()
                .any(|source| source.group.id == exclusion.group.id)
        {
            return Err(AppError::new(
                "same_exclusion_group",
                "不能将目标群自身设为排除群，请选择其他群。",
            ));
        }
        if exclusion.excluded_count != 0
            || !exclusion.exclusions.is_empty()
            || exclusion.exclude_friends
            || exclusion.friend_filtered_count != 0
            || exclusion.friend_filter_status != "not_requested"
            || exclusion.sent_filtered_count != 0
            || !exclusion.filtered_members.is_empty()
        {
            return Err(AppError::new(
                "filtered_exclusion_group",
                "排除群需要使用原始获取名单，不能使用已筛选结果。",
            ));
        }
        if !seen_groups.insert(exclusion.group.id.clone()) {
            continue;
        }
        let mut removed_count = 0;
        let numbers = exclusion
            .members
            .iter()
            .filter_map(|member| normalize_number(&member.qq_number))
            .collect::<HashSet<_>>();
        for number in &numbers {
            if remaining.remove(number) {
                removed_count += 1;
                if let Some(member) = original_members.get(number) {
                    target.filtered_members.push(FilteredMember {
                        member: member.clone(),
                        reason: "group".into(),
                        detail: if exclusion.group.name.is_empty() {
                            format!("群号 {}", exclusion.group.id)
                        } else {
                            exclusion.group.name.clone()
                        },
                    });
                }
            }
        }
        unverified_exclusion |= exclusion.status != "matched";
        target.exclusions.push(ExclusionSummary {
            group: exclusion.group,
            acquired_count: numbers.len(),
            reported_count: exclusion.reported_count,
            status: exclusion.status,
            removed_count,
        });
        // 获取起止时间覆盖整个组合任务，保留参考群读取所需的时间区间。
        if let (Ok(current), Ok(candidate)) = (
            chrono::DateTime::parse_from_rfc3339(&target.finished_at),
            chrono::DateTime::parse_from_rfc3339(&exclusion.finished_at),
        ) {
            if candidate > current {
                target.finished_at = exclusion.finished_at;
            }
        }
    }
    target
        .members
        .retain(|member| remaining.contains(&member.qq_number));
    target.excluded_count = target.source_count - target.members.len();
    sort_filtered_members(&mut target.filtered_members);
    if unverified_exclusion {
        if target.status == "matched" {
            target.status = "unverified".into();
        }
        target
            .warnings
            .push("排除名单未完成核验，可能还有重合成员未排除。".into());
    }
    let original_message = if target.status == "matched" {
        format!("目标群数量核验一致，共 {} 人。", target.source_count)
    } else {
        target.message.clone()
    };
    target.message = format!(
        "{original_message} 已按 QQ 号排除 {} 人，剩余 {} 人。{}",
        target.excluded_count,
        target.members.len(),
        if unverified_exclusion {
            "排除名单仍待核验。"
        } else {
            "名单以本次获取时为准。"
        },
    );
    Ok(target)
}

/// 按本次获取账号的关系排除好友；仅允许在群重合过滤后、历史已发送过滤前调用。
///
/// `expected_account` 必须是宿主为当前任务实时核实的账号，号码和来源均须与快照一致。
/// 原始目标的每个人（包括已因群重合移除的人）都必须有明确关系，否则返回错误而不产生新快照。
/// 已完成的相同过滤可重复调用且不重复计数；本函数不查询好友列表、不访问数据库或发信。
pub fn apply_friend_filter(
    mut target: Snapshot,
    expected_account: &Account,
) -> AppResult<Snapshot> {
    if target.account.qq_number != expected_account.qq_number
        || target.account.source != expected_account.source
    {
        return Err(AppError::new(
            "friend_account_mismatch",
            "好友关系与当前 QQ 账号不一致，请重新获取名单。",
        ));
    }
    if target.exclude_friends {
        validate_friend_filter(&target)?;
        return Ok(target);
    }
    if target.source_count == 0
        || target.exclude_sent
        || target.sent_filtered_count != 0
        || target.friend_filtered_count != 0
        || target.friend_filter_status != "not_requested"
        || target
            .filtered_members
            .iter()
            .any(|item| item.reason != "group")
        || target.source_count != target.members.len() + target.filtered_members.len()
    {
        return Err(AppError::new(
            "invalid_friend_filter_state",
            "请重新获取目标群后再排除好友，不能调整已筛选名单的过滤顺序。",
        ));
    }

    // 检查整个目标群，不能借由先排除某个群而掩盖接口缺失的好友关系。
    let unknown_count = target
        .members
        .iter()
        .chain(target.filtered_members.iter().map(|item| &item.member))
        .filter(|member| member.friend_relation == FriendRelation::Unknown)
        .count();
    if unknown_count > 0 {
        return Err(AppError::new(
            "friend_relation_unknown",
            format!(
                "有 {unknown_count} 位成员的好友关系无法确认，未更新名单。请重新获取，或取消“排除好友”后重试。"
            ),
        ));
    }

    let before = target.members.len();
    let mut retained = Vec::with_capacity(before);
    for member in std::mem::take(&mut target.members) {
        if member.friend_relation == FriendRelation::Friend {
            target.filtered_members.push(FilteredMember {
                member,
                reason: "friend".into(),
                detail: "已是获取账号的 QQ 好友".into(),
            });
        } else {
            retained.push(member);
        }
    }
    target.members = retained;
    target.exclude_friends = true;
    target.friend_filtered_count = before - target.members.len();
    target.friend_unknown_count = 0;
    target.friend_filter_status = "completed".into();
    sort_filtered_members(&mut target.filtered_members);
    target.message = format!(
        "{} 已排除好友 {} 人，剩余 {} 人。",
        target.message,
        target.friend_filtered_count,
        target.members.len(),
    );
    validate_friend_filter(&target)?;
    Ok(target)
}

/// 导出与邮件绑定共用的好友过滤检查，防止未完成或不一致的快照被当成最终名单使用。
///
/// 启用后必须已完成、无未知关系；仅用户明确恢复且留有原明细的成员可作为本版本例外。
/// 本函数只检查固定快照，不重新解释其他账号的好友关系，也不访问网络或修改数据。
pub fn validate_friend_filter(snapshot: &Snapshot) -> AppResult<()> {
    validate_manual_filter(snapshot)?;
    crate::snapshot_restoration::validate_restoration(snapshot)?;
    if !snapshot.exclude_friends {
        return Ok(());
    }
    let restored = snapshot
        .restoration
        .as_ref()
        .map(|value| {
            value
                .members
                .iter()
                .map(|item| item.member.qq_number.as_str())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let invalid_relationship = snapshot.members.iter().any(|member| {
        member.friend_relation != FriendRelation::NotFriend
            && !restored.contains(member.qq_number.as_str())
    }) || snapshot.filtered_members.iter().any(|item| {
        item.member.friend_relation == FriendRelation::Unknown
            || (item.reason == "friend" && item.member.friend_relation != FriendRelation::Friend)
            || (matches!(item.reason.as_str(), "sent" | "manual")
                && item.member.friend_relation != FriendRelation::NotFriend
                && !restored.contains(item.member.qq_number.as_str()))
    });
    let friend_count = snapshot
        .filtered_members
        .iter()
        .filter(|item| item.reason == "friend")
        .count();
    let group_count = snapshot
        .filtered_members
        .iter()
        .filter(|item| item.reason == "group")
        .count();
    let sent_count = snapshot
        .filtered_members
        .iter()
        .filter(|item| item.reason == "sent")
        .count();
    let mut unique_numbers = HashSet::new();
    let repeated_number = snapshot
        .members
        .iter()
        .chain(snapshot.filtered_members.iter().map(|item| &item.member))
        .any(|member| !unique_numbers.insert(member.qq_number.as_str()));
    if snapshot.friend_filter_status != "completed"
        || snapshot.friend_unknown_count != 0
        || snapshot.friend_filtered_count != friend_count
        || snapshot.excluded_count != group_count
        || snapshot.sent_filtered_count != sent_count
        || snapshot.filtered_members.len()
            != friend_count + group_count + sent_count + snapshot.manual_filtered_count
        || snapshot.source_count != snapshot.members.len() + snapshot.filtered_members.len()
        || repeated_number
        || invalid_relationship
    {
        return Err(AppError::new(
            "friend_filter_incomplete",
            "好友过滤尚未完成或名单状态不一致，请重新获取后再导出或写邮件。",
        ));
    }
    Ok(())
}

/// 手动叠加层必须保持来源人数与互斥过滤明细一致；不查询实时表，实际版本由宿主在操作前复核。
/// 老快照没有该层时不额外改变兼容语义；含manual时拒绝伪造计数、重复成员或未知过滤原因。
pub fn validate_manual_filter(snapshot: &Snapshot) -> AppResult<()> {
    let manual_count = snapshot
        .filtered_members
        .iter()
        .filter(|item| item.reason == "manual")
        .count();
    if manual_count == 0 && snapshot.manual_filtered_count == 0 && snapshot.restoration.is_none() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let invalid = snapshot
        .members
        .iter()
        .chain(snapshot.filtered_members.iter().map(|item| &item.member))
        .any(|member| !seen.insert(member.qq_number.as_str()));
    let counts = |reason: &str| {
        snapshot
            .filtered_members
            .iter()
            .filter(|item| item.reason == reason)
            .count()
    };
    if manual_count != snapshot.manual_filtered_count
        || counts("group") != snapshot.excluded_count
        || counts("friend") != snapshot.friend_filtered_count
        || counts("sent") != snapshot.sent_filtered_count
        || snapshot.source_count != snapshot.members.len() + snapshot.filtered_members.len()
        || snapshot
            .filtered_members
            .iter()
            .any(|item| !matches!(item.reason.as_str(), "group" | "friend" | "sent" | "manual"))
        || invalid
    {
        return Err(AppError::new(
            "manual_filter_incomplete",
            "手动排除名单状态不一致，请刷新当前名单后重试。",
        ));
    }
    Ok(())
}

/// 从剩余名单中移除本机数据库确认已发送的 QQ 号，保留可查看的过滤记录。
///
/// `sent_qq_numbers` 应由宿主从本机全局成功发送记录查询，失败/取消的投递不应出现在集合中。
/// 本函数不访问数据库、不发送邮件；同一人已因群重合或好友关系被移除时不会重复计数。
/// 可重复调用，只有新移除的人才会增加计数，始终保持原始人数等于剩余人数加过滤人数。
pub fn apply_sent_filter(mut target: Snapshot, sent_qq_numbers: &HashSet<String>) -> Snapshot {
    let normalized = sent_qq_numbers
        .iter()
        .filter_map(|number| normalize_number(number))
        .collect::<HashSet<_>>();
    let before = target.members.len();
    let mut retained = Vec::with_capacity(before);
    for member in std::mem::take(&mut target.members) {
        if normalized.contains(&member.qq_number) {
            target.filtered_members.push(FilteredMember {
                member,
                reason: "sent".into(),
                detail: "本机已有确认成功的发送记录".into(),
            });
        } else {
            retained.push(member);
        }
    }
    target.members = retained;
    let removed = before - target.members.len();
    target.sent_filtered_count += removed;
    let first_application = !target.exclude_sent;
    target.exclude_sent = true;
    sort_filtered_members(&mut target.filtered_members);
    if first_application || removed > 0 {
        target.message = format!(
            "{} 已过滤历史已发送 {} 人，最终剩余 {} 人。",
            target.message,
            target.sent_filtered_count,
            target.members.len(),
        );
    }
    target
}

/// 已过滤表按号码数值顺序排列；使用字符串长度比较以兼容超过 u64 精度的标识。
fn sort_filtered_members(members: &mut [FilteredMember]) {
    members.sort_by(|left, right| {
        left.member
            .qq_number
            .len()
            .cmp(&right.member.qq_number.len())
            .then_with(|| left.member.qq_number.cmp(&right.member.qq_number))
    });
}

/// 验证数字标识并消除纯编码的前导零；20 位上限用于限制异常来源值，不是 QQ 位数业务规则。
fn normalize_number(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let canonical = value.trim_start_matches('0');
    (!canonical.is_empty()).then(|| canonical.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(number: &str) -> Member {
        Member {
            qq_number: number.into(),
            nickname: "中文🙂".into(),
            card: "群名片".into(),
            friend_relation: FriendRelation::Unknown,
        }
    }

    fn snapshot(result: FetchResult, group_count: Option<usize>) -> Snapshot {
        create_snapshot(
            "test-task".into(),
            Account {
                qq_number: "10001".into(),
                nickname: "测试账号".into(),
                source: "web".into(),
            },
            Group {
                id: "123456".into(),
                name: "测试群".into(),
                member_count: group_count,
                role: "member".into(),
            },
            result,
            "2026-11-05T06:00:00Z".into(),
            "2026-11-05T06:00:01Z".into(),
        )
    }

    fn result(members: Vec<Member>, total: Option<usize>) -> FetchResult {
        FetchResult {
            members,
            reported_count: total,
            freshness_confirmed: true,
            truncated: false,
            warnings: vec![],
        }
    }

    #[test]
    fn validates_1688_unique_members_without_numeric_precision_loss() {
        let members = (1..=1688)
            .map(|index| member(&(10_000_000_000u64 + index).to_string()))
            .collect();
        let view = snapshot(result(members, Some(1688)), Some(1688));
        assert_eq!(view.status, "matched");
        assert_eq!(view.members.len(), 1688);
        assert_eq!(view.members[0].qq_number, "10000000001");
    }

    #[test]
    fn missing_total_never_falls_back_to_member_length() {
        let view = snapshot(result(vec![member("123")], None), None);
        assert_eq!(view.reported_count, None);
        assert_eq!(view.status, "unverified");
        let list_only = snapshot(result(vec![member("123")], None), Some(1));
        assert_eq!(list_only.reported_count, Some(1));
        assert_eq!(list_only.status, "unverified");
        assert!(list_only
            .warnings
            .iter()
            .any(|warning| warning.contains("群列表")));
    }

    #[test]
    fn canonical_numbers_deduplicate_without_altering_names() {
        let mut first = member(" 000123 ");
        first.nickname = "=原始昵称\n🙂".into();
        let view = snapshot(
            result(vec![first, member("123"), member("9")], Some(2)),
            Some(2),
        );
        assert_eq!(view.duplicate_count, 1);
        assert_eq!(view.invalid_count, 0);
        assert_eq!(view.members[0].qq_number, "9");
        assert_eq!(view.members[1].nickname, "=原始昵称\n🙂");
        assert_eq!(view.status, "matched");
    }

    #[test]
    fn invalid_numbers_are_excluded_and_mark_result_partial() {
        let invalid = [
            "",
            "0",
            "000",
            "uid_123",
            "１２３",
            "+123",
            "123456789012345678901",
        ];
        let mut members = invalid
            .iter()
            .map(|number| member(number))
            .collect::<Vec<_>>();
        members.push(member("12345678901234567890"));
        let view = snapshot(result(members, Some(1)), None);
        assert_eq!(view.invalid_count, invalid.len());
        assert_eq!(view.members.len(), 1);
        assert_eq!(view.status, "partial");
    }

    #[test]
    fn conflicting_counts_and_stale_cache_cannot_match() {
        let view = snapshot(result(vec![member("123")], Some(1)), Some(2));
        assert_eq!(view.status, "unverified");
        let mut stale = result(vec![member("123")], Some(1));
        stale.freshness_confirmed = false;
        assert_eq!(snapshot(stale, Some(1)).status, "unverified");
    }

    #[test]
    fn short_and_interrupted_results_are_partial() {
        let short = snapshot(result(vec![member("123")], Some(2)), Some(2));
        assert_eq!(short.status, "partial");
        assert!(short.message.contains("相差 1"));
        let mut interrupted = result(vec![member("123")], Some(1));
        interrupted.truncated = true;
        assert_eq!(snapshot(interrupted, Some(1)).status, "partial");
    }

    #[test]
    fn empty_and_excess_results_are_never_success() {
        assert_eq!(snapshot(result(vec![], None), None).status, "unverified");
        assert_eq!(snapshot(result(vec![], Some(1)), Some(1)).status, "partial");
        assert_eq!(
            snapshot(result(vec![member("123"), member("456")], Some(1)), Some(1)).status,
            "unverified"
        );
    }

    fn group_snapshot(group_id: &str, numbers: impl IntoIterator<Item = u64>) -> Snapshot {
        let members = numbers
            .into_iter()
            .map(|number| member(&number.to_string()))
            .collect::<Vec<_>>();
        let count = members.len();
        let mut view = snapshot(result(members, Some(count)), Some(count));
        view.group.id = group_id.into();
        view
    }

    #[test]
    fn exclusion_removes_only_intersection_by_qq_number() {
        let target = group_snapshot("100", 1000..2000);
        let excluded = group_snapshot("200", (1000..1020).chain(5000..5180));
        let view = apply_exclusions(target, vec![excluded]).unwrap();
        assert_eq!(view.source_count, 1000);
        assert_eq!(view.excluded_count, 20);
        assert_eq!(view.members.len(), 980);
        assert_eq!(view.exclusions[0].acquired_count, 200);
        assert_eq!(view.exclusions[0].removed_count, 20);
        assert_eq!(view.filtered_members.len(), 20);
        assert_eq!(view.filtered_members[0].reason, "group");
        assert_eq!(view.status, "matched");
        // 所有测试成员昵称相同，因此剩余 980 人也证明不按昵称匹配。
        assert!(view
            .members
            .iter()
            .all(|member| member.nickname == "中文🙂"));
    }

    #[test]
    fn duplicate_exclusion_groups_and_overlapping_people_are_counted_once() {
        let target = group_snapshot("100", 1000..1010);
        let first = group_snapshot("200", 1000..1003);
        let second = group_snapshot("300", 1002..1005);
        let view = apply_exclusions(target, vec![first.clone(), first, second]).unwrap();
        assert_eq!(view.excluded_count, 5);
        assert_eq!(view.exclusions.len(), 2);
        assert_eq!(view.exclusions[0].removed_count, 3);
        assert_eq!(view.exclusions[1].removed_count, 2);
    }

    #[test]
    fn all_excluded_is_a_valid_empty_result_with_original_provenance() {
        let target = group_snapshot("100", 1000..1003);
        let excluded = group_snapshot("200", 1000..1003);
        let view = apply_exclusions(target, vec![excluded]).unwrap();
        assert!(view.members.is_empty());
        assert_eq!(view.source_count, 3);
        assert_eq!(view.excluded_count, 3);
        assert_eq!(view.status, "matched");
    }

    #[test]
    fn unverified_exclusion_downgrades_matched_target_and_keeps_partial_target() {
        let target = group_snapshot("100", 1000..1003);
        let mut excluded = group_snapshot("200", 1000..1001);
        excluded.status = "unverified".into();
        let view = apply_exclusions(target.clone(), vec![excluded.clone()]).unwrap();
        assert_eq!(view.status, "unverified");
        assert!(view
            .warnings
            .iter()
            .any(|warning| warning.contains("排除名单未完成核验")));
        let mut partial = target;
        partial.status = "partial".into();
        assert_eq!(
            apply_exclusions(partial, vec![excluded]).unwrap().status,
            "partial"
        );
    }

    #[test]
    fn exclusion_rejects_different_accounts_sources_and_target_group_itself() {
        let target = group_snapshot("100", 1000..1003);
        let mut mismatch = group_snapshot("200", 1000..1001);
        mismatch.account.qq_number = "20002".into();
        assert!(apply_exclusions(target.clone(), vec![mismatch.clone()]).is_err());
        mismatch.account.qq_number = target.account.qq_number.clone();
        mismatch.account.source = "historical".into();
        assert!(apply_exclusions(target.clone(), vec![mismatch]).is_err());
        assert!(apply_exclusions(target.clone(), vec![target]).is_err());
    }

    #[test]
    fn sent_filter_runs_after_group_exclusions_without_double_counting() {
        let target = group_snapshot("100", 1000..2000);
        let excluded = group_snapshot("200", (1000..1020).chain(5000..5180));
        let target = apply_exclusions(target, vec![excluded]).unwrap();
        // 1000..1020 已因群重合移除；1030..1060 才是本次新增的 30 人。
        let sent = (1000..1020)
            .chain(1030..1060)
            .map(|number| number.to_string())
            .collect();
        let view = apply_sent_filter(target, &sent);
        assert!(view.exclude_sent);
        assert_eq!(view.source_count, 1000);
        assert_eq!(view.excluded_count, 20);
        assert_eq!(view.sent_filtered_count, 30);
        assert_eq!(view.members.len(), 950);
        assert_eq!(view.filtered_members.len(), 50);
        assert_eq!(
            view.source_count,
            view.members.len() + view.filtered_members.len()
        );
        let repeated = apply_sent_filter(view, &sent);
        assert_eq!(repeated.sent_filtered_count, 30);
        assert_eq!(repeated.filtered_members.len(), 50);
    }

    #[test]
    fn sent_filter_can_legitimately_remove_all_acquired_members() {
        let target = group_snapshot("100", 1000..1003);
        let sent = (1000..1003).map(|number| number.to_string()).collect();
        let view = apply_sent_filter(target, &sent);
        assert!(view.members.is_empty());
        assert_eq!(view.sent_filtered_count, 3);
        assert_eq!(view.filtered_members.len(), 3);
        assert_eq!(view.source_count, 3);
    }

    /// 冲突关系一旦降为未知，后续重复记录不能再把它恢复成一个看似确定的值。
    #[test]
    fn duplicate_friend_relationship_conflicts_remain_unknown() {
        let mut friend = member("000123");
        friend.friend_relation = FriendRelation::Friend;
        let mut not_friend = member("123");
        not_friend.friend_relation = FriendRelation::NotFriend;
        let view = snapshot(
            result(vec![friend.clone(), not_friend, friend], Some(1)),
            Some(1),
        );
        assert_eq!(view.duplicate_count, 2);
        assert_eq!(view.members[0].friend_relation, FriendRelation::Unknown);
        assert_eq!(view.friend_unknown_count, 1);
        let account = view.account.clone();
        assert_eq!(
            apply_friend_filter(view, &account).unwrap_err().code,
            "friend_relation_unknown"
        );
    }

    /// 群重合优先，其后好友，再后已发送；每位成员只能占一个过滤原因且总人数守恒。
    #[test]
    fn friend_filter_preserves_order_counts_and_idempotency() {
        let mut target = group_snapshot("100", 1000..1006);
        for item in &mut target.members {
            item.friend_relation = if ["1000", "1001", "1002"].contains(&item.qq_number.as_str()) {
                FriendRelation::Friend
            } else {
                FriendRelation::NotFriend
            };
        }
        let account = target.account.clone();
        let excluded = group_snapshot("200", 1000..1002);
        let view = apply_exclusions(target, vec![excluded]).unwrap();
        let view = apply_friend_filter(view, &account).unwrap();
        assert_eq!(view.excluded_count, 2);
        assert_eq!(view.friend_filtered_count, 1);
        assert_eq!(view.friend_filter_status, "completed");
        assert_eq!(view.friend_unknown_count, 0);
        let message = view.message.clone();
        let view = apply_friend_filter(view, &account).unwrap();
        assert_eq!(view.message, message);
        let sent = ["1000".into(), "1002".into(), "1003".into()]
            .into_iter()
            .collect();
        let view = apply_sent_filter(view, &sent);
        assert_eq!(view.sent_filtered_count, 1);
        assert_eq!(view.members.len(), 2);
        assert_eq!(view.filtered_members.len(), 4);
        assert_eq!(
            view.source_count,
            view.members.len() + view.filtered_members.len()
        );
        assert_eq!(view.filtered_members[2].reason, "friend");
        assert!(validate_friend_filter(&view).is_ok());
        assert_eq!(
            apply_friend_filter(view, &account)
                .unwrap()
                .friend_filtered_count,
            1
        );
    }

    /// 已被群重合移除的人也属于原目标；其关系未知时不能显示“好友排除已完成”。
    #[test]
    fn unknown_relation_in_group_filtered_member_blocks_friend_filter() {
        let mut target = group_snapshot("100", 1000..1003);
        for member in &mut target.members[1..] {
            member.friend_relation = FriendRelation::NotFriend;
        }
        let account = target.account.clone();
        let target = apply_exclusions(target, vec![group_snapshot("200", 1000..1001)]).unwrap();
        let error = apply_friend_filter(target, &account).unwrap_err();
        assert_eq!(error.code, "friend_relation_unknown");
        assert!(error.message.contains("1 位"));
    }

    /// 关系只属于获取账号；旧账号、来源错配和错误过滤顺序都必须重新获取。
    #[test]
    fn friend_filter_rejects_account_source_and_order_mismatches() {
        let mut target = group_snapshot("100", 1000..1001);
        target.members[0].friend_relation = FriendRelation::NotFriend;
        let account = target.account.clone();
        let mut other = account.clone();
        other.qq_number = "20002".into();
        assert_eq!(
            apply_friend_filter(target.clone(), &other)
                .unwrap_err()
                .code,
            "friend_account_mismatch"
        );
        other.qq_number = account.qq_number.clone();
        other.source = "historical".into();
        assert_eq!(
            apply_friend_filter(target.clone(), &other)
                .unwrap_err()
                .code,
            "friend_account_mismatch"
        );
        let sent = apply_sent_filter(target, &HashSet::new());
        assert_eq!(
            apply_friend_filter(sent, &account).unwrap_err().code,
            "invalid_friend_filter_state"
        );
    }

    /// 列表权威覆盖旧fr，群交集优先且每人只记一次；全好友集合中群外号码不进入名单或导出。
    #[test]
    fn bound_friend_list_overrides_member_flags_and_preserves_filter_conservation() {
        let mut target = group_snapshot("100", 1000..1006);
        target.members[0].friend_relation = FriendRelation::NotFriend;
        target.members[1].friend_relation = FriendRelation::Friend;
        let account = target.account.clone();
        let friends = FriendList {
            account: account.clone(),
            qq_numbers: ["1000", "1002", "99999"]
                .map(String::from)
                .into_iter()
                .collect(),
        };
        let target = bind_friend_list(target, &friends).unwrap();
        assert_eq!(target.friend_unknown_count, 0);
        assert_eq!(target.members[0].friend_relation, FriendRelation::Friend);
        assert_eq!(target.members[1].friend_relation, FriendRelation::NotFriend);
        assert!(!target
            .members
            .iter()
            .any(|member| member.qq_number == "99999"));
        let target = apply_exclusions(target, vec![group_snapshot("200", 1000..1002)]).unwrap();
        let target = apply_friend_filter(target, &account).unwrap();
        let target = apply_sent_filter(
            target,
            &["1002".into(), "1003".into()].into_iter().collect(),
        );
        assert_eq!(
            (
                target.source_count,
                target.excluded_count,
                target.friend_filtered_count,
                target.sent_filtered_count,
                target.members.len()
            ),
            (6, 2, 1, 1, 2)
        );
        assert_eq!(target.filtered_members.len(), 4);
        validate_friend_filter(&target).unwrap();
    }

    /// 账号、来源、集合编码和原始名单阶段都是绑定前提，不能用新账号重解释旧快照。
    #[test]
    fn friend_binding_rejects_other_accounts_invalid_sets_and_already_filtered_versions() {
        let target = group_snapshot("100", 1000..1003);
        let mut friends = FriendList {
            account: target.account.clone(),
            qq_numbers: ["1000".into()].into_iter().collect(),
        };
        friends.account.qq_number = "other".into();
        assert_eq!(
            bind_friend_list(target.clone(), &friends).unwrap_err().code,
            "friend_account_mismatch"
        );
        friends.account = target.account.clone();
        friends.account.source = "other".into();
        assert_eq!(
            bind_friend_list(target.clone(), &friends).unwrap_err().code,
            "friend_account_mismatch"
        );
        friends.account = target.account.clone();
        friends.qq_numbers.insert("00123".into());
        assert_eq!(
            bind_friend_list(target.clone(), &friends).unwrap_err().code,
            "friend_list_invalid"
        );
        friends.qq_numbers.remove("00123");
        let excluded =
            apply_exclusions(target.clone(), vec![group_snapshot("200", 1000..1001)]).unwrap();
        assert_eq!(
            bind_friend_list(excluded, &friends).unwrap_err().code,
            "invalid_friend_filter_state"
        );
        let sent = apply_sent_filter(target, &HashSet::new());
        assert_eq!(
            bind_friend_list(sent, &friends).unwrap_err().code,
            "invalid_friend_filter_state"
        );
    }

    /// 提供方明确核实的空集合与所有成员都是好友分别产生保留全部/合法空结果，不依赖旧fr。
    #[test]
    fn verified_empty_and_all_friend_sets_produce_consistent_results() {
        let target = group_snapshot("100", 1000..1003);
        let account = target.account.clone();
        let no_friends = FriendList {
            account: account.clone(),
            qq_numbers: HashSet::new(),
        };
        let retained = apply_friend_filter(
            bind_friend_list(target.clone(), &no_friends).unwrap(),
            &account,
        )
        .unwrap();
        assert_eq!(retained.members.len(), 3);
        assert_eq!(retained.friend_filtered_count, 0);
        let all_friends = FriendList {
            account: account.clone(),
            qq_numbers: (1000..1003).map(|value| value.to_string()).collect(),
        };
        let empty =
            apply_friend_filter(bind_friend_list(target, &all_friends).unwrap(), &account).unwrap();
        assert!(empty.members.is_empty());
        assert_eq!(empty.friend_filtered_count, 3);
        validate_friend_filter(&empty).unwrap();
    }

    /// 全部是好友时允许合法空结果；消费方仍核对状态、关系与计数，而不是只信状态字符串。
    #[test]
    fn all_friend_result_is_valid_but_inconsistent_snapshots_are_rejected() {
        let mut target = group_snapshot("100", 1000..1003);
        for member in &mut target.members {
            member.friend_relation = FriendRelation::Friend;
        }
        let account = target.account.clone();
        let view = apply_friend_filter(target, &account).unwrap();
        assert!(view.members.is_empty());
        assert_eq!(view.friend_filtered_count, 3);
        assert!(validate_friend_filter(&view).is_ok());
        let mut invalid = view.clone();
        invalid.friend_filter_status = "not_requested".into();
        assert_eq!(
            validate_friend_filter(&invalid).unwrap_err().code,
            "friend_filter_incomplete"
        );
        invalid = view.clone();
        invalid.filtered_members[0].member.friend_relation = FriendRelation::NotFriend;
        assert!(validate_friend_filter(&invalid).is_err());
        invalid = view.clone();
        invalid.friend_filtered_count = 2;
        assert!(validate_friend_filter(&invalid).is_err());
        invalid = view.clone();
        invalid.source_count = 4;
        assert!(validate_friend_filter(&invalid).is_err());
        invalid = view;
        let retained_friend = invalid.filtered_members.pop().unwrap().member;
        invalid.members.push(retained_friend);
        invalid.friend_filtered_count = 2;
        assert!(validate_friend_filter(&invalid).is_err());
    }
}
