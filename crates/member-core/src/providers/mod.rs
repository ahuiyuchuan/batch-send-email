//! QQ 网页读取模块，仅返回最小成员数据，不持久化登录凭据。
//! 作者：ahui。

mod shared;
mod web;

pub use web::WebProvider;
