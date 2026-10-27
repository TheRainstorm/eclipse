use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("configuration is unusable: {0}")]
    Config(String),
    #[error("invalid session transition: {from} -> {to}")]
    BadTransition { from: &'static str, to: &'static str },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
