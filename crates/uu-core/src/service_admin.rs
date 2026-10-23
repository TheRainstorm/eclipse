//! Local administrator protocol for the installed background host. This is
//! never accepted on a network listener or the native streaming control channel.
use crate::{
    device_auth::PairingMethod,
    device_trust::{Permissions, PublicDevice},
    host_admin,
};
use anyhow::{ensure, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_MESSAGE: usize = 256 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Approve {
        id: String,
        code: String,
        permissions: Permissions,
    },
    Reject {
        id: String,
    },
    Admin {
        request: host_admin::Request,
    },
    Restart,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    pub id: String,
    pub device: PublicDevice,
    pub method: PairingMethod,
    pub waiting: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Status {
        endpoints: Vec<(String, String)>,
        pending: Vec<Pairing>,
        snapshot: host_admin::Snapshot,
    },
    Admin {
        response: host_admin::Response,
    },
    Applied,
    Error {
        message: String,
    },
}
pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let length = stream.read_u32().await? as usize;
    ensure!(length > 0 && length <= MAX_MESSAGE, "后台服务消息长度无效");
    let mut bytes = zeroize::Zeroizing::new(vec![0u8; length]);
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_MESSAGE,
        "后台服务消息过大"
    );
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(windows)]
pub mod windows;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn framed_administration_rejects_large_truncated_and_unknown_messages() {
        for bytes in [vec![0, 4, 0, 1], vec![0, 0, 0, 3, b'{'], vec![0, 0, 0, 0]] {
            let mut stream = bytes.as_slice();
            assert!(read::<Request>(&mut stream).await.is_err());
        }
        assert!(
            serde_json::from_str::<Request>(r#"{"action":"run_command","command":"ignored"}"#)
                .is_err()
        );
        let mut output = Vec::new();
        write(&mut output, &Request::Status).await.unwrap();
        assert!(matches!(
            read::<Request>(&mut output.as_slice()).await.unwrap(),
            Request::Status
        ));
    }
}
