//! 已发送记录的本地维护，人工记录与SMTP接受记录区分展示。作者：ahui。

use crate::{commands::publish, state::AppState};
use member_core::{
    database::{Database, ImportOutcome, SentRecord, UnresolvedDelivery},
    diagnostics,
    models::*,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

/// total 为成功/人工历史真实总量；records 最近 10000 条，unresolved 为全部待人工核实记录。
#[derive(Serialize)]
pub struct HistoryView {
    records: Vec<SentRecord>,
    total: usize,
    unresolved: Vec<UnresolvedDelivery>,
}

/// 读取与单条/批量核实共享同一返回契约，页面可直接替换列表，不用拼接旧状态。
fn read_history(database: &Database) -> AppResult<HistoryView> {
    Ok(HistoryView {
        records: database.sent_records()?,
        total: database.sent_count()?,
        unresolved: database.unresolved_deliveries()?,
    })
}

/// 读取成功/人工历史和待核实投递；unknown 单列，不被当成成功或可安全重试。
#[tauri::command]
pub async fn get_sent_history(state: State<'_, Arc<AppState>>) -> AppResult<HistoryView> {
    let mut diagnostic = diagnostics::operation("history", "read");
    let database = state.database()?;
    let history = tokio::task::spawn_blocking(move || read_history(&database))
        .await
        .map_err(|_| AppError::new("history_error", "历史记录读取中断，请重试。"))??;
    diagnostics::event(
        "info",
        "history",
        "read_completed",
        serde_json::json!({"total": history.total, "visible": history.records.len(), "unresolved": history.unresolved.len()}),
    );
    diagnostic.finish("success");
    Ok(history)
}

/// 用户人工核实一条不明投递；accepted=true 记人工确认已发送，false 允许该条重试。
/// 运行中任务禁止修改；只处理指定 unknown，重复提交或发送中记录由数据库拒绝。
/// 返回更新后的完整列表；人工核实不会声称程序收到了 SMTP 接受证明。
#[tauri::command]
pub async fn resolve_mail_delivery(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
    accepted: bool,
) -> AppResult<HistoryView> {
    resolve_selected_deliveries(&app, &state, vec![id], accepted).await
}

/// 用户明确选择的一批待核实投递，统一记为人工确认已发送或未发送；不启动 SMTP 或自动重试。
/// 原始输入最多 10000 条，数据库去重并原子核验全部状态，任一过期选择使整批保持原样。
#[tauri::command]
pub async fn resolve_mail_deliveries(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    ids: Vec<i64>,
    accepted: bool,
) -> AppResult<HistoryView> {
    resolve_selected_deliveries(&app, &state, ids, accepted).await
}

/// 人工核实与获取/发送共用门闩；提交后刷新失败应明确提示已保存，不能让用户误以为整批回滚。
async fn resolve_selected_deliveries(
    app: &AppHandle,
    state: &AppState,
    ids: Vec<i64>,
    accepted: bool,
) -> AppResult<HistoryView> {
    let mut diagnostic = diagnostics::operation("history", "resolve_selected");
    diagnostics::event(
        "info",
        "history",
        "resolution_requested",
        serde_json::json!({"input_count": ids.len(), "accepted": accepted}),
    );
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let history = tokio::task::spawn_blocking(move || {
        let count = database.resolve_deliveries(&ids, accepted)?;
        diagnostics::event(
            "info",
            "history",
            "resolution_saved",
            serde_json::json!({"count": count, "accepted": accepted}),
        );
        read_history(&database).map_err(|_| {
            AppError::new(
                "history_refresh",
                "核实结果已保存，但记录列表刷新失败，请刷新记录确认当前状态。",
            )
        })
    })
    .await
    .map_err(|_| {
        AppError::new(
            "history_error",
            "核实结果保存中断，请刷新记录确认当前状态。",
        )
    })?;
    // 核实提交后即使列表刷新失败，也尽量发布新的历史计数；返回原刷新错误要求页面重新读取。
    if let Err(error) = history {
        diagnostics::event(
            "warn",
            "history",
            "resolution_refresh_failed",
            serde_json::json!({"result_saved": true}),
        );
        let _ = update_count(app, state);
        return Err(error);
    }
    let history = history?;
    update_count(app, state).map_err(|_| {
        AppError::new(
            "history_refresh",
            "核实结果已保存，但历史计数刷新失败，请刷新记录确认当前状态。",
        )
    })?;
    diagnostic.finish("success");
    Ok(history)
}

/// 刷新公开计数，发送和人工维护共用同一 SQLite 历史来源。
pub(crate) fn update_count(app: &AppHandle, state: &AppState) -> AppResult<()> {
    // 历史发生变化后旧确认范围不能再用；只读刷新不经过此处，不反复过滤当前计划。
    let invalidated = state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if invalidated.is_some() {
        diagnostics::event(
            "info",
            "recipients",
            "plan_invalidated",
            serde_json::json!({"reason": "history_changed"}),
        );
    }
    let count = state.database()?.sent_count()?;
    state
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .view
        .history_count = count;
    publish(app, state);
    Ok(())
}

/// 用户明确粘贴“已发送”的记录；非数字 QQ 邮箱不会猜测对应 QQ 号。
#[tauri::command]
pub async fn add_sent_records(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    text: String,
) -> AppResult<ImportOutcome> {
    let mut diagnostic = diagnostics::operation("history", "add_records");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let result = tokio::task::spawn_blocking(move || database.import_sent(&text, "manual"))
        .await
        .map_err(|_| AppError::new("history_error", "记录保存中断，请重试。"))??;
    update_count(&app, &state)?;
    diagnostics::event(
        "info",
        "history",
        "records_added",
        serde_json::json!({"added": result.added, "skipped": result.skipped, "invalid": result.invalid}),
    );
    diagnostic.finish("success");
    Ok(result)
}

/// 只有原生文件选择器可提供路径，文本必须是 UTF-8/BOM；不猜测系统 ANSI 编码。
#[tauri::command]
pub async fn import_sent_records(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> AppResult<Option<ImportOutcome>> {
    let mut diagnostic = diagnostics::operation("history", "import_records");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导入已发送 QQ 号或邮箱（UTF-8）")
        .add_filter("文本或 CSV", &["txt", "csv"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let path = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "无法取得所选文件。"))?;
    let Some(path) = path else {
        diagnostic.finish("cancelled");
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|_| AppError::new("invalid_path", "请选择本机文本文件。"))?;
    let database = state.database()?;
    let result = tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&path)
            .map_err(|_| AppError::new("import_read", "无法读取导入文件。"))?;
        if metadata.len() > 4 * 1024 * 1024 {
            return Err(AppError::new("import_large", "导入文件不能超过 4 MiB。"));
        }
        // File size can change after metadata. Limit the actual read before UTF-8
        // decoding, so a growing file cannot cause an unbounded allocation.
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| AppError::new("import_read", "无法读取导入文件。"))?;
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::new("import_read", "无法读取导入文件。"))?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(AppError::new("import_large", "导入文件不能超过 4 MiB。"));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            AppError::new(
                "import_encoding",
                "文件必须是 UTF-8 编码的 TXT/CSV，请转换后重试。",
            )
        })?;
        database.import_sent(&text, "imported")
    })
    .await
    .map_err(|_| AppError::new("import_error", "文件导入中断，请重试。"))??;
    update_count(&app, &state)?;
    diagnostics::event(
        "info",
        "history",
        "records_imported",
        serde_json::json!({"added": result.added, "skipped": result.skipped, "invalid": result.invalid}),
    );
    diagnostic.finish("success");
    Ok(Some(result))
}

/// 删除后该地址将不再参与已发送过滤；前端需先让用户确认这一影响。
#[tauri::command]
pub async fn remove_sent_record(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> AppResult<()> {
    let mut diagnostic = diagnostics::operation("history", "remove_record");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    tokio::task::spawn_blocking(move || database.remove_sent(id))
        .await
        .map_err(|_| AppError::new("history_error", "删除操作中断，请重试。"))??;
    update_count(&app, &state)?;
    diagnostic.finish("success");
    Ok(())
}
