use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("指定的连接签名文件不存在，请检查 UU_SIGNING_KEY_FILE 的路径；未使用默认签名配置")]
    SigningKeyMissing,
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
