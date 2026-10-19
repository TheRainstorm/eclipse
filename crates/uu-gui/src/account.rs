//! Local account lifecycle. Device identity and preferences survive sign-out.
use std::path::Path;
use uu_core::api::ClientState;

fn read_state(path: &Path) -> anyhow::Result<Option<ClientState>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn read_token(path: &Path) -> anyhow::Result<Option<String>> {
    Ok(read_state(path)?
        .and_then(|state| state.token)
        .filter(|token| !token.is_empty()))
}

pub fn store_login(path: &Path, token: String, user_id: Option<String>) -> anyhow::Result<()> {
    anyhow::ensure!(!token.is_empty(), "登录响应缺少凭据");
    let mut state = read_state(path)?.unwrap_or_default();
    state.token = Some(token);
    state.user_id = user_id.clone();
    state.identity.user_id = user_id;
    state.save(&path.to_path_buf())?;
    Ok(())
}

pub fn sign_out(path: &Path) -> anyhow::Result<()> {
    if let Some(mut state) = read_state(path)? {
        state.token = None;
        state.user_id = None;
        state.identity.user_id = None;
        state.save(&path.to_path_buf())?;
    }
    Ok(())
}

impl super::App {
    pub(super) fn account_button(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui::{self, RichText};
        let response = super::ui::icon_button(ui, super::ui::Icon::Person, "账号");
        let id = ui.make_persistent_id("account-menu");
        if response.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(id));
        }
        egui::popup_below_widget(
            ui,
            id,
            &response,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(176.0);
                ui.strong(if self.logged_in {
                    "账号已登录"
                } else {
                    "尚未登录"
                });
                ui.separator();
                if self.logged_in {
                    ui.label(RichText::new("退出后将断开本客户端的串流").small());
                    if ui.button("退出账号").clicked() {
                        ui.close_menu();
                        self.sign_out();
                    }
                } else if ui.button("扫码登录").clicked() {
                    ui.close_menu();
                    self.start_login();
                }
            },
        );
    }

    fn sign_out(&mut self) {
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
        self.account_token = token;
        self.logged_in = self.account_token.is_some();
        self.login_rx = None;
        self.login_texture = None;
        self.login_status.clear();
        // Discard device results started under the previous login.
        self.rx = None;
        self.devices.clear();
        self.wallpapers.clear();
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
        if self.account_poll_at.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.account_poll_at = std::time::Instant::now();
        let Some(config) = &self.config else { return };
        // A partial write or transient read failure must not end a healthy session.
        if let Ok(token) = read_token(&config.at("state.json")) {
            if token != self.account_token {
                self.apply_account(token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        store_login(&path, "test-credential".into(), Some("test-account".into())).unwrap();
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
        store_login(&path, "next-credential".into(), None).unwrap();
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
