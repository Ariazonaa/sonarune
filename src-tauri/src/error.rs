use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("network error: {0}")]
    Network(reqwest::Error),
    #[error("server returned {status}: {message}")]
    Server { status: u16, message: String },
    #[error("not connected to a server")]
    NotConnected,
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("credential store error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("{0}")]
    Other(String),
}

impl From<reqwest::Error> for AppError {
    /// Strip the URL before the error can reach a log or the UI banner.
    ///
    /// `reqwest`'s Display includes the request URL, and our URLs carry values
    /// that must not be shown or logged — the Quick Connect secret, `api_key`,
    /// `userId`, `deviceId`. The host is already known to the user, so nothing
    /// diagnostic is lost.
    fn from(err: reqwest::Error) -> Self {
        AppError::Network(err.without_url())
    }
}

// Commands return Result<T, AppError>; the frontend receives the display string.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
