//! 发送确认前准备实际收件范围。作者：ahui。
//! 只读全库历史一次，短期固定结果与账号分配交给发送任务；不保存正文、SMTP连接配置、凭据或模板数据。

use crate::{mail_commands::GroupAudienceSelection, state::AppState};
use member_core::{database::PreparedMailRecipients, diagnostics, mail, models::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::State;

/// 准备输入只含候选范围、历史开关、账号标识与本次分封规则，不接受客户端声明的过滤后人数。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientPreparationInput {
    recipients: String,
    skip_sent: bool,
    group_audience: Option<GroupAudienceSelection>,
    /// 缺省保留旧单账号调用；显式空数组必须报错，不能隐式改用其他发件人。
    #[serde(default)]
    account_ids: Option<Vec<String>>,
    /// 页面最后读取的目录版本；存在时必须一致，授权码变化同样使版本递增。
    #[serde(default)]
    account_revision: Option<u64>,
    /// 预检同时固定封数；缺省bcc，避免确认后改变模式而发出不同数量的邮件。
    #[serde(default)]
    send_mode: Option<String>,
    /// 缺省每封20人，individual实际每封1人；仍校验显式值为1—1000。
    #[serde(default)]
    batch_size: Option<usize>,
    /// ordered沿用目录与名单顺序；random只在本次预检打乱一次，确认后不再随机。
    #[serde(default)]
    allocation_mode: Option<String>,
}

/// 确认中展示每个选中账号的实际人数与封数；分到零人的账号仍返回，便于明确提示未使用。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountAssignmentView {
    account_id: String,
    name: String,
    from_address: String,
    recipient_count: usize,
    message_count: usize,
}

/// 账号目录与分批规则的确认绑定，只保存公开标识和版本，不持有授权码。
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PreparedAccountSelection {
    /// 本次确认冻结的账号顺序；random时与目录顺序不同，提交核验按集合后必须复用此顺序。
    pub(crate) ids: Vec<String>,
    pub(crate) revision: u64,
    pub(crate) send_mode: String,
    pub(crate) batch_size: usize,
    pub(crate) allocation_mode: String,
}

/// 确认弹窗使用的实际人数；id是当前进程内的短期句柄，不落库或进入模板。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientPreparationView {
    id: String,
    total: usize,
    skipped: usize,
    /// 与成功历史跳过分开计数，避免把用户的独立排除操作误写成已经发送。
    manual_excluded: usize,
    /// 返回规范化完整邮箱供本次确认查看；不写日志，也不进入模板或发送历史。
    excluded_recipients: Vec<String>,
    /// 固定确认时的全局排除版本；启动时必须仍相同，不能悄悄改用另一份收件范围。
    exclusion_revision: u64,
    group_member_count: usize,
    extra_count: usize,
    /// 旧单账号确认没有目录版本，返回 null；新路径按本次确认的冻结顺序返回全部选中账号。
    account_revision: Option<u64>,
    account_ids: Vec<String>,
    account_assignments: Vec<AccountAssignmentView>,
    total_message_count: usize,
}

/// 同时只保留最后一次已确认候选范围，5分钟过期；历史有写入则立即失效。
#[derive(Clone)]
pub(crate) struct PreparedRecipients {
    id: String,
    created_at: Instant,
    original: Vec<String>,
    eligible: Vec<String>,
    skip_sent: bool,
    skipped: usize,
    excluded: Vec<String>,
    exclusion_revision: u64,
    selected_accounts: Option<PreparedAccountSelection>,
    /// 仅供只读结束时间估算；按已冻结顺序保存公开From及封数，避免每秒复制收件名单或读取凭据。
    estimate_accounts: Vec<(String, usize)>,
}

/// 估算只接受已有计划句柄和时间选项；人数、分封及账号身份全部从宿主冻结计划读取。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationEstimateInput {
    plan_id: String,
    interval_seconds: u64,
    account_switch_interval_seconds: u64,
}

/// 动态确认估算不消费计划、不随机、不写历史/冷却、不连接SMTP；过期和并发任务由同一门闩拒绝。
#[tauri::command]
pub async fn estimate_prepared_mail_completion(
    state: State<'_, Arc<AppState>>,
    input: ConfirmationEstimateInput,
) -> AppResult<Option<i64>> {
    let state = Arc::clone(&state);
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let accounts = {
        let stored = state
            .prepared_recipients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        stored
            .as_ref()
            .filter(|plan| {
                plan.id == input.plan_id && plan.created_at.elapsed() < Duration::from_secs(300)
            })
            .ok_or_else(|| {
                AppError::new(
                    "mail_preparation_expired",
                    "发送确认已过期，请重新点击发送。",
                )
            })?
            .estimate_accounts
            .clone()
    };
    let estimate_state = Arc::clone(&state);
    // 首次发送前可能只读SQLite；放到阻塞池，不阻塞界面IPC事件循环，也不持有短Mutex做I/O。
    tokio::task::spawn_blocking(move || {
        crate::mail_commands::estimate_confirmation_completion(
            &estimate_state,
            &accounts,
            input.interval_seconds,
            input.account_switch_interval_seconds,
        )
    })
    .await
    .map_err(|_| AppError::new("mail_estimate", "结束时间估算中断，请稍后重试。"))?
}

/// 先验证固定群名单，再准备历史过滤范围；零待发也返回统计供界面说明，不触发投递。
#[tauri::command]
pub async fn prepare_mail_recipients(
    state: State<'_, Arc<AppState>>,
    input: RecipientPreparationInput,
) -> AppResult<RecipientPreparationView> {
    prepare_mail_recipients_inner(&state, input).await
}

/// 与IPC共用完整预检链路，便于用独立SQLite验证过滤、随机冻结和账号绑定，不创建SMTP连接。
async fn prepare_mail_recipients_inner(
    state: &AppState,
    input: RecipientPreparationInput,
) -> AppResult<RecipientPreparationView> {
    let mut diagnostic = diagnostics::operation("recipients", "prepare");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let send_mode = input.send_mode.as_deref().unwrap_or("bcc");
    let batch_size = input.batch_size.unwrap_or(20);
    let effective_batch_size = confirmed_batch_size(send_mode, batch_size)?;
    let allocation_mode = input.allocation_mode.as_deref().unwrap_or("ordered");
    validate_allocation_mode(allocation_mode)?;
    // 仅读取目录元数据；分到零人的账号无需可用授权码，确认阶段也不接触密码明文。
    let mut selected = input
        .account_ids
        .as_ref()
        .map(|ids| crate::sender_account_commands::selected_account_metadata(state, ids))
        .transpose()?;
    if selected.is_none() && input.account_revision.is_some() {
        return Err(accounts_changed());
    }
    if let (Some(expected), Some(selected)) = (input.account_revision, selected.as_ref()) {
        if expected != selected.revision {
            return Err(accounts_changed());
        }
    }
    let recipients = mail::parse_recipients(&input.recipients)?
        .into_iter()
        .map(|item| item.email)
        .collect::<Vec<_>>();
    let database = state.database()?;
    let skip_sent = input.skip_sent;
    let (original, mut prepared, group_numbers) = tokio::task::spawn_blocking(move || {
        let group_numbers = if let Some(group) = input.group_audience {
            database.validate_group_mail(&group.snapshot_id, &group.qq_numbers, &recipients)?;
            group
                .qq_numbers
                .into_iter()
                .map(|qq| format!("{qq}@qq.com"))
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let prepared = database.prepare_mail_recipients_with_exclusions(&recipients, skip_sent)?;
        Ok::<_, AppError>((recipients, prepared, group_numbers))
    })
    .await
    .map_err(|_| AppError::new("mail_preparation", "收件范围读取中断，请重试。"))??;
    if allocation_mode == "random" {
        // 原始候选仍保留用于提交比对；仅实际待发名单与账号顺序各随机一次，并存入同一确认计划。
        shuffle_for_confirmation(&mut prepared.recipients);
        if let Some(selected) = selected.as_mut() {
            shuffle_for_confirmation(&mut selected.accounts);
        }
    }
    let selected_accounts = selected.as_ref().map(|selected| PreparedAccountSelection {
        ids: selected
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect(),
        revision: selected.revision,
        send_mode: send_mode.to_owned(),
        batch_size,
        allocation_mode: allocation_mode.to_owned(),
    });
    let group_member_count = prepared
        .recipients
        .iter()
        .filter(|email| group_numbers.contains(*email))
        .count();
    let total = prepared.recipients.len();
    let manual_excluded = prepared.excluded.len();
    let exclusion_revision = prepared.exclusion_revision;
    let account_assignments = if let Some(selected) = selected.as_ref() {
        let ranges = mail::balanced_recipient_ranges(total, selected.accounts.len())?;
        selected
            .accounts
            .iter()
            .zip(ranges)
            .map(|(account, range)| AccountAssignmentView {
                account_id: account.id.clone(),
                name: account.name.clone(),
                from_address: account.from_address.clone(),
                recipient_count: range.len(),
                message_count: range.len().div_ceil(effective_batch_size),
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let total_message_count = if selected.is_some() {
        account_assignments
            .iter()
            .map(|account| account.message_count)
            .sum()
    } else {
        total.div_ceil(effective_batch_size)
    };
    let id = uuid::Uuid::new_v4().to_string();
    diagnostics::event(
        "info",
        "recipients",
        "prepared",
        serde_json::json!({"plan": diagnostics::correlate(&id), "candidates": original.len(), "actual": total, "skipped": prepared.skipped_sent, "manual_excluded": manual_excluded, "exclusion_revision": exclusion_revision, "group": group_member_count, "extra": total - group_member_count, "skip_sent": skip_sent, "lifetime_seconds": 300}),
    );
    *state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(PreparedRecipients {
        id: id.clone(),
        created_at: Instant::now(),
        original,
        eligible: prepared.recipients,
        skip_sent,
        skipped: prepared.skipped_sent,
        excluded: prepared.excluded.clone(),
        exclusion_revision,
        selected_accounts: selected_accounts.clone(),
        estimate_accounts: account_assignments
            .iter()
            .map(|account| (account.from_address.clone(), account.message_count))
            .collect(),
    });
    diagnostic.finish("success");
    Ok(RecipientPreparationView {
        id,
        total,
        skipped: prepared.skipped_sent,
        manual_excluded,
        excluded_recipients: prepared.excluded,
        exclusion_revision,
        group_member_count,
        extra_count: total - group_member_count,
        account_revision: selected_accounts
            .as_ref()
            .map(|selection| selection.revision),
        account_ids: selected_accounts
            .map(|selection| selection.ids)
            .unwrap_or_default(),
        account_assignments,
        total_message_count,
    })
}

/// 返回实际每封人数并校验确认参数，individual 仍要求批大小合法以免无效输入被静默接受。
pub(crate) fn confirmed_batch_size(send_mode: &str, batch_size: usize) -> AppResult<usize> {
    if !matches!(send_mode, "bcc" | "individual") || !(1..=1000).contains(&batch_size) {
        return Err(AppError::new(
            "mail_batch",
            "发送方式无效，每封人数应为1到1000；还须遵守发件服务实际限制。",
        ));
    }
    Ok(if send_mode == "individual" {
        1
    } else {
        batch_size
    })
}

/// 分配策略只有两种明确值；未知值不能静默按顺序分配，让确认与实发出现差异。
pub(crate) fn validate_allocation_mode(mode: &str) -> AppResult<()> {
    if matches!(mode, "ordered" | "random") {
        Ok(())
    } else {
        Err(AppError::new(
            "mail_allocation_mode",
            "账号分配方式无效，请重新点击发送并确认。",
        ))
    }
}

/// 使用已有UUID v4随机源生成一次性排序键；不增加依赖，不承诺每次必定不同。
/// 仅在预检调用，O(N log N)；结果必须随计划冻结，发送重试不得再次打乱。
fn shuffle_for_confirmation<T>(values: &mut [T]) {
    values.sort_by_cached_key(|_| uuid::Uuid::new_v4());
}

/// 配置、目录顺序或凭据变化都要求重新确认，不能悄悄使用新的发件身份或重新分配。
pub(crate) fn accounts_changed() -> AppError {
    AppError::new(
        "mail_accounts_changed",
        "发件账号、授权码或分配规则已变化，请重新点击发送并核对分配。",
    )
}

/// 验证确认时的账号全集、目录版本及封数参数；降级到旧路径也不能绕过已确认的多账号选择。
/// 调用方须持有 operation 门闩；此处不消费计划，后续 MIME 或凭据校验失败可继续修正。
pub(crate) fn validate_prepared_accounts(
    state: &AppState,
    plan_id: Option<&str>,
    selection: Option<&PreparedAccountSelection>,
) -> AppResult<Option<PreparedAccountSelection>> {
    if plan_id.is_none() && selection.is_none() {
        return Ok(None);
    }
    let stored = state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if selection.is_none() {
        // 旧单账号确认继续由prepared_for_send返回原来的过期/候选变化错误码；只拦截对已知多账号计划的降级。
        if stored.as_ref().is_some_and(|plan| {
            Some(plan.id.as_str()) == plan_id && plan.selected_accounts.is_some()
        }) {
            return Err(accounts_changed());
        }
        return Ok(None);
    }
    let plan = stored
        .as_ref()
        .filter(|plan| {
            Some(plan.id.as_str()) == plan_id
                && plan.created_at.elapsed() < Duration::from_secs(300)
        })
        .ok_or_else(accounts_changed)?;
    let matches = match (plan.selected_accounts.as_ref(), selection) {
        (None, None) => true,
        (Some(frozen), Some(requested)) => {
            frozen.revision == requested.revision
                && frozen.send_mode == requested.send_mode
                && frozen.batch_size == requested.batch_size
                && frozen.allocation_mode == requested.allocation_mode
                && if frozen.allocation_mode == "random" {
                    // 输入经过目录选择校验，不含重复或失效ID；随机实际账号顺序只从计划取。
                    frozen.ids.len() == requested.ids.len()
                        && frozen.ids.iter().collect::<HashSet<_>>()
                            == requested.ids.iter().collect::<HashSet<_>>()
                } else {
                    frozen.ids == requested.ids
                }
        }
        _ => false,
    };
    if !matches {
        return Err(accounts_changed());
    }
    Ok(plan.selected_accounts.clone())
}

/// 消费前验证固定候选与开关一致，不再逐人重复查询历史。历史写入会清除此句柄，
/// 过期、排除版本变化或输入不匹配要求重新打开确认，避免用旧统计悄悄发送不同人数。
/// current_exclusion_revision 必须在调用方持有 operation 门闩时从数据库读取；此函数不做数据库 I/O。
/// 成功建任务后由调用方清除，设置校验失败不得提前消费。
pub(crate) fn prepared_for_send(
    state: &AppState,
    id: &str,
    original: &[String],
    skip_sent: bool,
    current_exclusion_revision: u64,
) -> AppResult<PreparedMailRecipients> {
    let stored = state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let plan = stored
        .as_ref()
        .filter(|plan| {
            plan.id == id
                && plan.created_at.elapsed() < Duration::from_secs(300)
                && plan.skip_sent == skip_sent
                && plan.original == original
                && plan.exclusion_revision == current_exclusion_revision
        })
        .ok_or_else(|| {
            // 不记录候选地址和原始句柄；只指出过期/输入变化等固定原因，便于核对重新确认路径。
            let reason = match stored.as_ref() {
                None => "missing_or_invalidated",
                Some(plan) if plan.created_at.elapsed() >= Duration::from_secs(300) => "expired",
                Some(plan) if plan.id != id => "different_plan",
                Some(plan) if plan.exclusion_revision != current_exclusion_revision => {
                    "manual_exclusions_changed"
                }
                Some(_) => "candidate_or_filter_changed",
            };
            diagnostics::event(
                "warn",
                "recipients",
                "plan_rejected",
                serde_json::json!({"plan": diagnostics::correlate(id), "reason": reason}),
            );
            AppError::new(
                "mail_preparation_expired",
                "收件范围已变化或确认已过期，请返回重新点击发送并核对人数。",
            )
        })?;
    diagnostics::event(
        "info",
        "recipients",
        "plan_reused",
        serde_json::json!({"plan": diagnostics::correlate(id), "actual": plan.eligible.len(), "skipped": plan.skipped, "manual_excluded": plan.excluded.len(), "exclusion_revision": plan.exclusion_revision}),
    );
    Ok(PreparedMailRecipients {
        recipients: plan.eligible.clone(),
        skipped_sent: plan.skipped,
        excluded: plan.excluded.clone(),
        exclusion_revision: plan.exclusion_revision,
    })
}

#[cfg(test)]
mod prepared_plan_tests {
    use super::*;
    use member_core::database::Database;

    /// 只为启动边界创建随机临时 SQLite，清理已知文件而不递归操作用户目录。
    struct TemporaryDatabase {
        database: Database,
        path: std::path::PathBuf,
    }

    impl TemporaryDatabase {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "qq-empty-plan-test-{}.sqlite3",
                uuid::Uuid::new_v4()
            ));
            Self {
                database: Database::open(&path).unwrap(),
                path,
            }
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            for path in [
                self.path.clone(),
                self.path.with_extension("sqlite3-wal"),
                self.path.with_extension("sqlite3-shm"),
            ] {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// 每项测试独立创建内存状态，不初始化数据库或网络，确保复用计划时不会再查历史。
    fn state_with_plan(age: Duration) -> AppState {
        let state = AppState::new();
        *state
            .prepared_recipients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreparedRecipients {
            id: "synthetic-plan".into(),
            created_at: Instant::now().checked_sub(age).unwrap(),
            original: original_recipients(),
            eligible: vec!["second@example.test".into()],
            skip_sent: true,
            skipped: 1,
            excluded: vec!["blocked@example.test".into()],
            exclusion_revision: 7,
            selected_accounts: None,
            estimate_accounts: Vec::new(),
        });
        state
    }

    /// 合成候选分别覆盖已发送、实际待发和独立排除，不读取用户地址或生产数据。
    fn original_recipients() -> Vec<String> {
        vec![
            "first@example.test".into(),
            "second@example.test".into(),
            "blocked@example.test".into(),
        ]
    }

    #[test]
    fn matching_plan_reuses_frozen_recipients_without_consuming_or_querying() {
        let state = state_with_plan(Duration::from_secs(1));
        let original = original_recipients();
        let mut prepared = prepared_for_send(&state, "synthetic-plan", &original, true, 7).unwrap();
        assert_eq!(prepared.recipients, vec!["second@example.test"]);
        assert_eq!(prepared.skipped_sent, 1);
        assert_eq!(prepared.excluded, vec!["blocked@example.test"]);
        assert_eq!(prepared.exclusion_revision, 7);

        // 正确的草稿仍可能在后续设置校验失败；读取计划不得消费，也不得把返回值与内部名单共享。
        prepared.recipients.clear();
        prepared.excluded.clear();
        let prepared_again =
            prepared_for_send(&state, "synthetic-plan", &original, true, 7).unwrap();
        assert_eq!(prepared_again.recipients, vec!["second@example.test"]);
        assert_eq!(prepared_again.skipped_sent, 1);
        assert_eq!(prepared_again.excluded, vec!["blocked@example.test"]);
    }

    #[test]
    fn changed_id_candidate_list_or_skip_setting_requires_new_confirmation() {
        let state = state_with_plan(Duration::from_secs(1));
        let original = original_recipients();
        let changed = vec!["first@example.test".into(), "third@example.test".into()];
        let reversed = vec!["second@example.test".into(), "first@example.test".into()];
        for (id, candidates, skip_sent) in [
            ("another-plan", original.as_slice(), true),
            ("synthetic-plan", changed.as_slice(), true),
            ("synthetic-plan", reversed.as_slice(), true),
            ("synthetic-plan", &original[..1], true),
            ("synthetic-plan", original.as_slice(), false),
        ] {
            assert_eq!(
                prepared_for_send(&state, id, candidates, skip_sent, 7)
                    .unwrap_err()
                    .code,
                "mail_preparation_expired"
            );
        }
        // 被篡改的调用不能消费仍正确的确认范围，用户修正输入后可继续使用有效计划。
        assert!(prepared_for_send(&state, "synthetic-plan", &original, true, 7).is_ok());
    }

    #[test]
    fn changed_exclusion_revision_requires_new_confirmation_even_if_plan_remains_in_memory() {
        let state = state_with_plan(Duration::from_secs(1));
        let original = original_recipients();
        assert_eq!(
            prepared_for_send(&state, "synthetic-plan", &original, true, 8)
                .unwrap_err()
                .code,
            "mail_preparation_expired"
        );
        // 版本检查失败不消费计划，便于上层明确决定是否使句柄失效；不会静默调整候选名单。
        assert!(prepared_for_send(&state, "synthetic-plan", &original, true, 7).is_ok());
    }

    #[test]
    fn empty_confirmed_plan_is_rejected_without_consuming_confirmation_or_creating_task() {
        let fixture = TemporaryDatabase::new();
        fixture
            .database
            .set_setting("smtp_public", "unchanged")
            .unwrap();
        let state = state_with_plan(Duration::from_secs(1));
        let original = original_recipients();
        {
            let mut stored = state.prepared_recipients.lock().unwrap();
            let plan = stored.as_mut().unwrap();
            plan.eligible.clear();
            plan.skipped = original.len();
            plan.excluded.clear();
            plan.exclusion_revision = 0;
        }
        assert_eq!(
            crate::mail_commands::recipients_for_mail_start(
                &state,
                &fixture.database,
                Some("synthetic-plan"),
                &original,
                true,
            )
            .unwrap_err()
            .code,
            "no_eligible_recipients"
        );
        let retained = prepared_for_send(&state, "synthetic-plan", &original, true, 0).unwrap();
        assert!(retained.recipients.is_empty());
        assert_eq!(retained.skipped_sent, original.len());
        assert_eq!(state.mail.lock().unwrap().status, "idle");
        assert!(state.mail_cancel.lock().unwrap().is_none());
        assert_eq!(
            fixture.database.setting("smtp_public").unwrap().as_deref(),
            Some("unchanged")
        );
        assert_eq!(fixture.database.sent_count().unwrap(), 0);
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
    }

    #[test]
    fn plan_expires_at_five_minutes_without_blocking_sleep() {
        let state = state_with_plan(Duration::from_secs(300));
        let original = original_recipients();
        assert_eq!(
            prepared_for_send(&state, "synthetic-plan", &original, true, 7)
                .unwrap_err()
                .code,
            "mail_preparation_expired"
        );
        // 新增账号确认校验不替换旧单账号计划原有的过期错误码，既有调用方仍可提示重新确认人数。
        assert!(
            validate_prepared_accounts(&state, Some("synthetic-plan"), None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cleared_plan_and_uninitialized_state_reject_previous_confirmation() {
        let state = state_with_plan(Duration::from_secs(1));
        let original = original_recipients();
        state
            .prepared_recipients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        for state in [&state, &AppState::new()] {
            assert_eq!(
                prepared_for_send(state, "synthetic-plan", &original, true, 7)
                    .unwrap_err()
                    .code,
                "mail_preparation_expired"
            );
        }
    }

    #[test]
    fn confirmed_accounts_bind_directory_credentials_order_and_batch_policy() {
        let state = state_with_plan(Duration::from_secs(1));
        let selected = PreparedAccountSelection {
            ids: vec!["account-a".into(), "account-b".into()],
            revision: 11,
            send_mode: "bcc".into(),
            batch_size: 20,
            allocation_mode: "ordered".into(),
        };
        state
            .prepared_recipients
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .selected_accounts = Some(selected.clone());
        assert!(
            validate_prepared_accounts(&state, Some("synthetic-plan"), Some(&selected)).is_ok()
        );
        let mut variants = Vec::new();
        let mut changed = selected.clone();
        changed.ids.reverse();
        variants.push(changed);
        let mut changed = selected.clone();
        changed.ids.pop();
        variants.push(changed);
        let mut changed = selected.clone();
        changed.revision += 1; // 包括仅保存或清除授权码的目录版本变化。
        variants.push(changed);
        let mut changed = selected.clone();
        changed.send_mode = "individual".into();
        variants.push(changed);
        let mut changed = selected.clone();
        changed.batch_size = 10;
        variants.push(changed);
        for changed in variants {
            assert_eq!(
                validate_prepared_accounts(&state, Some("synthetic-plan"), Some(&changed))
                    .unwrap_err()
                    .code,
                "mail_accounts_changed"
            );
        }
        assert!(validate_prepared_accounts(&state, Some("synthetic-plan"), None).is_err());
        assert!(validate_prepared_accounts(&state, None, Some(&selected)).is_err());
        // 所有拒绝均不消费确认，不创建任务，也不要求初始化数据库或读取密码。
        assert!(
            validate_prepared_accounts(&state, Some("synthetic-plan"), Some(&selected)).is_ok()
        );
        assert_eq!(state.mail.lock().unwrap().status, "idle");
    }

    #[test]
    fn random_confirmation_freezes_account_and_recipient_order_across_retries() {
        let state = state_with_plan(Duration::from_secs(1));
        let frozen = PreparedAccountSelection {
            ids: vec!["account-c".into(), "account-a".into(), "account-b".into()],
            revision: 19,
            send_mode: "bcc".into(),
            batch_size: 20,
            allocation_mode: "random".into(),
        };
        {
            let mut stored = state.prepared_recipients.lock().unwrap();
            let plan = stored.as_mut().unwrap();
            plan.selected_accounts = Some(frozen.clone());
            plan.eligible = vec!["third@example.test".into(), "second@example.test".into()];
        }
        let mut requested = frozen.clone();
        requested.ids.sort(); // 客户端仍提交目录顺序，不得在start再次随机或覆盖确认顺序。
        for _ in 0..5 {
            let actual =
                validate_prepared_accounts(&state, Some("synthetic-plan"), Some(&requested))
                    .unwrap()
                    .unwrap();
            assert_eq!(actual, frozen);
            let recipients =
                prepared_for_send(&state, "synthetic-plan", &original_recipients(), true, 7)
                    .unwrap();
            assert_eq!(
                recipients.recipients,
                vec!["third@example.test", "second@example.test"]
            );
        }
        requested.allocation_mode = "ordered".into();
        assert!(
            validate_prepared_accounts(&state, Some("synthetic-plan"), Some(&requested)).is_err()
        );
    }

    #[test]
    fn random_permutations_keep_balance_coverage_and_original_input_unchanged() {
        for count in [0, 1, 2, 3, 5, 8, 10] {
            let original = (0..count).collect::<Vec<_>>();
            for _ in 0..8 {
                let mut recipients = original.clone();
                let mut accounts = (0..5).collect::<Vec<_>>();
                shuffle_for_confirmation(&mut recipients);
                shuffle_for_confirmation(&mut accounts);
                let ranges =
                    mail::balanced_recipient_ranges(recipients.len(), accounts.len()).unwrap();
                let sizes = ranges.iter().map(|range| range.len()).collect::<Vec<_>>();
                assert!(sizes.iter().max().unwrap() - sizes.iter().min().unwrap() <= 1);
                let mut covered = ranges
                    .into_iter()
                    .flat_map(|range| recipients[range].to_vec())
                    .collect::<Vec<_>>();
                covered.sort();
                accounts.sort();
                assert_eq!(covered, original);
                assert_eq!(accounts, vec![0, 1, 2, 3, 4]);
            }
        }
        assert!(validate_allocation_mode("unknown").is_err());
    }

    #[tokio::test]
    async fn actual_preparation_filters_then_freezes_random_assignments_for_all_boundary_counts() {
        let fixture = TemporaryDatabase::new();
        let state = AppState::new();
        state.initialize_database(fixture.database.clone()).unwrap();
        for index in 0..5 {
            let catalog = fixture.database.sender_accounts().unwrap();
            let settings = member_core::sender_accounts::SenderPublicSettings {
                host: "smtp.example.test".into(),
                port: 465,
                tls_mode: "tls".into(),
                username: format!("sender-{index}@example.test"),
                from_address: format!("sender-{index}@example.test"),
                from_name: "合成发件账号".into(),
            };
            fixture
                .database
                .save_sender_account(
                    catalog.revision,
                    &uuid::Uuid::new_v4().simple().to_string(),
                    &format!("账号{index}"),
                    &settings,
                    None,
                )
                .unwrap();
        }
        let catalog = fixture.database.sender_accounts().unwrap();
        let ids = catalog
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let mut reversed_ids = ids.clone();
        reversed_ids.reverse();
        let sent = vec!["sent@example.test".into()];
        fixture
            .database
            .begin_delivery("history-fixture", &sent)
            .unwrap();
        fixture
            .database
            .finish_delivery("history-fixture", &sent, true, "合成成功历史")
            .unwrap();
        fixture
            .database
            .add_manual_exclusions("blocked@example.test", "合成排除规则")
            .unwrap();
        for count in [0, 1, 2, 3, 5, 8, 10] {
            let mut candidates = vec![
                "sent@example.test".to_owned(),
                "blocked@example.test".to_owned(),
            ];
            candidates.extend((0..count).map(|index| format!("recipient-{index}@example.test")));
            let input: RecipientPreparationInput = serde_json::from_value(serde_json::json!({
                "recipients": format!("{};sent@EXAMPLE.TEST", candidates.join(";")),
                "skipSent": true, "accountIds": reversed_ids, "accountRevision": catalog.revision,
                "sendMode": "bcc", "batchSize": 2, "allocationMode": "random"
            }))
            .unwrap();
            // 全部账号都没有密码，预检仍能得到准确分配；只有启动真正参与的账号时才要求凭据。
            let view = prepare_mail_recipients_inner(&state, input).await.unwrap();
            assert_eq!(
                (view.total, view.skipped, view.manual_excluded),
                (count, 1, 1)
            );
            assert_eq!(view.account_revision, Some(catalog.revision));
            assert_eq!(view.account_assignments.len(), 5);
            assert_eq!(
                view.account_assignments
                    .iter()
                    .map(|account| account.recipient_count)
                    .sum::<usize>(),
                count
            );
            let mut counts = view
                .account_assignments
                .iter()
                .map(|account| account.recipient_count)
                .collect::<Vec<_>>();
            counts.sort();
            assert!(counts[4] - counts[0] <= 1);
            assert_eq!(
                view.total_message_count,
                view.account_assignments
                    .iter()
                    .map(|account| account.message_count)
                    .sum::<usize>()
            );
            for assignment in &view.account_assignments {
                let account = catalog
                    .accounts
                    .iter()
                    .find(|account| account.id == assignment.account_id)
                    .unwrap();
                assert_eq!(assignment.from_address, account.settings.from_address);
                assert_eq!(
                    assignment.message_count,
                    assignment.recipient_count.div_ceil(2)
                );
            }
            let requested = PreparedAccountSelection {
                ids: ids.clone(),
                revision: catalog.revision,
                send_mode: "bcc".into(),
                batch_size: 2,
                allocation_mode: "random".into(),
            };
            let frozen = validate_prepared_accounts(&state, Some(&view.id), Some(&requested))
                .unwrap()
                .unwrap();
            assert_eq!(frozen.ids, view.account_ids);
            let prepared =
                prepared_for_send(&state, &view.id, &candidates, true, view.exclusion_revision)
                    .unwrap();
            let retry =
                prepared_for_send(&state, &view.id, &candidates, true, view.exclusion_revision)
                    .unwrap();
            assert_eq!(prepared.recipients, retry.recipients);
            let mut actual = prepared.recipients;
            actual.sort();
            let mut expected = candidates[2..].to_vec();
            expected.sort();
            assert_eq!(actual, expected);
        }
        let empty:RecipientPreparationInput=serde_json::from_value(serde_json::json!({"recipients":"new@example.test","skipSent":false,"accountIds":[],"allocationMode":"random"})).unwrap();
        assert!(
            matches!(prepare_mail_recipients_inner(&state, empty).await,Err(error) if error.code=="sender_account_selection")
        );
        assert_eq!(fixture.database.sent_count().unwrap(), 1);
        assert!(fixture.database.unresolved_deliveries().unwrap().is_empty());
        assert_eq!(state.mail.lock().unwrap().status, "idle");
    }
}
