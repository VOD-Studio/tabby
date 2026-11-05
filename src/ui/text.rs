use crate::api::error::ApiError;

/// One-line, localized summary of a failed request, for toasts and error pages.
pub fn describe(err: &ApiError) -> String {
    match err {
        ApiError::Unauthorized => "Secret 错误，请用 --secret 或修改配置".to_owned(),
        ApiError::NotFound => "接口不存在（404）".to_owned(),
        ApiError::Http { status, .. } => format!("控制器返回错误（HTTP {status}）"),
        ApiError::Timeout => "请求超时".to_owned(),
        ApiError::Connect(_) => "无法连接控制器".to_owned(),
        ApiError::Decode(_) => "控制器返回了无法解析的数据".to_owned(),
    }
}

/// The untranslated cause behind [`describe`], when it adds information.
pub fn detail(err: &ApiError) -> Option<&str> {
    match err {
        ApiError::Connect(cause) | ApiError::Decode(cause) => Some(cause),
        ApiError::Http { message, .. } if !message.is_empty() => Some(message),
        ApiError::Http { .. } | ApiError::Unauthorized | ApiError::NotFound | ApiError::Timeout => {
            None
        }
    }
}
