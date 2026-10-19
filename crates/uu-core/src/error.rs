use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("未配置连接签名密钥，请先提供本地凭据文件（UU_SIGNING_KEY_FILE）；项目不内置第三方服务密钥")]
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
