use thiserror::Error;
#[derive(Debug,Error)]
pub enum CoreError {
    #[error("{}",uu_i18n::text("指定的连接签名文件不存在，请检查 UU_SIGNING_KEY_FILE 的路径；未使用默认签名配置"))]
    SigningKeyMissing,
    #[error("{}",uu_i18n::format!("本地账号配置不可用：{0}。原文件已保留，请检查配置或恢复备份后重试",.0))]
    AccountState(&'static str),
    #[error("configuration is unusable: {0}")]
    Config(String),
    #[error("signaling request failed: {0}")]
    Transport(#[from]reqwest::Error),
    #[error("service rejected the request: {code} {message}")]
    Service {code:i32,message:String},
    #[error("not authenticated")]
    Unauthenticated,
    #[error(transparent)]
    Io(#[from]std::io::Error),
    #[error(transparent)]
    Engine(#[from]uu_core::CoreError),
}
