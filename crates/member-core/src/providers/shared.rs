//! 网页请求的 JSON、HTTP 边界；远端响应正文与凭据不得进入错误消息。
//! 作者：ahui。

use crate::models::{AppError, AppResult};
use reqwest::{Response, StatusCode};
use serde_json::Value;
use std::time::Duration;

/// 单次请求上限；外层任务还可以施加总超时和用户取消。
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// 防止错误页面或异常服务无限占用内存，正常 QQ 群成员响应远小于此值。
pub(super) const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

/// 接受字符串或整数类型的标识；不转换浮点数，避免把损坏的号码静默四舍五入。
pub(super) fn identifier(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) if number.is_u64() || number.is_i64() => number.to_string(),
        _ => String::new(),
    }
}

/// 业务标识只允许非空 ASCII 数字，限制长度以拒绝异常输入；不假设 QQ 号固定长度。
pub(super) fn is_numeric_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|character| character.is_ascii_digit())
        && value.bytes().any(|character| character != b'0')
}

/// 读取服务端真正提供的非负人数；字段缺失或类型异常时保留 None，不能回填数组长度。
pub(super) fn count(value: Option<&Value>) -> Option<usize> {
    match value {
        Some(Value::Number(number)) => number.as_u64().and_then(|n| usize::try_from(n).ok()),
        Some(Value::String(text)) if text.bytes().all(|c| c.is_ascii_digit()) => text.parse().ok(),
        _ => None,
    }
}

/// 昵称和名片为空或未知时显示空文本；数字昵称可以保留其原始十进制表示。
pub(super) fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// 读取有上限的响应，避免把远端 HTML、URL 或成员正文拼接到错误消息。
pub(super) async fn response_bytes(response: Response, maximum: usize) -> AppResult<Vec<u8>> {
    let status = response.status();
    if !status.is_success() {
        return Err(http_error(status));
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(AppError::new(
            "response_too_large",
            "服务响应过大，请稍后重试。",
        ));
    }

    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(AppError::new(
                "response_too_large",
                "服务响应过大，请稍后重试。",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 仅接受 JSON 对象；登录 HTML 与成功空响应不能伪装成空群列表。
pub(super) async fn response_json(response: Response) -> AppResult<Value> {
    let body = response_bytes(response, MAX_JSON_BYTES).await?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        AppError::new(
            "invalid_response",
            "接口没有返回有效 JSON，请重新扫码登录后重试。",
        )
    })?;
    if !value.is_object() {
        return Err(schema_error());
    }
    Ok(value)
}

/// 网络错误仅按类别说明原因，不调用 Display，以免泄露带 Cookie 参数的 URL。
pub(super) fn network_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::new("request_timeout", "连接超时，请检查网络后重试。")
    } else if error.is_connect() {
        AppError::new("connection_failed", "无法连接 QQ 服务，请检查网络后重试。")
    } else if error.is_redirect() {
        AppError::new(
            "redirect_rejected",
            "服务返回了不受支持的跳转，请重新扫码登录后重试。",
        )
    } else {
        AppError::new("network_error", "服务连接失败，请检查网络后重试。")
    }
}

/// HTTP 错误不附带响应正文；限流交回用户，避免凭据失效时循环重试。
pub(super) fn http_error(status: StatusCode) -> AppError {
    match status {
        StatusCode::UNAUTHORIZED => AppError::new("unauthorized", "登录状态无效，请重新扫码登录。"),
        StatusCode::FORBIDDEN => {
            AppError::new("forbidden", "服务拒绝访问，请检查当前账号的群成员权限。")
        }
        StatusCode::TOO_MANY_REQUESTS => {
            AppError::new("rate_limited", "请求过于频繁，请稍后再试。")
        }
        _ => AppError::new(
            "http_error",
            format!("服务暂时不可用（HTTP {}），请稍后再试。", status.as_u16()),
        ),
    }
}

/// 固定的结构错误，不把未知数据内容显示给页面或写入日志。
pub(super) fn schema_error() -> AppError {
    AppError::new(
        "invalid_response",
        "接口数据格式与当前版本不兼容，请更新程序或稍后重试。",
    )
}

/// 用户取消统一使用稳定代码，迟到结果由调用方任务标识进一步隔离。
pub(super) fn cancelled_error() -> AppError {
    AppError::new("cancelled", "本次操作已取消。")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_never_convert_fractional_numbers_or_internal_uid() {
        assert_eq!(identifier(Some(&json!(123456789))), "123456789");
        assert_eq!(identifier(Some(&json!(123.4))), "");
        assert!(!is_numeric_id("u_internal_id"));
        assert!(!is_numeric_id("00000"));
        assert!(!is_numeric_id("１２３４５"));
        assert!(is_numeric_id("123456789"));
    }

    #[test]
    fn unknown_count_remains_unknown() {
        assert_eq!(count(None), None);
        assert_eq!(count(Some(&Value::Null)), None);
        assert_eq!(count(Some(&json!(-1))), None);
        assert_eq!(count(Some(&json!(1688))), Some(1688));
        assert_eq!(count(Some(&json!("1688"))), Some(1688));
    }
}
