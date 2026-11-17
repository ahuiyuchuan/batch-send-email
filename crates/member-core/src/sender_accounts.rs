//! 有序 SMTP 发件账号目录与独立加密凭据引用。作者：ahui。
//! SQLite settings 只保存公开配置、凭据文件随机标识和目录版本，不保存密码、授权码或 QQ 登录信息。
//! 密文文件先写新版本、目录指针与 revision 再同事务提交；失败可丢弃新文件而不破坏旧账号凭据。

use crate::{
    database::{db_error, Database},
    mail::{MailSender, MailSettings},
    models::{AppError, AppResult},
    smtp_credential_store::{SmtpCredentialBinding, SmtpCredentialStore},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

/// 有序目录最多二十个账号，限制配置管理与启动时的凭据状态检查；不是服务商发送额度。
pub const MAX_SENDER_ACCOUNTS: usize = 20;
/// 旧账号迁移使用固定标识并保留旧密文文件，删除后由持久目录标记阻止再次自动迁回。
pub const LEGACY_SENDER_ACCOUNT_ID: &str = "legacy-default";
const CATALOG_KEY: &str = "smtp_sender_accounts_v1";

/// 可持久化的公开 SMTP 配置；不包含密码，所有发送凭据只在宿主内存组装。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SenderPublicSettings {
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub username: String,
    pub from_address: String,
    pub from_name: String,
}

impl SenderPublicSettings {
    /// 从可能含临时密码的输入提取白名单字段，不序列化整个 MailSettings。
    pub fn from_mail(settings: &MailSettings) -> Self {
        Self {
            host: settings.host.clone(),
            port: settings.port,
            tls_mode: settings.tls_mode.clone(),
            username: settings.username.clone(),
            from_address: settings.from_address.clone(),
            from_name: settings.from_name.clone(),
        }
    }

    /// 仅在宿主内存中组合授权码；空串只适用于生成匹配键，不能直接用于认证。
    pub fn with_password(&self, password: String) -> MailSettings {
        MailSettings {
            host: self.host.clone(),
            port: self.port,
            tls_mode: self.tls_mode.clone(),
            username: self.username.clone(),
            password,
            from_address: self.from_address.clone(),
            from_name: self.from_name.clone(),
        }
    }
}

/// 内部文件引用不从页面接收；Legacy 只用于首次兼容旧配置，Named 使用宿主生成的随机版本。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SenderCredentialReference {
    #[default]
    Missing,
    Legacy,
    Named {
        key: String,
    },
}

/// 完整目录条目仅在后端使用；页面需显式转换为公开字段和凭据是否可用状态。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SenderAccount {
    pub id: String,
    pub name: String,
    pub settings: SenderPublicSettings,
    #[serde(default)]
    credential: SenderCredentialReference,
}

impl SenderAccount {
    /// 只返回不可用于读取密码的文件引用元信息，供宿主事务成功后清理不再使用的旧密文。
    pub fn credential_reference(&self) -> &SenderCredentialReference {
        &self.credential
    }

    /// 固定数据目录与严格标识共同构成路径；不接受任何页面路径或账户名称作为文件名。
    pub fn credential_store(&self, directory: &Path) -> AppResult<Option<SmtpCredentialStore>> {
        credential_store(directory, &self.id, &self.credential)
    }

    /// 新文件绑定账号ID；只读旧默认文件时沿用原绑定，迁移不修改旧密文或延长其用途。
    pub fn credential_binding(&self) -> SmtpCredentialBinding {
        let settings = self.settings.with_password(String::new());
        if self.credential == SenderCredentialReference::Legacy {
            SmtpCredentialBinding::from_settings(&settings)
        } else {
            SmtpCredentialBinding::for_account(&self.id, &settings)
        }
    }
}

/// 目录顺序用于账号管理和兼容的顺序分配；随机均分由预检另行冻结本次任务顺序。
/// revision 每次实际目录变更或凭据版本替换时递增，任何已确认任务都不能跨版本沿用。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SenderAccountCatalog {
    version: u32,
    pub revision: u64,
    pub accounts: Vec<SenderAccount>,
}

impl Database {
    /// 首次读取在立即事务内迁移旧公开配置，之后即使删光也保留空目录标记，禁止旧账号自动复活。
    /// 不读取或解密凭据；旧默认账号仍引用原文件，新建库没有配置时返回空目录。
    pub fn sender_accounts(&self) -> AppResult<SenderAccountCatalog> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let catalog = read_or_initialize(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(catalog)
    }

    /// 新建或修改一个明确标识的账号，追加新账号、保持原账号位置；只允许唯一的 SMTP 认证/发件绑定。
    /// expected_revision 必须来自同一次读取，防止凭据写入期间另一实例修改目录；失败整批回滚。
    /// credential=None 保留原文件引用，Some 用于显式保存/清除；所有内容都不包含密码。
    pub fn save_sender_account(
        &self,
        expected_revision: u64,
        id: &str,
        name: &str,
        settings: &SenderPublicSettings,
        credential: Option<SenderCredentialReference>,
    ) -> AppResult<SenderAccountCatalog> {
        validate_id(id)?;
        let name = validate_name(name)?;
        // 复用邮件连接构造的格式校验，固定占位授权码不联网、不持久化或用于发送。
        MailSender::new(&settings.with_password("configuration-validation-only".into()))?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let mut catalog = read_or_initialize(&transaction)?;
        check_revision(&catalog, expected_revision)?;
        let binding = SmtpCredentialBinding::from_settings(&settings.with_password(String::new()));
        if catalog.accounts.iter().any(|item| {
            item.id != id
                && SmtpCredentialBinding::from_settings(&item.settings.with_password(String::new()))
                    == binding
        }) {
            return Err(AppError::new(
                "sender_account_duplicate",
                "同一 SMTP 账号和发件身份已存在，请编辑已有账号，避免重复分配。",
            ));
        }
        if let Some(existing) = catalog.accounts.iter_mut().find(|item| item.id == id) {
            existing.name = name;
            existing.settings = settings.clone();
            if let Some(credential) = credential {
                validate_reference(id, &credential)?;
                existing.credential = credential;
            }
        } else {
            if catalog.accounts.len() >= MAX_SENDER_ACCOUNTS {
                return Err(AppError::new(
                    "sender_account_limit",
                    "最多保存20个发件账号，请先删除不需要的账号。",
                ));
            }
            let credential = credential.unwrap_or_default();
            validate_reference(id, &credential)?;
            catalog.accounts.push(SenderAccount {
                id: id.into(),
                name,
                settings: settings.clone(),
                credential,
            });
        }
        write_catalog(&transaction, &mut catalog)?;
        transaction.commit().map_err(db_error)?;
        Ok(catalog)
    }

    /// 删除一个仍存在的账号，不影响全局已发送历史、手动排除或其他账号；密文清理由宿主在提交后完成。
    pub fn remove_sender_account(
        &self,
        expected_revision: u64,
        id: &str,
    ) -> AppResult<SenderAccountCatalog> {
        validate_id(id)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let mut catalog = read_or_initialize(&transaction)?;
        check_revision(&catalog, expected_revision)?;
        let position = catalog
            .accounts
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(account_missing)?;
        catalog.accounts.remove(position);
        write_catalog(&transaction, &mut catalog)?;
        transaction.commit().map_err(db_error)?;
        Ok(catalog)
    }

    /// 精确重排整个目录，必须一一包含所有账号且不能重复；不接受部分移动或新增标识。
    pub fn reorder_sender_accounts(
        &self,
        expected_revision: u64,
        ids: &[String],
    ) -> AppResult<SenderAccountCatalog> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let mut catalog = read_or_initialize(&transaction)?;
        check_revision(&catalog, expected_revision)?;
        let mut seen = HashSet::new();
        if ids.len() != catalog.accounts.len()
            || ids
                .iter()
                .any(|id| !seen.insert(id) || !catalog.accounts.iter().any(|item| &item.id == id))
        {
            return Err(AppError::new(
                "sender_account_order",
                "账号排序已变化，请刷新后重新排序。",
            ));
        }
        let mut reordered = Vec::with_capacity(ids.len());
        for id in ids {
            reordered.push(
                catalog
                    .accounts
                    .iter()
                    .find(|item| &item.id == id)
                    .ok_or_else(account_missing)?
                    .clone(),
            );
        }
        if reordered != catalog.accounts {
            catalog.accounts = reordered;
            write_catalog(&transaction, &mut catalog)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(catalog)
    }
}

/// 新随机凭据的固定路径供宿主先写后提交；key 必须为32位小写十六进制，路径不能由页面控制。
pub fn credential_store(
    directory: &Path,
    id: &str,
    reference: &SenderCredentialReference,
) -> AppResult<Option<SmtpCredentialStore>> {
    validate_id(id)?;
    validate_reference(id, reference)?;
    Ok(match reference {
        SenderCredentialReference::Missing => None,
        SenderCredentialReference::Legacy => Some(SmtpCredentialStore::new(
            directory.join("smtp-credential.bin"),
        )),
        SenderCredentialReference::Named { key } => Some(SmtpCredentialStore::new(
            directory
                .join("smtp-accounts")
                .join(format!("{id}-{key}.bin")),
        )),
    })
}

fn read_or_initialize(connection: &Connection) -> AppResult<SenderAccountCatalog> {
    let text: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [CATALOG_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if let Some(text) = text {
        if text.len() > 256 * 1024 {
            return Err(catalog_error());
        }
        let catalog: SenderAccountCatalog =
            serde_json::from_str(&text).map_err(|_| catalog_error())?;
        validate_catalog(&catalog)?;
        return Ok(catalog);
    }
    let legacy: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key='smtp_public'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let accounts = if let Some(text) = legacy {
        let settings: SenderPublicSettings =
            serde_json::from_str(&text).map_err(|_| catalog_error())?;
        vec![SenderAccount {
            id: LEGACY_SENDER_ACCOUNT_ID.into(),
            name: "默认发件账号".into(),
            settings,
            credential: SenderCredentialReference::Legacy,
        }]
    } else {
        Vec::new()
    };
    let catalog = SenderAccountCatalog {
        version: 1,
        revision: 0,
        accounts,
    };
    validate_catalog(&catalog)?;
    let text = serde_json::to_string(&catalog).map_err(|_| catalog_error())?;
    connection
        .execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)",
            params![CATALOG_KEY, text],
        )
        .map_err(db_error)?;
    Ok(catalog)
}

fn write_catalog(connection: &Connection, catalog: &mut SenderAccountCatalog) -> AppResult<()> {
    catalog.revision = catalog.revision.checked_add(1).ok_or_else(catalog_error)?;
    let text = serde_json::to_string(catalog).map_err(|_| catalog_error())?;
    connection
        .execute(
            "UPDATE settings SET value=?1 WHERE key=?2",
            params![text, CATALOG_KEY],
        )
        .map_err(db_error)?;
    Ok(())
}
fn validate_catalog(catalog: &SenderAccountCatalog) -> AppResult<()> {
    if catalog.version != 1 || catalog.accounts.len() > MAX_SENDER_ACCOUNTS {
        return Err(catalog_error());
    }
    let mut ids = HashSet::new();
    let mut bindings = Vec::new();
    for account in &catalog.accounts {
        validate_id(&account.id)?;
        validate_name(&account.name)?;
        validate_reference(&account.id, &account.credential)?;
        if !ids.insert(&account.id) {
            return Err(catalog_error());
        }
        let binding =
            SmtpCredentialBinding::from_settings(&account.settings.with_password(String::new()));
        if bindings.contains(&binding) {
            return Err(catalog_error());
        }
        bindings.push(binding);
    }
    Ok(())
}
fn validate_id(id: &str) -> AppResult<()> {
    if id != LEGACY_SENDER_ACCOUNT_ID
        && (id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(AppError::new(
            "sender_account_id",
            "发件账号标识无效，请刷新账号列表。",
        ));
    }
    Ok(())
}
fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(AppError::new(
            "sender_account_name",
            "账号名称请填写1到80字，且不能包含换行或控制字符。",
        ));
    }
    Ok(name.into())
}
fn validate_reference(id: &str, reference: &SenderCredentialReference) -> AppResult<()> {
    match reference {
        SenderCredentialReference::Legacy if id != LEGACY_SENDER_ACCOUNT_ID => Err(catalog_error()),
        SenderCredentialReference::Named { key }
            if key.len() != 32
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Err(catalog_error())
        }
        _ => Ok(()),
    }
}
fn check_revision(catalog: &SenderAccountCatalog, expected: u64) -> AppResult<()> {
    if catalog.revision != expected {
        return Err(AppError::new(
            "sender_accounts_changed",
            "发件账号已变化，请刷新后重新确认发送范围。",
        ));
    }
    Ok(())
}
fn account_missing() -> AppError {
    AppError::new(
        "sender_account_missing",
        "该发件账号已不存在，请刷新账号列表。",
    )
}
fn catalog_error() -> AppError {
    AppError::new(
        "sender_accounts_storage",
        "发件账号目录格式无法读取或保存，请保留数据库并检查本机数据目录。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(number: usize) -> SenderPublicSettings {
        SenderPublicSettings {
            host: "smtp.example.test".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: format!("sender{number}@example.test"),
            from_address: format!("sender{number}@example.test"),
            from_name: "合成发件人".into(),
        }
    }
    fn id(number: usize) -> String {
        format!("{number:032x}")
    }

    /// 初次迁移只复制公开设置和旧文件引用，不读密文；删除最后一个后重开仍为空，不从旧设置复活。
    #[test]
    fn legacy_migration_and_empty_marker_are_persistent_without_secret_access() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("data.sqlite3");
        let db = Database::open(&path).unwrap();
        db.set_setting("smtp_public", &serde_json::to_string(&settings(1)).unwrap())
            .unwrap();
        let catalog = db.sender_accounts().unwrap();
        assert_eq!(catalog.accounts.len(), 1);
        assert_eq!(catalog.accounts[0].id, LEGACY_SENDER_ACCOUNT_ID);
        assert!(matches!(
            catalog.accounts[0].credential_reference(),
            SenderCredentialReference::Legacy
        ));
        assert!(!folder.path().join("smtp-credential.bin").exists());
        let removed = db
            .remove_sender_account(catalog.revision, LEGACY_SENDER_ACCOUNT_ID)
            .unwrap();
        assert!(removed.accounts.is_empty());
        drop(db);
        let reopened = Database::open(&path).unwrap();
        assert!(reopened.sender_accounts().unwrap().accounts.is_empty());
        assert!(reopened.setting("smtp_public").unwrap().is_some());
        assert_eq!(
            reopened
                .connection()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            5
        );
    }

    /// 账号顺序、公开设置及凭据版本指针一起持久化；版本冲突或非法完整排序不写入任何部分。
    #[test]
    fn ordered_crud_revision_and_stale_writes_are_atomic() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let empty = db.sender_accounts().unwrap();
        assert!(empty.accounts.is_empty());
        let first = db
            .save_sender_account(empty.revision, &id(1), "  合成一  ", &settings(1), None)
            .unwrap();
        let second = db
            .save_sender_account(first.revision, &id(2), "合成二", &settings(2), None)
            .unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.accounts[0].name, "合成一");
        assert_eq!(
            db.save_sender_account(0, &id(3), "失效写入", &settings(3), None)
                .err()
                .unwrap()
                .code,
            "sender_accounts_changed"
        );
        assert!(db
            .reorder_sender_accounts(second.revision, &[id(1), id(1)])
            .is_err());
        assert!(db
            .reorder_sender_accounts(second.revision, &[id(1)])
            .is_err());
        let reordered = db
            .reorder_sender_accounts(second.revision, &[id(2), id(1)])
            .unwrap();
        assert_eq!(reordered.accounts[0].id, id(2));
        assert_eq!(reordered.revision, 3);
        let same = db
            .reorder_sender_accounts(reordered.revision, &[id(2), id(1)])
            .unwrap();
        assert_eq!(same.revision, reordered.revision);
        let changed = db
            .save_sender_account(
                same.revision,
                &id(2),
                "改名",
                &settings(2),
                Some(SenderCredentialReference::Named { key: id(9) }),
            )
            .unwrap();
        assert_eq!(changed.revision, 4);
        assert_eq!(changed.accounts[0].name, "改名");
        let removed = db.remove_sender_account(changed.revision, &id(2)).unwrap();
        assert_eq!(removed.revision, 5);
        assert_eq!(removed.accounts.len(), 1);
    }

    /// 多账号上限和重复认证/发件身份在持写锁时核验，不因显示名不同而允许同一账号假分配。
    #[test]
    fn duplicate_identity_capacity_and_generated_path_boundaries_are_enforced() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let mut catalog = db.sender_accounts().unwrap();
        catalog = db
            .save_sender_account(catalog.revision, &id(1), "首账号", &settings(1), None)
            .unwrap();
        let mut duplicate = settings(1);
        duplicate.host = " SMTP.EXAMPLE.TEST ".into();
        duplicate.from_name = "其他显示名".into();
        assert_eq!(
            db.save_sender_account(catalog.revision, &id(2), "重复账号", &duplicate, None)
                .err()
                .unwrap()
                .code,
            "sender_account_duplicate"
        );
        for number in 2..=20 {
            catalog = db
                .save_sender_account(
                    catalog.revision,
                    &id(number),
                    "合成账号",
                    &settings(number),
                    None,
                )
                .unwrap();
        }
        assert_eq!(
            db.save_sender_account(catalog.revision, &id(21), "超限账号", &settings(21), None)
                .err()
                .unwrap()
                .code,
            "sender_account_limit"
        );
        assert_eq!(db.sender_accounts().unwrap().accounts.len(), 20);
        for bad in ["../bad", "C:\\password.bin", "abcd", ""] {
            assert!(
                credential_store(folder.path(), bad, &SenderCredentialReference::Missing).is_err()
            );
        }
        assert!(credential_store(
            folder.path(),
            &id(1),
            &SenderCredentialReference::Named {
                key: "../bad".into()
            }
        )
        .is_err());
        assert!(
            credential_store(folder.path(), &id(1), &SenderCredentialReference::Legacy).is_err()
        );
    }

    /// 实际SQL失败点保证修改账号、顺序或密码指针时目录与revision一起回滚；不声称文件和SQLite同事务。
    #[test]
    fn catalog_sql_failure_preserves_existing_revision_and_credentials_reference() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let initial = db.sender_accounts().unwrap();
        let before = db
            .save_sender_account(
                initial.revision,
                &id(1),
                "原账号",
                &settings(1),
                Some(SenderCredentialReference::Named { key: id(9) }),
            )
            .unwrap();
        db.connection().unwrap().execute_batch("CREATE TRIGGER reject_sender_catalog BEFORE UPDATE ON settings WHEN OLD.key='smtp_sender_accounts_v1' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert!(db
            .save_sender_account(
                before.revision,
                &id(1),
                "新账号",
                &settings(1),
                Some(SenderCredentialReference::Missing)
            )
            .is_err());
        assert!(db.remove_sender_account(before.revision, &id(1)).is_err());
        let after = db.sender_accounts().unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.accounts[0].name, "原账号");
        assert!(after.accounts[0].credential == before.accounts[0].credential);
    }

    /// 本地目录损坏也必须拒绝重复绑定，不能因绕过保存入口而让同一SMTP身份被当作多个账号分配。
    /// 读取不修复用户原始数据，以便保留证据并由用户恢复有效备份。
    #[test]
    fn duplicate_binding_in_stored_catalog_is_rejected_without_repairing_user_data() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let settings = settings(1);
        let catalog = SenderAccountCatalog {
            version: 1,
            revision: 8,
            accounts: vec![
                SenderAccount {
                    id: id(1),
                    name: "合成一".into(),
                    settings: settings.clone(),
                    credential: SenderCredentialReference::Missing,
                },
                SenderAccount {
                    id: id(2),
                    name: "合成二".into(),
                    settings,
                    credential: SenderCredentialReference::Missing,
                },
            ],
        };
        let stored = serde_json::to_string(&catalog).unwrap();
        db.set_setting(CATALOG_KEY, &stored).unwrap();
        assert_eq!(
            db.sender_accounts().err().unwrap().code,
            "sender_accounts_storage"
        );
        assert_eq!(db.setting(CATALOG_KEY).unwrap().unwrap(), stored);
    }

    #[test]
    fn malformed_stored_directory_never_falls_back_to_legacy_or_paths() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::open(folder.path().join("data.sqlite3")).unwrap();
        db.set_setting("smtp_public", &serde_json::to_string(&settings(1)).unwrap())
            .unwrap();
        for value in [
            "{invalid",
            r#"{"version":2,"revision":0,"accounts":[]}"#,
            r#"{"version":1,"revision":0,"accounts":[],"password":"forbidden"}"#,
        ] {
            db.set_setting(CATALOG_KEY, value).unwrap();
            assert!(db.sender_accounts().is_err());
        }
        assert!(!folder.path().join("smtp-credential.bin").exists());
    }
}
