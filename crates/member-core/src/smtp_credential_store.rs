//! 发件授权码的独立本机加密存储。作者：ahui。
//! 授权码绑定具体 SMTP 连接及发件身份；没有有效期，不随 QQ 退出删除，只由明确操作更新。

use crate::{
    mail::MailSettings,
    models::{AppError, AppResult},
    protected_data::{self, Purpose},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8] = b"SMTPSECRET1\0";
const MAX_FILE_BYTES: u64 = 32 * 1024;
const MAX_PASSWORD_BYTES: usize = 4096;

/// 使用授权码的公开身份；显示姓名不参与认证，其他任一字段变化均不得复用旧授权码。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SmtpCredentialBinding {
    /// 多账号凭据同时绑定目录账号标识；旧单账号文件缺少此字段时仍按原格式读取。
    /// None 不写入旧文件格式，保持 smtp-credential.bin 对既有程序的兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    host: String,
    port: u16,
    tls_mode: String,
    username: String,
    from_address: String,
}

impl SmtpCredentialBinding {
    /// 与 MailSender 的连接规则一致：域名大小写和首尾空白不影响连接；账号保持原样。
    /// 发件地址只去除调用方已经允许的首尾空白，不改写邮箱本地部分的大小写。
    pub fn from_settings(settings: &MailSettings) -> Self {
        Self {
            account_id: None,
            host: settings.host.trim().to_ascii_lowercase(),
            port: settings.port,
            tls_mode: settings.tls_mode.clone(),
            username: settings.username.clone(),
            from_address: settings.from_address.trim().to_owned(),
        }
    }

    /// 为多账号独立文件创建绑定；即使两个配置相同，交换账号文件也不能借用另一目录身份的授权码。
    /// account_id 必须是宿主生成且已由发件目录验证的稳定标识，不能使用用户输入的文件路径。
    pub fn for_account(account_id: &str, settings: &MailSettings) -> Self {
        let mut binding = Self::from_settings(settings);
        binding.account_id = Some(account_id.to_owned());
        binding
    }
}

/// 此类型没有序列化或 Debug；即使凭据存在，也只能由宿主在内存中用于 SMTP 认证。
pub enum SmtpCredentialLookup {
    Missing,
    Mismatch,
    Password(Zeroizing<String>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCredential {
    version: u32,
    binding: SmtpCredentialBinding,
    password: String,
}

impl Drop for SavedCredential {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// 固定路径由宿主提供；文件独立于 SQLite 和 QQ 会话，绝不接受前端提交的文件路径。
#[derive(Clone)]
pub struct SmtpCredentialStore {
    path: PathBuf,
}

impl SmtpCredentialStore {
    /// 构建轻量句柄，不访问文件；path 必须来自宿主固定的数据目录与已验证的文件引用。
    /// 旧默认账号使用 smtp-credential.bin，新账号使用 smtp-accounts 内独立随机版本文件。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 读取匹配连接身份的授权码；缺失与不匹配分开处理，损坏、错用户或读取失败明确报错。
    /// 明文仅在当前调用的 Zeroizing 容器中存活；调用方不能返回页面或记录到日志。
    pub fn load(&self, binding: &SmtpCredentialBinding) -> AppResult<SmtpCredentialLookup> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SmtpCredentialLookup::Missing)
            }
            Err(_) => return Err(unavailable_error()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable_error())?;
        if bytes.len() as u64 > MAX_FILE_BYTES || !bytes.starts_with(MAGIC) {
            return Err(unavailable_error());
        }
        let plain = Zeroizing::new(
            protected_data::unprotect(&bytes[MAGIC.len()..], Purpose::SmtpCredential)
                .map_err(|_| unavailable_error())?,
        );
        let mut saved: SavedCredential =
            serde_json::from_slice(&plain).map_err(|_| unavailable_error())?;
        if saved.version != 1
            || saved.password.is_empty()
            || saved.password.len() > MAX_PASSWORD_BYTES
        {
            return Err(unavailable_error());
        }
        if saved.binding != *binding {
            return Ok(SmtpCredentialLookup::Mismatch);
        }
        Ok(SmtpCredentialLookup::Password(Zeroizing::new(
            std::mem::take(&mut saved.password),
        )))
    }

    /// 校验后先加密，再以同目录临时文件原子替换；失败保留原文件且绝不明文降级。
    /// 公共配置另由 SQLite 保存，宿主须明确报告后续数据库失败，不能宣称跨文件原子事务。
    pub fn save(&self, binding: &SmtpCredentialBinding, password: &str) -> AppResult<()> {
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return Err(AppError::new(
                "invalid_smtp_credentials",
                "授权码须为 1 到 4096 字节，请重新输入后保存。",
            ));
        }
        let saved = SavedCredential {
            version: 1,
            binding: binding.clone(),
            password: password.to_owned(),
        };
        let plain = Zeroizing::new(serde_json::to_vec(&saved).map_err(|_| storage_error())?);
        if plain.len() > MAX_FILE_BYTES as usize / 2 {
            return Err(storage_error());
        }
        let cipher = protected_data::protect(&plain, Purpose::SmtpCredential)
            .map_err(|_| storage_error())?;
        let parent = self.path.parent().ok_or_else(storage_error)?;
        fs::create_dir_all(parent).map_err(|_| storage_error())?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| storage_error())?;
        file.write_all(MAGIC)
            .and_then(|_| file.write_all(&cipher))
            .and_then(|_| file.flush())
            .map_err(|_| storage_error())?;
        file.as_file().sync_all().map_err(|_| storage_error())?;
        file.persist(&self.path).map_err(|_| storage_error())?;
        Ok(())
    }

    /// 用户明确清除已保存授权码；文件缺失是成功，删除失败必须告知仍可能恢复。
    pub fn clear(&self) -> AppResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AppError::new(
                "smtp_password_clear",
                "未能清除已保存授权码，请检查本机数据目录权限后重试。",
            )),
        }
    }
}

fn storage_error() -> AppError {
    AppError::new(
        "smtp_password_storage",
        "无法加密保存授权码，请确认使用当前 Windows 用户并检查本机数据目录权限后重试。",
    )
}

fn unavailable_error() -> AppError {
    AppError::new(
        "smtp_password_unavailable",
        "已保存授权码无法读取或解密，请重新输入并保存，或先清除已保存授权码。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> MailSettings {
        MailSettings {
            host: "smtp.example.test".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: "sender@example.test".into(),
            password: String::new(),
            from_address: "sender@example.test".into(),
            from_name: "隔离测试".into(),
        }
    }

    #[test]
    fn missing_corrupt_oversized_and_failed_clear_are_distinct() {
        let folder = tempfile::tempdir().unwrap();
        let store = SmtpCredentialStore::new(folder.path().join("credential.bin"));
        let binding = SmtpCredentialBinding::from_settings(&settings());
        assert!(matches!(
            store.load(&binding).unwrap(),
            SmtpCredentialLookup::Missing
        ));
        fs::write(&store.path, b"invalid-secret-format").unwrap();
        assert_eq!(
            store.load(&binding).err().unwrap().code,
            "smtp_password_unavailable"
        );
        fs::write(&store.path, vec![0u8; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(store.load(&binding).is_err());
        store.clear().unwrap();
        store.clear().unwrap();
        let blocked = SmtpCredentialStore::new(folder.path().to_owned());
        assert_eq!(blocked.clear().unwrap_err().code, "smtp_password_clear");
    }

    #[cfg(windows)]
    #[test]
    fn encrypted_roundtrip_is_bound_to_every_connection_field_and_can_be_replaced() {
        let folder = tempfile::tempdir().unwrap();
        let store = SmtpCredentialStore::new(folder.path().join("credential.bin"));
        let original = settings();
        let binding = SmtpCredentialBinding::from_settings(&original);
        let secret = "synthetic-only-password-never-a-real-account";
        store.save(&binding, secret).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        assert!(!bytes
            .windows(secret.len())
            .any(|part| part == secret.as_bytes()));
        match store.load(&binding).unwrap() {
            SmtpCredentialLookup::Password(password) => assert_eq!(password.as_str(), secret),
            _ => panic!("saved credential must be available for the same identity"),
        }
        for field in ["host", "port", "tls", "username", "from"] {
            let mut changed = original.clone();
            match field {
                "host" => changed.host = "other.example.test".into(),
                "port" => changed.port = 587,
                "tls" => changed.tls_mode = "starttls".into(),
                "username" => changed.username = "other@example.test".into(),
                _ => changed.from_address = "other@example.test".into(),
            }
            assert!(matches!(
                store
                    .load(&SmtpCredentialBinding::from_settings(&changed))
                    .unwrap(),
                SmtpCredentialLookup::Mismatch
            ));
        }
        let mut display_only = original;
        display_only.from_name = "更改显示姓名".into();
        display_only.host = " SMTP.EXAMPLE.TEST ".into();
        assert!(matches!(
            store
                .load(&SmtpCredentialBinding::from_settings(&display_only))
                .unwrap(),
            SmtpCredentialLookup::Password(_)
        ));
        // 非法更新不能覆盖仍可用的旧凭据；合法更新则原子替换。
        assert!(store.save(&binding, "").is_err());
        store.save(&binding, "synthetic-replacement").unwrap();
        match store.load(&binding).unwrap() {
            SmtpCredentialLookup::Password(password) => {
                assert_eq!(password.as_str(), "synthetic-replacement")
            }
            _ => panic!("replacement must remain readable"),
        }
        let mut damaged = fs::read(&store.path).unwrap();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        fs::write(&store.path, damaged).unwrap();
        assert_eq!(
            store.load(&binding).err().unwrap().code,
            "smtp_password_unavailable"
        );
        store.clear().unwrap();
        assert!(matches!(
            store.load(&binding).unwrap(),
            SmtpCredentialLookup::Missing
        ));
    }

    /// 独立账号标识是密文绑定的一部分；即使所有SMTP公开字段相同，互换文件也不得跨账号复用。
    #[cfg(windows)]
    #[test]
    fn account_binding_rejects_another_id_with_identical_smtp_settings() {
        let folder = tempfile::tempdir().unwrap();
        let store = SmtpCredentialStore::new(folder.path().join("credential.bin"));
        let settings = settings();
        let first =
            SmtpCredentialBinding::for_account("00000000000000000000000000000001", &settings);
        let second =
            SmtpCredentialBinding::for_account("00000000000000000000000000000002", &settings);
        store.save(&first, "synthetic-independent-secret").unwrap();
        assert!(matches!(
            store.load(&first).unwrap(),
            SmtpCredentialLookup::Password(_)
        ));
        for other in [second, SmtpCredentialBinding::from_settings(&settings)] {
            assert!(matches!(
                store.load(&other).unwrap(),
                SmtpCredentialLookup::Mismatch
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn failed_atomic_write_reports_error_and_leaves_existing_directory_intact() {
        let folder = tempfile::tempdir().unwrap();
        let store = SmtpCredentialStore::new(folder.path().to_owned());
        let binding = SmtpCredentialBinding::from_settings(&settings());
        assert_eq!(
            store.save(&binding, "synthetic-password").unwrap_err().code,
            "smtp_password_storage"
        );
        assert!(folder.path().is_dir());
    }
}
