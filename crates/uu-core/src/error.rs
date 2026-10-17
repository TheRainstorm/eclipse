use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("configuration is unusable: {0}")]
    Config(String),
    #[error("signaling request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("service rejected the request: {code} {message}")]
    Service { code: i32, message: String },
    #[error("invalid session transition: {from} -> {to}")]
    BadTransition { from: &'static str, to: &'static str },
    #[error("not authenticated")]
    Unauthenticated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
