//! User preferences are separate from credentials and installation files.
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub use uu_core::client_settings::{ClientSettings, CodecPreference, CursorMode};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceView { #[default] Cards, List }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    #[serde(flatten)]
    pub client: ClientSettings,
    pub lan_discovery: bool,
    // Retained only to migrate older configurations into connection services.
    pub lan_service_port: u16,
    pub lan_discovery_port: u16,
    pub device_view: DeviceView,
    pub wallpaper_enabled: bool,
    pub app_autostart:bool,
    pub wallpaper_api: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self { client: ClientSettings::default(), lan_discovery:true,
            lan_service_port:uu_core::native_discovery::SERVICE_PORT,
            lan_discovery_port:uu_core::native_discovery::DISCOVERY_PORT,
            device_view:DeviceView::Cards, wallpaper_enabled:true, app_autostart:false,
            wallpaper_api:"https://picsum.photos/1920/1080".into() }
    }
}
impl std::ops::Deref for Settings {
    type Target = ClientSettings;
    fn deref(&self) -> &Self::Target { &self.client }
}
impl std::ops::DerefMut for Settings {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.client }
}
impl Settings {
    fn validate(&self)->anyhow::Result<()> {
        self.client.validate()?;
        anyhow::ensure!(self.lan_service_port>0&&self.lan_discovery_port>0,"局域网端口必须在 1–65535 之间");
        if self.wallpaper_enabled {
            anyhow::ensure!(self.wallpaper_api.len()<=2048,"壁纸接口地址过长");
            let url=reqwest::Url::parse(&self.wallpaper_api).map_err(|_|anyhow::anyhow!("请输入有效的壁纸图片 URL"))?;
            anyhow::ensure!(matches!(url.scheme(),"http"|"https")&&url.host_str().is_some()&&url.username().is_empty()&&url.password().is_none(),"壁纸接口须为不含登录凭据的 HTTP / HTTPS 图片地址");
        }
        Ok(())
    }
    fn capture_values(&self)->(u32,usize,u32,usize,bool,CodecPreference,bool) {
        (self.fps,self.bitrate,if self.bitrate==4{self.custom_mbps}else{0},self.resolution,self.cursor_mode.remote(),self.codec,self.hevc_preferred())
    }
    pub fn path() -> Option<PathBuf> {
        uu_core::Config::from_env()
            .ok()
            .map(|c| c.at("gui-settings.json"))
    }
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = match read_bounded(path, 64 * 1024) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => anyhow::bail!("无法读取设置，或文件超过 64 KiB；原文件已保留"),
        };
        let mut s: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("设置文件格式无效；原文件已保留，未自动覆盖"))?;
        // These two legacy fields only seed first migration into services.
        if s.lan_service_port==47998 { s.lan_service_port=uu_core::native_discovery::SERVICE_PORT; }
        if s.lan_discovery_port==47999 { s.lan_discovery_port=uu_core::native_discovery::DISCOVERY_PORT; }
        s.fps = s.fps.clamp(5, 144);
        s.bitrate = s.bitrate.min(4);
        s.custom_mbps = s.custom_mbps.clamp(1, 100);
        s.resolution = s.resolution.min(3);
        s.volume = s.volume.min(100);
        anyhow::ensure!(s.lan_service_port != 0 && s.lan_discovery_port != 0,
            "局域网端口必须在 1–65535 之间；原文件已保留");
        s.validate()?;
        Ok(s)
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(self.lan_service_port != 0 && self.lan_discovery_port != 0,
            "局域网端口必须在 1–65535 之间");
        // Automatic HUD/audio changes must not overwrite a broken original.
        // Recheck on every write, including after a second window changes it.
        Self::load(path)?;
        save_json(self, path)
    }

    /// A compatibility stream saves only client preferences. Software settings
    /// may have changed in the launcher while this window was open.
    fn save_client(&self, path: &Path) -> anyhow::Result<()> {
        let mut latest = Self::load(path)?;
        latest.client = self.client.clone();
        latest.save(path)
    }

    /// Only called by the user's explicit recovery action in Settings.
    pub fn backup_and_save(&self, path: &Path) -> anyhow::Result<Option<PathBuf>> {
        self.validate()?;
        if Self::load(path).is_ok() {
            self.save(path)?;
            return Ok(None);
        }
        let original = read_bounded(path, 1024 * 1024)
            .map_err(|_| anyhow::anyhow!("无法安全备份异常设置；请先修复文件或读取权限后重试"))?;
        let backup = path.with_file_name(format!("gui-settings.backup-{}.json", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&backup)?;
        if let Err(error) = file.write_all(&original).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&backup);
            return Err(error.into());
        }
        drop(file);
        anyhow::ensure!(read_bounded(path, 1024 * 1024)? == original,
            "设置在备份期间发生变化，已保留备份；请重新读取后重试");
        save_json(self, path).map_err(|_| anyhow::anyhow!("异常设置已备份，但新设置保存失败；请检查目录写权限"))?;
        Ok(Some(backup))
    }
}

pub(super) fn read_bounded(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "settings size limit exceeded"));
    }
    Ok(bytes)
}

pub(super) fn save_json(value: &impl Serialize, path: &Path) -> anyhow::Result<()> {
    save_bytes(path,&serde_json::to_vec_pretty(value)?)
}
pub(super) fn save_bytes(path:&Path,bytes:&[u8])->anyhow::Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("设置目录无效"))?,
    )?;
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // std also supports Windows replacement while another window still
        // has the old file open, via its POSIX-rename fallback.
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

impl super::App {
    pub(super) fn set_settings_notice(&mut self, text: impl Into<String>) {
        self.settings_notice = text.into();
        self.settings_notice_at = Some(std::time::Instant::now());
    }

    fn expire_settings_notice(&mut self, ctx: &egui::Context) {
        if !self.settings_notice.is_empty()
            && !self.settings_storage_error
            && !super::ui_feedback::looks_like_error(&self.settings_notice)
            && self
                .settings_notice_at
                .is_some_and(|at| at.elapsed() >= std::time::Duration::from_secs(2))
        {
            self.settings_notice.clear();
            self.settings_notice_at = None;
        } else if self.settings_notice_at.is_some()
            && !super::ui_feedback::looks_like_error(&self.settings_notice)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    pub(super) fn prepare_home_settings(&mut self) {
        self.snapshot_preferences();
        self.settings_draft = self.preferences.clone();
        self.about_open = false;
        self.native_ui.open = false;
        self.native_host.open = false;
        self.lan.host.open = false;
        self.displays.menu_open = false;
        self.stream_menu.open = false;
        self.context.memory_mut(|memory| memory.close_popup());
        self.context.request_repaint();
    }

    pub(super) fn discard_home_settings(&mut self) {
        self.settings_draft = self.preferences.clone();
        self.settings_notice.clear();
        self.settings_notice_at = None;
    }

    pub(super) fn settings_home(&mut self, ctx: &egui::Context) {
        self.expire_settings_notice(ctx);
        let view = self.workspace.route.settings;
        let dirty = self.settings_draft != self.preferences;
        let mut actions = super::settings_page::Actions::default();
        egui::SidePanel::left("home-settings-navigation")
            .exact_width(if ctx.screen_rect().width() < 720.0 { 156.0 } else { 190.0 })
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(super::theme::SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(14.0, 18.0)),
            )
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("应用设置").size(13.0).strong().color(super::theme::MUTED));
                ui.add_space(10.0);
                for (tab, label) in [
                    (super::workspace_ui::SettingsView::Software, "软件设置"),
                    (super::workspace_ui::SettingsView::About, "关于"),
                ] {
                    if ui
                        .add_sized(
                            [ui.available_width(), 40.0],
                            egui::SelectableLabel::new(view == tab, label),
                        )
                        .clicked()
                    {
                        self.workspace.route.settings = tab;
                    }
                    ui.add_space(4.0);
                }
            });
        if view == super::workspace_ui::SettingsView::Software {
            egui::TopBottomPanel::bottom("home-settings-actions")
                .exact_height(if ctx.screen_rect().height() < 520.0 { 54.0 } else { 66.0 })
                .frame(
                    egui::Frame::none()
                        .fill(super::theme::SURFACE)
                        .inner_margin(egui::Margin::symmetric(20.0, 12.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        if ui.button("恢复默认").clicked() {
                            actions.reset = true;
                        }
                        if dirty {
                            ui.label(egui::RichText::new("有未保存的更改").size(12.0).color(super::theme::MUTED));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("保存并应用").color(egui::Color32::WHITE),
                                    )
                                    .fill(super::theme::ACCENT),
                                )
                                .clicked()
                            {
                                actions.apply = true;
                            }
                            if ui.button("取消").clicked() {
                                actions.back = true;
                            }
                        });
                    });
                });
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(super::theme::CANVAS)
                    .inner_margin(if ctx.screen_rect().height() < 520.0 { 14.0 } else { 24.0 }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(("home-settings-content", view as u8))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width().min(860.0));
                        ui.heading(match view {
                            super::workspace_ui::SettingsView::Software => "软件设置",
                            super::workspace_ui::SettingsView::About => "关于 Eclipse",
                        });
                        ui.add_space(12.0);
                        if !self.settings_notice.is_empty() {
                            let error = self.settings_storage_error
                                || self.settings_notice.contains("失败")
                                || self.settings_notice.contains("无法");
                            ui.label(
                                egui::RichText::new(&self.settings_notice)
                                    .color(if error { super::theme::DANGER } else { super::theme::SUCCESS }),
                            );
                            ui.add_space(10.0);
                        }
                        match view {
                            super::workspace_ui::SettingsView::Software => {
                                super::settings_page::software(
                                    ui,
                                    &mut self.settings_draft,
                                    &mut actions,
                                );
                            }
                            super::workspace_ui::SettingsView::About => {
                                self.about_content(ui);
                            }
                        }
                    });
            });
        if actions.back {
            self.leave_settings();
        }
        if actions.reset {
            let defaults = Settings::default();
            self.settings_draft.wallpaper_enabled = defaults.wallpaper_enabled;
            self.settings_draft.wallpaper_api = defaults.wallpaper_api;
            self.settings_draft.lan_discovery = defaults.lan_discovery;
            self.settings_draft.app_autostart = defaults.app_autostart;
        }
        if actions.refresh_wallpaper && self.settings_draft.wallpaper_enabled {
            self.background.sync(&self.settings_draft, ctx);
            self.background.refresh(ctx);
        }
        if actions.apply {
            if let Err(error) = self.settings_draft.validate() {
                self.settings_storage_error = true;
                self.set_settings_notice(format!("无法应用：{error:#}"));
                return;
            }
            if self.preferences.app_autostart != self.settings_draft.app_autostart {
                if let Err(error) = super::app_startup::apply(self.settings_draft.app_autostart) {
                    self.settings_storage_error = true;
                    self.set_settings_notice(format!("无法保存软件自启动：{error:#}"));
                    return;
                }
            }
            let wallpaper_changed = self.preferences.wallpaper_api != self.settings_draft.wallpaper_api
                || self.preferences.wallpaper_enabled != self.settings_draft.wallpaper_enabled;
            self.preferences = self.settings_draft.clone();
            self.persist_preferences();
            if wallpaper_changed {
                self.background.sync(&self.preferences, ctx);
            }
        }
    }

    pub(super) fn open_stream_settings(&mut self) {
        self.snapshot_preferences();
        self.settings_draft=self.preferences.clone();
        self.stream_settings_open = true;
        self.about_open=false;
        self.native_ui.open=false;
        self.native_host.open=false;
        self.lan.host.open=false;
        self.displays.menu_open=false;
        self.stream_menu.open = false;
        self.context.memory_mut(|m| m.close_popup());
        self.release_all_keys();
        self.release_pointer_capture(&self.context.clone());
        self.context.request_repaint();
        for message in self
            .pointer
            .release_all(self.input_screen(self.selected_track))
        {
            self.send_input(message, 15);
        }
    }
    fn snapshot_preferences(&mut self) {
        self.preferences.fps = self.target_fps;
        self.preferences.bitrate = self.bitrate_idx;
        self.preferences.custom_mbps = self.custom_mbps;
        self.preferences.resolution = self.resolution_idx;
        self.preferences.hud = self.hud;
        self.preferences.floating_menu = self.stream_menu.visible;
        self.preferences.nested_shortcuts = self.nested_passthrough;
        if let Some(h) = self.controller.as_ref().filter(|h|h.stats.capabilities.allows(uu_core::capabilities::Capability::Audio)) {
            use std::sync::atomic::Ordering::Relaxed;
            self.preferences.volume = h.stats.audio.volume.load(Relaxed);
            self.preferences.muted = h.stats.audio.muted.load(Relaxed);
        }
    }
    pub(super) fn persist_preferences(&mut self) {
        self.snapshot_preferences();
        if self.is_native() {
            self.set_settings_notice("已应用到当前串流");
            return;
        }
        let result = Settings::path()
            .ok_or_else(|| anyhow::anyhow!("无法确定设置目录"))
            .and_then(|p| if self.stream_target.is_some() {
                self.preferences.save_client(&p)
            } else {
                self.preferences.save(&p)
            });
        self.settings_storage_error = result.is_err();
        let notice = match result {
            Ok(()) => "已保存应用设置".to_owned(),
            Err(error) => format!("设置保存失败：{error}"),
        };
        self.set_settings_notice(notice);
    }
    pub(super) fn stream_settings_ui(&mut self, ctx: &egui::Context) {
        if !self.stream_settings_open {return;}
        self.expire_settings_notice(ctx);
        let dirty=self.settings_draft!=self.preferences;
        let gamepad_status=self.gamepad_status();
        let child=self.stream_target.is_some();
        let actions=super::settings_page::show(ctx,&mut self.settings_draft,&mut self.settings_section,
            super::settings_page::State {
                streaming:self.stream_target.is_some(),
                can_save_profile:self.stream_target.as_ref().is_some_and(|target| matches!(target.connection,super::sessions::ConnectionTarget::NativeProfile{..})),
                dirty,
                storage_error:self.settings_storage_error,notice:&self.settings_notice,
                presentation_vsync:self.presentation_vsync,
                gamepad_status:gamepad_status.as_deref(),
            });
        if actions.back {self.workspace.discard_server_draft();self.stream_settings_open=false;ctx.request_repaint();}
        if actions.maximize {self.toggle_maximized(ctx);}
        if actions.reset {
            if child {self.settings_draft.client=ClientSettings::default();}
            else {
                let defaults=Settings::default();
                self.settings_draft.wallpaper_enabled=defaults.wallpaper_enabled;
                self.settings_draft.wallpaper_api=defaults.wallpaper_api;
                self.settings_draft.lan_discovery=defaults.lan_discovery;
                self.settings_draft.app_autostart=defaults.app_autostart;
            }
            if self.workspace.host.running() {
                self.settings_draft.lan_service_port=self.preferences.lan_service_port;
                self.settings_draft.lan_discovery_port=self.preferences.lan_discovery_port;
            }
        }
        if actions.about {self.open_about();}
        if actions.refresh_wallpaper && self.settings_draft.wallpaper_enabled {self.background.sync(&self.settings_draft,ctx);self.background.refresh(ctx);}
        if actions.host {self.stream_settings_open=false;self.open_native_host();}
        let apply=actions.apply;
        let save_profile=actions.save_profile;
        let recover=actions.recover;
        if apply {
            if let Err(error)=self.settings_draft.validate() {self.set_settings_notice(format!("无法应用：{error:#}"));return;}
            if !child && self.preferences.app_autostart!=self.settings_draft.app_autostart {
                if let Err(error)=super::app_startup::apply(self.settings_draft.app_autostart){self.set_settings_notice(format!("无法保存软件自启动：{error:#}"));return;}
            }
            let capture_changed=self.preferences.capture_values()!=self.settings_draft.capture_values();
            self.preferences=self.settings_draft.clone();
            self.release_pointer_capture(ctx);
            self.target_fps = self.preferences.fps;
            self.fps_draft = self.target_fps;
            self.bitrate_idx = self.preferences.bitrate;
            self.custom_mbps = self.preferences.custom_mbps;
            self.resolution_idx = self.preferences.resolution;
            self.hud = self.preferences.hud;
            self.stream_menu.visible = self.preferences.floating_menu;
            self.nested_passthrough = self.preferences.nested_shortcuts;
            if let Some(h) = &self.controller {
                use std::sync::atomic::Ordering::Relaxed;
                h.stats.audio.volume.store(self.preferences.volume, Relaxed);
                h.stats.audio.muted.store(self.preferences.muted, Relaxed);
            }
            if recover && self.stream_target.is_none() {
                self.snapshot_preferences();
                let result = Settings::path().ok_or_else(|| anyhow::anyhow!("无法确定设置目录"))
                    .and_then(|path|self.preferences.backup_and_save(&path));
                self.settings_storage_error = result.is_err();
                let notice = match result {
                    Ok(Some(path)) => format!("已保存。原设置已备份为 {}",path.file_name().unwrap_or_default().to_string_lossy()),
                    Ok(None) => "设置文件已恢复正常，当前设置已保存".into(),
                    Err(error) => format!("设置恢复失败：{error}"),
                };
                self.set_settings_notice(notice);
            } else { self.persist_preferences(); }
            if capture_changed {self.send_capture_settings();}
            if save_profile {
                if let Err(error) = self.save_stream_profile_settings() {
                    self.settings_storage_error = true;
                    self.set_settings_notice(format!("无法保存到设备配置：{error:#}"));
                } else {
                    self.set_settings_notice("已保存到设备配置；下次连接继续使用");
                }
            }
        }
    }

    fn save_stream_profile_settings(&mut self) -> anyhow::Result<()> {
        let id = match &self.stream_target {
            Some(super::sessions::StreamTarget {
                connection: super::sessions::ConnectionTarget::NativeProfile { id, .. },
                ..
            }) => id.clone(),
            _ => anyhow::bail!("当前串流没有可编辑的设备配置"),
        };
        let store = self
            .workspace
            .store
            .clone()
            .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))?;
        let mut profile = store.read()?.device(&id)?.clone();
        let settings = self.preferences.client.clone();
        if let Some(selected) = profile.selected_client.clone() {
            if let Some(config) = profile
                .client_configs
                .iter_mut()
                .find(|config| config.id == selected)
            {
                config.settings = settings;
            } else {
                let config = uu_core::client_settings::NamedClientSettings::new("串流设置", settings);
                profile.selected_client = Some(config.id.clone());
                profile.client_configs.push(config);
            }
        } else {
            let config = uu_core::client_settings::NamedClientSettings::new("串流设置", settings);
            profile.selected_client = Some(config.id.clone());
            profile.client_configs.push(config);
        }
        store.save_device(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_preferences_survive_reopen_without_reverting_launcher_settings() {
        let root=std::env::temp_dir().join(format!("eclipse-client-settings-{}",uuid::Uuid::new_v4()));
        let path=root.join("gui-settings.json");
        let mut stream=Settings::default();
        stream.save(&path).unwrap();
        let mut launcher=Settings::load(&path).unwrap();
        launcher.wallpaper_enabled=false;
        launcher.lan_discovery=false;
        launcher.save(&path).unwrap();
        stream.codec=CodecPreference::Hevc;
        stream.fps=120;
        stream.vsync=false;
        stream.save_client(&path).unwrap();
        let reopened=Settings::load(&path).unwrap();
        assert_eq!(reopened.client,stream.client);
        assert!(!reopened.wallpaper_enabled);
        assert!(!reopened.lan_discovery);
        std::fs::write(&path,b"broken").unwrap();
        assert!(stream.save_client(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),b"broken");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn lan_ports_persist_and_invalid_ports_preserve_the_original_file() {
        let root=std::env::temp_dir().join(format!("eclipse-port-settings-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path=root.join("gui-settings.json");
        let mut settings=Settings::default();
        assert!(serde_json::from_str::<Settings>("{}").unwrap().lan_discovery);
        settings.lan_discovery=false;
        settings.lan_service_port=51000;
        settings.lan_discovery_port=51001;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(),settings);
        let invalid=br#"{"lan_service_port":0}"#;
        std::fs::write(&path,invalid).unwrap();
        assert!(Settings::load(&path).is_err());
        assert!(settings.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),invalid);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cursor_modes_migrate_legacy_settings_and_keep_display_ownership() {
        for (json,expected) in [(r#"{}"#,CursorMode::Both),
            (r#"{"remote_cursor":false}"#,CursorMode::Both),
            (r#"{"remote_cursor":true}"#,CursorMode::Remote),
            (r#"{"cursor_mode":"local"}"#,CursorMode::Both),
            (r#"{"cursor_mode":"remote"}"#,CursorMode::Remote),
            (r#"{"cursor_mode":"both"}"#,CursorMode::Both)] {
            let settings:Settings=serde_json::from_str(json).unwrap();
            assert_eq!(settings.cursor_mode,expected);
            let saved=serde_json::to_value(&settings).unwrap();
            assert!(saved.get("remote_cursor").is_none());
            assert_eq!(serde_json::from_value::<Settings>(saved).unwrap(),settings);
        }
        let mut mode=CursorMode::Both;
        for expected in CursorMode::ALL {
            assert_eq!(mode,expected);
            assert_eq!(mode.local(),mode!=CursorMode::Remote);
            assert!(mode.remote());
            mode=mode.next();
        }
        assert_eq!(mode,CursorMode::Both);
        assert!(serde_json::from_str::<Settings>(r#"{"cursor_mode":"unknown"}"#).is_err());
    }

    #[test]
    fn automatic_save_preserves_corrupt_settings_and_requires_explicit_backup() {
        let root = std::env::temp_dir().join(format!("uu-settings-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("gui-settings.json");
        let original = br#"{"volume":"private-fixture-marker"}"#;
        std::fs::write(&path, original).unwrap();
        let mut current = Settings::default();
        current.hud = true;
        let error = current.save(&path).unwrap_err().to_string();
        assert!(!error.contains("private-fixture-marker"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let backup = current.backup_and_save(&path).unwrap().unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        assert_eq!(Settings::load(&path).unwrap(), current);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            for file in [&path, &backup] {
                assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        assert!(current.backup_and_save(&path).unwrap().is_none());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn settings_changed_by_another_window_are_rechecked_before_save() {
        let root = std::env::temp_dir().join(format!("uu-settings-recheck-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("gui-settings.json");
        Settings::default().save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        std::fs::write(&path, b"partial replacement").unwrap();
        assert!(loaded.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"partial replacement");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn settings_size_limits_and_failed_backups_do_not_replace_originals() {
        let root = std::env::temp_dir().join(format!("uu-settings-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("gui-settings.json");
        let data = vec![b' '; 1024 * 1024 + 1];
        std::fs::write(&path, &data).unwrap();
        assert!(Settings::load(&path).is_err());
        assert!(Settings::default().backup_and_save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(Settings::default().backup_and_save(&path).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn partial_settings_keep_defaults_and_repeated_save_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("uu-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("gui.json");
        std::fs::write(&p, br#"{"fps":120,"volume":500,"bitrate":99}"#).unwrap();
        let mut s = Settings::load(&p).unwrap();
        assert_eq!(s.fps, 120);
        assert_eq!(s.volume, 100);
        assert!(s.capture_keyboard);
        assert_eq!(s.display_mode,uu_core::screen_control::DisplayMode::Preserve);
        assert_eq!(s.bitrate, 4);
        s.save(&p).unwrap();
        s.fps = 90;
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).unwrap(), s);
        std::fs::write(&p, b"invalid").unwrap();
        assert!(Settings::load(&p).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
