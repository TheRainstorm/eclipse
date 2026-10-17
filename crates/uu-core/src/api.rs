//! High-level API flows: device registration, QR login, device list.
//!
//! Each step was verified against the live service before being encoded here
//! (see `docs/protocol-notes.md` §2.6).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreError;
use crate::sign::ClientIdentity;
use crate::signal::{SignalClient, SignedRequest};

/// Persisted client state (identity + session token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientState {
    pub identity: ClientIdentity,
    pub device_id: Option<String>,
    pub token: Option<String>,
    pub user_id: Option<String>,
}

impl ClientState {
    pub fn new() -> Self {
        Self {
            identity: ClientIdentity::default(),
            device_id: None,
            token: None,
            user_id: None,
        }
    }

    pub fn load(path: &PathBuf) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(Self::new)
    }

    pub fn save(&self, path: &PathBuf) -> Result<(), CoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let data = serde_json::to_vec_pretty(self).map_err(|e| CoreError::Config(e.to_string()))?;
        std::fs::write(path, data)?;
        Ok(())
    }
}

impl Default for ClientState {
    fn default() -> Self {
        Self::new()
    }
}

/// The QR code handed to the user for scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrLogin {
    pub jump_url: String,
    pub token: String,
}

/// Response of the device registration call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevice {
    pub device_id: String,
    pub alias: String,
}

pub struct UuApi {
    client: SignalClient,
    pub state: ClientState,
    state_path: PathBuf,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_system_id() -> String {
    // Shape observed on the Windows client:
    // 20260415-8013-1656-2BF6-801316562BFA
    let d = unix_now();
    let a = Uuid::new_v4().simple().to_string();
    format!(
        "{:04}{:02}{:02}-{:04}-{:04}-{}-{}",
        (d / 31_536_000) % 10_000,     // rough year component
        (d / 2_592_000) % 12 + 1,
        (d / 86_400) % 28 + 1,
        Uuid::new_v4().as_u128() as u16,
        Uuid::new_v4().as_u128() as u16,
        &a[0..4].to_uppercase(),
        &a[4..16].to_uppercase()
    )
}

impl UuApi {
    pub fn new(client: SignalClient, state_path: PathBuf) -> Self {
        let state = ClientState::load(&state_path);
        Self { client, state, state_path }
    }

    pub fn has_identity(&self) -> bool {
        !self.state.identity.client_id.is_empty() && self.state.device_id.is_some()
    }

    /// Register this machine and persist the assigned ids.
    pub async fn register(&mut self, name: &str) -> Result<RegisteredDevice, CoreError> {
        if self.state.identity.client_id.is_empty() {
            self.state.identity.client_id = Uuid::new_v4().to_string();
        }
        let system_id = if self.state.identity.device_id.is_empty() {
            new_system_id()
        } else {
            self.state.identity.device_id.clone()
        };
        self.state.identity.device_id = system_id.clone();
        self.state.identity.user_id = None;

        let body = serde_json::json!({
            "name": name,
            "client_id": self.state.identity.client_id,
            "system_id": system_id,
            "machine_guid": Uuid::new_v4().to_string(),
            "os": std::env::consts::OS,
            "base_board": "unknown",
            "cpu": std::env::consts::ARCH,
            "video": ["unknown"],
            "mac": "02:00:00:00:00:01",
            "memory": "unknown",
            "screen": "1920x1080",
        });
        let resp = self
            .client
            .call(
                &self.state.identity,
                None,
                SignedRequest {
                    method: "POST",
                    path_with_query: "/api/v1/device/windows/init",
                    body: Some(body),
                },
            )
            .await?;
        let data = &resp.body["data"];
        let device_id = data["device_id"]
            .as_str()
            .ok_or_else(|| CoreError::Service {
                code: resp.body["code"].as_i64().unwrap_or(-1) as i32,
                message: resp.body["msg"].as_str().unwrap_or("registration failed").to_string(),
            })?
            .to_string();
        self.state.device_id = Some(device_id.clone());
        self.state.save(&self.state_path)?;
        Ok(RegisteredDevice {
            device_id,
            alias: data["alias"].as_str().unwrap_or(name).to_string(),
        })
    }

    /// Ask the service for a login QR code.
    pub async fn gen_qr_login(&self) -> Result<QrLogin, CoreError> {
        let device_id = self
            .state
            .device_id
            .clone()
            .ok_or_else(|| CoreError::Config("register the device first".into()))?;
        let mut identity = self.state.identity.clone();
        identity.device_id = device_id;
        identity.user_id = None;
        let resp = self
            .client
            .call(
                &identity,
                None,
                SignedRequest {
                    method: "POST",
                    path_with_query: "/api/v1/qrcode/gen/login",
                    body: None,
                },
            )
            .await?;
        let data = &resp.body["data"];
        Ok(QrLogin {
            jump_url: data["qrcode_jump_url"].as_str().unwrap_or_default().to_string(),
            token: data["token"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// Long-poll the QR status once. Blocks until the phone acts on it.
    pub async fn poll_qr_login(&mut self, qr: &QrLogin, status: i32) -> Result<crate::signal::ApiResponse, CoreError> {
        let device_id = self.state.device_id.clone().unwrap_or_default();
        let mut identity = self.state.identity.clone();
        identity.device_id = device_id;
        identity.user_id = None;
        let body = serde_json::json!({
            "login_status": status,
            "qrcode_jump_url": qr.jump_url,
            "token": qr.token,
        });
        let resp = self
            .client
            .call(
                &identity,
                None,
                SignedRequest {
                    method: "POST",
                    path_with_query: "/api/v1/qrcode/login/status",
                    body: Some(body),
                },
            )
            .await?;
        Ok(resp)
    }

    /// Exchange a scanned QR for a session (endpoint recovered from the
    /// native client's endpoint table).
    pub async fn exchange_qr(&self, qr: &QrLogin) -> Result<serde_json::Value, CoreError> {
        let device_id = self.state.device_id.clone().unwrap_or_default();
        let mut identity = self.state.identity.clone();
        identity.device_id = device_id;
        identity.user_id = None;
        let body = serde_json::json!({
            "qrcode_jump_url": qr.jump_url,
            "token": qr.token,
        });
        let resp = self
            .client
            .call(
                &identity,
                None,
                SignedRequest {
                    method: "POST",
                    path_with_query: "/api/v1/login/by_qrcode",
                    body: Some(body),
                },
            )
            .await?;
        Ok(resp.body)
    }

    /// Persist a token obtained from the login flow.
    pub fn store_token(&mut self, token: &str, user_id: Option<String>) -> Result<(), CoreError> {
        self.state.token = Some(token.to_string());
        if let Some(u) = user_id {
            self.state.identity.user_id = Some(u.clone());
            self.state.user_id = Some(u);
        }
        self.state.save(&self.state_path)
    }

    /// Open (or join) a remote-control room for `device`.
    ///
    /// The response carries the signaling endpoint, session token and ICE
    /// servers. Verified live: a second controller gets `2002` while the
    /// target already has an active session.
    pub async fn room_join(
        &self,
        device: &str,
        force: bool,
    ) -> Result<serde_json::Value, CoreError> {
        let token = self.state.token.clone().ok_or(CoreError::Unauthenticated)?;
        let mut identity = self.state.identity.clone();
        if let Some(dev) = &self.state.device_id {
            identity.device_id = dev.clone();
        }
        let path = format!("/api/v1/room/join/by_device/{device}");
        let body = serde_json::json!({ "force_join": force });
        let resp = self
            .client
            .call(
                &identity,
                Some(&token),
                SignedRequest {
                    method: "POST",
                    path_with_query: &path,
                    body: Some(body),
                },
            )
            .await?;
        Ok(resp.body)
    }

    pub async fn device_list(&self) -> Result<serde_json::Value, CoreError> {
        let token = self
            .state
            .token
            .clone()
            .ok_or(CoreError::Unauthenticated)?;
        // `identity.device_id` holds the system id; the request header must
        // carry the device id the service assigned at registration.
        let mut identity = self.state.identity.clone();
        if let Some(dev) = &self.state.device_id {
            identity.device_id = dev.clone();
        }
        let resp = self
            .client
            .call(
                &identity,
                Some(&token),
                SignedRequest {
                    method: "GET",
                    path_with_query: "/api/v1/device/list",
                    body: None,
                },
            )
            .await?;
        Ok(resp.body)
    }
}
