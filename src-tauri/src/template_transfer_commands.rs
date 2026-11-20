//! 邮件模板文件交换的原生边界。作者：ahui。
//! 页面只提交模板标识或短期确认令牌；文件路径、包字节和完整资源始终留在宿主。
//! 导入先限长读取、核心校验并预览，再按固定目录基线整批提交，不修改当前邮件草稿或发送状态。

use crate::state::AppState;
use member_core::{
    diagnostics,
    models::{AppError, AppResult},
    templates::{
        decode_template_package, TemplateImportPolicy, TemplateImportPreview, TemplatePackage,
        TemplateSummary, MAX_TEMPLATE_PACKAGE_BYTES,
    },
};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tempfile::NamedTempFile;

/// 包格式使用专属扩展名，不能把模板文件另存覆盖应用数据库或凭据文件。
const PACKAGE_EXTENSION: &str = "qqmailtemplates";
/// 五分钟只限制本次预览确认；不会给模板或附件引入持久有效期。
const IMPORT_CONFIRMATION_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// 只在内存保留一个已校验包；Arc 避免提交时复制最多 80 MiB 的正文和资源。
/// expires_at 使用单调时钟，系统时间校准不会延长或缩短已经展示的确认窗口。
pub(crate) struct PendingTemplateImport {
    token: String,
    expires_at: Instant,
    package: TemplatePackage,
    preview: TemplateImportPreview,
}

/// 取消保存返回 saved=false、count=0；成功人数仅表示写入的模板数，不包含任何磁盘路径。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateExportOutcome {
    saved: bool,
    count: usize,
}

/// 导入确认仅向页面提供轻量元信息，HTML、资源标识和附件字节不能从预览泄露到渲染层。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportItemView {
    name: String,
    subject: String,
    inline_image_count: usize,
    attachment_count: usize,
    conflict: bool,
}

/// token 只能确认本次已读取的文件；total_bytes 是原文件 UTF-8 JSON 字节数，含资源的编码开销。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportPreviewView {
    token: String,
    templates: Vec<TemplateImportItemView>,
    conflict_count: usize,
    overwrite_allowed: bool,
    total_bytes: usize,
}

/// 只返回本次新增/替换的摘要，跳过项不重复返回；页面可合并摘要而不重建当前草稿。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportResultView {
    added: usize,
    replaced: usize,
    skipped: usize,
    templates: Vec<TemplateSummary>,
}

/// 阻塞任务或文件故障只返回固定消息，系统异常可能包含用户路径，不能直接格式化输出。
fn transfer_task_error() -> AppError {
    AppError::new(
        "template_transfer_task",
        "模板文件操作中断，请重试；原有模板和草稿保留。",
    )
}

/// 对明确选中的模板生成完整包，再由原生保存对话框选择目标；单个、多个及全选共用此入口。
/// operation 覆盖数据库读取、对话框和原子写入，避免导出过程中被模板维护或业务任务改写范围。
#[tauri::command]
pub async fn mail_template_export(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
) -> AppResult<TemplateExportOutcome> {
    let mut diagnostic = diagnostics::operation("templates", "export_package");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let (bytes, count) = tokio::task::spawn_blocking(move || {
        // 先由核心校验选择总量和标识，再为已经受限的有效选择去重计数。
        let bytes = database.export_mail_template_package(&ids)?;
        let count = ids.iter().collect::<HashSet<_>>().len();
        Ok::<_, AppError>((bytes, count))
    })
    .await
    .map_err(|_| transfer_task_error())??;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导出邮件模板（包含正文图片和附件）")
        .add_filter("邮件模板包", &[PACKAGE_EXTENSION])
        .set_file_name("邮件模板.qqmailtemplates")
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let selected = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "无法取得模板保存位置，请重试。"))?;
    let path = selected
        .map(|selected| {
            selected
                .into_path()
                .map_err(|_| AppError::new("invalid_path", "请选择本机文件保存位置。"))
        })
        .transpose()?;
    let byte_count = bytes.len();
    let result = tokio::task::spawn_blocking(move || save_package_selection(&bytes, count, path))
        .await
        .map_err(|_| transfer_task_error())??;
    diagnostics::event(
        "info",
        "templates",
        "package_export_finished",
        serde_json::json!({"saved":result.saved,"count":result.count,"bytes":if result.saved {byte_count} else {0}}),
    );
    diagnostic.finish(if result.saved { "success" } else { "cancelled" });
    Ok(result)
}

/// 只接收原生选择器的路径，限长读取后把文件转换为核心校验模型；取消选择不报错、不写数据库。
/// 开始重选即释放旧预览，不能在取消新文件后误提交之前那个包。
#[tauri::command]
pub async fn mail_template_import_preview(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> AppResult<Option<TemplateImportPreviewView>> {
    let mut diagnostic = diagnostics::operation("templates", "preview_import_package");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    *state
        .pending_template_import
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导入邮件模板包")
        .add_filter("邮件模板包", &[PACKAGE_EXTENSION])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let selected = receiver
        .await
        .map_err(|_| AppError::new("dialog_error", "无法取得所选模板包，请重试。"))?;
    let Some(selected) = selected else {
        diagnostic.finish("cancelled");
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|_| AppError::new("invalid_path", "请选择本机模板包文件。"))?;
    let database = state.database()?;
    let (package, preview, total_bytes) = tokio::task::spawn_blocking(move || {
        let bytes = read_package_file(&path)?;
        let package = decode_template_package(&bytes)?;
        let preview = database.preview_template_import(&package)?;
        Ok::<_, AppError>((package, preview, bytes.len()))
    })
    .await
    .map_err(|_| transfer_task_error())??;
    let pending = Arc::new(PendingTemplateImport {
        token: uuid::Uuid::new_v4().to_string(),
        expires_at: Instant::now() + IMPORT_CONFIRMATION_LIFETIME,
        package,
        preview,
    });
    let response = preview_view(&pending, total_bytes);
    *state
        .pending_template_import
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(pending.clone());
    // 只捕获弱宿主引用和令牌，不让过期定时器额外保留资源；旧定时器不能清除重选后的新包。
    let owner = Arc::downgrade(state.inner());
    let token = pending.token.clone();
    let deadline = pending.expires_at;
    tokio::spawn(async move {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        if let Some(owner) = owner.upgrade() {
            expire_pending(&owner, &token, Instant::now());
        }
    });
    diagnostics::event(
        "info",
        "templates",
        "package_import_previewed",
        serde_json::json!({"count":response.templates.len(),"conflicts":response.conflict_count,"bytes":total_bytes}),
    );
    diagnostic.finish("success");
    Ok(Some(response))
}

/// 用户明确选择冲突策略后提交整批模板；目录基线由核心事务重新比较，变化时必须重新选择文件。
/// 成功、过期或基线失效释放包；普通可重试错误保留包及固定预览，不触碰正在编辑的正文/附件。
#[tauri::command]
pub async fn mail_template_import_commit(
    state: State<'_, Arc<AppState>>,
    token: String,
    policy: TemplateImportPolicy,
) -> AppResult<TemplateImportResultView> {
    let mut diagnostic = diagnostics::operation("templates", "commit_import_package");
    let result = commit_pending_import(&state, token, policy).await?;
    diagnostics::event(
        "info",
        "templates",
        "package_import_committed",
        serde_json::json!({"added":result.added,"replaced":result.replaced,"skipped":result.skipped}),
    );
    diagnostic.finish("success");
    Ok(result)
}

/// 核心提交流程与 IPC 响应分开，使隔离测试能够覆盖真实任务门闩、失效处理及整批提交行为。
async fn commit_pending_import(
    state: &AppState,
    token: String,
    policy: TemplateImportPolicy,
) -> AppResult<TemplateImportResultView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let pending = pending_for_commit(state, &token, Instant::now())?;
    let database = state.database()?;
    let result = tokio::task::spawn_blocking(move || {
        database.import_template_package(&pending.package, &pending.preview, policy)
    })
    .await
    .map_err(|_| transfer_task_error())?;
    match result {
        Ok(result) => {
            clear_matching_pending(state, &token);
            Ok(TemplateImportResultView {
                added: result.added,
                replaced: result.replaced,
                skipped: result.skipped,
                templates: result.summaries,
            })
        }
        Err(error) => {
            if error.code == "template_import_stale" {
                clear_matching_pending(state, &token);
            }
            Err(error)
        }
    }
}

/// 显式取消仅释放同一令牌的内存包；不回滚先前已提交模板，也不清除另一轮预览。
/// 无待确认包时幂等成功，便于页面在本机五分钟截止后正常关闭确认窗口。
#[tauri::command]
pub async fn mail_template_import_cancel(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> AppResult<()> {
    let mut diagnostic = diagnostics::operation("templates", "cancel_import_package");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    cancel_pending(&state, &token, Instant::now())?;
    diagnostic.finish("cancelled");
    Ok(())
}

/// 有界读取同时检查文件类型、元信息和实际读取量，防止文件增长绕过 128 MiB 限制。
fn read_package_file(path: &Path) -> AppResult<Vec<u8>> {
    let file = File::open(path).map_err(|_| {
        AppError::new(
            "template_package_read",
            "无法读取模板包，请检查文件权限后重试。",
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::new("template_package_read", "无法读取模板包，请重新选择文件。"))?;
    if !metadata.is_file() || metadata.len() > MAX_TEMPLATE_PACKAGE_BYTES as u64 {
        return Err(AppError::new(
            "template_package_limit",
            "请选择不超过 128 MiB 的模板包文件。",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_TEMPLATE_PACKAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            AppError::new(
                "template_package_read",
                "模板包读取未完成，请重新选择文件。",
            )
        })?;
    if bytes.len() > MAX_TEMPLATE_PACKAGE_BYTES {
        return Err(AppError::new(
            "template_package_limit",
            "模板包不能超过 128 MiB。",
        ));
    }
    Ok(bytes)
}

/// 仅完整写入同目录临时文件并同步成功后替换目标；写入失败时原文件不被截断或清空。
fn save_package_selection(
    bytes: &[u8],
    count: usize,
    path: Option<PathBuf>,
) -> AppResult<TemplateExportOutcome> {
    let Some(path) = path else {
        return Ok(TemplateExportOutcome {
            saved: false,
            count: 0,
        });
    };
    if !path.extension().is_some_and(|extension| {
        extension
            .to_string_lossy()
            .eq_ignore_ascii_case(PACKAGE_EXTENSION)
    }) {
        return Err(AppError::new(
            "template_package_extension",
            "请使用 .qqmailtemplates 扩展名保存模板包。",
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| AppError::new("template_package_write", "保存目录不可用，请重新选择。"))?;
    let write_error = || {
        AppError::new(
            "template_package_write",
            "模板包保存失败，请检查磁盘空间和目录权限；原文件保留。",
        )
    };
    let mut temporary = NamedTempFile::new_in(parent).map_err(|_| write_error())?;
    temporary.write_all(bytes).map_err(|_| write_error())?;
    temporary.flush().map_err(|_| write_error())?;
    temporary.as_file().sync_all().map_err(|_| write_error())?;
    temporary.persist(&path).map_err(|_| {
        AppError::new(
            "template_package_replace",
            "无法替换模板包，请关闭占用文件的程序或另选文件名；原文件保留。",
        )
    })?;
    Ok(TemplateExportOutcome { saved: true, count })
}

/// 显式映射核心字段，避免以后核心模型新增正文/资源字段时意外扩展 IPC 数据范围。
fn preview_view(pending: &PendingTemplateImport, total_bytes: usize) -> TemplateImportPreviewView {
    TemplateImportPreviewView {
        token: pending.token.clone(),
        templates: pending
            .preview
            .entries
            .iter()
            .map(|entry| TemplateImportItemView {
                name: entry.name.clone(),
                subject: entry.subject.clone(),
                inline_image_count: entry.image_count,
                attachment_count: entry.attachment_count,
                conflict: entry.conflict,
            })
            .collect(),
        conflict_count: pending.preview.conflict_count,
        overwrite_allowed: pending.preview.overwrite_allowed,
        total_bytes,
    }
}

/// 校验时先淘汰过期包；错误令牌不释放仍有效的另一轮确认，资源也不通过错误消息返回。
fn pending_for_commit(
    state: &AppState,
    token: &str,
    now: Instant,
) -> AppResult<Arc<PendingTemplateImport>> {
    let mut slot = state
        .pending_template_import
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|pending| now >= pending.expires_at)
    {
        *slot = None;
    }
    let pending = slot.as_ref().ok_or_else(|| {
        AppError::new(
            "template_import_expired",
            "模板导入确认已过期，请重新选择文件。",
        )
    })?;
    if token.len() > 128 || token != pending.token {
        return Err(AppError::new(
            "template_import_token",
            "模板导入确认不匹配，请重新选择文件。",
        ));
    }
    Ok(pending.clone())
}

/// 完成或基线失效后只释放对应包，旧命令/旧计时器不能清除后续新确认。
fn clear_matching_pending(state: &AppState, token: &str) {
    let mut slot = state
        .pending_template_import
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot.as_ref().is_some_and(|pending| pending.token == token) {
        *slot = None;
    }
}

/// 定时清理同时校验 token 和本包截止时间；不争用业务 operation，不执行文件或数据库操作。
fn expire_pending(state: &AppState, token: &str, now: Instant) {
    let mut slot = state
        .pending_template_import
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|pending| pending.token == token && now >= pending.expires_at)
    {
        *slot = None;
    }
}

/// 取消与超时都允许页面关闭；错误令牌不能取消另一轮仍有效的确认。
fn cancel_pending(state: &AppState, token: &str, now: Instant) -> AppResult<()> {
    let mut slot = state
        .pending_template_import
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(pending) = slot.as_ref() {
        if now < pending.expires_at && (token.len() > 128 || token != pending.token) {
            return Err(AppError::new(
                "template_import_token",
                "模板导入确认不匹配，请重新选择文件。",
            ));
        }
    }
    *slot = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use member_core::database::Database;
    use tempfile::TempDir;

    /// 所有正文、模板和数据库均在随机临时目录中构造；不读取真实模板库、草稿、凭据或网络。
    fn fixture() -> (TempDir, Arc<AppState>, Arc<PendingTemplateImport>) {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("synthetic.sqlite3")).unwrap();
        let saved = database
            .save_mail_template("合成模板", "合成主题", "<p>隔离内容</p>", &[], None)
            .unwrap();
        let bytes = database.export_mail_template_package(&[saved.id]).unwrap();
        let package = decode_template_package(&bytes).unwrap();
        let preview = database.preview_template_import(&package).unwrap();
        let pending = Arc::new(PendingTemplateImport {
            token: uuid::Uuid::new_v4().to_string(),
            expires_at: Instant::now() + IMPORT_CONFIRMATION_LIFETIME,
            package,
            preview,
        });
        let state = Arc::new(AppState::new());
        state.initialize_database(database).unwrap();
        *state.pending_template_import.lock().unwrap() = Some(pending.clone());
        (directory, state, pending)
    }

    #[test]
    fn wrong_token_does_not_drop_valid_preview_and_deadline_is_exclusive() {
        let (_directory, state, pending) = fixture();
        assert_eq!(
            pending_for_commit(&state, "wrong", Instant::now())
                .err()
                .unwrap()
                .code,
            "template_import_token"
        );
        assert!(state.pending_template_import.lock().unwrap().is_some());
        assert!(pending_for_commit(
            &state,
            &pending.token,
            pending.expires_at - Duration::from_nanos(1)
        )
        .is_ok());
        assert_eq!(
            pending_for_commit(&state, &pending.token, pending.expires_at)
                .err()
                .unwrap()
                .code,
            "template_import_expired"
        );
        assert!(state.pending_template_import.lock().unwrap().is_none());
    }

    #[test]
    fn stale_timer_and_wrong_cancel_cannot_release_new_preview() {
        let (_directory, state, pending) = fixture();
        expire_pending(&state, "old-preview-token", pending.expires_at);
        assert!(state.pending_template_import.lock().unwrap().is_some());
        expire_pending(
            &state,
            &pending.token,
            pending.expires_at - Duration::from_nanos(1),
        );
        assert!(state.pending_template_import.lock().unwrap().is_some());
        assert_eq!(
            cancel_pending(&state, "wrong", Instant::now())
                .unwrap_err()
                .code,
            "template_import_token"
        );
        assert!(state.pending_template_import.lock().unwrap().is_some());
        cancel_pending(&state, &pending.token, Instant::now()).unwrap();
        cancel_pending(&state, &pending.token, Instant::now()).unwrap();
        assert!(state.pending_template_import.lock().unwrap().is_none());
    }

    #[test]
    fn expiry_releases_package_and_preview_excludes_resource_payloads() {
        let (_directory, state, pending) = fixture();
        let value = serde_json::to_value(preview_view(&pending, 1234)).unwrap();
        assert_eq!(value["totalBytes"], 1234);
        assert!(value["templates"][0].get("inlineImageCount").is_some());
        assert!(value["templates"][0].get("html").is_none());
        assert!(value["templates"][0].get("resources").is_none());
        assert!(value.get("catalog").is_none());
        expire_pending(&state, &pending.token, pending.expires_at);
        assert!(state.pending_template_import.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn commit_obeys_task_gate_and_success_consumes_only_fixed_package() {
        let (_directory, state, pending) = fixture();
        state.attachments.lock().unwrap().insert(
            "existing-draft-attachment".into(),
            crate::mail_commands::StoredAttachment {
                name: "草稿附件.txt".into(),
                bytes: b"existing draft bytes".to_vec(),
                inline: None,
            },
        );
        state.inner.lock().unwrap().view.task.status = "running".into();
        assert_eq!(
            commit_pending_import(&state, pending.token.clone(), TemplateImportPolicy::Copy)
                .await
                .err()
                .unwrap()
                .code,
            "task_running"
        );
        state.inner.lock().unwrap().view.task.status = "idle".into();
        state.mail.lock().unwrap().status = "running".into();
        assert_eq!(
            commit_pending_import(&state, pending.token.clone(), TemplateImportPolicy::Copy)
                .await
                .err()
                .unwrap()
                .code,
            "mail_running"
        );
        state.mail.lock().unwrap().status = "idle".into();
        assert!(state.pending_template_import.lock().unwrap().is_some());
        let result =
            commit_pending_import(&state, pending.token.clone(), TemplateImportPolicy::Copy)
                .await
                .unwrap();
        assert_eq!((result.added, result.replaced, result.skipped), (1, 0, 0));
        assert_eq!(result.templates.len(), 1);
        assert!(state.pending_template_import.lock().unwrap().is_none());
        let attachments = state.attachments.lock().unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments["existing-draft-attachment"].bytes,
            b"existing draft bytes"
        );
        assert!(state.view().account.is_none());
    }

    #[tokio::test]
    async fn catalog_change_requires_new_preview_and_does_not_import_anything() {
        let (_directory, state, pending) = fixture();
        state
            .database()
            .unwrap()
            .save_mail_template("其他合成模板", "", "", &[], None)
            .unwrap();
        let result =
            commit_pending_import(&state, pending.token.clone(), TemplateImportPolicy::Copy).await;
        assert_eq!(result.err().unwrap().code, "template_import_stale");
        assert!(state.pending_template_import.lock().unwrap().is_none());
        assert_eq!(
            state
                .database()
                .unwrap()
                .list_mail_templates("")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn native_save_cancel_is_successful_and_wrong_extension_preserves_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = save_package_selection(b"package", 2, None).unwrap();
        assert!(!cancelled.saved);
        assert_eq!(cancelled.count, 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let protected = directory.path().join("data.sqlite3");
        std::fs::write(&protected, b"old database").unwrap();
        assert_eq!(
            save_package_selection(b"new package", 1, Some(protected.clone()))
                .err()
                .unwrap()
                .code,
            "template_package_extension"
        );
        assert_eq!(std::fs::read(&protected).unwrap(), b"old database");
        let output = directory.path().join("synthetic.qqmailtemplates");
        std::fs::write(&output, b"old package").unwrap();
        let saved = save_package_selection(b"new package", 2, Some(output.clone())).unwrap();
        assert!(saved.saved);
        assert_eq!(saved.count, 2);
        assert_eq!(read_package_file(&output).unwrap(), b"new package");
    }

    #[test]
    fn rejected_atomic_replace_preserves_existing_directory_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("occupied.qqmailtemplates");
        std::fs::create_dir(&target).unwrap();
        let original = target.join("keep.txt");
        std::fs::write(&original, b"keep").unwrap();
        assert_eq!(
            save_package_selection(b"new", 1, Some(target))
                .err()
                .unwrap()
                .code,
            "template_package_replace"
        );
        assert_eq!(std::fs::read(original).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_native_file_is_rejected_before_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("too-large.qqmailtemplates");
        File::create(&path)
            .unwrap()
            .set_len(MAX_TEMPLATE_PACKAGE_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            read_package_file(&path).err().unwrap().code,
            "template_package_limit"
        );
    }
}
