//! Local account lifecycle. Device identity and preferences survive sign-out.
use std::path::Path;
use uu_core::api::ClientState;

fn read_state(path: &Path) -> anyhow::Result<Option<ClientState>> {
    Ok(ClientState::read(path)?)
}

pub fn read_token(path: &Path) -> anyhow::Result<Option<String>> {
    Ok(read_state(path)?
        .and_then(|state| state.token)
        .filter(|token| !token.is_empty()))
}

pub fn store_login(path: &Path, token: String, user_id: Option<String>, device_name: Option<String>) -> anyhow::Result<()> {
    anyhow::ensure!(!token.is_empty(), "登录响应缺少凭据");
    let mut state = read_state(path)?.unwrap_or_default();
    state.token = Some(token);
    state.user_id = user_id.clone();
    state.identity.user_id = user_id;
    if let Some(name) = device_name { state.device_name = Some(name); }
    state.save(&path.to_path_buf())?;
    Ok(())
}

pub fn store_device_name(path: &Path, token: &str, name: String) -> anyhow::Result<()> {
    let mut state = read_state(path)?.ok_or_else(|| anyhow::anyhow!("本地账号配置不存在"))?;
    anyhow::ensure!(state.token.as_deref() == Some(token), "账号已变化，请重新保存设备名称");
    state.device_name = Some(name);
    state.save(&path.to_path_buf())?;
    Ok(())
}

pub fn sign_out(path: &Path) -> anyhow::Result<()> {
    sign_out_matching(path, None).map(|_| ())
}

/// An asynchronous unbind may finish after another process has logged in.
pub fn sign_out_matching(path: &Path, expected: Option<&str>) -> anyhow::Result<bool> {
    if let Some(mut state) = read_state(path)? {
        if expected.is_some() && state.token.as_deref() != expected { return Ok(false); }
        state.token = None;
        state.user_id = None;
        state.identity.user_id = None;
        state.save(&path.to_path_buf())?;
        return Ok(true);
    }
    Ok(false)
}

impl super::App {
    pub(super) fn sign_out(&mut self) {
        // Cancel and join a QR worker before changing its shared state file.
        self.login_rx = None;
        let result = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无法确定登录目录"))
            .and_then(|config| sign_out(&config.at("state.json")));
        match result {
            Ok(()) => self.apply_account(None),
            Err(error) => self.status = format!("退出账号失败：{error}"),
        }
    }

    pub(super) fn apply_account(&mut self, token: Option<String>) {
        self.uu_devices = Default::default();
        if self.account_token.is_some() && self.account_token!=token && self.workspace.book.sharing.uu_enabled && self.workspace.host.running() {
            self.workspace.host.stop();self.workspace.feedback.error(super::ui_feedback::Scope::Services(0),"UU 账号已变化，已停止本机共享；确认账号后可重新开启。");
        }
        self.account_token = token;
        self.account_error = None;
        self.logged_in = self.account_token.is_some();
        self.login_rx = None;
        self.login_texture = None;
        self.login_status.clear();
        // Discard device results started under the previous login.
        self.rx = None;
        self.devices.clear();
        self.device_load_error = None;
        self.device_library = super::device_library::Library::for_account(self.config.as_ref(), self.account_token.as_deref());
        self.device_filter = super::device_library::Filter::All;
        self.wallpapers.clear();
        self.wallpaper_worker = None;
        self.selected = None;
        self.search.clear();
        self.autoconnect_done = true;
        if self.stream_target.is_some() {
            self.disconnect();
        } else if self.logged_in {
            self.start_load();
        } else {
            self.status = "已退出账号，请重新扫码登录".into();
        }
    }

    pub(super) fn poll_account(&mut self) {
        if self.is_native(){return;}
        if self.account_poll_at.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.account_poll_at = std::time::Instant::now();
        let Some(config) = &self.config else { return };
        // A partial write or transient read failure must not end a healthy session.
        match read_token(&config.at("state.json")) {
            Ok(token) => {
                self.account_error = None;
                if token != self.account_token { self.apply_account(token); }
            },
            Err(error) => self.account_error = Some(error.to_string()),
        }
    }

    pub(super) fn retry_account_read(&mut self) {
        self.account_poll_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
        self.poll_account();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_device_name_preserves_identity_and_rejects_a_stale_account() {
        let dir = std::env::temp_dir().join(format!("uu-name-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("state.json");
        let mut state = ClientState::new();
        state.device_id = Some("registered-device".into());
        state.identity.client_id = "registered-client".into();
        state.identity.device_id = "system-id".into();
        state.token = Some("current-account-token".into());
        state.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(store_device_name(&path, "previous-account-token", "New name".into()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        store_device_name(&path, "current-account-token", "Office PC".into()).unwrap();
        let after = ClientState::load(&path).unwrap();
        state.device_name = Some("Office PC".into());
        assert_eq!(serde_json::to_value(after).unwrap(), serde_json::to_value(&state).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert!(!sign_out_matching(&path, Some("previous-account-token")).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(sign_out_matching(&path, Some("current-account-token")).unwrap());
        assert_eq!(ClientState::load(&path).unwrap().device_name, state.device_name);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sign_out_removes_credentials_but_preserves_registered_device_and_preferences() {
        let dir = std::env::temp_dir().join(format!("uu-account-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let mut state = ClientState::new();
        state.device_id = Some("registered-device".into());
        state.identity.client_id = "registered-client".into();
        state.identity.device_id = "system-id".into();
        state.save(&path).unwrap();
        let preferences = dir.join("gui-settings.json");
        std::fs::write(&preferences, b"unchanged preferences").unwrap();
        store_login(&path, "test-credential".into(), Some("test-account".into()), None).unwrap();
        assert_eq!(
            read_token(&path).unwrap().as_deref(),
            Some("test-credential")
        );
        sign_out(&path).unwrap();
        sign_out(&path).unwrap();
        let after = read_state(&path).unwrap().unwrap();
        assert!(
            after.token.is_none() && after.user_id.is_none() && after.identity.user_id.is_none()
        );
        assert_eq!(after.device_id, state.device_id);
        assert_eq!(after.identity.client_id, state.identity.client_id);
        assert_eq!(after.identity.device_id, state.identity.device_id);
        assert_eq!(
            std::fs::read(&preferences).unwrap(),
            b"unchanged preferences"
        );
        store_login(&path, "next-credential".into(), None, None).unwrap();
        assert_eq!(
            read_token(&path).unwrap().as_deref(),
            Some("next-credential")
        );
        assert!(read_state(&path)
            .unwrap()
            .unwrap()
            .identity
            .user_id
            .is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_state_is_not_treated_as_sign_out_or_overwritten() {
        let path =
            std::env::temp_dir().join(format!("uu-account-malformed-{}.json", std::process::id()));
        std::fs::write(&path, b"partial state").unwrap();
        assert!(read_token(&path).is_err());
        assert!(sign_out(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"partial state");
        std::fs::remove_file(path).unwrap();
    }
}
