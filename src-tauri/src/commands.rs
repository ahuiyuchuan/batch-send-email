//! Tauri 命令入口，只允许打包窗口使用业务命令。作者：ahui。
//! 每个获取任务绑定不可变账号与群；取消会立即废弃任务代次，迟到请求不能回写。

use crate::state::{AppState, AppView, TaskView};
use chrono::Local;
use member_core::{
    diagnostics,
    export::{self, ExportFormat},
    models::*,
    providers::WebProvider,
    verify,
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;

/// 最长请求耗时；网络超时不会被解释成空群或成功名单。
const OPERATION_TIMEOUT: Duration = Duration::from_secs(35);
const TASK_TIMEOUT: Duration = Duration::from_secs(120);

/// 同步完整公开状态；发事件时不持有内部互斥锁。
pub(crate) fn publish(app: &AppHandle, state: &AppState) -> AppView {
    let view = state.view();
    let _ = app.emit("app-state", &view);
    view
}

/// 限定远端调用时长，并将超时转换为清晰、无凭据的错误。
async fn bounded<T>(future: impl std::future::Future<Output = AppResult<T>>) -> AppResult<T> {
    tokio::time::timeout(OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| AppError::new("timeout", "请求超时，请检查网络后重试。"))?
}

/// 仅用于已有连接的刷新身份守卫；主动扫码允许切换账号，不受此检查限制。
fn check_identity(state: &AppState, account: &Account) -> AppResult<()> {
    if let Some(current) = state.view().account {
        if current.qq_number != account.qq_number {
            return Err(AppError::new(
                "account_changed",
                "刷新期间登录身份发生变化，请重新扫码。",
            ));
        }
    }
    Ok(())
}

/// 将本次联网核验通过的群目录原子保存到账号自己的 SQLite 分区，再允许界面采用新目录。
/// 调用者必须持有 operation；阻塞数据库写入不持有 inner 锁，失败不会清空旧目录或公开身份。
/// 缓存只用于持久记录，不能把历史账号或群列表当成已通过本次远端验证的登录状态。
pub(crate) async fn persist_group_catalog(
    state: &AppState,
    account: &Account,
    groups: &[Group],
) -> AppResult<()> {
    let database = state.database()?;
    let account = account.clone();
    let groups = groups.to_vec();
    tokio::task::spawn_blocking(move || database.save_group_catalog(&account, &groups))
        .await
        .map_err(|_| {
            AppError::new(
                "group_catalog_save",
                "群列表保存任务中断，请重试；原有本机群目录保留。",
            )
        })??;
    Ok(())
}

/// 取得真实空态或当前结果，不返回任何来源凭据。
#[tauri::command]
pub async fn get_app_state(state: State<'_, Arc<AppState>>) -> AppResult<AppView> {
    let _operation = state.operation.lock().await;
    crate::exclusion_commands::refresh_current_overlay(&state).await
}

/// 新建扫码流程；不会自动确认手机授权，重复开始会丢弃旧二维码会话。
#[tauri::command]
pub async fn start_web_login(state: State<'_, Arc<AppState>>) -> AppResult<QrCode> {
    let mut diagnostic = diagnostics::operation("login", "start_qr");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let previous = state
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pending_web
        .take();
    if let Some(previous) = previous {
        previous.cancel_login().await;
    }
    let web = Arc::new(WebProvider::new()?);
    {
        let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.pending_web = Some(web.clone());
    }
    let qr = bounded(web.start_login()).await?;
    diagnostic.finish("success");
    Ok(qr)
}

/// 查询本轮二维码；本次授权账号及群目录验证通过后直接切换，不要求与旧账号相同。
#[tauri::command]
pub async fn poll_web_login(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> AppResult<LoginPoll> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let web = state
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pending_web
        .clone()
        .ok_or_else(|| AppError::new("login_missing", "请先获取二维码。"))?;
    let result = bounded(web.poll_login()).await?;
    if result.status == "success" {
        let mut diagnostic = diagnostics::operation("login", "complete_qr");
        let account = bounded(web.account()).await?;
        let groups = bounded(web.groups()).await?;
        // 先确认二维码仍属本轮，再保存已核验目录；阻塞写入结束后还会再次检查取消状态。
        // 取消期间最多留下真实核验的历史目录，不会因此发布登录身份或替换正式连接。
        if !state
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending_web
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, &web))
        {
            return Err(AppError::new(
                "login_cancelled",
                "本次扫码已取消，请重新扫码。",
            ));
        }
        persist_group_catalog(&state, &account, &groups).await?;
        diagnostics::event(
            "info",
            "login",
            "identity_verified",
            serde_json::json!({"account": diagnostics::correlate(&account.qq_number), "groups": groups.len()}),
        );
        commit_qr_login(&state, web, account, groups)?;
        publish(&app, &state);
        diagnostic.finish("success");
    }
    Ok(result)
}

/// 原子提交最新二维码身份；旧会话只在成功时替换，取消或迟到的扫码不能覆盖新账号。
/// 调用方持有 operation 并已完成 QQ 和本机群目录验证；短时内存锁内不进行网络或文件写入。
fn commit_qr_login(
    state: &AppState,
    web: Arc<WebProvider>,
    account: Account,
    groups: Vec<Group>,
) -> AppResult<()> {
    let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
    if !inner
        .pending_web
        .as_ref()
        .is_some_and(|pending| Arc::ptr_eq(pending, &web))
    {
        return Err(AppError::new(
            "login_cancelled",
            "本次扫码已取消，请重新扫码。",
        ));
    }
    inner.web = Some(web);
    inner.pending_web = None;
    inner.view.source = Some("web".into());
    inner.view.account = Some(account);
    inner.view.groups = groups;
    // 旧身份的当前名单/任务不能混入新账号；按账号保存的 SQLite 历史继续保留。
    inner.view.snapshot = None;
    inner.view.task = TaskView::default();
    inner.view.session =
        crate::state::SessionView::new("active", "当前账号已登录，关闭程序后需重新扫码。");
    Ok(())
}

/// 关闭登录弹层时废弃二维码，取消不会结束 QQ 客户端或影响其他程序登录。
#[tauri::command]
pub async fn cancel_web_login(state: State<'_, Arc<AppState>>) -> AppResult<()> {
    let mut diagnostic = diagnostics::operation("login", "cancel_qr");
    let web = state
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pending_web
        .take();
    if let Some(web) = web {
        web.cancel_login().await;
    }
    diagnostic.finish("cancelled");
    Ok(())
}

/// 手动刷新并持久化群列表；网络或本机写入失败均保留旧页面目录，明确返回错误，不伪装成没有群。
#[tauri::command]
pub async fn refresh_groups(app: AppHandle, state: State<'_, Arc<AppState>>) -> AppResult<AppView> {
    let mut diagnostic = diagnostics::operation("groups", "refresh");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let provider = state.provider()?;
    let account = match bounded(provider.account()).await {
        Ok(account) => account,
        Err(error) => {
            if crate::session_commands::authentication_failed(&error) {
                crate::session_commands::handle_authentication_failure(&state, &error);
                publish(&app, &state);
            }
            return Err(error);
        }
    };
    check_identity(&state, &account)?;
    let groups = match bounded(provider.groups()).await {
        Ok(groups) => groups,
        Err(error) => {
            if crate::session_commands::authentication_failed(&error) {
                crate::session_commands::handle_authentication_failure(&state, &error);
                publish(&app, &state);
            }
            return Err(error);
        }
    };
    // 成功写入后才切换页面目录；本机写入错误明确上报，不能把仅内存更新误报为已保存。
    persist_group_catalog(&state, &account, &groups).await?;
    diagnostics::event(
        "info",
        "groups",
        "refreshed",
        serde_json::json!({"count": groups.len()}),
    );
    state
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .view
        .groups = groups;
    diagnostic.finish("success");
    Ok(publish(&app, &state))
}

/// 创建唯一后台任务；群必须来自当前账号的群列表，获取开始和结束时都会核对真实账号。
/// exclude_friends 缺省为 false，兼容旧页面；开启时读取本次账号的完整好友集合并绑定到目标成员。
#[tauri::command]
pub async fn fetch_members(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    group_id: Option<String>,
    group_ids: Option<Vec<String>>,
    exclude_group_ids: Option<Vec<String>>,
    exclude_sent: Option<bool>,
    exclude_friends: Option<bool>,
) -> AppResult<AppView> {
    let mut diagnostic = diagnostics::operation("members", "start_fetch");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let provider = state.provider()?;
    let (account, group, additional_groups, exclusions) = {
        let view = state.view();
        let account = view
            .account
            .ok_or_else(|| AppError::new("not_connected", "请先连接账号。"))?;
        // 新页面多选、旧页面单选共用入口；显式空数组报错，不能退回其他群或静默缩小范围。
        let mut ids = group_ids.unwrap_or_else(|| group_id.into_iter().collect());
        member_core::multi_group::selection_key(&ids)?;
        ids.sort();
        ids.dedup();
        let mut targets = ids
            .iter()
            .map(|id| {
                view.groups
                    .iter()
                    .find(|g| &g.id == id)
                    .cloned()
                    .ok_or_else(|| {
                        AppError::new("group_missing", "某个目标群已不在当前群列表，请重新选择。")
                    })
            })
            .collect::<AppResult<Vec<_>>>()?;
        let group = targets.remove(0);
        let mut exclusions = Vec::new();
        for exclude_id in exclude_group_ids.unwrap_or_default() {
            if ids.contains(&exclude_id) {
                return Err(AppError::new(
                    "invalid_exclusion",
                    "目标群不能同时作为排除群。",
                ));
            }
            if exclusions.iter().any(|g: &Group| g.id == exclude_id) {
                continue;
            }
            let excluded = view
                .groups
                .iter()
                .find(|g| g.id == exclude_id)
                .cloned()
                .ok_or_else(|| {
                    AppError::new("group_missing", "某个排除群已不在当前群列表，请重新选择。")
                })?;
            exclusions.push(excluded);
        }
        (account, group, targets, exclusions)
    };
    let id = uuid::Uuid::new_v4().to_string();
    // Freeze the sent-history filter at task creation. UI edits are locked until completion.
    let sent_filter = if exclude_sent.unwrap_or(false) {
        Some(state.database()?.sent_qq_numbers()?)
    } else {
        None
    };
    let cancel = CancellationToken::new();
    diagnostics::event(
        "info",
        "members",
        "task_created",
        serde_json::json!({"job": diagnostics::correlate(&id), "group": diagnostics::correlate(&group.id), "exclude_groups": exclusions.len(), "exclude_sent": sent_filter.is_some(), "exclude_friends": exclude_friends.unwrap_or(false)}),
    );
    {
        let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.cancel = Some(cancel.clone());
        inner.view.task = TaskView {
            id: id.clone(),
            status: "running".into(),
            message: "正在核对账号和群成员资格…".into(),
        };
    }
    let initial = publish(&app, &state);
    let shared = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        let request = FetchRequest {
            provider,
            account,
            group,
            additional_groups,
            exclusions,
            sent_filter,
            exclude_friends: exclude_friends.unwrap_or(false),
            id,
            cancel,
        };
        run_fetch(app, shared, request).await;
    });
    diagnostic.finish("scheduled");
    Ok(initial)
}

/// 获取任务的不可变输入，账号、网页会话与过滤条件共用一个生命周期，不能中途替换。
struct FetchRequest {
    provider: Arc<dyn MemberProvider>,
    account: Account,
    group: Group,
    /// 同任务的其他目标；单群为空。逐群读取，最后统一去重/筛选，任一错误都不发布半成品。
    additional_groups: Vec<Group>,
    exclusions: Vec<Group>,
    sent_filter: Option<std::collections::HashSet<String>>,
    /// 冻结本次开关；不能复用其他账号、其他群或旧名单的好友关系。
    exclude_friends: bool,
    id: String,
    cancel: CancellationToken,
}

/// 仅在本次勾选排除好友时读取好友集合；错误、取消或账号绑定失败直接终止，不改动旧快照。
///
/// 目标必须是已经过人数核验的原始名单。完整好友集合只在本次调用中存活，返回结果只保留
/// 目标群成员的关系；后续仍按群重合、好友、已发送的固定顺序过滤，不保存或展示全体好友。
async fn bind_requested_friend_list(
    provider: &dyn MemberProvider,
    target: Snapshot,
    exclude_friends: bool,
    cancel: CancellationToken,
    progress: &Progress,
) -> AppResult<Snapshot> {
    if !exclude_friends {
        return Ok(target);
    }
    if cancel.is_cancelled() {
        return Err(AppError::new("cancelled", "获取已取消，上次结果不受影响。"));
    }
    progress("正在读取 QQ 好友…".into());
    let friends = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(AppError::new("cancelled", "获取已取消，上次结果不受影响。")),
        result = provider.friends(cancel.clone()) => result?,
    };
    // 请求刚完成时也可能已收到取消；不能让迟到的好友响应进入关系绑定或后续过滤。
    if cancel.is_cancelled() {
        return Err(AppError::new("cancelled", "获取已取消，上次结果不受影响。"));
    }
    let target = verify::bind_friend_list(target, &friends)?;
    diagnostics::event(
        "info",
        "members",
        "friend_relations_bound",
        serde_json::json!({
            "source": "qq_friend_list",
            "target_count": target.source_count,
            "friend_count": target.members.iter().filter(|member| member.friend_relation == FriendRelation::Friend).count(),
            "unknown_count": target.friend_unknown_count,
        }),
    );
    Ok(target)
}

/// 收集并核验一次任务结果，不直接落库或更新页面；便于用隔离读取模块验证完整管线。
/// 账号在所有网络读取前后核对，好友集合仅在勾选时读取，最终提交仍由 run_fetch 的门闩统一处理。
async fn collect_snapshot(
    request: &FetchRequest,
    progress: Progress,
    started_at: String,
) -> AppResult<Snapshot> {
    let FetchRequest {
        provider,
        account,
        group,
        additional_groups,
        exclusions,
        sent_filter,
        exclude_friends,
        id,
        cancel,
    } = request;
    let before = provider.account().await?;
    if before.qq_number != account.qq_number {
        return Err(AppError::new(
            "account_changed",
            "当前登录账号已变化，结果已丢弃，请重新扫码登录。",
        ));
    }
    let groups = provider.groups().await?;
    let mut targets = Vec::new();
    for (index, group) in std::iter::once(group).chain(additional_groups).enumerate() {
        if cancel.is_cancelled() {
            return Err(AppError::new("cancelled", "获取已取消，上次结果不受影响。"));
        }
        let live_group = groups
            .iter()
            .find(|g| g.id == group.id)
            .cloned()
            .ok_or_else(|| {
                AppError::new("group_missing", "当前账号已无法访问该群，请刷新群列表。")
            })?;
        progress(format!(
            "正在读取目标群 {}/{}：{}",
            index + 1,
            additional_groups.len() + 1,
            live_group.name
        ));
        let result = provider
            .fetch(&live_group, progress.clone(), cancel.clone())
            .await?;
        if result.members.is_empty() {
            return Err(AppError::new(
                "empty_members",
                "目标群没有返回有效成员，可能受权限、登录状态或版本影响。请重新扫码登录后重试。",
            ));
        }
        let target = verify::create_snapshot(
            if additional_groups.is_empty() {
                id.clone()
            } else {
                format!("{id}:{}", group.id)
            },
            account.clone(),
            live_group,
            result,
            started_at.clone(),
            Local::now().to_rfc3339(),
        );
        if target.source_count == 0 {
            return Err(AppError::new(
                "invalid_members",
                "目标群没有返回合法的数字 QQ 号，结果未更新。",
            ));
        }
        diagnostics::event(
            "info",
            "members",
            "target_verified",
            serde_json::json!({"job": diagnostics::correlate(id), "members": target.source_count, "reported": target.reported_count, "invalid": target.invalid_count, "duplicates": target.duplicate_count, "verification": target.status}),
        );
        targets.push(target);
    }
    let target = member_core::multi_group::merge_targets(targets, id.clone())?;
    let target = bind_requested_friend_list(
        provider.as_ref(),
        target,
        *exclude_friends,
        cancel.clone(),
        &progress,
    )
    .await?;
    let mut excluded_snapshots = Vec::new();
    for (index, excluded) in exclusions.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(AppError::new("cancelled", "获取已取消。"));
        }
        let live_excluded = groups
            .iter()
            .find(|g| g.id == excluded.id)
            .cloned()
            .ok_or_else(|| {
                AppError::new(
                    "group_missing",
                    "某个排除群已无法访问，已停止本次获取，请刷新群列表。",
                )
            })?;
        progress(format!(
            "正在读取排除群 {}/{}：{}",
            index + 1,
            exclusions.len(),
            live_excluded.name
        ));
        let fetched = provider
            .fetch(&live_excluded, progress.clone(), cancel.clone())
            .await
            .map_err(|error| {
                AppError::new(
                    error.code,
                    format!("排除群读取失败，本次结果未更新。{}", error.message),
                )
            })?;
        if fetched.members.is_empty() {
            return Err(AppError::new(
                "empty_exclusion",
                "排除群没有返回有效成员，无法按要求排除，本次结果未更新。",
            ));
        }
        excluded_snapshots.push(verify::create_snapshot(
            format!("{}:{}", id, excluded.id),
            account.clone(),
            live_excluded,
            fetched,
            started_at.clone(),
            Local::now().to_rfc3339(),
        ));
        if let Some(snapshot) = excluded_snapshots.last() {
            diagnostics::event(
                "info",
                "members",
                "exclusion_verified",
                serde_json::json!({"job": diagnostics::correlate(id), "index": index + 1, "total": exclusions.len(), "members": snapshot.source_count, "reported": snapshot.reported_count, "verification": snapshot.status}),
            );
        }
    }
    let after = provider.account().await?;
    if after.qq_number != account.qq_number {
        return Err(AppError::new(
            "account_changed",
            "获取期间当前登录账号已变化，结果已丢弃，请重新扫码登录。",
        ));
    }
    let mut snapshot = verify::apply_exclusions(target, excluded_snapshots)?;
    // 先完成原始人数核验与本次好友集合绑定，再按群重合、好友、已发送的固定优先级过滤；未知关系整次失败，
    // 走既有错误路径保留旧快照，不得保存“好友已排除”的半成品或交给发信。
    if *exclude_friends {
        snapshot = verify::apply_friend_filter(snapshot, account)?;
    }
    if let Some(sent) = sent_filter {
        snapshot = verify::apply_sent_filter(snapshot, sent);
    }
    snapshot.finished_at = Local::now().to_rfc3339();
    Ok(snapshot)
}

/// 后台任务执行体；取消、超时或换代后丢弃结果，避免旧数据覆盖新的页面状态。
async fn run_fetch(app: AppHandle, state: Arc<AppState>, request: FetchRequest) {
    let mut diagnostic = diagnostics::operation("members", "run_fetch");
    let started_at = Local::now().to_rfc3339();
    let progress_state = state.clone();
    let progress_id = request.id.clone();
    let progress_app = app.clone();
    let progress: Progress = Arc::new(move |message| {
        {
            let mut inner = progress_state
                .inner
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if inner.view.task.id != progress_id || inner.view.task.status != "running" {
                return;
            }
            inner.view.task.message = message;
        }
        publish(&progress_app, &progress_state);
    });
    let work = collect_snapshot(&request, progress, started_at);
    let result = tokio::select! {
        _ = request.cancel.cancelled() => Err(AppError::new("cancelled", "获取已取消，上次结果不受影响。")),
        // 每个目标/排除群沿用单群120秒预算，总任务最多30分钟；串行请求避免突发并发压力。
        result = tokio::time::timeout(TASK_TIMEOUT.saturating_mul((1 + request.additional_groups.len() + request.exclusions.len()).min(15) as u32), work) => match result {
            Ok(value) => value,
            Err(_) => Err(AppError::new("timeout", "获取超过本次任务时限，已停止并保留上次结果。请减少选择的群或检查网络后重试。")),
        },
    };
    let id = request.id;
    // Final persistence and cancellation share the operation gate. A cancelled task
    // must not become the latest SQLite snapshot while its UI result is discarded.
    let _operation = state.operation.lock().await;
    {
        let inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.view.task.id != id || inner.view.task.status != "running" {
            diagnostics::event(
                "info",
                "members",
                "result_discarded",
                serde_json::json!({"job": diagnostics::correlate(&id), "stage": "before_persistence"}),
            );
            diagnostic.finish("superseded");
            return;
        }
    }
    let result = match result {
        Ok(snapshot) => match state.database() {
            Ok(database) => {
                let persisted = snapshot.clone();
                match tokio::task::spawn_blocking(move || {
                    // 历史只保存原始获取结果；手动排除随当前版本派生，不挤占20份快照保留额度。
                    database.save_snapshot(&persisted)?;
                    database.apply_manual_exclusions(&persisted)
                })
                .await
                {
                    Ok(Ok(derived)) => Ok(derived),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(AppError::new(
                        "snapshot_save",
                        "保存结果的任务中断，上次结果未更新。",
                    )),
                }
            }
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    if let Some(error) = result
        .as_ref()
        .err()
        .filter(|error| crate::session_commands::authentication_failed(error))
    {
        // 当前任务仍报告原始失败；认证失效只撤下内存连接，后续需要重新扫码。
        crate::session_commands::handle_authentication_failure(&state, error);
    }
    {
        let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.view.task.id != id || inner.view.task.status != "running" {
            diagnostics::event(
                "info",
                "members",
                "result_discarded",
                serde_json::json!({"job": diagnostics::correlate(&id), "stage": "after_persistence"}),
            );
            diagnostic.finish("superseded");
            return;
        }
        inner.cancel = None;
        match result {
            Ok(snapshot) => {
                diagnostics::event(
                    "info",
                    "members",
                    "task_completed",
                    serde_json::json!({"job": diagnostics::correlate(&id), "source_count": snapshot.source_count, "final_count": snapshot.members.len(), "group_filtered": snapshot.excluded_count, "friend_filtered": snapshot.friend_filtered_count, "sent_filtered": snapshot.sent_filtered_count, "duplicates": snapshot.duplicate_count, "invalid": snapshot.invalid_count, "verification": snapshot.status}),
                );
                diagnostic.finish("success");
                // 群列表接口经常不返回人数。使用本次获取的总数更新目标及排除群，
                // 保留未知值，不把过滤后的名单长度或本群新增排除人数当成群人数。
                for group in &mut inner.view.groups {
                    let reported = if let Some(source) = snapshot
                        .source_groups
                        .iter()
                        .find(|source| source.group.id == group.id)
                    {
                        source.reported_count
                    } else if group.id == snapshot.group.id {
                        snapshot.reported_count
                    } else {
                        snapshot
                            .exclusions
                            .iter()
                            .find(|item| item.group.id == group.id)
                            .and_then(|item| item.reported_count)
                    };
                    if let Some(total) = reported {
                        group.member_count = Some(total);
                    }
                }
                inner.view.task.status = "completed".into();
                inner.view.task.message = snapshot.message.clone();
                inner.view.exclusion_revision = snapshot.exclusion_revision;
                inner.view.snapshot = Some(snapshot);
            }
            Err(error) => {
                diagnostics::event(
                    "warn",
                    "members",
                    "task_stopped",
                    serde_json::json!({"job": diagnostics::correlate(&id), "code": error.code}),
                );
                diagnostic.finish(if error.code == "cancelled" {
                    "cancelled"
                } else {
                    "error"
                });
                inner.view.task.status = if error.code == "cancelled" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                inner.view.task.message = error.message;
            }
        }
    }
    publish(&app, &state);
}

/// 停止后续请求；若最终提交已开始，先等提交结束，避免取消与落库状态相互矛盾。
#[tauri::command]
pub async fn cancel_fetch(app: AppHandle, state: State<'_, Arc<AppState>>) -> AppResult<AppView> {
    let mut diagnostic = diagnostics::operation("members", "cancel_fetch");
    let _operation = state.operation.lock().await;
    {
        let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = inner.cancel.take() {
            cancel.cancel();
        }
        if inner.view.task.status == "running" {
            diagnostics::event(
                "info",
                "members",
                "cancel_requested",
                serde_json::json!({"job": diagnostics::correlate(&inner.view.task.id)}),
            );
            inner.view.task.status = "cancelled".into();
            inner.view.task.message = "获取已取消，保留上次结果。".into();
        }
    }
    diagnostic.finish("cancelled");
    Ok(publish(&app, &state))
}

/// 主动退出清空当前内存身份和任务；QQ Cookie 不落盘，SQLite 与 SMTP 授权码继续保留。
#[tauri::command]
pub async fn logout(app: AppHandle, state: State<'_, Arc<AppState>>) -> AppResult<AppView> {
    let mut diagnostic = diagnostics::operation("session", "logout");
    let _operation = state.operation.lock().await;
    state.cancel_all();
    {
        let mut inner = state.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.view.source = None;
        inner.view.task = TaskView::default();
        inner.view.session = crate::state::SessionView::default();
    }
    diagnostic.finish("success");
    Ok(publish(&app, &state))
}

/// 保存结果；saved=false 表示用户取消对话框，不是导出失败。
#[derive(Serialize)]
pub struct ExportOutcome {
    saved: bool,
    path: Option<String>,
}

/// 按当前已验证账号和群读取本机最近结果。没有保存记录返回 None，不自动联网获取。
///
/// 本机读取不改变全局最近获取状态；账号和群归属从宿主核对，防止切群时混用旧账号结果。
#[tauri::command]
pub async fn get_group_snapshot(
    state: State<'_, Arc<AppState>>,
    group_id: Option<String>,
    group_ids: Option<Vec<String>>,
) -> AppResult<Option<Snapshot>> {
    let mut diagnostic = diagnostics::operation("members", "read_group_snapshot");
    let _operation = state.operation.lock().await;
    let view = state.view();
    let account = view
        .account
        .ok_or_else(|| AppError::new("login_required", "请先登录账号。"))?;
    let ids = group_ids.unwrap_or_else(|| group_id.into_iter().collect());
    let group_id = member_core::multi_group::selection_key(&ids)?;
    if ids
        .iter()
        .any(|id| !view.groups.iter().any(|group| &group.id == id))
    {
        return Err(AppError::new(
            "group_missing",
            "当前账号的群列表中没有这个群，请刷新后重试。",
        ));
    }
    let database = state.database()?;
    let account_id = account.qq_number.clone();
    let source = account.source.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        database
            .latest_group_snapshot(&account_id, &source, &group_id)?
            .map(|snapshot| database.apply_manual_exclusions(&snapshot))
            .transpose()
    })
    .await
    .map_err(|_| AppError::new("snapshot_read", "名单读取中断，请重试。"))??;
    // 数据库读取期间可能退出或切换账号，不将旧身份结果交给新的界面上下文。
    if state.view().account.as_ref().map(|value| &value.qq_number) != Some(&account.qq_number) {
        return Err(AppError::new(
            "account_changed",
            "账号已变化，请重新选择群。",
        ));
    }
    diagnostics::event(
        "info",
        "members",
        "snapshot_read",
        serde_json::json!({"found": snapshot.is_some()}),
    );
    diagnostic.finish("success");
    Ok(snapshot)
}

/// 由原生保存对话框选路径，前端不能传任意路径；按页面明确提交的快照导出。
/// 淘汰或不存在的版本直接报错，绝不能回退为另一个群的最近结果。
#[tauri::command]
pub async fn export_members(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    format: String,
    snapshot_id: String,
    exclusion_revision: Option<u64>,
) -> AppResult<ExportOutcome> {
    let mut diagnostic = diagnostics::operation("export", "members");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let format = ExportFormat::parse(&format)?;
    diagnostics::event(
        "info",
        "export",
        "requested",
        serde_json::json!({"format": format.extension(), "snapshot": diagnostics::correlate(&snapshot_id)}),
    );
    if snapshot_id.is_empty() || snapshot_id.len() > 128 {
        return Err(AppError::new("no_result", "请先选择已获取的成员名单。"));
    }
    let database = state.database()?;
    let snapshot = tokio::task::spawn_blocking(move || {
        crate::exclusion_commands::snapshot_view_for_export(
            &database,
            &snapshot_id,
            exclusion_revision,
        )
    })
    .await
    .map_err(|_| AppError::new("snapshot_read", "名单读取中断，请重试。"))??;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("保存成员名单")
        .add_filter("成员名单", &[format.extension()])
        .set_file_name(export::default_filename(&snapshot, format))
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let selected = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "无法取得保存位置，请重试。"))?;
    let Some(selected) = selected else {
        diagnostic.finish("cancelled");
        return Ok(ExportOutcome {
            saved: false,
            path: None,
        });
    };
    let path = selected
        .into_path()
        .map_err(|_| AppError::new("invalid_path", "请选择本机文件保存位置。"))?;
    let shown_path = path.to_string_lossy().into_owned();
    let output_bytes = tokio::task::spawn_blocking(move || {
        export::write_export(&snapshot, &path, format)?;
        // 文件统计失败不改变已经成功写出的业务结果；不记录所选路径或文件名。
        Ok::<_, AppError>(std::fs::metadata(&path).ok().map(|metadata| metadata.len()))
    })
    .await
    .map_err(|_| AppError::new("export_error", "文件生成任务中断，请重试。"))??;
    diagnostics::event(
        "info",
        "export",
        "saved",
        serde_json::json!({"format": format.extension(), "bytes": output_bytes}),
    );
    diagnostic.finish("success");
    Ok(ExportOutcome {
        saved: true,
        path: Some(shown_path),
    })
}

#[cfg(test)]
mod friend_pipeline_tests {
    use super::*;
    use std::{
        collections::HashSet,
        future::Future,
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };

    /// 手写与 async_trait 相同的盒装 Future 签名，不为宿主测试新增运行依赖。
    type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = AppResult<T>> + Send + 'a>>;

    #[derive(Clone, Copy)]
    enum FriendReply {
        Complete,
        Error,
        Cancel,
        Pending,
        OtherAccount,
        OtherSource,
    }

    /// 所有号码、名字和群都为固定合成数据；调用轨迹用于验证好友只读一次及身份核对顺序。
    struct FakeProvider {
        reply: FriendReply,
        friends_calls: AtomicUsize,
        account_calls: AtomicUsize,
        change_final_account: bool,
        trace: Mutex<Vec<&'static str>>,
    }

    fn account() -> Account {
        Account {
            qq_number: "90001".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        }
    }

    fn group(id: &str) -> Group {
        Group {
            id: id.into(),
            name: if id == "target" {
                "目标群"
            } else {
                "排除群"
            }
            .into(),
            member_count: Some(if id == "target" { 4 } else { 1 }),
            role: "member".into(),
        }
    }

    impl MemberProvider for FakeProvider {
        fn source(&self) -> &'static str {
            "web"
        }

        fn account<'life0, 'async_trait>(&'life0 self) -> ProviderFuture<'async_trait, Account>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.trace.lock().unwrap().push("account");
                let mut value = account();
                if self.account_calls.fetch_add(1, Ordering::SeqCst) > 0
                    && self.change_final_account
                {
                    value.qq_number = "90002".into();
                }
                Ok(value)
            })
        }

        fn groups<'life0, 'async_trait>(&'life0 self) -> ProviderFuture<'async_trait, Vec<Group>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.trace.lock().unwrap().push("groups");
                Ok(vec![
                    group("target"),
                    group("excluded"),
                    group("900001"),
                    group("900002"),
                    group("900003"),
                ])
            })
        }

        fn fetch<'life0, 'life1, 'async_trait>(
            &'life0 self,
            group: &'life1 Group,
            _progress: Progress,
            _cancel: CancellationToken,
        ) -> ProviderFuture<'async_trait, FetchResult>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let numbers = if group.id == "target" || group.id == "900001" {
                    self.trace.lock().unwrap().push("target");
                    vec!["10001", "10002", "10003", "10004"]
                } else if group.id == "900002" {
                    self.trace.lock().unwrap().push("second_target");
                    vec!["10002", "10005"]
                } else {
                    self.trace.lock().unwrap().push("excluded");
                    vec!["10001"]
                };
                Ok(FetchResult {
                    reported_count: Some(numbers.len()),
                    members: numbers
                        .into_iter()
                        .map(|number| Member {
                            qq_number: number.into(),
                            nickname: "合成成员".into(),
                            card: String::new(),
                            // 即使群接口给出错误或未知关系，也必须由本次完整好友集合覆盖。
                            friend_relation: if number == "10001" {
                                FriendRelation::NotFriend
                            } else {
                                FriendRelation::Unknown
                            },
                        })
                        .collect(),
                    freshness_confirmed: true,
                    truncated: false,
                    warnings: vec![],
                })
            })
        }

        fn friends<'life0, 'async_trait>(
            &'life0 self,
            cancel: CancellationToken,
        ) -> ProviderFuture<'async_trait, FriendList>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.friends_calls.fetch_add(1, Ordering::SeqCst);
                self.trace.lock().unwrap().push("friends");
                let mut owner = account();
                match self.reply {
                    FriendReply::Error => {
                        return Err(AppError::new(
                            "synthetic_friends_failure",
                            "合成好友读取失败。",
                        ))
                    }
                    FriendReply::Cancel => {
                        cancel.cancel();
                        return std::future::pending().await;
                    }
                    FriendReply::Pending => return std::future::pending().await,
                    FriendReply::OtherAccount => owner.qq_number = "90002".into(),
                    FriendReply::OtherSource => owner.source = "synthetic_other_source".into(),
                    FriendReply::Complete => {}
                }
                Ok(FriendList {
                    account: owner,
                    // 90099 不在目标群中，最终快照不得保存这个额外好友号码。
                    qq_numbers: ["10001", "10002", "90099"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                })
            })
        }
    }

    fn fixture(
        reply: FriendReply,
        exclude_friends: bool,
        change_final_account: bool,
    ) -> (FetchRequest, Arc<FakeProvider>) {
        let provider = Arc::new(FakeProvider {
            reply,
            friends_calls: AtomicUsize::new(0),
            account_calls: AtomicUsize::new(0),
            change_final_account,
            trace: Mutex::new(vec![]),
        });
        let request = FetchRequest {
            provider: provider.clone(),
            account: account(),
            group: group("target"),
            additional_groups: vec![],
            exclusions: vec![],
            sent_filter: None,
            exclude_friends,
            id: "synthetic-task".into(),
            cancel: CancellationToken::new(),
        };
        (request, provider)
    }

    #[tokio::test]
    async fn full_pipeline_reads_friends_once_and_keeps_group_friend_sent_priority() {
        let (mut request, provider) = fixture(FriendReply::Complete, true, false);
        request.exclusions.push(group("excluded"));
        request.sent_filter = Some(
            ["10002", "10003"]
                .into_iter()
                .map(str::to_owned)
                .collect::<HashSet<_>>(),
        );
        let messages = Arc::new(Mutex::new(Vec::new()));
        let capture = messages.clone();
        let progress: Progress = Arc::new(move |message| capture.lock().unwrap().push(message));
        let snapshot = collect_snapshot(&request, progress, Local::now().to_rfc3339())
            .await
            .unwrap();
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.trace.lock().unwrap().as_slice(),
            ["account", "groups", "target", "friends", "excluded", "account"]
        );
        assert!(messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| message == "正在读取 QQ 好友…"));
        assert_eq!(snapshot.source_count, 4);
        assert_eq!(
            (
                snapshot.excluded_count,
                snapshot.friend_filtered_count,
                snapshot.sent_filtered_count
            ),
            (1, 1, 1)
        );
        assert_eq!(snapshot.members[0].qq_number, "10004");
        assert_eq!(snapshot.members.len(), 1);
        assert_eq!(snapshot.friend_filter_status, "completed");
        assert_eq!(snapshot.friend_unknown_count, 0);
        assert_eq!(
            snapshot
                .filtered_members
                .iter()
                .find(|item| item.member.qq_number == "10001")
                .unwrap()
                .reason,
            "group"
        );
        assert_eq!(
            snapshot
                .filtered_members
                .iter()
                .find(|item| item.member.qq_number == "10002")
                .unwrap()
                .reason,
            "friend"
        );
        assert!(!serde_json::to_string(&snapshot).unwrap().contains("90099"));
    }

    #[tokio::test]
    async fn disabled_friend_option_never_calls_friend_endpoint() {
        let (request, provider) = fixture(FriendReply::Error, false, false);
        let snapshot = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap();
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider.account_calls.load(Ordering::SeqCst), 2);
        assert!(!snapshot.exclude_friends);
        assert_eq!(snapshot.members.len(), 4);
        assert_eq!(snapshot.friend_filter_status, "not_requested");
    }

    /// 多目标先合并再统一过滤；好友只读一次，跨群重合只计一次且原始归属可追溯。
    #[tokio::test]
    async fn multi_target_pipeline_reuses_one_friend_list_and_one_filter_sequence() {
        let (mut request, provider) = fixture(FriendReply::Complete, true, false);
        request.group = group("900001");
        request.additional_groups.push(group("900002"));
        request.exclusions.push(group("900003"));
        request.sent_filter = Some(["10003".to_owned()].into_iter().collect());
        let snapshot = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap();
        assert_eq!(snapshot.id, request.id);
        assert_eq!(snapshot.group.id, "multi:900001,900002");
        assert_eq!(snapshot.source_groups.len(), 2);
        assert_eq!(snapshot.source_count, 5);
        assert_eq!(snapshot.duplicate_count, 1);
        assert_eq!(snapshot.reported_count, None);
        assert_eq!(
            snapshot
                .members
                .iter()
                .map(|m| m.qq_number.as_str())
                .collect::<Vec<_>>(),
            ["10004", "10005"]
        );
        assert_eq!(
            (
                snapshot.excluded_count,
                snapshot.friend_filtered_count,
                snapshot.sent_filtered_count
            ),
            (1, 1, 1)
        );
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.account_calls.load(Ordering::SeqCst), 2);
    }

    /// 缺失的后续目标或中途取消必须返回错误；收集函数不写库，run_fetch只提交整份成功结果。
    #[tokio::test]
    async fn missing_later_target_or_cancel_cannot_return_partial_multi_snapshot() {
        let (mut request, provider) = fixture(FriendReply::Complete, true, false);
        request.group = group("900001");
        request.additional_groups.push(group("900004"));
        let error = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap_err();
        assert_eq!(error.code, "group_missing");
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 0);
        request.cancel.cancel();
        let error = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap_err();
        assert_eq!(error.code, "cancelled");
    }

    #[tokio::test]
    async fn friend_read_failure_returns_no_new_snapshot_or_later_group_reads() {
        let (mut request, provider) = fixture(FriendReply::Error, true, false);
        request.exclusions.push(group("excluded"));
        let error = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap_err();
        assert_eq!(error.code, "synthetic_friends_failure");
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.trace.lock().unwrap().as_slice(),
            ["account", "groups", "target", "friends"]
        );
    }

    #[tokio::test]
    async fn cancellation_during_friend_read_drops_the_result() {
        let (request, provider) = fixture(FriendReply::Cancel, true, false);
        let error = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.account_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelled_before_friend_step_does_not_start_another_request() {
        let (request, provider) = fixture(FriendReply::Complete, true, false);
        request.cancel.cancel();
        let target = verify::create_snapshot(
            "synthetic-target".into(),
            account(),
            group("target"),
            provider
                .fetch(&group("target"), Arc::new(|_| {}), CancellationToken::new())
                .await
                .unwrap(),
            Local::now().to_rfc3339(),
            Local::now().to_rfc3339(),
        );
        let progress: Progress = Arc::new(|_| {});
        let error =
            bind_requested_friend_list(provider.as_ref(), target, true, request.cancel, &progress)
                .await
                .unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn friend_account_or_source_mismatch_cannot_complete_filtering() {
        for reply in [FriendReply::OtherAccount, FriendReply::OtherSource] {
            let (request, provider) = fixture(reply, true, false);
            assert!(
                collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
                    .await
                    .is_err()
            );
            assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.account_calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn final_account_check_still_rejects_identity_change_after_friends() {
        let (request, provider) = fixture(FriendReply::Complete, true, true);
        let error = collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339())
            .await
            .unwrap_err();
        assert_eq!(error.code, "account_changed");
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.account_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pending_friend_read_obeys_task_timeout_without_producing_snapshot() {
        let (request, provider) = fixture(FriendReply::Pending, true, false);
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            collect_snapshot(&request, Arc::new(|_| {}), Local::now().to_rfc3339()),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(provider.friends_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.account_calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod group_catalog_tests {
    use super::*;
    use member_core::database::Database;

    /// 无网络提交回归：任意新扫码身份都可替换，但必须仍属于当前 pending 会话。
    #[test]
    fn latest_scan_replaces_identity_but_stale_cancelled_scan_cannot_commit() {
        let state = AppState::new();
        let account = |number: &str| Account {
            qq_number: number.into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        };
        let group = |id: &str| Group {
            id: id.into(),
            name: "合成群".into(),
            member_count: Some(1),
            role: "member".into(),
        };
        let old = Arc::new(WebProvider::new().unwrap());
        let stale = Arc::new(WebProvider::new().unwrap());
        let latest = Arc::new(WebProvider::new().unwrap());
        {
            let mut inner = state.inner.lock().unwrap();
            inner.web = Some(old.clone());
            inner.view.account = Some(account("10001"));
            inner.view.groups = vec![group("90001")];
            inner.view.history_count = 5;
            inner.pending_web = Some(latest.clone());
        }
        assert_eq!(
            commit_qr_login(&state, stale, account("10002"), vec![])
                .unwrap_err()
                .code,
            "login_cancelled"
        );
        assert!(Arc::ptr_eq(
            state.inner.lock().unwrap().web.as_ref().unwrap(),
            &old
        ));
        commit_qr_login(&state, latest, account("10003"), vec![group("90003")]).unwrap();
        assert_eq!(state.view().account.unwrap().qq_number, "10003");
        assert_eq!(state.view().groups[0].id, "90003");
        assert_eq!(state.view().history_count, 5);
        assert_eq!(state.view().session.status, "active");
        assert!(state.view().snapshot.is_none());
        let cancelled = Arc::new(WebProvider::new().unwrap());
        assert_eq!(
            commit_qr_login(&state, cancelled, account("10004"), vec![])
                .unwrap_err()
                .code,
            "login_cancelled"
        );
        assert_eq!(state.view().account.unwrap().qq_number, "10003");
        let same = Arc::new(WebProvider::new().unwrap());
        state.inner.lock().unwrap().pending_web = Some(same.clone());
        commit_qr_login(&state, same, account("10003"), vec![group("90005")]).unwrap();
        assert_eq!(state.view().groups[0].id, "90005");
    }

    /// 合成目录只写入随机临时 SQLite；持久化本身不得创建连接身份或改变当前可见目录。
    #[tokio::test]
    async fn verified_catalog_survives_reopen_without_publishing_cached_login() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let database = Database::open(&path).unwrap();
        let state = AppState::new();
        state.initialize_database(database).unwrap();
        let account = Account {
            qq_number: "900001".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        };
        let groups = vec![Group {
            id: "800001".into(),
            name: "合成群目录".into(),
            member_count: Some(35),
            role: "member".into(),
        }];
        let _operation = state.operation.lock().await;
        persist_group_catalog(&state, &account, &groups)
            .await
            .unwrap();
        let reopened = Database::open(path).unwrap();
        assert_eq!(reopened.saved_group_catalog(&account).unwrap(), groups);
        assert!(state.view().account.is_none());
        assert!(state.view().groups.is_empty());
        assert_eq!(state.view().session.status, "signed_out");
    }

    /// 模拟数据库位置不可用，验证失败明确返回且不把待保存目录覆盖到现有界面。
    #[tokio::test]
    async fn failed_catalog_write_preserves_existing_view() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let database = Database::open(&path).unwrap();
        let state = AppState::new();
        state.initialize_database(database).unwrap();
        let old = Group {
            id: "800001".into(),
            name: "旧合成目录".into(),
            member_count: None,
            role: "member".into(),
        };
        state.inner.lock().unwrap().view.groups = vec![old.clone()];
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let account = Account {
            qq_number: "900001".into(),
            nickname: "合成账号".into(),
            source: "web".into(),
        };
        let changed = Group {
            name: "本次新目录".into(),
            ..old.clone()
        };
        let _operation = state.operation.lock().await;
        assert!(persist_group_catalog(&state, &account, &[changed])
            .await
            .is_err());
        assert_eq!(state.view().groups, vec![old]);
        assert!(state.view().account.is_none());
    }
}
