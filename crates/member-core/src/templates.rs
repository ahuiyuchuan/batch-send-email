//! 用户明确保存的邮件内容模板及 SQLite 事务操作。作者：ahui。
//! 模板只包含主题、正文、正文图片与附件的独立副本；不保存收件人、QQ 群或 SMTP 凭据。
//! 完整资源对象不直接序列化；宿主只返回已验证图片的预览字节与普通附件元信息。
//! 应用模板时重新建立草稿资源，普通附件的完整 BLOB 始终留在后端。

use crate::{
    database::{db_error, Database},
    mail::{validate_inline_image, MAX_ATTACHMENT_BYTES, MAX_BODY_BYTES, MAX_INLINE_IMAGES},
    models::{AppError, AppResult},
};
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::collections::HashSet;

// 模板包沿用本模块的内容校验与事务工具；独立文件只分隔文件协议，不建立第二套保存规则。
#[path = "template_transfer.rs"]
mod transfer;
pub use transfer::{
    decode_template_package, TemplateImportEntry, TemplateImportPolicy, TemplateImportPreview,
    TemplateImportResult, TemplatePackage, MAX_TEMPLATE_PACKAGE_BYTES,
};

/// 本机模板数量上限；修改已有模板不消耗名额，复制与新建均占用一份。
pub const MAX_MAIL_TEMPLATES: usize = 200;

/// 一份模板拥有的资源副本；有 content_id/mime_type 的项是正文图片，两者必须同时存在。
/// 不实现 Serialize/Debug，防止图片、附件内容或内部标识被意外记录或直接传给页面。
#[derive(Clone)]
pub struct TemplateResource {
    /// 仅在所属模板内唯一；应用到草稿时由宿主重新生成草稿标识。
    pub id: String,
    /// 文件显示名，不是磁盘路径，后续发送仍由邮件模块清理 MIME 名称。
    pub name: String,
    /// 正文图片的真实标准 MIME；普通附件为 None。
    pub mime_type: Option<String>,
    /// 正文 HTML 使用的 cid: 标识；普通附件为 None，且不得与资源标识混用。
    pub content_id: Option<String>,
    /// 已复制的完整文件内容，模板生命周期不依赖原文件是否仍存在。
    pub bytes: Vec<u8>,
}

/// 完整模板仅供宿主加载和复制；不包含发送范围，也不直接通过 IPC 返回资源字节。
#[derive(Clone)]
pub struct MailTemplate {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub html: String,
    pub resources: Vec<TemplateResource>,
    /// 带偏移的 RFC 3339 本机时间；更新时保持首次创建时间。
    pub created_at: String,
    pub updated_at: String,
}

/// 列表与管理操作的轻量摘要；统计只读取资源元信息，不加载附件 BLOB。
#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TemplateSummary {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub attachment_count: usize,
    pub inline_image_count: usize,
    pub updated_at: String,
}

impl Database {
    /// 按名称和主题的普通文本搜索模板，空查询返回全部；最近更新时间优先，标识稳定打破同刻并列。
    /// 查询最多 200 字符，使用绑定参数与 instr，不把 %、_ 或引号解释为通配符/SQL。
    pub fn list_mail_templates(&self, query: &str) -> AppResult<Vec<TemplateSummary>> {
        let query = query.trim();
        if query.chars().count() > 200 || query.chars().any(char::is_control) {
            return Err(AppError::new(
                "template_query",
                "搜索内容请控制在 200 字以内。",
            ));
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT t.id,t.name,t.subject,t.updated_at,
                    (SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NULL),
                    (SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NOT NULL)
               FROM mail_templates t
              WHERE ?1='' OR instr(lower(t.name),lower(?1))>0 OR instr(lower(t.subject),lower(?1))>0
              ORDER BY julianday(t.updated_at) DESC,t.id DESC"
        ).map_err(db_error)?;
        let summaries = statement
            .query_map([query], summary_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(summaries)
    }

    /// 读取固定模板及独立资源副本；不存在返回 None，不回退到其他模板或读取原磁盘文件。
    /// 损坏、超限或关系矛盾的存储数据返回错误，不向宿主提供残缺内容。
    pub fn mail_template(&self, id: &str) -> AppResult<Option<MailTemplate>> {
        validate_id(id)?;
        let mut connection = self.connection()?;
        // 元信息、资源总量和 BLOB 必须来自同一读取快照，不能拼接并发更新前后的两个版本。
        let transaction = connection.transaction().map_err(db_error)?;
        let template = read_template(&transaction, id)?;
        transaction.commit().map_err(db_error)?;
        Ok(template)
    }

    /// 新建或原子更新模板；name 会去首尾空白，空主题/正文可保存为内容片段，发送时仍须满足邮件规则。
    /// update_id 为 Some 时必须命中已有模板；新建/复制最多 200 份。任何验证或 SQL 失败保留旧模板和资源。
    /// 主题最多 200 字符、正文最多 2 MiB；图片和普通附件分别最多 10 项、原始字节共用 18 MiB。
    pub fn save_mail_template(
        &self,
        name: &str,
        subject: &str,
        html: &str,
        resources: &[TemplateResource],
        update_id: Option<&str>,
    ) -> AppResult<MailTemplate> {
        let name = validate_template_content(name, subject, html, resources)?;
        if let Some(id) = update_id {
            validate_id(id)?;
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = Local::now().to_rfc3339();
        let (id, created_at) = if let Some(id) = update_id {
            let created_at: Option<String> = transaction
                .query_row(
                    "SELECT created_at FROM mail_templates WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            let created_at = created_at.ok_or_else(template_missing)?;
            transaction.execute(
                "UPDATE mail_templates SET name=?1,subject=?2,html=?3,updated_at=?4 WHERE id=?5",
                params![name, subject, html, now, id],
            ).map_err(db_error)?;
            transaction
                .execute(
                    "DELETE FROM mail_template_resources WHERE template_id=?1",
                    [id],
                )
                .map_err(db_error)?;
            (id.to_owned(), created_at)
        } else {
            check_capacity(&transaction)?;
            let id = new_id(&transaction)?;
            transaction.execute(
                "INSERT INTO mail_templates(id,name,subject,html,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)",
                params![id, name, subject, html, now],
            ).map_err(db_error)?;
            (id, now.clone())
        };
        write_resources(&transaction, &id, resources)?;
        transaction.commit().map_err(db_error)?;
        Ok(MailTemplate {
            id,
            name,
            subject: subject.into(),
            html: html.into(),
            resources: resources.to_vec(),
            created_at,
            updated_at: now,
        })
    }

    /// 只更改显示名和更新时间，不重新上传附件或改变主题/正文；不存在时返回 template_missing。
    pub fn rename_mail_template(&self, id: &str, name: &str) -> AppResult<TemplateSummary> {
        validate_id(id)?;
        let name = validate_name(name)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if transaction
            .execute(
                "UPDATE mail_templates SET name=?1,updated_at=?2 WHERE id=?3",
                params![name, Local::now().to_rfc3339(), id],
            )
            .map_err(db_error)?
            == 0
        {
            return Err(template_missing());
        }
        let summary = read_summary(&transaction, id)?;
        transaction.commit().map_err(db_error)?;
        Ok(summary)
    }

    /// 将模板内容和 BLOB 复制到新的模板标识下；原模板更新/删除不会影响副本。
    /// 读取、数量核验和复制在同一立即事务内完成，失败不会留下一半元信息或资源。
    pub fn duplicate_mail_template(&self, id: &str, name: &str) -> AppResult<TemplateSummary> {
        validate_id(id)?;
        let name = validate_name(name)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let original = read_template(&transaction, id)?.ok_or_else(template_missing)?;
        check_capacity(&transaction)?;
        let id = new_id(&transaction)?;
        let now = Local::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO mail_templates(id,name,subject,html,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)",
            params![id, name, original.subject, original.html, now],
        ).map_err(db_error)?;
        write_resources(&transaction, &id, &original.resources)?;
        let summary = read_summary(&transaction, &id)?;
        transaction.commit().map_err(db_error)?;
        Ok(summary)
    }

    /// 删除模板并在同一事务内级联删除其资源副本；不会触碰已应用到草稿的内存内容或其他模板。
    /// 不存在时返回 template_missing，避免页面把过期选择误当成成功操作。
    pub fn delete_mail_template(&self, id: &str) -> AppResult<()> {
        validate_id(id)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if transaction
            .execute("DELETE FROM mail_templates WHERE id=?1", [id])
            .map_err(db_error)?
            == 0
        {
            return Err(template_missing());
        }
        transaction.commit().map_err(db_error)
    }
}

/// 保存、读取与宿主应用共用相同边界，不信任仅通过页面校验或外部修改过的数据库。
///
/// 输入为内容与独立资源副本；成功返回去掉首尾空白的名称，空正文/主题可保存为半成品。
/// 资源类别、真实图片格式、唯一标识或数量/大小不合法时返回可读错误，不修改任何数据。
pub fn validate_template_content(
    name: &str,
    subject: &str,
    html: &str,
    resources: &[TemplateResource],
) -> AppResult<String> {
    let name = validate_name(name)?;
    if subject.chars().count() > 200 || subject.chars().any(char::is_control) {
        return Err(AppError::new(
            "template_subject",
            "模板主题最多 200 字，且不能包含换行或控制字符。",
        ));
    }
    if html.len() > MAX_BODY_BYTES {
        return Err(AppError::new(
            "body_too_large",
            "模板正文超过 2 MiB，请精简后保存。",
        ));
    }
    let mut ids = HashSet::new();
    let mut content_ids = HashSet::new();
    let mut inline_count = 0;
    let mut attachment_count = 0;
    let mut total = 0usize;
    if resources.len() > 20 {
        return Err(resource_error());
    }
    for resource in resources {
        validate_id(&resource.id)?;
        if !ids.insert(&resource.id)
            || resource.name.trim().is_empty()
            || resource.name.chars().count() > 255
            || resource.name.chars().any(char::is_control)
        {
            return Err(resource_error());
        }
        total = total
            .checked_add(resource.bytes.len())
            .ok_or_else(resource_error)?;
        match (&resource.content_id, &resource.mime_type) {
            (None, None) => attachment_count += 1,
            (Some(content_id), Some(mime_type)) => {
                validate_id(content_id)?;
                if !content_ids.insert(content_id) {
                    return Err(resource_error());
                }
                validate_inline_image(&resource.bytes, mime_type)?;
                inline_count += 1;
            }
            _ => return Err(resource_error()),
        }
    }
    if inline_count > MAX_INLINE_IMAGES || attachment_count > 10 || total > MAX_ATTACHMENT_BYTES {
        return Err(resource_error());
    }
    Ok(name)
}

/// 名称只做首尾去空白，不改写用户正文；允许同名模板，界面和操作始终使用内部 id 区分。
fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(AppError::new(
            "template_name",
            "模板名称请填写 1 到 80 字，且不能包含换行。",
        ));
    }
    Ok(name.into())
}

/// 标识不是磁盘路径，仅接受与现有 CID 兼容的 ASCII 安全字符，最长 128 字节。
fn validate_id(id: &str) -> AppResult<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@._-".contains(&byte))
    {
        return Err(AppError::new(
            "template_id",
            "模板或资源标识无效，请重新选择。",
        ));
    }
    Ok(())
}

fn resource_error() -> AppError {
    AppError::new(
        "template_resources",
        "模板资源无效或超限：最多 10 个附件、10 张正文图片，合计不超过 18 MiB。",
    )
}

fn template_missing() -> AppError {
    AppError::new("template_missing", "该模板已不存在，请刷新模板列表后重试。")
}

/// 立即事务持有写锁时检查配额，避免两个并发新建同时看到最后一个空位。
fn check_capacity(connection: &Connection) -> AppResult<()> {
    let count: usize = connection
        .query_row("SELECT count(*) FROM mail_templates", [], |row| row.get(0))
        .map_err(db_error)?;
    if count >= MAX_MAIL_TEMPLATES {
        return Err(AppError::new(
            "template_limit",
            "本机最多保存 200 份模板，请先删除不需要的模板。",
        ));
    }
    Ok(())
}

/// 使用 SQLite 自带随机源生成 128 位不含路径的标识，不引入额外依赖或使用时间戳代替随机标识。
fn new_id(connection: &Connection) -> AppResult<String> {
    connection
        .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
        .map_err(db_error)
}

/// 同一个模板内的资源行共同提交；模板复制保留内部引用标识，但行所有者与 BLOB 副本独立。
fn write_resources(
    connection: &Connection,
    template_id: &str,
    resources: &[TemplateResource],
) -> AppResult<()> {
    let mut statement = connection.prepare("INSERT INTO mail_template_resources(template_id,resource_id,position,name,mime_type,content_id,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)").map_err(db_error)?;
    for (position, resource) in resources.iter().enumerate() {
        statement
            .execute(params![
                template_id,
                resource.id,
                position,
                resource.name,
                resource.mime_type,
                resource.content_id,
                resource.bytes
            ])
            .map_err(db_error)?;
    }
    Ok(())
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TemplateSummary> {
    Ok(TemplateSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        subject: row.get(2)?,
        updated_at: row.get(3)?,
        attachment_count: row.get(4)?,
        inline_image_count: row.get(5)?,
    })
}

/// 调用方事务内读取刚修改的摘要，保证返回信息与本次提交一致而不是并发操作后的其他版本。
fn read_summary(connection: &Connection, id: &str) -> AppResult<TemplateSummary> {
    connection.query_row("SELECT t.id,t.name,t.subject,t.updated_at,(SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NULL),(SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NOT NULL) FROM mail_templates t WHERE t.id=?1", [id], summary_from_row).map_err(db_error)
}

/// 先核验资源总量再分配 BLOB 内存；独立快照读事务由公开读取入口负责，写入调用已持有事务。
fn read_template(connection: &Connection, id: &str) -> AppResult<Option<MailTemplate>> {
    let Some(mut template) = connection
        .query_row(
            "SELECT id,name,subject,html,created_at,updated_at FROM mail_templates WHERE id=?1",
            [id],
            |row| {
                Ok(MailTemplate {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    subject: row.get(2)?,
                    html: row.get(3)?,
                    resources: vec![],
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(db_error)?
    else {
        return Ok(None);
    };
    let (count, bytes): (usize, usize) = connection.query_row("SELECT count(*),coalesce(sum(length(bytes)),0) FROM mail_template_resources WHERE template_id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db_error)?;
    if count > 20 || bytes > MAX_ATTACHMENT_BYTES {
        return Err(resource_error());
    }
    let mut statement = connection.prepare("SELECT resource_id,name,mime_type,content_id,bytes FROM mail_template_resources WHERE template_id=?1 ORDER BY position,resource_id").map_err(db_error)?;
    template.resources = statement
        .query_map([id], |row| {
            Ok(TemplateResource {
                id: row.get(0)?,
                name: row.get(1)?,
                mime_type: row.get(2)?,
                content_id: row.get(3)?,
                bytes: row.get(4)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    validate_template_content(
        &template.name,
        &template.subject,
        &template.html,
        &template.resources,
    )?;
    Ok(Some(template))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 全部使用合成文件，不依赖真实邮箱、磁盘原附件或任何网络资源。
    fn resources() -> Vec<TemplateResource> {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        vec![
            TemplateResource {
                id: "z-attachment".into(),
                name: "saved.txt".into(),
                mime_type: None,
                content_id: None,
                bytes: b"independent file content".to_vec(),
            },
            TemplateResource {
                id: "a-image".into(),
                name: "image.png".into(),
                mime_type: Some("image/png".into()),
                content_id: Some("test-image@template.local".into()),
                bytes: png.into_inner(),
            },
        ]
    }

    fn save(database: &Database, name: &str) -> MailTemplate {
        database
            .save_mail_template(
                name,
                "模拟主题",
                "<p>正文</p><img src=\"cid:test-image@template.local\">",
                &resources(),
                None,
            )
            .unwrap()
    }

    /// 原图片/附件删除后重启仍读取独立副本，并覆盖搜索、重命名和更新；资源顺序与保存时一致。
    #[test]
    fn template_crud_search_and_resources_survive_restart() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("data.sqlite3");
        let database = Database::open(&path).unwrap();
        // 模拟用户先从真实文件选入资源；保存后删除原文件，避免只验证内存数据往返而漏掉路径依赖。
        let mut selected = resources();
        let originals = [
            folder.path().join("original.txt"),
            folder.path().join("original.png"),
        ];
        for (resource, original) in selected.iter_mut().zip(&originals) {
            std::fs::write(original, &resource.bytes).unwrap();
            resource.bytes = std::fs::read(original).unwrap();
        }
        let saved = database
            .save_mail_template(
                "  欢迎模板  ",
                "模拟主题",
                "<p>正文</p><img src=\"cid:test-image@template.local\">",
                &selected,
                None,
            )
            .unwrap();
        assert_eq!(saved.name, "欢迎模板");
        assert_eq!(saved.id.len(), 32);
        assert!(saved.id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        for original in &originals {
            std::fs::remove_file(original).unwrap();
            assert!(!original.exists());
        }
        drop(database);
        let database = Database::open(&path).unwrap();
        let restored = database.mail_template(&saved.id).unwrap().unwrap();
        assert_eq!(restored.subject, saved.subject);
        assert_eq!(restored.html, saved.html);
        assert_eq!(restored.created_at, saved.created_at);
        assert_eq!(restored.resources[0].id, "z-attachment");
        assert_eq!(
            restored.resources[1].content_id,
            saved.resources[1].content_id
        );
        assert_eq!(restored.resources[0].bytes, saved.resources[0].bytes);
        assert_eq!(restored.resources[1].bytes, saved.resources[1].bytes);
        let summaries = database.list_mail_templates("欢迎").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].attachment_count, 1);
        assert_eq!(summaries[0].inline_image_count, 1);
        let renamed = database
            .rename_mail_template(&saved.id, "English 100%_'")
            .unwrap();
        assert_eq!(renamed.id, saved.id);
        assert_eq!(database.list_mail_templates("english").unwrap().len(), 1);
        assert_eq!(database.list_mail_templates("%_").unwrap().len(), 1);
        assert!(database
            .list_mail_templates("' OR 1=1 --")
            .unwrap()
            .is_empty());
        let updated = database
            .save_mail_template(
                "更新模板",
                "新的主题",
                "<p>新的正文</p>",
                &[],
                Some(&saved.id),
            )
            .unwrap();
        assert_eq!(updated.created_at, saved.created_at);
        assert!(updated.resources.is_empty());
        assert_eq!(database.list_mail_templates("新的主题").unwrap().len(), 1);
        assert!(database.mail_template("missing").unwrap().is_none());
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &[], Some("missing"))
                .err()
                .unwrap()
                .code,
            "template_missing"
        );
        assert_eq!(
            database
                .rename_mail_template("missing", "名称")
                .unwrap_err()
                .code,
            "template_missing"
        );
        database.delete_mail_template(&saved.id).unwrap();
        assert!(database.list_mail_templates("").unwrap().is_empty());
        assert_eq!(
            database.delete_mail_template(&saved.id).unwrap_err().code,
            "template_missing"
        );
    }

    /// 复制拥有独立资源行；原模板更新/删除不改副本，已读取到草稿的内存内容也不受模板删除影响。
    #[test]
    fn duplicate_and_loaded_content_are_independent_from_original() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = save(&database, "原模板");
        let duplicate = database
            .duplicate_mail_template(&original.id, "副本")
            .unwrap();
        assert_ne!(duplicate.id, original.id);
        let applied_copy = database.mail_template(&duplicate.id).unwrap().unwrap();
        database
            .save_mail_template("原模板改名", "修改", "", &[], Some(&original.id))
            .unwrap();
        database.delete_mail_template(&original.id).unwrap();
        let remaining = database.mail_template(&duplicate.id).unwrap().unwrap();
        assert_eq!(remaining.resources[0].bytes, original.resources[0].bytes);
        assert_eq!(remaining.resources[1].bytes, original.resources[1].bytes);
        assert_eq!(remaining.html, original.html);
        database.delete_mail_template(&duplicate.id).unwrap();
        let resource_count: usize = database
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mail_template_resources", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(resource_count, 0);
        assert_eq!(applied_copy.resources[0].bytes, original.resources[0].bytes);
    }

    /// 文本按字符与 UTF-8 字节分别限制；资源标识、CID、MIME 及图片内容都再次核验。
    #[test]
    fn template_validation_rejects_invalid_text_and_resource_relationships() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        for name in [" ".to_string(), "名".repeat(81), "行\n名".into()] {
            assert_eq!(
                database
                    .save_mail_template(&name, "", "", &[], None)
                    .err()
                    .unwrap()
                    .code,
                "template_name"
            );
        }
        for subject in ["题".repeat(201), "题\r\nInjected: value".into()] {
            assert_eq!(
                database
                    .save_mail_template("名称", &subject, "", &[], None)
                    .err()
                    .unwrap()
                    .code,
                "template_subject"
            );
        }
        assert_eq!(
            database
                .save_mail_template("名称", "", &"x".repeat(MAX_BODY_BYTES + 1), &[], None)
                .err()
                .unwrap()
                .code,
            "body_too_large"
        );
        assert_eq!(
            database.mail_template("../bad").err().unwrap().code,
            "template_id"
        );
        let mut duplicate_id = resources();
        duplicate_id[1].id = duplicate_id[0].id.clone();
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &duplicate_id, None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        let mut image = resources().remove(1);
        image.mime_type = None;
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &[image], None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        let mut image = resources().remove(1);
        image.mime_type = Some("image/jpeg".into());
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &[image], None)
                .err()
                .unwrap()
                .code,
            "inline_image_format"
        );
        let image = resources().remove(1);
        let mut duplicate_cid = image.clone();
        duplicate_cid.id = "another-image".into();
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &[image, duplicate_cid], None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        assert!(database.list_mail_templates("").unwrap().is_empty());
        let partial = database
            .save_mail_template("名".repeat(80).as_str(), &"题".repeat(200), "", &[], None)
            .unwrap();
        assert_eq!(partial.name.chars().count(), 80);
    }

    /// 数量和原始字节上限与发送共用，最大普通附件可保存，超限拒绝且不写入半个模板。
    #[test]
    fn template_resource_limits_match_mail_limits() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let attachment = resources().remove(0);
        let eleven_files = (0..11)
            .map(|index| {
                let mut resource = attachment.clone();
                resource.id = format!("file-{index}");
                resource
            })
            .collect::<Vec<_>>();
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &eleven_files, None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        let image = resources().remove(1);
        let eleven_images = (0..11)
            .map(|index| {
                let mut resource = image.clone();
                resource.id = format!("image-{index}");
                resource.content_id = Some(format!("image-{index}@template.local"));
                resource
            })
            .collect::<Vec<_>>();
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &eleven_images, None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        let mut large = attachment;
        large.bytes = vec![0; MAX_ATTACHMENT_BYTES + 1];
        assert_eq!(
            database
                .save_mail_template("名称", "", "", &[large.clone()], None)
                .err()
                .unwrap()
                .code,
            "template_resources"
        );
        large.bytes.pop();
        let saved = database
            .save_mail_template("最大附件", "", "", &[large], None)
            .unwrap();
        assert_eq!(
            database
                .mail_template(&saved.id)
                .unwrap()
                .unwrap()
                .resources[0]
                .bytes
                .len(),
            MAX_ATTACHMENT_BYTES
        );
    }

    /// 写锁内核验模板名额；满额仍可更新，复制/新建失败后删除一份即可继续保存。
    #[test]
    fn template_capacity_applies_to_new_and_duplicate_but_not_update() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let first = database
            .save_mail_template("模板", "", "", &[], None)
            .unwrap();
        for _ in 1..MAX_MAIL_TEMPLATES {
            database
                .save_mail_template("模板", "", "", &[], None)
                .unwrap();
        }
        assert_eq!(
            database
                .save_mail_template("额外", "", "", &[], None)
                .err()
                .unwrap()
                .code,
            "template_limit"
        );
        assert_eq!(
            database
                .duplicate_mail_template(&first.id, "副本")
                .unwrap_err()
                .code,
            "template_limit"
        );
        assert!(database
            .save_mail_template("仍可修改", "", "", &[], Some(&first.id))
            .is_ok());
        assert_eq!(
            database.list_mail_templates("").unwrap().len(),
            MAX_MAIL_TEMPLATES
        );
        database.delete_mail_template(&first.id).unwrap();
        assert!(database
            .save_mail_template("释放名额", "", "", &[], None)
            .is_ok());
    }

    /// 用真实 SQLite 触发器模拟资源写入/级联删除失败，验证元信息和 BLOB 共同回滚而不是部分成功。
    #[test]
    fn template_transactions_rollback_new_update_duplicate_and_delete_failures() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let original = save(&database, "原模板");
        database.connection().unwrap().execute_batch("CREATE TRIGGER fail_template_insert BEFORE INSERT ON mail_template_resources WHEN NEW.name='fail.bin' OR (SELECT name FROM mail_templates WHERE id=NEW.template_id)='copy fail' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        let mut failing = resources();
        failing[1].name = "fail.bin".into();
        assert_eq!(
            database
                .save_mail_template("更新失败", "新主题", "新正文", &failing, Some(&original.id))
                .err()
                .unwrap()
                .code,
            "database_error"
        );
        assert_eq!(
            database
                .save_mail_template("新建失败", "", "", &failing, None)
                .err()
                .unwrap()
                .code,
            "database_error"
        );
        assert_eq!(
            database
                .duplicate_mail_template(&original.id, "copy fail")
                .unwrap_err()
                .code,
            "database_error"
        );
        let restored = database.mail_template(&original.id).unwrap().unwrap();
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.subject, original.subject);
        assert_eq!(restored.html, original.html);
        assert_eq!(restored.updated_at, original.updated_at);
        assert_eq!(restored.resources[1].bytes, original.resources[1].bytes);
        assert_eq!(database.list_mail_templates("").unwrap().len(), 1);
        database.connection().unwrap().execute_batch("CREATE TRIGGER fail_template_delete BEFORE DELETE ON mail_template_resources BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert_eq!(
            database
                .delete_mail_template(&original.id)
                .unwrap_err()
                .code,
            "database_error"
        );
        assert!(database.mail_template(&original.id).unwrap().is_some());
        let resource_count: usize = database
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mail_template_resources", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(resource_count, 2);
    }

    /// 用真实旧版表结构建立版本 1，迁移保留历史与设置，并使每个新连接的外键持续生效。
    #[test]
    fn schema_one_migrates_without_losing_existing_data() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("old.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE sent_history(id INTEGER PRIMARY KEY,recipient_key TEXT NOT NULL UNIQUE,qq_number TEXT NOT NULL DEFAULT '',email TEXT NOT NULL,sent_at TEXT NOT NULL,origin TEXT NOT NULL,note TEXT NOT NULL DEFAULT '');
            CREATE TABLE snapshots(id TEXT PRIMARY KEY,finished_at TEXT NOT NULL,payload TEXT NOT NULL);
            CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE mail_delivery(id INTEGER PRIMARY KEY,job_id TEXT NOT NULL,recipient TEXT NOT NULL,state TEXT NOT NULL,updated_at TEXT NOT NULL,message TEXT NOT NULL DEFAULT '');
            INSERT INTO settings(key,value) VALUES('preserved','value');
            INSERT INTO sent_history(recipient_key,qq_number,email,sent_at,origin) VALUES('synthetic@example.test','','synthetic@example.test','2026-11-05T00:00:00Z','imported');
            PRAGMA user_version=1;").unwrap();
        drop(old);
        let database = Database::open(&path).unwrap();
        assert_eq!(database.sent_count().unwrap(), 1);
        let connection = database.connection().unwrap();
        let preserved: String = connection
            .query_row(
                "SELECT value FROM settings WHERE key='preserved'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, "value");
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(connection.execute("INSERT INTO mail_template_resources(template_id,resource_id,position,name,bytes) VALUES('missing','file',0,'name',x'00')", []).is_err());
        assert!(save(&database, "迁移后模板").resources.len() == 2);
    }

    /// 未知新版本拒绝降级；旧版本迁移遇到真实结构错误时，新增表和版本号全部回滚。
    #[test]
    fn schema_rejects_future_version_and_rolls_back_failed_migration() {
        let folder = tempfile::tempdir().unwrap();
        let future = folder.path().join("future.sqlite3");
        let connection = Connection::open(&future).unwrap();
        connection.execute_batch("CREATE TABLE preserved(value TEXT); INSERT INTO preserved VALUES('keep'); PRAGMA user_version=6;").unwrap();
        drop(connection);
        assert_eq!(
            Database::open(&future).err().unwrap().code,
            "database_version"
        );
        let connection = Connection::open(&future).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
        let invalid = folder.path().join("invalid.sqlite3");
        let connection = Connection::open(&invalid).unwrap();
        connection.execute_batch("CREATE TABLE preserved(value TEXT); INSERT INTO preserved VALUES('keep'); CREATE VIEW mail_templates AS SELECT value FROM preserved; PRAGMA user_version=1;").unwrap();
        drop(connection);
        assert_eq!(
            Database::open(&invalid).err().unwrap().code,
            "database_error"
        );
        let connection = Connection::open(&invalid).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE name='sent_history' OR name='mail_template_resources'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(
            connection
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }
}
