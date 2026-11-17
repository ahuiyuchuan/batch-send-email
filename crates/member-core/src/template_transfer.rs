//! 邮件模板包的版本化文件协议与整批导入事务。作者：ahui。
//! 只包含模板内容及内嵌资源，不带本机模板标识、收件人、SMTP 设置或登录凭据；不读取原文件或访问网络。
//! 宿主负责文件对话框、限长读取、原子导出及五分钟确认生命周期，本模块负责内容校验与数据库一致性。

use super::{
    check_capacity, new_id, read_summary, read_template, summary_from_row,
    validate_template_content, write_resources, TemplateResource, TemplateSummary,
    MAX_MAIL_TEMPLATES,
};
use crate::{database::db_error, database::Database, mail::MAX_ATTACHMENT_BYTES, models::*};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::Local;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{de, Deserialize, Deserializer, Serialize};
use std::{
    collections::HashSet,
    fmt,
    io::{self, Write},
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// UTF-8 JSON 模板包的文件上限；宿主须在读取前与读取后各检查一次，防止并发增长。
pub const MAX_TEMPLATE_PACKAGE_BYTES: usize = 128 * 1024 * 1024;
/// 正文与解码后资源的共享预算，限制 base64 和 JSON 转义膨胀之前的真实内容总量。
const MAX_PACKAGE_CONTENT_BYTES: usize = 80 * 1024 * 1024;
const PACKAGE_KIND: &str = "qq-mail-templates";

/// 已验证完整内容仅留在宿主，不实现 Debug/Serialize；identity 将确认预览绑定到同一份解码结果。
pub struct TemplatePackage {
    templates: Vec<DecodedTemplate>,
    identity: Arc<()>,
}

struct DecodedTemplate {
    name: String,
    subject: String,
    html: String,
    resources: Vec<TemplateResource>,
}

/// 同名以去首尾空白后的精确名称比较，区分大小写；默认复制，覆盖只允许无歧义的一对一匹配。
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemplateImportPolicy {
    #[default]
    Copy,
    Skip,
    Overwrite,
}

/// 确认页的轻量条目；conflict 表示预览时本机至少存在一份同名模板，不返回正文或附件内容。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportEntry {
    pub name: String,
    pub subject: String,
    pub image_count: usize,
    pub attachment_count: usize,
    pub conflict: bool,
}

/// 只序列化用户需核对的摘要；完整目录基线和包身份留在后端，不能由页面伪造覆盖目标。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportPreview {
    pub entries: Vec<TemplateImportEntry>,
    pub conflict_count: usize,
    pub overwrite_allowed: bool,
    #[serde(skip)]
    catalog: Vec<TemplateSummary>,
    #[serde(skip)]
    package_identity: Arc<()>,
}

/// 整批提交后的真实数量与本次写入摘要；跳过项不算写入，不修改既有模板和资源。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportResult {
    pub added: usize,
    pub replaced: usize,
    pub skipped: usize,
    pub summaries: Vec<TemplateSummary>,
}

/// 所有层级拒绝未知字段，避免把收件范围或凭据误识别为可携带的模板设置。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedPackage {
    kind: String,
    version: u32,
    #[serde(deserialize_with = "deserialize_templates")]
    templates: Vec<EncodedTemplate>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedTemplate {
    name: String,
    subject: String,
    html: String,
    #[serde(deserialize_with = "deserialize_resources")]
    resources: Vec<EncodedResource>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedResource {
    id: String,
    name: String,
    mime_type: Option<String>,
    content_id: Option<String>,
    data_base64: String,
}

/// 解码受限 UTF-8 JSON；空包、未知协议、非法 base64、内容超限、图片/CID 关系异常均拒绝整包。
/// 原样保留合法正文；这里只解析 HTML 的资源引用，不执行脚本、读取路径、解压文件或访问外链。
pub fn decode_template_package(bytes: &[u8]) -> AppResult<TemplatePackage> {
    if bytes.len() > MAX_TEMPLATE_PACKAGE_BYTES {
        return Err(package_limit());
    }
    let encoded: EncodedPackage = serde_json::from_slice(bytes).map_err(|_| package_invalid())?;
    if encoded.kind != PACKAGE_KIND || encoded.version != 1 || encoded.templates.is_empty() {
        return Err(package_invalid());
    }
    let mut used = 0usize;
    let mut templates = Vec::with_capacity(encoded.templates.len());
    for template in encoded.templates {
        add_content_budget(&mut used, template.html.len())?;
        let mut resources = Vec::with_capacity(template.resources.len());
        for resource in template.resources {
            // 先限制编码长度和解码后预算，再分配解码输出；不依赖实际分配成功后才拒绝超大资源。
            if resource.data_base64.len() > MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 {
                return Err(package_limit());
            }
            let estimated = base64::decoded_len_estimate(resource.data_base64.len());
            if estimated > MAX_PACKAGE_CONTENT_BYTES.saturating_sub(used) + 2 {
                return Err(package_limit());
            }
            let bytes = STANDARD
                .decode(&resource.data_base64)
                .map_err(|_| package_invalid())?;
            add_content_budget(&mut used, bytes.len())?;
            resources.push(TemplateResource {
                id: resource.id,
                name: resource.name,
                mime_type: resource.mime_type,
                content_id: resource.content_id,
                bytes,
            });
        }
        let name = validate_template_content(
            &template.name,
            &template.subject,
            &template.html,
            &resources,
        )?;
        validate_cid_references(&template.html, &resources)?;
        templates.push(DecodedTemplate {
            name,
            subject: template.subject,
            html: template.html,
            resources,
        });
    }
    Ok(TemplatePackage {
        templates,
        identity: Arc::new(()),
    })
}

impl Database {
    /// 同一读取事务导出指定模板与全部资源；空选择、非法/过期标识拒绝，重复标识按首次顺序去重。
    /// 返回 UTF-8 无 BOM、自描述 v1 JSON 字节；包过大时整批拒绝并提示分批，不返回残缺包。
    pub fn export_mail_template_package(&self, ids: &[String]) -> AppResult<Vec<u8>> {
        if ids.is_empty() || ids.len() > MAX_MAIL_TEMPLATES {
            return Err(package_invalid());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let mut templates = Vec::new();
        let mut seen = HashSet::new();
        let mut used = 0usize;
        for id in ids {
            super::validate_id(id)?;
            if !seen.insert(id) {
                continue;
            }
            let template = read_template(&transaction, id)?.ok_or_else(super::template_missing)?;
            validate_cid_references(&template.html, &template.resources)?;
            add_content_budget(&mut used, template.html.len())?;
            let mut resources = Vec::with_capacity(template.resources.len());
            for resource in template.resources {
                add_content_budget(&mut used, resource.bytes.len())?;
                resources.push(EncodedResource {
                    id: resource.id,
                    name: resource.name,
                    mime_type: resource.mime_type,
                    content_id: resource.content_id,
                    data_base64: STANDARD.encode(resource.bytes),
                });
            }
            templates.push(EncodedTemplate {
                name: template.name,
                subject: template.subject,
                html: template.html,
                resources,
            });
        }
        transaction.commit().map_err(db_error)?;
        let mut output = PackageWriter(Vec::new());
        serde_json::to_writer(
            &mut output,
            &EncodedPackage {
                kind: PACKAGE_KIND.into(),
                version: 1,
                templates,
            },
        )
        .map_err(|_| package_limit())?;
        Ok(output.0)
    }

    /// 在一个只读事务中固定全部本机模板摘要；预览不写数据库，不替用户选择覆盖对象。
    /// 包内重名或本机同名目标超过一份时禁用覆盖，复制/跳过仍可由用户确认。
    pub fn preview_template_import(
        &self,
        package: &TemplatePackage,
    ) -> AppResult<TemplateImportPreview> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let catalog = read_catalog(&transaction)?;
        let (entries, overwrite_allowed) = preview_entries(package, &catalog);
        transaction.commit().map_err(db_error)?;
        Ok(TemplateImportPreview {
            conflict_count: entries.iter().filter(|entry| entry.conflict).count(),
            entries,
            overwrite_allowed,
            catalog,
            package_identity: package.identity.clone(),
        })
    }

    /// 仅接受同一解码包的确认预览；写锁内复核完整目录基线，再按明确策略整批提交。
    /// 任一容量、目标、资源或 SQL 失败都回滚全部内容；覆盖保留目标标识及首次创建时间。
    /// 复制为默认策略，同名生成 80 字以内的唯一后缀名；跳过也检查本批已添加项，避免源包重名产生意外副本。
    pub fn import_template_package(
        &self,
        package: &TemplatePackage,
        preview: &TemplateImportPreview,
        policy: TemplateImportPolicy,
    ) -> AppResult<TemplateImportResult> {
        if !Arc::ptr_eq(&package.identity, &preview.package_identity) {
            return Err(import_stale());
        }
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let catalog = read_catalog(&transaction)?;
        if catalog != preview.catalog {
            return Err(import_stale());
        }
        let (_, overwrite_allowed) = preview_entries(package, &catalog);
        if policy == TemplateImportPolicy::Overwrite && !overwrite_allowed {
            return Err(AppError::new(
                "template_import_ambiguous",
                "存在重复来源名称或多个同名模板，无法确定覆盖目标。请选择保留副本或跳过同名。",
            ));
        }
        let mut names: HashSet<String> = catalog
            .iter()
            .map(|item| item.name.trim().to_owned())
            .collect();
        let mut result = TemplateImportResult {
            added: 0,
            replaced: 0,
            skipped: 0,
            summaries: Vec::new(),
        };
        let now = Local::now().to_rfc3339();
        for template in &package.templates {
            let matches: Vec<_> = catalog
                .iter()
                .filter(|item| item.name.trim() == template.name)
                .collect();
            if policy == TemplateImportPolicy::Skip && names.contains(&template.name) {
                result.skipped += 1;
                continue;
            }
            let replacing = policy == TemplateImportPolicy::Overwrite && matches.len() == 1;
            let name = if policy == TemplateImportPolicy::Copy {
                unique_copy_name(&template.name, &names)?
            } else {
                template.name.clone()
            };
            let id = if replacing {
                let id = matches[0].id.clone();
                transaction.execute("UPDATE mail_templates SET name=?1,subject=?2,html=?3,updated_at=?4 WHERE id=?5", params![name, template.subject, template.html, now, id]).map_err(db_error)?;
                transaction
                    .execute(
                        "DELETE FROM mail_template_resources WHERE template_id=?1",
                        [&id],
                    )
                    .map_err(db_error)?;
                result.replaced += 1;
                id
            } else {
                check_capacity(&transaction)?;
                let id = new_id(&transaction)?;
                transaction.execute("INSERT INTO mail_templates(id,name,subject,html,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)", params![id, name, template.subject, template.html, now]).map_err(db_error)?;
                result.added += 1;
                id
            };
            write_resources(&transaction, &id, &template.resources)?;
            names.insert(name);
            result.summaries.push(read_summary(&transaction, &id)?);
        }
        transaction.commit().map_err(db_error)?;
        Ok(result)
    }
}

/// 排序固定且只读元信息；更新时间由现有所有模板写入口同步维护，用于确认后的目录变化检测。
fn read_catalog(connection: &Connection) -> AppResult<Vec<TemplateSummary>> {
    let mut statement = connection.prepare("SELECT t.id,t.name,t.subject,t.updated_at,(SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NULL),(SELECT count(*) FROM mail_template_resources r WHERE r.template_id=t.id AND r.content_id IS NOT NULL) FROM mail_templates t ORDER BY t.id").map_err(db_error)?;
    let result = statement
        .query_map([], summary_from_row)
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(result)
}

/// 根据固定目录计算冲突，不依赖页面回传的统计；包内重复名会阻止覆盖，避免顺序决定覆盖结果。
fn preview_entries(
    package: &TemplatePackage,
    catalog: &[TemplateSummary],
) -> (Vec<TemplateImportEntry>, bool) {
    let mut source_names = HashSet::new();
    let mut overwrite_allowed = true;
    let entries = package
        .templates
        .iter()
        .map(|template| {
            let matches = catalog
                .iter()
                .filter(|item| item.name.trim() == template.name)
                .count();
            if !source_names.insert(&template.name) || matches > 1 {
                overwrite_allowed = false;
            }
            let image_count = template
                .resources
                .iter()
                .filter(|resource| resource.content_id.is_some())
                .count();
            TemplateImportEntry {
                name: template.name.clone(),
                subject: template.subject.clone(),
                image_count,
                attachment_count: template.resources.len() - image_count,
                conflict: matches > 0,
            }
        })
        .collect();
    (entries, overwrite_allowed)
}

/// 在完整名称集合中查找空闲副本名，截断按 Unicode 字符处理，不切断 UTF-8。
fn unique_copy_name(original: &str, names: &HashSet<String>) -> AppResult<String> {
    if !names.contains(original) {
        return Ok(original.into());
    }
    for index in 1..=MAX_MAIL_TEMPLATES + 1 {
        let suffix = format!("（导入副本 {index}）");
        let prefix: String = original.chars().take(80 - suffix.chars().count()).collect();
        let candidate = format!("{prefix}{suffix}");
        if !names.contains(&candidate) {
            return Ok(candidate);
        }
    }
    // 正常 200 份配额内必有空闲名；外部损坏数据突破配额时仍返回错误，不能因名称冲突崩溃。
    Err(package_limit())
}

/// 用现有 HTML 解析器检查真正 img.src 中的 CID；正文/代码示例中的 cid: 文本不会误判。
/// 只检查资源引用存在性；宿主预览和应用继续使用既有正文净化器，不会加载外部地址。
fn validate_cid_references(html: &str, resources: &[TemplateResource]) -> AppResult<()> {
    let ids: HashSet<String> = resources
        .iter()
        .filter_map(|resource| resource.content_id.clone())
        .collect();
    let invalid = Arc::new(AtomicBool::new(false));
    let observed = invalid.clone();
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(["img"].iter())
        .add_tag_attributes("img", ["src"].iter())
        .add_url_schemes(["cid"].iter())
        .attribute_filter(move |tag, attribute, value| {
            if tag == "img"
                && attribute == "src"
                && value.starts_with("cid:")
                && !ids.contains(&value[4..])
            {
                observed.store(true, Ordering::Relaxed);
            }
            Some(std::borrow::Cow::Owned(value.to_owned()))
        });
    let _ = builder.clean(html);
    if invalid.load(Ordering::Relaxed) {
        return Err(package_invalid());
    }
    Ok(())
}

fn add_content_budget(used: &mut usize, bytes: usize) -> AppResult<()> {
    *used = used.checked_add(bytes).ok_or_else(package_limit)?;
    if *used > MAX_PACKAGE_CONTENT_BYTES {
        return Err(package_limit());
    }
    Ok(())
}

/// 流式序列化一到上限就停止，避免大量 HTML 控制字符被 JSON 转义后再分配数倍内存。
struct PackageWriter(Vec<u8>);
impl Write for PackageWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_TEMPLATE_PACKAGE_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("template package size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 解析数组时逐项限量，避免恶意文件使用大量空对象绕过单份资源和模板数量限制。
fn limited_vec<'de, D, T>(deserializer: D, limit: usize) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Limited<T> {
        limit: usize,
        marker: PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> de::Visitor<'de> for Limited<T> {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a bounded template array")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element()? {
                if items.len() >= self.limit {
                    return Err(de::Error::custom("template array limit"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    deserializer.deserialize_seq(Limited {
        limit,
        marker: PhantomData,
    })
}
fn deserialize_templates<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<EncodedTemplate>, D::Error> {
    limited_vec(deserializer, MAX_MAIL_TEMPLATES)
}
fn deserialize_resources<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<EncodedResource>, D::Error> {
    limited_vec(deserializer, 20)
}

fn package_invalid() -> AppError {
    AppError::new(
        "template_package_invalid",
        "模板包格式、版本或资源引用无效，请使用本程序导出的模板包。",
    )
}
fn package_limit() -> AppError {
    AppError::new(
        "template_package_limit",
        "模板包超过限制：文件最多128 MiB，正文和资源合计最多80 MiB。请减少选择并分批导出或导入。",
    )
}
fn import_stale() -> AppError {
    AppError::new(
        "template_import_stale",
        "模板目录或待导入内容已变化，请重新预览后确认导入。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Cursor;

    /// 全部模板与数据库均为隔离合成数据；验证资源自包含，不依赖原始附件或任何真实账号。
    fn fixture_package() -> Vec<u8> {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        serde_json::to_vec(&json!({
            "kind": PACKAGE_KIND, "version": 1,
            "templates": [{ "name":"  中文模板  ", "subject":"中文主题😀",
                "html":"<p>说明 cid:文本不是引用</p><img src=\"cid:pic@template.local\">",
                "resources":[
                {"id":"pic", "name":"中文图片.png", "mimeType":"image/png", "contentId":"pic@template.local", "dataBase64":STANDARD.encode(png.into_inner())},
                {"id":"file", "name":"二进制.bin", "mimeType":null, "contentId":null, "dataBase64":STANDARD.encode([0,255,13,10,128])}
                ] }]
        })).unwrap()
    }

    fn database() -> (tempfile::TempDir, Database) {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("synthetic.sqlite3")).unwrap();
        (folder, database)
    }

    fn named_package(names: &[&str]) -> TemplatePackage {
        decode_template_package(&serde_json::to_vec(&json!({
            "kind":PACKAGE_KIND, "version":1,
            "templates": names.iter().map(|name| json!({"name":name,"subject":"导入内容","html":"<p>正文</p>","resources":[]})).collect::<Vec<_>>()
        })).unwrap()).unwrap()
    }

    /// 中文、HTML、CID 和二进制字节完整往返，包不含源数据库模板 ID 或额外设置。
    #[test]
    fn package_roundtrip_keeps_all_content_and_independent_resources() {
        let (_folder, source) = database();
        let package = decode_template_package(&fixture_package()).unwrap();
        let preview = source.preview_template_import(&package).unwrap();
        assert_eq!(preview.entries[0].name, "中文模板");
        assert_eq!(preview.entries[0].image_count, 1);
        assert_eq!(preview.entries[0].attachment_count, 1);
        let result = source
            .import_template_package(&package, &preview, TemplateImportPolicy::default())
            .unwrap();
        assert_eq!((result.added, result.replaced, result.skipped), (1, 0, 0));
        let id = result.summaries[0].id.clone();
        let original = source.mail_template(&id).unwrap().unwrap();
        let bytes = source
            .export_mail_template_package(&[id.clone(), id.clone()])
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["templates"].as_array().unwrap().len(), 1);
        assert!(json["templates"][0].get("id").is_none());
        assert!(!String::from_utf8_lossy(&bytes).contains(&id));
        let (_target_folder, target) = database();
        let decoded = decode_template_package(&bytes).unwrap();
        let preview = target.preview_template_import(&decoded).unwrap();
        let imported = target
            .import_template_package(&decoded, &preview, TemplateImportPolicy::Copy)
            .unwrap();
        let restored = target
            .mail_template(&imported.summaries[0].id)
            .unwrap()
            .unwrap();
        assert_ne!(original.id, restored.id);
        assert_eq!(original.html, restored.html);
        assert_eq!(original.subject, restored.subject);
        for (before, after) in original.resources.iter().zip(&restored.resources) {
            assert_eq!(before.bytes, after.bytes);
            assert_eq!(before.content_id, after.content_id);
        }
        assert!(source.export_mail_template_package(&[]).is_err());
        assert_eq!(
            source
                .export_mail_template_package(&[id, "missing".into()])
                .err()
                .unwrap()
                .code,
            "template_missing"
        );
        let public = serde_json::to_value(&preview).unwrap();
        assert!(public.get("catalog").is_none());
        assert!(public.get("packageIdentity").is_none());
    }

    /// 三种冲突策略均显式计数；覆盖保留目标 ID/创建时间，副本名按字符截断且不会覆盖原模板。
    #[test]
    fn copy_skip_and_overwrite_have_distinct_atomic_results() {
        for policy in [
            TemplateImportPolicy::Copy,
            TemplateImportPolicy::Skip,
            TemplateImportPolicy::Overwrite,
        ] {
            let (_folder, db) = database();
            let original = db
                .save_mail_template("同名", "原主题", "原正文", &[], None)
                .unwrap();
            let package = named_package(&["同名", "新名"]);
            let preview = db.preview_template_import(&package).unwrap();
            assert_eq!(preview.conflict_count, 1);
            assert!(preview.overwrite_allowed);
            let result = db
                .import_template_package(&package, &preview, policy)
                .unwrap();
            let current = db.mail_template(&original.id).unwrap().unwrap();
            match policy {
                TemplateImportPolicy::Copy => {
                    assert_eq!((result.added, result.replaced, result.skipped), (2, 0, 0));
                    assert_eq!(current.html, "原正文");
                    assert!(result
                        .summaries
                        .iter()
                        .any(|item| item.name == "同名（导入副本 1）"));
                }
                TemplateImportPolicy::Skip => {
                    assert_eq!((result.added, result.replaced, result.skipped), (1, 0, 1));
                    assert_eq!(current.html, "原正文");
                }
                TemplateImportPolicy::Overwrite => {
                    assert_eq!((result.added, result.replaced, result.skipped), (1, 1, 0));
                    assert_eq!(current.html, "<p>正文</p>");
                    assert_eq!(current.created_at, original.created_at);
                }
            }
            assert_eq!(result.summaries.len(), result.added + result.replaced);
        }
        let (_folder, db) = database();
        let name = "中".repeat(80);
        db.save_mail_template(&name, "", "", &[], None).unwrap();
        let package = named_package(&[&name, &name]);
        let preview = db.preview_template_import(&package).unwrap();
        let result = db
            .import_template_package(&package, &preview, TemplateImportPolicy::Copy)
            .unwrap();
        assert!(result
            .summaries
            .iter()
            .all(|item| item.name.chars().count() == 80));
        assert_ne!(result.summaries[0].name, result.summaries[1].name);
    }

    #[test]
    fn ambiguous_overwrite_is_rejected_and_skip_deduplicates_source_names() {
        for target_duplicates in [false, true] {
            let (_folder, db) = database();
            if target_duplicates {
                for _ in 0..2 {
                    db.save_mail_template("重名", "原", "", &[], None).unwrap();
                }
            }
            let package = if target_duplicates {
                named_package(&["重名"])
            } else {
                named_package(&["重名", "重名"])
            };
            let preview = db.preview_template_import(&package).unwrap();
            assert!(!preview.overwrite_allowed);
            let count = db.list_mail_templates("").unwrap().len();
            assert_eq!(
                db.import_template_package(&package, &preview, TemplateImportPolicy::Overwrite)
                    .err()
                    .unwrap()
                    .code,
                "template_import_ambiguous"
            );
            assert_eq!(db.list_mail_templates("").unwrap().len(), count);
            let result = db
                .import_template_package(&package, &preview, TemplateImportPolicy::Skip)
                .unwrap();
            assert_eq!(
                (result.added, result.skipped),
                if target_duplicates { (0, 1) } else { (1, 1) }
            );
        }
    }

    /// 预览后新增、改名、改内容或删目标均须重新预览；不同包不能借用另一个包的确认。
    #[test]
    fn stale_catalog_and_mismatched_package_cannot_write() {
        for mutation in 0..4 {
            let (_folder, db) = database();
            let original = db.save_mail_template("原模板", "", "", &[], None).unwrap();
            let package = named_package(&["导入模板"]);
            let preview = db.preview_template_import(&package).unwrap();
            match mutation {
                0 => {
                    db.save_mail_template("另一个", "", "", &[], None).unwrap();
                }
                1 => {
                    db.rename_mail_template(&original.id, "新名").unwrap();
                }
                2 => {
                    db.save_mail_template(
                        "原模板",
                        "变更主题",
                        "变更正文",
                        &[],
                        Some(&original.id),
                    )
                    .unwrap();
                }
                _ => {
                    db.delete_mail_template(&original.id).unwrap();
                }
            }
            assert_eq!(
                db.import_template_package(&package, &preview, TemplateImportPolicy::Copy)
                    .err()
                    .unwrap()
                    .code,
                "template_import_stale"
            );
            assert!(db.list_mail_templates("导入模板").unwrap().is_empty());
        }
        let (_folder, db) = database();
        let first = named_package(&["A"]);
        let second = named_package(&["B"]);
        let preview = db.preview_template_import(&first).unwrap();
        assert_eq!(
            db.import_template_package(&second, &preview, TemplateImportPolicy::Copy)
                .err()
                .unwrap()
                .code,
            "template_import_stale"
        );
    }

    /// 第二项 SQL 失败时，前一项新增与覆盖资源均回滚；容量不足也不能留下半批结果。
    #[test]
    fn later_failure_rolls_back_insert_overwrite_and_capacity_changes() {
        for policy in [TemplateImportPolicy::Copy, TemplateImportPolicy::Overwrite] {
            let (_folder, db) = database();
            let original = db
                .save_mail_template("原模板", "原主题", "原正文", &[], None)
                .unwrap();
            let package = named_package(&["原模板", "失败项"]);
            let preview = db.preview_template_import(&package).unwrap();
            db.connection().unwrap().execute_batch("CREATE TRIGGER reject_import BEFORE INSERT ON mail_templates WHEN NEW.name='失败项' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
            assert_eq!(
                db.import_template_package(&package, &preview, policy)
                    .err()
                    .unwrap()
                    .code,
                "database_error"
            );
            let unchanged = db.mail_template(&original.id).unwrap().unwrap();
            assert_eq!(unchanged.html, original.html);
            assert_eq!(unchanged.updated_at, original.updated_at);
            assert_eq!(db.list_mail_templates("").unwrap().len(), 1);
        }
        let (_folder, db) = database();
        for i in 0..199 {
            db.save_mail_template(&format!("已有{i}"), "", "", &[], None)
                .unwrap();
        }
        let package = named_package(&["新A", "新B"]);
        let preview = db.preview_template_import(&package).unwrap();
        assert_eq!(
            db.import_template_package(&package, &preview, TemplateImportPolicy::Copy)
                .err()
                .unwrap()
                .code,
            "template_limit"
        );
        assert_eq!(db.list_mail_templates("").unwrap().len(), 199);
        assert!(db.list_mail_templates("新").unwrap().is_empty());
    }

    #[test]
    fn malformed_unknown_fields_base64_and_dangling_cid_reject_the_entire_package() {
        let fixture: Value = serde_json::from_slice(&fixture_package()).unwrap();
        let mut variants = Vec::new();
        for (field, value) in [
            ("version", json!(2)),
            ("kind", json!("other")),
            ("credentials", json!("must-not-be-accepted")),
        ] {
            let mut invalid = fixture.clone();
            invalid[field] = value;
            variants.push(invalid);
        }
        let mut invalid = fixture.clone();
        invalid["templates"][0]["smtp"] = json!({});
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["resources"][0]["token"] = json!("invalid");
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["resources"][0]["dataBase64"] = json!("$$$bad");
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["html"] = json!("<img src='cid:missing'>");
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["html"] = json!("<img src='cid:missing&#64;local'>");
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["resources"][0]["mimeType"] = json!("image/jpeg");
        variants.push(invalid);
        let mut invalid = fixture.clone();
        invalid["templates"][0]["resources"][1]["id"] = json!("pic");
        variants.push(invalid);
        for value in variants {
            assert!(decode_template_package(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for bytes in [&b""[..], &b"{broken"[..], &[0xff, 0xfe][..]] {
            assert!(decode_template_package(bytes).is_err());
        }
        assert!(decode_template_package(
            &serde_json::to_vec(&json!({"kind":PACKAGE_KIND,"version":1,"templates":[]})).unwrap()
        )
        .is_err());
    }

    #[test]
    fn package_count_body_resource_and_shared_budget_limits_are_enforced() {
        let too_many: Vec<_> = (0..201)
            .map(|i| json!({"name":format!("模板{i}"),"subject":"","html":"","resources":[]}))
            .collect();
        assert!(decode_template_package(
            &serde_json::to_vec(&json!({"kind":PACKAGE_KIND,"version":1,"templates":too_many}))
                .unwrap()
        )
        .is_err());
        let mut value: Value = serde_json::from_slice(&fixture_package()).unwrap();
        value["templates"][0]["html"] = json!("x".repeat(crate::mail::MAX_BODY_BYTES + 1));
        assert_eq!(
            decode_template_package(&serde_json::to_vec(&value).unwrap())
                .err()
                .unwrap()
                .code,
            "body_too_large"
        );
        let resource =
            json!({"id":"file","name":"test.bin","mimeType":null,"contentId":null,"dataBase64":""});
        value["templates"][0]["html"] = json!("");
        value["templates"][0]["resources"] = json!(vec![resource; 21]);
        assert!(decode_template_package(&serde_json::to_vec(&value).unwrap()).is_err());
        // 共享预算在真实 decode/export 路径均调用同一 checked_add；直接用边界值避免回归额外占用数百 MiB。
        let mut used = MAX_PACKAGE_CONTENT_BYTES - 1;
        add_content_budget(&mut used, 1).unwrap();
        assert_eq!(
            add_content_budget(&mut used, 1).unwrap_err().code,
            "template_package_limit"
        );
        let mut writer = PackageWriter(vec![0; MAX_TEMPLATE_PACKAGE_BYTES - 1]);
        writer.write_all(&[1]).unwrap();
        assert!(writer.write_all(&[2]).is_err());
    }
}
