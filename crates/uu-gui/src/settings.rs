//! User preferences are separate from credentials and installation files.
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub fps: u32,
    pub bitrate: usize,
    pub custom_mbps: u32,
    pub resolution: usize,
    pub volume: u32,
    pub muted: bool,
    pub hud: bool,
    pub floating_menu: bool,
    pub capture_keyboard: bool,
    pub vsync: bool,
    pub software_decoder: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate: 0,
            custom_mbps: 55,
            resolution: 0,
            volume: 100,
            muted: false,
            hud: false,
            floating_menu: true,
            capture_keyboard: true,
            vsync: true,
            software_decoder: false,
        }
    }
}
impl Settings {
    pub fn path() -> Option<PathBuf> {
        uu_core::Config::from_env()
            .ok()
            .map(|c| c.at("gui-settings.json"))
    }
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut s: Self = serde_json::from_slice(&bytes)?;
        s.fps = s.fps.clamp(5, 144);
        s.bitrate = s.bitrate.min(4);
        s.custom_mbps = s.custom_mbps.clamp(1, 100);
        s.resolution = s.resolution.min(3);
        s.volume = s.volume.min(100);
        Ok(s)
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
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
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            drop(file);
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                #[link(name = "kernel32")]
                extern "system" {
                    fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
                }
                let from: Vec<_> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
                let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            #[cfg(not(windows))]
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

impl super::App {
    pub(super) fn open_settings(&mut self) {
        self.snapshot_preferences();
        self.settings_open = true;
        self.stream_menu.open = false;
        self.context.memory_mut(|m| m.close_popup());
        self.release_all_keys();
        for message in self
            .pointer
            .release_all(self.input_screen(self.selected_track))
        {
            self.send_input(message, 15);
        }
        self.remote_focus = false;
    }
    fn snapshot_preferences(&mut self) {
        self.preferences.fps = self.target_fps;
        self.preferences.bitrate = self.bitrate_idx;
        self.preferences.custom_mbps = self.custom_mbps;
        self.preferences.resolution = self.resolution_idx;
        self.preferences.hud = self.hud;
        self.preferences.floating_menu = self.stream_menu.visible;
        if let Some(h) = &self.controller {
            use std::sync::atomic::Ordering::Relaxed;
            self.preferences.volume = h.stats.audio.volume.load(Relaxed);
            self.preferences.muted = h.stats.audio.muted.load(Relaxed);
        }
    }
    pub(super) fn persist_preferences(&mut self) {
        self.snapshot_preferences();
        self.settings_notice = match Settings::path()
            .ok_or_else(|| anyhow::anyhow!("无法确定设置目录"))
            .and_then(|p| self.preferences.save(&p))
        {
            Ok(()) => "已保存；新串流窗口使用这些默认设置".into(),
            Err(error) => format!("设置保存失败：{error}"),
        };
    }
    pub(super) fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new("软件设置").open(&mut open).collapsible(false).resizable(false)
            .default_width(440.0).anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO).show(ctx,|ui| {
                let s=&mut self.preferences;
                egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-140.0).max(180.0)).show(ui,|ui| {
                    ui.strong("画面");
                    ui.horizontal(|ui|{ui.label("目标帧率");ui.add(egui::DragValue::new(&mut s.fps).range(5..=144).suffix(" FPS"));});
                    egui::ComboBox::from_label("画质").selected_text(super::BITRATE_PRESETS[s.bitrate].0).show_ui(ui,|ui|{
                        for (i,(label,_,_)) in super::BITRATE_PRESETS.iter().enumerate(){ui.selectable_value(&mut s.bitrate,i,*label);}
                    });
                    if s.bitrate==4 {ui.add(egui::Slider::new(&mut s.custom_mbps,1..=100).text("Mbps"));}
                    let resolutions=["跟随远端分辨率","1920 × 1080","2560 × 1440","3840 × 2160"];
                    egui::ComboBox::from_label("分辨率").selected_text(resolutions[s.resolution]).show_ui(ui,|ui|{for (i,label) in resolutions.iter().enumerate(){ui.selectable_value(&mut s.resolution,i,*label);}});
                    ui.checkbox(&mut s.hud,"显示串流统计 · Ctrl+Alt+S");
                    ui.checkbox(&mut s.floating_menu,"全屏显示可隐藏的悬浮入口");
                    ui.separator();ui.strong("声音");
                    ui.checkbox(&mut s.muted,"静音");ui.add(egui::Slider::new(&mut s.volume,0..=100).text("音量"));
                    ui.separator();ui.strong("输入");
                    ui.checkbox(&mut s.capture_keyboard,"全屏时将系统快捷键发送到远端");
                    ui.small("Ctrl+Alt+Q 退出串流；Ctrl+Alt+F / X 切换全屏。\nWin+Shift+方向键始终移动本地窗口。Ctrl+Alt+Del 由 Windows 保留。");
                    ui.separator();ui.strong("启动选项");
                    ui.checkbox(&mut s.vsync,"垂直同步");ui.checkbox(&mut s.software_decoder,"使用软件解码（默认优先硬件解码）");
                    ui.small("启动选项在重新打开软件后生效，其他选项应用到当前串流并保存为默认值。");
                });
                ui.separator();
                ui.horizontal(|ui| {if ui.button("保存并应用").clicked(){apply=true;}if ui.button("恢复默认").clicked(){self.preferences=Settings::default();}});
                if !self.settings_notice.is_empty(){ui.label(&self.settings_notice);}
            });
        self.settings_open = open;
        if apply {
            self.target_fps = self.preferences.fps;
            self.fps_draft = self.target_fps;
            self.bitrate_idx = self.preferences.bitrate;
            self.custom_mbps = self.preferences.custom_mbps;
            self.resolution_idx = self.preferences.resolution;
            self.hud = self.preferences.hud;
            self.stream_menu.visible = self.preferences.floating_menu;
            if let Some(h) = &self.controller {
                use std::sync::atomic::Ordering::Relaxed;
                h.stats.audio.volume.store(self.preferences.volume, Relaxed);
                h.stats.audio.muted.store(self.preferences.muted, Relaxed);
            }
            self.persist_preferences();
            self.send_capture_settings();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
