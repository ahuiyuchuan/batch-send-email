//! 邮件内容模板的桌面边界。作者：ahui。
//! SQLite 资源副本与当前草稿句柄相互独立；预览/保存不触发 SMTP，也不接收收件人与凭据。
//! 所有草稿变更与发送共用 operation 门闩；替换先完成全部校验和分配，再一次性交换内存仓库。

use crate::{
    mail_commands::{AttachmentView, InlineImageView, StoredAttachment},
    state::AppState,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use member_core::{
    diagnostics, mail,
    models::{AppError, AppResult},
    templates::{MailTemplate, TemplateResource, TemplateSummary},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tauri::State;

/// 只保存内容及宿主已登记资源；includeAttachments 默认 false，附件句柄与图片句柄不可互换。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveTemplateInput {
    name: String,
    subject: String,
    html: String,
    #[serde(default)]
    inline_image_ids: Vec<String>,
    #[serde(default)]
    attachment_ids: Vec<String>,
    #[serde(default)]
    include_attachments: bool,
    /// 只有明确更新才传入；None 始终新建，不根据名称猜测覆盖目标。
    update_id: Option<String>,
}

/// 预览和应用共用的内容响应；普通附件只提供元信息，图片字节限于已校验的栅格预览。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateContentView {
    id: String,
    name: String,
    subject: String,
    html: String,
    inline_images: Vec<InlineImageView>,
    attachments: Vec<AttachmentView>,
}

/// 数据库任务中断不回显 SQL、正文或文件数据；原模板事务仍由 SQLite 回滚。
fn task_error() -> AppError {
    AppError::new("template_storage", "模板操作中断，请重试；原模板未被清空。")
}

/// 列表只读轻量摘要；query 为名称/主题普通文本，匹配规则和数量边界由核心层统一校验。
#[tauri::command]
pub async fn mail_template_list(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> AppResult<Vec<TemplateSummary>> {
    let mut diagnostic = diagnostics::operation("templates", "list");
    let database = state.database()?;
    let templates = tokio::task::spawn_blocking(move || database.list_mail_templates(&query))
        .await
        .map_err(|_| task_error())??;
    diagnostics::event(
        "info",
        "templates",
        "listed",
        serde_json::json!({"count": templates.len()}),
    );
    diagnostic.finish("success");
    Ok(templates)
}

/// 按固定模板标识读取完整副本；不存在必须刷新，不能自动选另一个模板。
async fn load_template(state: &AppState, id: String) -> AppResult<MailTemplate> {
    let database = state.database()?;
    tokio::task::spawn_blocking(move || {
        database
            .mail_template(&id)?
            .ok_or_else(|| AppError::new("template_missing", "模板已不存在，请刷新列表。"))
    })
    .await
    .map_err(|_| task_error())?
}

/// 预览不登记草稿句柄，不改变当前邮件或模板；远程图和脚本由共用邮件清理器移除。
#[tauri::command]
pub async fn mail_template_preview(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<TemplateContentView> {
    let mut diagnostic = diagnostics::operation("templates", "preview");
    let template = load_template(&state, id).await?;
    let content = prepare_content(&template, false)?.0;
    log_content("previewed", &content);
    diagnostic.finish("success");
    Ok(content)
}

/// 将当前资源复制进单个 SQLite 事务；不保存任何路径、blob URL、收件人或 SMTP 设置。
#[tauri::command]
pub async fn mail_template_save(
    state: State<'_, Arc<AppState>>,
    input: SaveTemplateInput,
) -> AppResult<TemplateSummary> {
    let mut diagnostic = diagnostics::operation("templates", "save");
    diagnostics::event(
        "info",
        "templates",
        "save_requested",
        serde_json::json!({"updating": input.update_id.is_some(), "inline_images": input.inline_image_ids.len(), "attachments": if input.include_attachments { input.attachment_ids.len() } else { 0 }}),
    );
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    if input.html.len() > mail::MAX_BODY_BYTES
        || input.inline_image_ids.len() > mail::MAX_INLINE_IMAGES
        || (input.include_attachments && input.attachment_ids.len() > 10)
    {
        return Err(AppError::new(
            "template_resources",
            "正文或资源数量超限，请精简后重试。",
        ));
    }
    let resources = {
        let stored = state.attachments.lock().unwrap_or_else(|p| p.into_inner());
        collect_resources(&stored, &input)?
    };
    let image_ids = resources
        .iter()
        .filter_map(|resource| resource.content_id.clone())
        .collect();
    let html = mail::sanitize_body_with_images(&input.html, &image_ids);
    let database = state.database()?;
    let saved = tokio::task::spawn_blocking(move || {
        let saved = database.save_mail_template(
            &input.name,
            &input.subject,
            &html,
            &resources,
            input.update_id.as_deref(),
        )?;
        Ok(summary(&saved))
    })
    .await
    .map_err(|_| task_error())??;
    log_summary("saved", &saved);
    diagnostic.finish("success");
    Ok(saved)
}

/// 把明确选中的草稿句柄转为独立资源字节；任一失效或类型不符都会阻止整次保存。
fn collect_resources(
    stored: &HashMap<String, StoredAttachment>,
    input: &SaveTemplateInput,
) -> AppResult<Vec<TemplateResource>> {
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0usize;
    let ids = input.inline_image_ids.iter().map(|id| (id, true)).chain(
        input
            .attachment_ids
            .iter()
            .filter(|_| input.include_attachments)
            .map(|id| (id, false)),
    );
    for (id, inline) in ids {
        if !seen.insert(id) {
            return Err(AppError::new(
                "template_resources",
                "资源标识重复，请重新选择内容。",
            ));
        }
        let file = stored
            .get(id)
            .filter(|file| file.inline.is_some() == inline)
            .ok_or_else(|| {
                AppError::new(
                    "template_resource_missing",
                    "正文图片或附件已失效，请重新插入后保存。",
                )
            })?;
        total = total
            .checked_add(file.bytes.len())
            .filter(|size| *size <= mail::MAX_ATTACHMENT_BYTES)
            .ok_or_else(|| {
                AppError::new("template_resources", "正文图片与附件合计不能超过 18 MiB。")
            })?;
        resources.push(TemplateResource {
            id: id.clone(),
            name: file.name.clone(),
            mime_type: file.inline.as_ref().map(|image| image.mime_type.clone()),
            content_id: file.inline.as_ref().map(|image| image.content_id.clone()),
            bytes: file.bytes.clone(),
        });
    }
    Ok(resources)
}

fn summary(template: &MailTemplate) -> TemplateSummary {
    let inline_image_count = template
        .resources
        .iter()
        .filter(|resource| resource.content_id.is_some())
        .count();
    TemplateSummary {
        id: template.id.clone(),
        name: template.name.clone(),
        subject: template.subject.clone(),
        attachment_count: template.resources.len() - inline_image_count,
        inline_image_count,
        updated_at: template.updated_at.clone(),
    }
}

/// 重命名只修改模板元信息，不改变已经应用到草稿的内容或资源。
#[tauri::command]
pub async fn mail_template_rename(
    state: State<'_, Arc<AppState>>,
    id: String,
    name: String,
) -> AppResult<TemplateSummary> {
    let mut diagnostic = diagnostics::operation("templates", "rename");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let saved = tokio::task::spawn_blocking(move || database.rename_mail_template(&id, &name))
        .await
        .map_err(|_| task_error())??;
    log_summary("renamed", &saved);
    diagnostic.finish("success");
    Ok(saved)
}

/// 复制拥有独立数据库行和资源副本；删除、修改任一模板不会影响另一份。
#[tauri::command]
pub async fn mail_template_duplicate(
    state: State<'_, Arc<AppState>>,
    id: String,
    name: String,
) -> AppResult<TemplateSummary> {
    let mut diagnostic = diagnostics::operation("templates", "duplicate");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let saved = tokio::task::spawn_blocking(move || database.duplicate_mail_template(&id, &name))
        .await
        .map_err(|_| task_error())??;
    log_summary("duplicated", &saved);
    diagnostic.finish("success");
    Ok(saved)
}

/// 删除模板及其资源在同一事务提交；草稿内存是另一份副本，仍可编辑、预览与发送。
#[tauri::command]
pub async fn mail_template_delete(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    let mut diagnostic = diagnostics::operation("templates", "delete");
    let template = diagnostics::correlate(&id);
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    tokio::task::spawn_blocking(move || database.delete_mail_template(&id))
        .await
        .map_err(|_| task_error())??;
    diagnostics::event(
        "info",
        "templates",
        "deleted",
        serde_json::json!({"template": template}),
    );
    diagnostic.finish("success");
    Ok(())
}

/// 用户确认后应用整套内容。旧资源只在全部验证成功后被交换，不逐个清除导致半失败。
#[tauri::command]
pub async fn mail_template_apply(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<TemplateContentView> {
    let mut diagnostic = diagnostics::operation("templates", "apply");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let template = load_template(&state, id).await?;
    let content = replace_content(&state, &template)?;
    log_content("applied", &content);
    diagnostic.finish("success");
    Ok(content)
}

/// 新建空模板工作稿前原子清除正文/附件仓库；收件范围、配置与任务记录完全不在此边界内。
#[tauri::command]
pub async fn mail_clear_draft_content(state: State<'_, Arc<AppState>>) -> AppResult<()> {
    let mut diagnostic = diagnostics::operation("templates", "clear_draft_resources");
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    state
        .attachments
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    diagnostic.finish("success");
    Ok(())
}

/// 只保留资源规模与不可逆标识，模板名称、主题、HTML、附件名和字节均不进入日志。
fn log_summary(event: &str, summary: &TemplateSummary) {
    diagnostics::event(
        "info",
        "templates",
        event,
        serde_json::json!({"template": diagnostics::correlate(&summary.id), "inline_images": summary.inline_image_count, "attachments": summary.attachment_count}),
    );
}

/// 预览与应用共享相同统计边界，资源内容只在既有 IPC 中传递给编辑器。
fn log_content(event: &str, content: &TemplateContentView) {
    diagnostics::event(
        "info",
        "templates",
        event,
        serde_json::json!({"template": diagnostics::correlate(&content.id), "inline_images": content.inline_images.len(), "attachments": content.attachments.len(), "resource_bytes": content.inline_images.iter().map(|image| image.size).sum::<usize>() + content.attachments.iter().map(|file| file.size).sum::<usize>()}),
    );
}

fn replace_content(state: &AppState, template: &MailTemplate) -> AppResult<TemplateContentView> {
    let (view, resources) = prepare_content(template, true)?;
    *state.attachments.lock().unwrap_or_else(|p| p.into_inner()) = resources;
    Ok(view)
}

/// 复用核心保存校验并建立响应/内存副本。remap 为 true 时每次生成新 id/CID，切模板不会串资源。
fn prepare_content(
    template: &MailTemplate,
    remap: bool,
) -> AppResult<(TemplateContentView, HashMap<String, StoredAttachment>)> {
    member_core::templates::validate_template_content(
        &template.name,
        &template.subject,
        &template.html,
        &template.resources,
    )?;
    let mut image_mapping = HashMap::new();
    let mut view = TemplateContentView {
        id: template.id.clone(),
        name: template.name.clone(),
        subject: template.subject.clone(),
        html: String::new(),
        inline_images: Vec::new(),
        attachments: Vec::new(),
    };
    let mut stored = HashMap::new();
    for resource in &template.resources {
        let id = if remap {
            uuid::Uuid::new_v4().to_string()
        } else {
            resource.id.clone()
        };
        let inline =
            if let (Some(old_cid), Some(mime_type)) = (&resource.content_id, &resource.mime_type) {
                let cid = if remap {
                    format!("{id}@inline.local")
                } else {
                    old_cid.clone()
                };
                image_mapping.insert(old_cid.clone(), cid.clone());
                view.inline_images.push(InlineImageView {
                    id: id.clone(),
                    content_id: cid.clone(),
                    name: resource.name.clone(),
                    size: resource.bytes.len(),
                    mime_type: mime_type.clone(),
                    data_base64: STANDARD.encode(&resource.bytes),
                });
                Some(mail::InlineImageMetadata {
                    content_id: cid,
                    mime_type: mime_type.clone(),
                })
            } else {
                view.attachments.push(AttachmentView {
                    id: id.clone(),
                    name: resource.name.clone(),
                    size: resource.bytes.len(),
                });
                None
            };
        stored.insert(
            id,
            StoredAttachment {
                name: resource.name.clone(),
                bytes: resource.bytes.clone(),
                inline,
            },
        );
    }
    // 在 HTML 解析器的属性回调中改 CID，不能用字符串替换误改正文中的示例代码或文字。
    view.html = mail::sanitize_body_with_image_mapping(&template.html, &image_mapping);
    Ok((view, stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MailTemplate {
        // 合法 1x1 PNG；真实图像头及像素无需依赖外部临时文件。
        let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
        MailTemplate {
            id: "template".into(), name: "通知".into(), subject: "测试".into(),
            html: "<p>cid:old@inline.local</p><p>src=\"cid:old@inline.local\"</p><img src=\"cid:old@inline.local\"><img src=\"https://invalid.example/a.png\"><script>bad()</script>".into(),
            resources: vec![TemplateResource { id: "image".into(), name: "图.png".into(), mime_type: Some("image/png".into()), content_id: Some("old@inline.local".into()), bytes },
                TemplateResource { id: "file".into(), name: "附件.txt".into(), mime_type: None, content_id: None, bytes: b"independent".to_vec() }],
            created_at: String::new(), updated_at: String::new(),
        }
    }

    #[test]
    fn applying_template_remaps_resources_without_mutating_template_or_draft_context() {
        let state = AppState::new();
        let template = fixture();
        let first = replace_content(&state, &template).unwrap();
        let second = replace_content(&state, &template).unwrap();
        assert_ne!(first.inline_images[0].id, second.inline_images[0].id);
        assert_ne!(
            first.inline_images[0].content_id,
            second.inline_images[0].content_id
        );
        assert!(second.html.contains(&format!(
            "src=\"cid:{}\"",
            second.inline_images[0].content_id
        )));
        assert!(second.html.contains("<p>cid:old@inline.local</p>"));
        assert!(second.html.contains("<p>src=\"cid:old@inline.local\"</p>"));
        assert!(!second.html.contains("https://"));
        assert!(!second.html.contains("script"));
        assert!(template.html.contains("src=\"cid:old@inline.local\""));
        let stored = state.attachments.lock().unwrap();
        assert_eq!(stored.len(), 2);
        assert!(!stored.contains_key(&first.inline_images[0].id));
        assert!(state.view().account.is_none());
        assert_eq!(state.mail.lock().unwrap().status, "idle");
    }

    #[test]
    fn invalid_template_keeps_old_draft_resources_and_preview_does_not_register() {
        let state = AppState::new();
        let mut template = fixture();
        let current = replace_content(&state, &template).unwrap();
        let (preview, _) = prepare_content(&template, false).unwrap();
        assert_eq!(preview.inline_images[0].id, "image");
        assert!(state
            .attachments
            .lock()
            .unwrap()
            .contains_key(&current.inline_images[0].id));
        template.resources[0].bytes = b"not an image".to_vec();
        assert!(replace_content(&state, &template).is_err());
        assert_eq!(state.attachments.lock().unwrap().len(), 2);
        assert!(state
            .attachments
            .lock()
            .unwrap()
            .contains_key(&current.attachments[0].id));
    }

    #[test]
    fn saving_without_attachments_copies_images_and_rejects_stale_or_wrong_handles() {
        let state = AppState::new();
        let current = replace_content(&state, &fixture()).unwrap();
        let mut input = SaveTemplateInput {
            name: "副本".into(),
            subject: current.subject,
            html: current.html,
            inline_image_ids: vec![current.inline_images[0].id.clone()],
            attachment_ids: vec![current.attachments[0].id.clone()],
            include_attachments: false,
            update_id: None,
        };
        let stored = state.attachments.lock().unwrap();
        let resources = collect_resources(&stored, &input).unwrap();
        assert_eq!(resources.len(), 1);
        input.include_attachments = true;
        assert_eq!(collect_resources(&stored, &input).unwrap().len(), 2);
        input.inline_image_ids = vec![current.attachments[0].id.clone()];
        assert_eq!(
            collect_resources(&stored, &input).err().unwrap().code,
            "template_resource_missing"
        );
    }
}
