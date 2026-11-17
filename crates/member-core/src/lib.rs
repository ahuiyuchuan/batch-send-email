//! 来源读取、结果核验和安全导出的共同边界。作者：ahui。
//! 页面只接收明确的业务数据；QQ 会话与 SMTP 授权码由宿主按独立用途加密保存。

pub mod database;
pub mod diagnostics;
pub mod export;
mod group_cache;
pub mod mail;
mod manual_exclusions;
pub mod models;
pub mod multi_group;
mod protected_data;
pub mod providers;
pub mod sender_accounts;
pub mod sender_cooldowns;
pub mod smtp_credential_store;
mod snapshot_restoration;
pub mod templates;
pub mod verify;
