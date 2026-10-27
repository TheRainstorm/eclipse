//! High-level API flows: device registration, QR login, device list.
//!
//! Registration and QR flows were verified against the live service
//! (see `docs/protocol-notes.md` §2.6). Device rename follows the PC client's
//! PUT /device/{id} alias request; authenticated phone UI validation is separate.

use std::{io::Read, path::{Path, PathBuf}};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreError;
use crate::sign::ClientIdentity;
use crate::signal::{SignalClient, SignedRequest};

/// Persisted client state (identity + session token).
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientState {
    pub identity: ClientIdentity,
    pub device_id: Option<String>,
    /// Last name confirmed by UU; older state files need no migration/reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub token: Option<String>,
    pub user_id: Option<String>,
}

impl std::fmt::Debug for ClientState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientState")
            .field("registered", &self.device_id.is_some())
            .field("authenticated", &self.token.as_ref().is_some_and(|token| !token.is_empty()))
            .finish_non_exhaustive()
    }
}

impl ClientState {
    pub fn new() -> Self {
        Self {
            identity: ClientIdentity::default(),
            device_id: None,
            device_name: None,
            token: None,
            user_id: None,
        }
    }

    pub fn read(path: &Path) -> Result<Option<Self>, CoreError> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(CoreError::AccountState(uu_i18n::text("没有读取权限")));
            },
            Err(_) => return Err(CoreError::AccountState(uu_i18n::text("无法读取文件"))),
        };
        const LIMIT: usize = 1024 * 1024;
        let mut bytes = Vec::new();
        file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)
            .map_err(|_| CoreError::AccountState(uu_i18n::text("无法读取文件")))?;
        if bytes.len() > LIMIT {
            return Err(CoreError::AccountState(uu_i18n::text("文件超过 1 MiB 大小限制")));
        }
        // serde errors may quote the rejected value, including account tokens.
        // Report the category without echoing any account contents or paths.
        serde_json::from_slice(&bytes).map(Some)
            .map_err(|_| CoreError::AccountState(uu_i18n::text("文件格式无效")))
    }

    pub fn load(path: &Path) -> Result<Self, CoreError> {
        Ok(Self::read(path)?.unwrap_or_default())
    }

    pub fn save(&self, path: &PathBuf) -> Result<(), CoreError> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| CoreError::Config(e.to_string()))?;
        crate::private_file::write_atomic(path, &data)?;
        Ok(())
    }
}

impl Default for ClientState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    use std::{fs, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread};

    #[test]
    fn account_management_lists_only_owned_devices_and_marks_current_identity() {
        let body = serde_json::json!({"code":0,"data":{
            "current_device":{"device_id":"current-device","alias":"Local PC"},
            "my_binded_devices":[{"device_id":"current-device","alias":"Local PC"},
                {"device_id":"official-device","alias":"Local PC"}],
            "others_shared_devices":[{"device_id":"shared-device","alias":"Shared PC"}]
        }});
        let devices = parse_account_devices(body, "current-device").unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].current);
        assert!(!devices[1].current);
        assert_eq!(devices[1].device_id, "official-device");
    }

    #[test]
    fn account_management_rejects_errors_and_invalid_targets_instead_of_an_empty_list() {
        for body in [serde_json::json!({"code":1120,"data":{"my_binded_devices":[]}}),
            serde_json::json!({"code":0}),
            serde_json::json!({"code":0,"data":{"my_binded_devices":"invalid"}}),
            serde_json::json!({"code":0,"data":{"my_binded_devices":[{"device_id":"../other","alias":"Wrong"}]}})] {
            assert!(parse_account_devices(body, "current-device").is_err());
        }
        assert!(parse_account_devices(serde_json::json!({"code":0,"data":{"my_binded_devices":[]}}), "current-device").unwrap().is_empty());
    }

    #[test]
    fn old_account_keeps_identity_when_loading_without_a_device_name() {
        let mut value = serde_json::to_value(ClientState::new()).unwrap();
        value.as_object_mut().unwrap().remove("device_name");
        value["device_id"] = serde_json::json!("registered-device");
        value["identity"]["client_id"] = serde_json::json!("registered-client");
        let state: ClientState = serde_json::from_value(value).unwrap();
        assert!(state.device_name.is_none());
        assert_eq!(state.device_id.as_deref(), Some("registered-device"));
        assert_eq!(state.identity.client_id, "registered-client");
    }

    #[test]
    fn registration_refresh_preserves_sign_out_and_rejects_replaced_device_ids() {
        let mut initial = ClientState::new();
        initial.identity.client_id = "generated-installation".into();
        initial.device_id = Some("generated-device".into());
        initial.token = Some("old-login".into());
        let mut latest = initial.clone();
        latest.token = None;
        latest.user_id = None;
        let merged = merge_registration(&initial, latest.clone(), "generated-device", "Desktop-Eclipse").unwrap();
        assert!(merged.token.is_none());
        assert_eq!(merged.device_name.as_deref(), Some("Desktop-Eclipse"));
        assert!(merge_registration(&initial, latest.clone(), "another-device", "Desktop-Eclipse").is_err());
        latest.identity.client_id = "another-installation".into();
        assert!(merge_registration(&initial, latest, "generated-device", "Desktop-Eclipse").is_err());
    }

    #[test]
    fn rename_validates_a_display_name_and_cannot_target_another_api_path() {
        for invalid in ["".into(), " \t ".into(), "PC\0name".into(), "PC\nname".into(), "名".repeat(65)] {
            assert!(device_name_update("registered-device", &invalid).is_err());
        }
        for invalid in ["", "../other", "device?query=1", "device/other"] {
            assert!(device_name_update(invalid, "Office PC").is_err());
        }
        let (path, body) = device_name_update("registered-device", "  工作站  ").unwrap();
        assert_eq!(path, "/api/v1/device/registered-device");
        assert_eq!(body, serde_json::json!({"alias":"工作站"}));
        assert!(device_name_update("registered-device", &"名".repeat(64)).is_ok());
    }

    #[test]
    fn missing_account_is_new_but_unreadable_or_corrupt_account_is_preserved() {
        let root = std::env::temp_dir().join(format!("uu-account-read-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state.json");
        assert!(ClientState::read(&path).unwrap().is_none());
        assert!(ClientState::load(&path).unwrap().token.is_none());
        assert!(!path.exists());
        let mut wrong_type = serde_json::to_value(ClientState::new()).unwrap();
        wrong_type["token"] = serde_json::json!({"private-fixture-marker": "not a string"});
        for bytes in [b"partial account with private-fixture-marker".to_vec(),
                      serde_json::to_vec(&wrong_type).unwrap(), vec![b'x'; 1024 * 1024 + 1]] {
            fs::write(&path, &bytes).unwrap();
            let error = ClientState::load(&path).unwrap_err().to_string();
            assert!(error.contains("本地账号配置不可用"));
            assert!(!error.contains("private-fixture-marker"));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(ClientState::read(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn api_construction_rejects_corrupt_state_before_registration_or_network() {
        let root = std::env::temp_dir().join(format!("uu-api-state-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state.json");
        fs::write(&path, b"broken private-fixture-marker").unwrap();
        let config = crate::Config { data_dir: root.clone(), state_dir: root.clone(),
            signaling_host: "unreachable.example.invalid".into() };
        let client = SignalClient::new(&config).unwrap();
        assert!(matches!(UuApi::new(client, path.clone()), Err(CoreError::AccountState(_))));
        assert_eq!(fs::read(&path).unwrap(), b"broken private-fixture-marker");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_atomic_account_updates_never_expose_partial_json_to_readers() {
        let root = std::env::temp_dir().join(format!("uu-account-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.json");
        let mut state = ClientState::new();
        state.identity.client_id = "fixture-client".into();
        state.device_id = Some("fixture-device".into());
        state.user_id = Some("fixture-user".into());
        state.token = Some("fixture-token".into());
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        state.save(&path).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = stop.clone();
        let reader_path = path.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let mut ready = Some(ready_tx);
            loop {
                let parsed: ClientState = serde_json::from_slice(&fs::read(&reader_path).unwrap()).unwrap();
                assert_eq!(parsed.identity.client_id, "fixture-client");
                assert!(parsed.token.unwrap().starts_with("fixture-"));
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
                if reader_stop.load(Ordering::Acquire) { break; }
                thread::yield_now();
            }
        });
        ready_rx.recv().unwrap();
        for n in 0..40 {
            state.token = Some(format!("fixture-{n}-{}", "x".repeat(64 * 1024)));
            state.save(&path).unwrap();
        }
        stop.store(true, Ordering::Release);
        reader.join().unwrap();
        assert_eq!(serde_json::to_value(ClientState::load(&path).unwrap()).unwrap(), serde_json::to_value(&state).unwrap());
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_and_qr_debug_output_do_not_include_credentials_or_identity() {
        let mut state = ClientState::new();
        state.token = Some("fixture-private-token".into());
        state.identity.client_id = "fixture-private-client".into();
        let debug = format!("{state:?}");
        assert!(!debug.contains("fixture-private"));
        let qr = QrLogin { jump_url: "https://example.invalid/fixture-qr".into(), token: "fixture-qr-token".into() };
        assert!(!format!("{qr:?}").contains("fixture-qr"));
    }
}

/// The QR code handed to the user for scanning.
#[derive(Clone, Serialize, Deserialize)]
pub struct QrLogin {
    pub jump_url: String,
    pub token: String,
}

impl std::fmt::Debug for QrLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrLogin").finish_non_exhaustive()
    }
}

/// Response of the device registration call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevice {
    pub device_id: String,
    pub alias: String,
}

/// A registration bound to the signed-in account, excluding shared-with-me rows.
#[derive(Clone, Debug, Deserialize)]
pub struct AccountDevice {
    pub device_id: String,
    pub alias: String,
    #[serde(default)]
    pub status: String,
    #[serde(skip)]
    pub current: bool,
}

fn parse_account_devices(body: serde_json::Value, current: &str) -> Result<Vec<AccountDevice>, CoreError> {
    if body["code"].as_i64() != Some(0) {
        return Err(CoreError::Service {
            code: body["code"].as_i64().unwrap_or(-1) as i32,
            message: body["msg"].as_str().unwrap_or(uu_i18n::text("获取账号设备失败")).into(),
        });
    }
    let data = body["data"].as_object().ok_or_else(|| CoreError::Config(uu_i18n::text("账号设备列表格式无效").into()))?;
    let mut values = data.get("my_binded_devices").and_then(serde_json::Value::as_array)
        .ok_or_else(|| CoreError::Config(uu_i18n::text("账号设备列表格式无效").into()))?.clone();
    if let Some(device) = data.get("current_device").filter(|d| !d.is_null()) {
        values.insert(0, device.clone());
    }
    let mut devices = Vec::<AccountDevice>::new();
    for value in values {
        let mut device: AccountDevice = serde_json::from_value(value)
            .map_err(|_| CoreError::Config(uu_i18n::text("账号设备记录格式无效").into()))?;
        device_path(&device.device_id)?;
        device.current = device.device_id == current;
        if !devices.iter().any(|d| d.device_id == device.device_id) { devices.push(device); }
    }
    Ok(devices)
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

pub fn validate_device_name(name: &str) -> Result<&str, CoreError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(CoreError::Config(uu_i18n::text("请输入有效的设备名称（最多64字）").into()));
    }
    Ok(name)
}

fn device_path(device: &str) -> Result<String, CoreError> {
    if device.is_empty() || device.len() > 128
        || !device.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return Err(CoreError::Config(uu_i18n::text("UU 设备编号无效").into()));
    }
    Ok(format!("/api/v1/device/{device}"))
}

fn device_name_update(device: &str, name: &str) -> Result<(String, serde_json::Value), CoreError> {
    let name = validate_device_name(name)?;
    // The PC client's rename operation uses alias, rather than init's name.
    Ok((device_path(device)?, serde_json::json!({"alias": name})))
}

/// Merge only registration fields into the current on-disk account. Account
/// changes in another window must survive a slow registration refresh.
fn merge_registration(initial: &ClientState, mut latest: ClientState, device: &str, alias: &str) -> Result<ClientState, CoreError> {
    if initial.device_id.as_deref().is_some_and(|expected| expected != device) {
        return Err(CoreError::Config(uu_i18n::text("UU 刷新注册返回了不同设备编号，已保留原身份，请勿重新注册").into()));
    }
    if latest.device_id != initial.device_id || (!latest.identity.client_id.is_empty()
        && latest.identity.client_id != initial.identity.client_id) {
        return Err(CoreError::Config(uu_i18n::text("本机 UU 身份已变化，请重新打开登录页面").into()));
    }
    if latest.identity.client_id.is_empty() { latest.identity = initial.identity.clone(); }
    latest.device_id = Some(device.into());
    latest.device_name = Some(alias.into());
    Ok(latest)
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
    pub fn new(client: SignalClient, state_path: PathBuf) -> Result<Self, CoreError> {
        let state = ClientState::load(&state_path)?;
        Ok(Self { client, state, state_path })
    }

    pub fn has_identity(&self) -> bool {
        !self.state.identity.client_id.is_empty() && self.state.device_id.is_some()
    }

    /// Register this machine and persist the assigned ids.
    pub async fn register(&mut self, name: &str) -> Result<RegisteredDevice, CoreError> {
        let name = validate_device_name(name)?;
        if self.state.identity.client_id.is_empty() {
            self.state.identity.client_id = Uuid::new_v4().to_string();
        }
        let system_id = if self.state.identity.device_id.is_empty() {
            new_system_id()
        } else {
            self.state.identity.device_id.clone()
        };
        self.state.identity.device_id = system_id.clone();

        let inventory = tokio::task::spawn_blocking(crate::registration::ComputerInfo::collect)
            .await.unwrap_or_default();
        let mut body = serde_json::to_value(inventory).map_err(|e| CoreError::Config(e.to_string()))?;
        body["name"] = name.into();
        body["client_id"] = self.state.identity.client_id.clone().into();
        body["system_id"] = system_id.into();
        body["machine_guid"] = crate::registration::installation_guid(&self.state.identity.client_id).into();
        let mut identity = self.state.identity.clone();
        identity.user_id = None;
        let resp = tokio::time::timeout(std::time::Duration::from_secs(15), self
            .client
            .call(
                &identity,
                None,
                SignedRequest {
                    method: "POST",
                    path_with_query: "/api/v1/device/windows/init",
                    body: Some(body),
                },
            )
            ).await.map_err(|_| CoreError::Config(uu_i18n::text("UU 注册信息更新超时，请重试").into()))??;
        let data = &resp.body["data"];
        let device_id = data["device_id"]
            .as_str()
            .ok_or_else(|| CoreError::Service {
                code: resp.body["code"].as_i64().unwrap_or(-1) as i32,
                message: resp.body["msg"].as_str().unwrap_or("registration failed").to_string(),
            })?
            .to_string();
        let alias = data["alias"].as_str().unwrap_or(name).to_string();
        let latest = ClientState::load(&self.state_path)?;
        let latest = merge_registration(&self.state, latest, &device_id, &alias)?;
        latest.save(&self.state_path)?;
        self.state = latest;
        Ok(RegisteredDevice {
            device_id,
            alias,
        })
    }

    /// Rename only this registered device. The caller owns local state updates,
    /// so a cancelled GUI worker cannot persist stale credentials or settings.
    pub async fn rename_current_device(&self, name: &str) -> Result<(), CoreError> {
        let token = self.state.token.as_deref().filter(|s| !s.is_empty())
            .ok_or(CoreError::Unauthenticated)?;
        let device = self.state.device_id.as_deref().ok_or(CoreError::Unauthenticated)?;
        let (path, body) = device_name_update(device, name)?;
        let mut identity = self.state.identity.clone();
        identity.device_id = device.into();
        let resp = tokio::time::timeout(std::time::Duration::from_secs(15),
            self.client.call(&identity, Some(token), SignedRequest {
                method: "PUT", path_with_query: &path, body: Some(body),
            })).await.map_err(|_| CoreError::Config(uu_i18n::text("设备改名请求超时，请重试").into()))??;
        if !(200..300).contains(&resp.status) || resp.body["code"].as_i64() != Some(0) {
            return Err(CoreError::Service {
                code: resp.body["code"].as_i64().unwrap_or(resp.status as i64) as i32,
                message: resp.body["msg"].as_str().unwrap_or(uu_i18n::text("设备改名失败")).into(),
            });
        }
        Ok(())
    }

    pub async fn account_devices(&self) -> Result<Vec<AccountDevice>, CoreError> {
        parse_account_devices(self.device_list().await?, self.state.device_id.as_deref().unwrap_or_default())
    }

    /// Remove an account binding, retaining local registration and native trust.
    /// This is the PC client's POST unbind operation, not a registration reset.
    pub async fn unbind_account_device(&self, device: &str) -> Result<(), CoreError> {
        let token = self.state.token.as_deref().filter(|s| !s.is_empty()).ok_or(CoreError::Unauthenticated)?;
        let mut identity = self.state.identity.clone();
        identity.device_id = self.state.device_id.clone().ok_or(CoreError::Unauthenticated)?;
        let path = format!("{}/unbind", device_path(device)?);
        let resp = self.client.call(&identity, Some(token), SignedRequest {
            method: "POST", path_with_query: &path, body: None,
        }).await?;
        if !(200..300).contains(&resp.status) || resp.body["code"].as_i64() != Some(0) {
            return Err(CoreError::Service {code:resp.body["code"].as_i64().unwrap_or(resp.status as i64) as i32,
                message:resp.body["msg"].as_str().unwrap_or(uu_i18n::text("设备解绑失败")).into()});
        }
        Ok(())
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

    /// Join an official assistance invitation without saving its code or
    /// converting it into a permanent native device authorization.
    pub async fn room_join_assistance(&self, request:&crate::assistance::Request) -> anyhow::Result<serde_json::Value> {
        let token=self.state.token.as_deref().ok_or(CoreError::Unauthenticated)?;
        let mut identity=self.state.identity.clone();
        if let Some(id)=&self.state.device_id {identity.device_id=id.clone();}
        for version in [2,1] {
            let path=format!("/api/v{version}/room/join/share/by_code");
            let response=self.client.call(&identity,Some(token),SignedRequest {
                method:"POST",path_with_query:&path,
                body:Some(serde_json::json!({"connect_id":request.id,"connect_code":request.code()})),
            }).await?;
            if crate::assistance::endpoint_fallback(version,response.status) {continue;}
            return crate::assistance::room(response.status,response.body,request,token);
        }
        unreachable!()
    }

    /// Create the controlled-side room using the current authenticated identity.
    pub async fn room_create(&self) -> Result<serde_json::Value, CoreError> {
        let token = self.state.token.as_deref().ok_or(CoreError::Unauthenticated)?;
        let mut identity = self.state.identity.clone();
        identity.device_id = self.state.device_id.clone().ok_or(CoreError::Unauthenticated)?;
        Ok(self.client.call(&identity, Some(token), SignedRequest {
            method: "POST", path_with_query: "/api/v1/room/create",
            body: Some(serde_json::json!({})),
        }).await?.body)
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
