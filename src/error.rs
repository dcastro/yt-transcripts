//! The errors of a transcript fetch, and their classes.

use std::time::Duration;

/// An error of one transcript fetch. Each variant has a different action (PLAN.md, section 7.5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FetchError {
    /// The video has no caption tracks. Do not try again.
    #[error("the video has no transcript")]
    NoTranscript,
    /// The video is private, removed, members-only or age-restricted. Do not try again.
    #[error("the video is not available ({0})")]
    Unavailable(String),
    /// A timeout, a connection error or an HTTP 5xx status. Try again later.
    #[error("temporary error: {0}")]
    Transient(String),
    /// HTTP 429. This can be the start of a block.
    #[error("HTTP 429 Too Many Requests")]
    RateLimited { retry_after: Option<Duration> },
    /// A clear block signal. Stop all requests.
    #[error("YouTube blocked the requests: {0}")]
    Blocked(String),
    /// A response with an unexpected format. YouTube possibly changed its site.
    #[error("unexpected response: {message}")]
    Unexpected {
        message: String,
        body: Option<String>,
    },
}

impl FetchError {
    pub(crate) fn unexpected(message: impl Into<String>, body: Option<&str>) -> Self {
        Self::Unexpected {
            message: message.into(),
            body: body.map(str::to_owned),
        }
    }
}
