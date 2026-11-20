//! 多 SMTP 发件账号管理与仅宿主可见的凭据解析。作者：ahui。
//! 页面只接收公开目录与密码状态；每次发送固定目录revision和选择，不把授权码加入预检摘要、模板或日志。
//! operation 门闩串行化配置、凭据和发送启动；任何目录变更都使已准备的发送确认失效。

use crate::state::AppState;
use member_core::{
    mail::{MailSender, MailSettings},
    models::{AppError, AppResult},
    sender_accounts::{
        credential_store, SenderAccount, SenderAccountCatalog, SenderCredentialReference,
        SenderPublicSettings,
    },
    smtp_credential_store::{SmtpCredentialBinding, SmtpCredentialLookup},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use tauri::State;

/// 页面目录不含内部凭据文件引用；顺序用于账号管理，随机发送顺序由每次预检另行冻结。
/// revision 包括密码保存与清除，不能只比较页面可见的配置字段是否相同。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccountsView {
    pub revision: u64,
    pub accounts: Vec<MailAccountView>,
}

/// 保持原 MailSettingsView 平铺字段，账号名仅用于区别配置，不参与 SMTP 认证。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccountView {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub settings: SenderPublicSettings,
    pub password_saved: bool,
    pub password_status: &'static str,
    pub password_message: &'static str,
}

/// 密码只允许作为明确保存的输入，None账号标识由宿主随机生成，页面不能指定新文件标识。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailAccountInput {
    pub id: Option<String>,
    pub name: String,
    pub settings: MailSettings,
}

/// 不含凭据的选择快照，供发送前确认人数及账号顺序；不能据此推断密码可用或 SMTP 已连通。
pub(crate) struct SelectedSenderAccounts {
    pub revision: u64,
    pub accounts: Vec<SelectedSenderAccount>,
}
pub(crate) struct SelectedSenderAccount {
    pub id: String,
    pub name: String,
    pub from_address: String,
}

/// 已解析账号仅能驻宿主内存，绝不实现Serialize/Debug；调用者应尽快交给 MailSender 构造并释放。
pub(crate) struct ResolvedSenderAccounts {
    pub revision: u64,
    pub accounts: Vec<ResolvedSenderAccount>,
}
pub(crate) struct ResolvedSenderAccount {
    pub id: String,
    pub name: String,
    pub settings: MailSettings,
}

/// 读取有序目录及匹配授权码状态；首次旧配置迁移不改写或搬动旧密文，不向页面返回密码。
#[tauri::command]
pub async fn mail_accounts(state: State<'_, Arc<AppState>>) -> AppResult<MailAccountsView> {
    let _operation = state.operation.lock().await;
    account_worker(state.inner().clone(), account_catalog_view).await
}

/// 保存公开字段和可选新授权码；空密码保留原凭据引用，只能在连接身份仍匹配时复用。
#[tauri::command]
pub async fn mail_account_save(
    state: State<'_, Arc<AppState>>,
    input: MailAccountInput,
) -> AppResult<MailAccountsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    account_worker(state.inner().clone(), move |state| {
        save_account(state, input)
    })
    .await
}

/// 先提交删除与revision，再清除对应密文；删除文件失败明确报告已删目录但残留密文，不能伪称全部完成。
#[tauri::command]
pub async fn mail_account_remove(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<MailAccountsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    account_worker(state.inner().clone(), move |state| {
        remove_account(state, &id)
    })
    .await
}

/// 一次提交完整账号顺序；任务进行中拒绝，避免改变正在发送的分段账号。
#[tauri::command]
pub async fn mail_accounts_reorder(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
) -> AppResult<MailAccountsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    account_worker(state.inner().clone(), move |state| {
        let database = state.database()?;
        let current = database.sender_accounts()?;
        let catalog = database.reorder_sender_accounts(current.revision, &ids)?;
        invalidate_preparation(state);
        view_for_catalog(state, catalog)
    })
    .await
}

/// 清除只影响指定账号的后续认证，不改变其他账号、QQ登录、模板或全局投递历史。
#[tauri::command]
pub async fn mail_account_clear_password(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<MailAccountsView> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    account_worker(state.inner().clone(), move |state| {
        clear_account_password(state, &id)
    })
    .await
}

/// 小文件与最多20个账号的DPAPI检查交给阻塞池；等待期间只占用operation门闩，不持有AppState短Mutex。
/// 工作中断只返回固定安全说明，不把可能含公开邮箱或系统路径的panic正文转给页面或日志。
async fn account_worker<T: Send + 'static>(
    state: Arc<AppState>,
    work: impl FnOnce(&AppState) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    tokio::task::spawn_blocking(move || work(&state))
        .await
        .map_err(|_| {
            AppError::new(
                "sender_accounts_storage",
                "发件账号操作中断，请刷新列表后重试。",
            )
        })?
}

/// 供旧接口及内部调用者读取公开目录；调用者须持有operation锁，不在AppState短Mutex内进行文件I/O。
pub(crate) fn account_catalog_view(state: &AppState) -> AppResult<MailAccountsView> {
    view_for_catalog(state, state.database()?.sender_accounts()?)
}

/// 按目录顺序固定所选元信息，不读取密文；后续分配零收件人的账号不必提供可用授权码。
pub(crate) fn selected_account_metadata(
    state: &AppState,
    ids: &[String],
) -> AppResult<SelectedSenderAccounts> {
    let catalog = state.database()?.sender_accounts()?;
    let accounts = selected_accounts(&catalog, ids)?
        .into_iter()
        .map(|account| SelectedSenderAccount {
            id: account.id.clone(),
            name: account.name.clone(),
            from_address: account.settings.from_address.clone(),
        })
        .collect();
    Ok(SelectedSenderAccounts {
        revision: catalog.revision,
        accounts,
    })
}

/// 仅解析真正参与发送的账号，顺序始终来自目录；缺失、重复、无选择和失配凭据均拒绝，不借用其他账号。
/// 调用者持有operation锁，并将返回revision与发送预检的固定revision比较后才能构造任务。
pub(crate) fn resolve_selected_accounts(
    state: &AppState,
    ids: &[String],
) -> AppResult<ResolvedSenderAccounts> {
    let catalog = state.database()?.sender_accounts()?;
    let directory = data_directory(state)?;
    let accounts = selected_accounts(&catalog, ids)?
        .into_iter()
        .map(|account| {
            let mut settings = account.settings.with_password(String::new());
            resolve_account_password(account, &directory, &mut settings)?;
            Ok(ResolvedSenderAccount {
                id: account.id.clone(),
                name: account.name.clone(),
                settings,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ResolvedSenderAccounts {
        revision: catalog.revision,
        accounts,
    })
}

/// 旧单账号接口始终映射目录首项；删光后返回None，不从smtp_public重新复活已移除的默认账号。
pub(crate) fn first_account_settings(state: &AppState) -> AppResult<Option<MailSettings>> {
    Ok(state
        .database()?
        .sender_accounts()?
        .accounts
        .first()
        .map(|account| account.settings.with_password(String::new())))
}

/// 兼容旧单账号保存，保持当前首账号标识与名称；空目录则新建，不更改其他账号顺序。
pub(crate) fn save_legacy_account(
    state: &AppState,
    settings: &MailSettings,
) -> AppResult<MailAccountsView> {
    let catalog = state.database()?.sender_accounts()?;
    let first = catalog.accounts.first();
    save_account(
        state,
        MailAccountInput {
            id: first.map(|item| item.id.clone()),
            name: first
                .map(|item| item.name.clone())
                .unwrap_or_else(|| "默认发件账号".into()),
            settings: settings.clone(),
        },
    )
}

/// 兼容旧清除按钮；没有首账号时不尝试读取旧文件，空目录是明确有效状态。
pub(crate) fn clear_legacy_account_password(state: &AppState) -> AppResult<MailAccountsView> {
    let catalog = state.database()?.sender_accounts()?;
    match catalog.accounts.first() {
        Some(account) => clear_account_password(state, &account.id),
        None => view_for_catalog(state, catalog),
    }
}

/// 旧调用提交的公开身份必须与当前首账号一致；临时手填密码仍仅用于本次，不自动持久化。
pub(crate) fn legacy_resolve_password(
    state: &AppState,
    settings: &mut MailSettings,
) -> AppResult<()> {
    if !settings.password.is_empty() {
        return Ok(());
    }
    let catalog = state.database()?.sender_accounts()?;
    let account = catalog.accounts.first().ok_or_else(missing_password)?;
    resolve_account_password(account, &data_directory(state)?, settings)
}

fn save_account(state: &AppState, input: MailAccountInput) -> AppResult<MailAccountsView> {
    let database = state.database()?;
    let current = database.sender_accounts()?;
    let existing = if let Some(id) = &input.id {
        Some(
            current
                .accounts
                .iter()
                .find(|item| &item.id == id)
                .ok_or_else(account_missing)?,
        )
    } else {
        None
    };
    let id = existing
        .map(|item| item.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let settings = SenderPublicSettings::from_mail(&input.settings);
    let validation = settings.with_password(if input.settings.password.is_empty() {
        "configuration-validation-only".into()
    } else {
        input.settings.password.clone()
    });
    MailSender::new(&validation)?;
    let directory = data_directory(state)?;
    let new_reference = if input.settings.password.is_empty() {
        None
    } else {
        Some(SenderCredentialReference::Named {
            key: uuid::Uuid::new_v4().simple().to_string(),
        })
    };
    let new_store = new_reference
        .as_ref()
        .map(|reference| credential_store(&directory, &id, reference))
        .transpose()?
        .flatten();
    if let Some(store) = &new_store {
        store.save(
            &SmtpCredentialBinding::for_account(&id, &validation),
            &input.settings.password,
        )?;
    }
    let result =
        database.save_sender_account(current.revision, &id, &input.name, &settings, new_reference);
    let catalog = match result {
        Ok(catalog) => catalog,
        Err(error) => {
            // 新文件尚未被目录引用；清理失败也不改变旧账号，但必须明确残留密文而不是谎称跨文件原子性。
            if new_store
                .as_ref()
                .is_some_and(|store| store.clear().is_err())
            {
                return Err(AppError::new(
                    "sender_account_partial_save",
                    "账号设置未改变，但新建的未使用加密文件清理失败。请检查数据目录权限后重试。",
                ));
            }
            return Err(error);
        }
    };
    invalidate_preparation(state);
    if new_store.is_some() {
        if let Some(old) = existing {
            // 兼容迁移后明确保存新密码才建立独立新文件；旧默认文件保留供回退，不再参与新目录认证。
            if matches!(
                old.credential_reference(),
                SenderCredentialReference::Named { .. }
            ) && old
                .credential_store(&directory)?
                .is_some_and(|store| store.clear().is_err())
            {
                return Err(AppError::new("sender_account_partial_save","新账号设置与授权码已保存，但旧加密文件未能清理。请刷新账号列表并检查数据目录权限。"));
            }
        }
    }
    view_for_catalog(state, catalog)
}

fn remove_account(state: &AppState, id: &str) -> AppResult<MailAccountsView> {
    let database = state.database()?;
    let current = database.sender_accounts()?;
    let account = current
        .accounts
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(account_missing)?;
    let store = account.credential_store(&data_directory(state)?)?;
    let catalog = database.remove_sender_account(current.revision, id)?;
    invalidate_preparation(state);
    if store.is_some_and(|store| store.clear().is_err()) {
        return Err(AppError::new(
            "sender_account_partial_remove",
            "账号已移除且发送确认已失效，但加密授权码文件未能删除。请检查数据目录权限。",
        ));
    }
    view_for_catalog(state, catalog)
}

fn clear_account_password(state: &AppState, id: &str) -> AppResult<MailAccountsView> {
    let database = state.database()?;
    let current = database.sender_accounts()?;
    let account = current
        .accounts
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(account_missing)?;
    let store = account.credential_store(&data_directory(state)?)?;
    let catalog = database.save_sender_account(
        current.revision,
        id,
        &account.name,
        &account.settings,
        Some(SenderCredentialReference::Missing),
    )?;
    invalidate_preparation(state);
    if store.is_some_and(|store| store.clear().is_err()) {
        return Err(AppError::new(
            "sender_account_partial_clear",
            "该账号已停用保存的授权码且发送确认已失效，但原加密文件未能删除。请检查数据目录权限。",
        ));
    }
    view_for_catalog(state, catalog)
}

fn selected_accounts<'a>(
    catalog: &'a SenderAccountCatalog,
    ids: &[String],
) -> AppResult<Vec<&'a SenderAccount>> {
    let unique: HashSet<_> = ids.iter().collect();
    if ids.is_empty()
        || unique.len() != ids.len()
        || ids
            .iter()
            .any(|id| !catalog.accounts.iter().any(|account| &account.id == id))
    {
        return Err(AppError::new(
            "sender_account_selection",
            "请选择仍在目录中的发件账号，不能重复选择同一账号。",
        ));
    }
    Ok(catalog
        .accounts
        .iter()
        .filter(|account| unique.contains(&account.id))
        .collect())
}

fn resolve_account_password(
    account: &SenderAccount,
    directory: &std::path::Path,
    settings: &mut MailSettings,
) -> AppResult<()> {
    // 提交身份与目录公开身份都要匹配，防止旧接口用首账号凭据认证另一服务器或发件人。
    if SmtpCredentialBinding::from_settings(settings)
        != SmtpCredentialBinding::from_settings(&account.settings.with_password(String::new()))
    {
        return Err(AppError::new(
            "smtp_password_mismatch",
            "当前发件配置与保存账号不匹配，请重新选择或保存账号。",
        ));
    }
    let lookup = account
        .credential_store(directory)?
        .map(|store| store.load(&account.credential_binding()))
        .transpose()?
        .unwrap_or(SmtpCredentialLookup::Missing);
    match lookup {
        SmtpCredentialLookup::Password(password) => settings.password = password.to_string(),
        SmtpCredentialLookup::Missing => return Err(missing_password()),
        SmtpCredentialLookup::Mismatch => {
            return Err(AppError::new(
                "smtp_password_mismatch",
                "账号的服务器或发件身份已改变，请重新输入并保存该账号授权码。",
            ))
        }
    }
    Ok(())
}

fn view_for_catalog(
    state: &AppState,
    catalog: SenderAccountCatalog,
) -> AppResult<MailAccountsView> {
    let directory = data_directory(state)?;
    let accounts = catalog
        .accounts
        .into_iter()
        .map(|account| {
            let lookup = account.credential_store(&directory).and_then(|store| {
                store
                    .map(|store| store.load(&account.credential_binding()))
                    .transpose()
            });
            let (password_saved, password_status, password_message) = match lookup {
                Ok(Some(SmtpCredentialLookup::Password(_))) => {
                    (true, "saved", "授权码已在本机加密保存，留空可继续使用。")
                }
                Ok(Some(SmtpCredentialLookup::Mismatch)) => (
                    false,
                    "mismatch",
                    "当前服务器或发件身份与已保存授权码不匹配，请输入新授权码后保存。",
                ),
                Ok(Some(SmtpCredentialLookup::Missing)) | Ok(None) => {
                    (false, "missing", "尚未保存该账号授权码，请输入后保存。")
                }
                Err(_) => (
                    false,
                    "unavailable",
                    "已保存授权码无法读取或解密，请重新输入并保存，或清除授权码。",
                ),
            };
            MailAccountView {
                id: account.id,
                name: account.name,
                settings: account.settings,
                password_saved,
                password_status,
                password_message,
            }
        })
        .collect();
    Ok(MailAccountsView {
        revision: catalog.revision,
        accounts,
    })
}
fn data_directory(state: &AppState) -> AppResult<PathBuf> {
    state
        .database()?
        .path()
        .parent()
        .map(|path| path.to_owned())
        .ok_or_else(|| AppError::new("sender_accounts_storage", "发件账号数据目录不可用。"))
}
fn invalidate_preparation(state: &AppState) {
    *state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = None;
}
fn missing_password() -> AppError {
    AppError::new(
        "missing_smtp_credentials",
        "参与发送的账号尚未保存可用授权码，请到发件账号中输入并保存。",
    )
}
fn account_missing() -> AppError {
    AppError::new(
        "sender_account_missing",
        "该发件账号已不存在，请刷新账号列表。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use member_core::database::Database;

    fn settings(number: usize, password: &str) -> MailSettings {
        MailSettings {
            host: "smtp.example.test".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: format!("sender{number}@example.test"),
            password: password.into(),
            from_address: format!("sender{number}@example.test"),
            from_name: "合成发件人".into(),
        }
    }
    fn fixture() -> (tempfile::TempDir, AppState) {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::open(folder.path().join("data.sqlite3")).unwrap();
        let state = AppState::new();
        *state.database.lock().unwrap() = Some(database);
        (folder, state)
    }
    fn save(
        state: &AppState,
        id: Option<String>,
        number: usize,
        password: &str,
    ) -> MailAccountsView {
        save_account(
            state,
            MailAccountInput {
                id,
                name: format!("合成账号{number}"),
                settings: settings(number, password),
            },
        )
        .unwrap()
    }

    /// 顺序只取目录而不是页面输入；预检元信息不要求密码，不参与人数的账号可以保持未配置授权码。
    #[test]
    fn selection_metadata_is_ordered_and_never_requires_unused_passwords() {
        let (_folder, state) = fixture();
        let first = save(&state, None, 1, "");
        let first_id = first.accounts[0].id.clone();
        let second = save(&state, None, 2, "");
        let second_id = second.accounts[1].id.clone();
        let selected =
            selected_account_metadata(&state, &[second_id.clone(), first_id.clone()]).unwrap();
        assert_eq!(selected.accounts[0].id, first_id);
        assert_eq!(selected.accounts[1].id, second_id);
        assert_eq!(selected.accounts[0].from_address, "sender1@example.test");
        assert!(selected_account_metadata(&state, &[]).is_err());
        assert!(selected_account_metadata(&state, &[first_id.clone(), first_id]).is_err());
        assert!(selected_account_metadata(&state, &["unknown".into()]).is_err());
        assert_eq!(second.accounts[1].password_status, "missing");
    }

    /// 每个密码版本独立DPAPI绑定账号，页面与SQLite始终只有公开字段和状态，不含合成授权码。
    #[cfg(windows)]
    #[test]
    fn independent_passwords_are_private_bound_and_preserved_on_blank_update() {
        let (folder, state) = fixture();
        let secret = "synthetic-account-one-secret";
        let first = save(&state, None, 1, secret);
        let first_id = first.accounts[0].id.clone();
        let second = save(&state, None, 2, "synthetic-account-two-secret");
        let second_id = second.accounts[1].id.clone();
        let public = serde_json::to_string(&second).unwrap();
        assert!(!public.contains("secret"));
        assert!(!public.contains("credential"));
        assert!(public.contains("passwordSaved"));
        let stored = state
            .database()
            .unwrap()
            .setting("smtp_sender_accounts_v1")
            .unwrap()
            .unwrap();
        assert!(!stored.contains(secret));
        assert!(!stored.contains("password"));
        let resolved =
            resolve_selected_accounts(&state, &[second_id.clone(), first_id.clone()]).unwrap();
        assert_eq!(resolved.accounts[0].id, first_id);
        assert_eq!(resolved.accounts[0].settings.password, secret);
        assert_eq!(
            resolved.accounts[1].settings.password,
            "synthetic-account-two-secret"
        );
        let unchanged = save(&state, Some(first_id.clone()), 1, "");
        assert!(unchanged.accounts[0].password_saved);
        let first_account = state
            .database()
            .unwrap()
            .sender_accounts()
            .unwrap()
            .accounts[0]
            .clone();
        let second_account = state
            .database()
            .unwrap()
            .sender_accounts()
            .unwrap()
            .accounts[1]
            .clone();
        let first_reference = first_account.credential_reference().clone();
        let first_path = match &first_reference {
            SenderCredentialReference::Named { key } => folder
                .path()
                .join("smtp-accounts")
                .join(format!("{}-{key}.bin", first_account.id)),
            _ => panic!("new credential must have named reference"),
        };
        let second_path = match second_account.credential_reference() {
            SenderCredentialReference::Named { key } => folder
                .path()
                .join("smtp-accounts")
                .join(format!("{}-{key}.bin", second_account.id)),
            _ => panic!("new credential must have named reference"),
        };
        let bytes = std::fs::read(&first_path).unwrap();
        assert!(!bytes
            .windows(secret.len())
            .any(|part| part == secret.as_bytes()));
        std::fs::copy(&first_path, &second_path).unwrap();
        assert_eq!(
            resolve_selected_accounts(&state, &[second_id])
                .err()
                .unwrap()
                .code,
            "smtp_password_mismatch"
        );
        // 仅参与的首账号仍可解析；没有因为第二个账号损坏而阻止其已固定的发送段。
        assert_eq!(
            resolve_selected_accounts(&state, &[first_id])
                .unwrap()
                .accounts[0]
                .settings
                .password,
            secret
        );
    }

    #[cfg(windows)]
    #[test]
    fn database_rejected_update_discards_new_cipher_and_keeps_old_account_usable() {
        let (folder, state) = fixture();
        let first = save(&state, None, 1, "synthetic-old-secret");
        let id = first.accounts[0].id.clone();
        let before = save(&state, None, 2, "synthetic-second-secret");
        let directory = folder.path().join("smtp-accounts");
        assert_eq!(
            save_account(
                &state,
                MailAccountInput {
                    id: Some(id.clone()),
                    name: "失败更新".into(),
                    settings: settings(2, "synthetic-new-secret")
                }
            )
            .err()
            .unwrap()
            .code,
            "sender_account_duplicate"
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
        let after = account_catalog_view(&state).unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.accounts[0].name, first.accounts[0].name);
        assert_eq!(
            resolve_selected_accounts(&state, &[id]).unwrap().accounts[0]
                .settings
                .password,
            "synthetic-old-secret"
        );
    }

    #[cfg(windows)]
    #[test]
    fn clear_and_remove_affect_only_the_selected_account_and_increment_revision() {
        let (_folder, state) = fixture();
        let first = save(&state, None, 1, "synthetic-one");
        let id = first.accounts[0].id.clone();
        let second = save(&state, None, 2, "synthetic-two");
        let second_id = second.accounts[1].id.clone();
        let cleared = clear_account_password(&state, &id).unwrap();
        assert!(cleared.revision > second.revision);
        assert!(!cleared.accounts[0].password_saved);
        assert!(cleared.accounts[1].password_saved);
        assert_eq!(
            resolve_selected_accounts(&state, std::slice::from_ref(&id))
                .err()
                .unwrap()
                .code,
            "missing_smtp_credentials"
        );
        let removed = remove_account(&state, &second_id).unwrap();
        assert!(removed.revision > cleared.revision);
        assert_eq!(removed.accounts.len(), 1);
        assert!(removed
            .accounts
            .iter()
            .all(|account| account.id != second_id));
        remove_account(&state, &id).unwrap();
        assert!(account_catalog_view(&state).unwrap().accounts.is_empty());
    }

    /// 文件删除失败时目录变更已经提交，必须给出部分完成错误；残留文件绝不能继续作为可用授权码解析。
    /// 合成测试把自建密文文件替换为同名目录以稳定触发删除失败，不修改任何真实账号数据。
    #[cfg(windows)]
    #[test]
    fn partial_file_cleanup_reports_committed_directory_change_without_reusing_cipher() {
        for remove in [false, true] {
            let (folder, state) = fixture();
            let before = save(&state, None, 1, "synthetic-cleanup-secret");
            let id = before.accounts[0].id.clone();
            let catalog = state.database().unwrap().sender_accounts().unwrap();
            let path = match catalog.accounts[0].credential_reference() {
                SenderCredentialReference::Named { key } => folder
                    .path()
                    .join("smtp-accounts")
                    .join(format!("{id}-{key}.bin")),
                _ => panic!("new credential must have named reference"),
            };
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            let error = if remove {
                remove_account(&state, &id).err().unwrap()
            } else {
                clear_account_password(&state, &id).err().unwrap()
            };
            assert_eq!(
                error.code,
                if remove {
                    "sender_account_partial_remove"
                } else {
                    "sender_account_partial_clear"
                }
            );
            let after = account_catalog_view(&state).unwrap();
            assert!(after.revision > before.revision);
            assert!(path.is_dir());
            if remove {
                assert!(after.accounts.is_empty());
            } else {
                assert_eq!(after.accounts.len(), 1);
                assert!(!after.accounts[0].password_saved);
                assert_eq!(after.accounts[0].password_status, "missing");
            }
            assert!(resolve_selected_accounts(&state, &[id]).is_err());
        }
    }

    /// 遗留默认文件不在迁移时移动或重加密；旧接口映射首账号，删光后不会重新读取旧公开设置。
    #[cfg(windows)]
    #[test]
    fn legacy_configuration_uses_old_binding_and_new_save_moves_to_account_scope() {
        let (folder, state) = fixture();
        let old = settings(1, "synthetic-legacy-secret");
        let public = SenderPublicSettings::from_mail(&old);
        state
            .database()
            .unwrap()
            .set_setting("smtp_public", &serde_json::to_string(&public).unwrap())
            .unwrap();
        state
            .smtp_credential_store()
            .unwrap()
            .save(&SmtpCredentialBinding::from_settings(&old), &old.password)
            .unwrap();
        let before = std::fs::read(folder.path().join("smtp-credential.bin")).unwrap();
        let view = account_catalog_view(&state).unwrap();
        assert_eq!(view.accounts[0].id, "legacy-default");
        assert!(view.accounts[0].password_saved);
        assert_eq!(
            std::fs::read(folder.path().join("smtp-credential.bin")).unwrap(),
            before
        );
        let mut resolved = first_account_settings(&state).unwrap().unwrap();
        legacy_resolve_password(&state, &mut resolved).unwrap();
        assert_eq!(resolved.password, old.password);
        save_legacy_account(&state, &settings(1, "synthetic-new-secret")).unwrap();
        assert_eq!(
            std::fs::read(folder.path().join("smtp-credential.bin")).unwrap(),
            before
        );
        assert_eq!(
            resolve_selected_accounts(&state, &["legacy-default".into()])
                .unwrap()
                .accounts[0]
                .settings
                .password,
            "synthetic-new-secret"
        );
        clear_legacy_account_password(&state).unwrap();
        assert!(!account_catalog_view(&state).unwrap().accounts[0].password_saved);
    }
}
