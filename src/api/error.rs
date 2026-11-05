use std::error::Error;

use reqwest::StatusCode;
use serde::Deserialize;

/// Failure of a request to the External Controller.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// HTTP 401: wrong or missing secret.
    #[error("unauthorized: the controller rejected the secret")]
    Unauthorized,
    /// HTTP 404: unknown path, proxy, group or provider.
    #[error("not found")]
    NotFound,
    /// Any other non-success status, with the controller's own message.
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("request timed out")]
    Timeout,
    #[error("cannot reach the controller: {0}")]
    Connect(String),
    #[error("unexpected response: {0}")]
    Decode(String),
}

/// Error body the controller sends: `{"message": "..."}`.
#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

impl ApiError {
    /// Maps a non-success response to an error. `body` is only used for the
    /// message of unmapped statuses.
    pub fn from_status(status: StatusCode, body: &str) -> Self {
        match status {
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::NOT_FOUND => Self::NotFound,
            _ => {
                let body = body.trim();
                let message = serde_json::from_str::<ErrorBody>(body)
                    .map(|parsed| parsed.message)
                    .ok()
                    .filter(|message| !message.is_empty())
                    .or_else(|| (!body.is_empty()).then(|| body.to_owned()))
                    .or_else(|| status.canonical_reason().map(str::to_owned))
                    .unwrap_or_default();
                Self::Http {
                    status: status.as_u16(),
                    message,
                }
            }
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout
        } else if err.is_decode() {
            Self::Decode(error_chain(&err))
        } else {
            Self::Connect(error_chain(&err))
        }
    }
}

/// `err: cause: root cause`; reqwest's own message omits the cause
/// (e.g. "Connection refused").
fn error_chain(err: &dyn Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
