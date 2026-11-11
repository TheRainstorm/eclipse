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
pub mod connections;
pub mod package;

pub const MAX_MESSAGE: usize = 256 * 1024;
/// The server may have completed a non-idempotent operation before its reply
/// was lost. Callers must inspect that same owner before offering mutations.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SubmittedUnknown(pub String);
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Scoped {
        profile: String,
        request: Box<Request>,
    },
    Status,
    ConfigurationSwitch { token: String, release: bool },
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
    VirtualDisplay {
        request: crate::native_virtual_display::LocalRequest,
    },
    Trust {
        request: TrustRequest,
    },
    Firewall {
        repair: bool,
    },
    Connections {
        request: connections::Request,
    },
    Plugins {
        request: PluginRequest,
    },
    Restart,
}
impl Request {
    pub fn response_timeout(&self) -> Option<std::time::Duration> {
        if let Self::Scoped { request, .. } = self { return request.response_timeout(); }
        if self.is_mutation() { return None; }
        Some(match self {
            Self::Firewall { repair: false } => crate::firewall::OPERATION_TIMEOUT + std::time::Duration::from_secs(10),
            _ => std::time::Duration::from_secs(5),
        })
    }
    /// Queries may time out safely. An accepted mutation must settle or report
    /// an unknown result; disconnecting its frontend never cancels the work.
    pub fn is_mutation(&self) -> bool {
        if let Self::Scoped { request, .. } = self { return request.is_mutation(); }
        !matches!(
            self,
            Self::Status
                | Self::Trust {
                    request: TrustRequest::Inspect
                }
                | Self::Firewall { repair: false }
                | Self::Connections {
                    request: connections::Request::Inspect
                }
                | Self::Plugins { request: PluginRequest::Inspect }
                | Self::VirtualDisplay {
                    request: crate::native_virtual_display::LocalRequest::Manage {
                        request: crate::native_virtual_display::Request::Inspect
                    }
                }
        )
    }
    pub fn for_profile(self, profile: Option<&str>) -> Result<Self> {
        match self {
            Self::Status => Ok(Self::Status),
            Self::Scoped { profile: expected, request } => {
                ensure!(profile == Some(expected.as_str()) && !matches!(*request, Self::Scoped { .. }),
                    "This host belongs to another Eclipse configuration. Activate this configuration before managing it.");
                Ok(*request)
            },
            request => {
                ensure!(profile.is_none(), "This host uses an independent Eclipse configuration. Open its matching version to manage it.");
                Ok(request)
            },
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrustRequest {
    Inspect,
    Password {
        password: Option<zeroize::Zeroizing<String>>,
        permissions: Permissions,
    },
    Authorize {
        device: PublicDevice,
        permissions: Permissions,
    },
    Revoke {
        peer: String,
    },
}
/// Public host identity and permissions only; no private key or password record.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustSnapshot {
    pub device: PublicDevice,
    pub peers: Vec<crate::device_trust::Peer>,
    pub password_permissions: Option<Permissions>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag="action",rename_all="snake_case",deny_unknown_fields)]
pub enum PluginRequest {
    Inspect,
    Rescan,
    Save {
        id:String,
        expected:crate::plugins::PluginPreferences,
        value:crate::plugins::PluginPreferences,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInfo {
    pub id:String,
    pub name:String,
    pub version:String,
    pub enabled:bool,
    pub compatible:bool,
    pub error:String,
    pub preferences:crate::plugins::PluginPreferences,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSnapshot {
    pub directory:String,
    pub plugins:Vec<PluginInfo>,
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
    ProfileStatus {
        profile: crate::service_profile::Info,
        response: Box<Response>,
    },
    Status {
        #[serde(default)]
        ready: bool,
        #[serde(default)]
        keep_virtual_displays: Option<bool>,
        endpoints: Vec<(String, String)>,
        pending: Vec<Pairing>,
        snapshot: host_admin::Snapshot,
    },
    Admin {
        response: host_admin::Response,
    },
    VirtualDisplay {
        response: crate::native_virtual_display::Response,
    },
    Trust {
        snapshot: TrustSnapshot,
    },
    Firewall {
        message: String,
    },
    Connections {
        snapshot: connections::Snapshot,
    },
    Plugins {
        snapshot: PluginSnapshot,
    },
    SubmittedUnknown {
        message: String,
    },
    Applied,
    Error {
        message: String,
    },
}
pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let length = stream.read_u32().await? as usize;
    ensure!(
        length > 0 && length <= MAX_MESSAGE,
        "{}",
        uu_i18n::format!("后台服务消息长度无效")
    );
    let mut bytes = zeroize::Zeroizing::new(vec![0u8; length]);
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_MESSAGE,
        "{}",
        uu_i18n::format!("后台服务消息过大")
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
    #[test]
    fn firewall_queries_outlive_os_work_at_every_management_hop() {
        let inspect = Request::Firewall { repair: false };
        let scoped = Request::Scoped { profile: "fixture".into(), request: Box::new(inspect.clone()) };
        assert!(inspect.response_timeout().unwrap() > crate::firewall::OPERATION_TIMEOUT);
        assert_eq!(scoped.response_timeout(), inspect.response_timeout());
        assert_eq!(Request::Status.response_timeout(), Some(std::time::Duration::from_secs(5)));
        assert_eq!(Request::Firewall { repair: true }.response_timeout(), None);
        assert_eq!(Request::Scoped { profile: "fixture".into(), request: Box::new(Request::Restart) }.response_timeout(), None);
    }
    #[test]
    fn independent_profiles_reject_cross_profile_and_nested_management() {
        let scoped = |id:&str, request| Request::Scoped { profile:id.into(), request:Box::new(request) };
        assert!(matches!(Request::Status.for_profile(Some("active")).unwrap(), Request::Status));
        assert!(Request::Restart.for_profile(Some("active")).is_err());
        assert!(scoped("other", Request::Restart).for_profile(Some("active")).is_err());
        assert!(scoped("active", scoped("active", Request::Restart)).for_profile(Some("active")).is_err());
        assert!(matches!(scoped("active", Request::Restart).for_profile(Some("active")).unwrap(), Request::Restart));
        assert!(scoped("active", Request::Restart).for_profile(None).is_err());
        assert!(Request::Restart.for_profile(None).is_ok());
        assert!(!scoped("active", Request::Status).is_mutation());
        assert!(scoped("active", Request::Restart).is_mutation());
    }
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
        let inspect=Request::Plugins{request:PluginRequest::Inspect};
        assert!(!inspect.is_mutation());
        let save=Request::Plugins{request:PluginRequest::Save{id:"fixture".into(),expected:Default::default(),value:Default::default()}};
        assert!(save.is_mutation());
        let wire=serde_json::to_vec(&save).unwrap();assert!(serde_json::from_slice::<Request>(&wire).is_ok());
    }
}
