//! Launcher-owned storage draft and background preparation; Config stays frozen.
use super::storage_relocation::Prepared;
use eframe::egui;
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
};

type Choice = Result<Option<PathBuf>, String>;
#[derive(Clone)]
pub(super) enum Destination {
    Relocate(PathBuf),
    Existing(uu_core::Config),
}
#[derive(Default)]
pub(super) struct Storage {
    source: Option<uu_core::Config>,
    draft: String,
    existing: Option<uu_core::Config>,
    picker: Option<mpsc::Receiver<Choice>>,
    helper: Option<mpsc::Receiver<Result<Prepared, String>>>,
    notice: String,
    restarting: bool,
}
impl Storage {
    pub fn new(source: Option<&uu_core::Config>) -> Self {
        let mut value = Self {
            source: source.cloned(),
            ..Default::default()
        };
        value.discard();
        value
    }
    pub fn dirty(&self) -> bool {
        if let Some(existing) = &self.existing {
            return self.source.as_ref().is_none_or(|source| {
                source.data_dir != existing.data_dir || source.state_dir != existing.state_dir
            });
        }
        let current = self.source.as_ref().and_then(|c| c.profile_dir());
        match current {
            Some(path) => Path::new(self.draft.trim()) != path,
            None => !self.draft.trim().is_empty(),
        }
    }
    pub fn busy(&self) -> bool {
        self.picker.is_some() || self.helper.is_some() || self.restarting
    }
    pub fn saving_label(&self) -> &'static str {
        uu_i18n::text(if self.existing.is_some() && self.dirty() {
            "使用已有配置并重启"
        } else if self.dirty() {
            "保存、迁移并重启"
        } else {
            "保存并应用"
        })
    }
    pub fn discard(&mut self) {
        if self.busy() {
            return;
        }
        self.existing = None;
        self.draft = self
            .source
            .as_ref()
            .and_then(|c| c.profile_dir())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.notice.clear();
    }
    pub fn reset(&mut self) {
        if self.busy() {
            return;
        }
        #[cfg(windows)]
        match std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(|exe| uu_core::Config::default_profile_directory(&exe))
        {
            Ok(path) => {
                self.existing = None;
                self.draft = path.display().to_string();
                self.notice.clear();
            }
            Err(error) => self.notice = format!("{error:#}"),
        }
        #[cfg(not(windows))]
        self.discard();
    }
    pub fn destination(&self) -> Result<Option<Destination>, String> {
        if !self.dirty() {
            return Ok(None);
        }
        if let Some(existing) = &self.existing {
            return Ok(Some(Destination::Existing(existing.clone())));
        }
        let path = PathBuf::from(self.draft.trim());
        if !path.is_absolute() {
            return Err(uu_i18n::text("配置目录必须是完整的绝对路径").into());
        }
        Ok(Some(Destination::Relocate(path)))
    }
    pub fn begin(&mut self, destination: Destination, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let Some(source) = self.source.clone() else {
            self.notice = uu_i18n::text("无法确定当前配置，未开始迁移").into();
            return;
        };
        let (tx, rx) = mpsc::channel();
        let context = ctx.clone();
        match std::thread::Builder::new()
            .name("eclipse-profile-prepare".into())
            .spawn(move || {
                let result = match destination {
                    Destination::Relocate(path) => {
                        super::storage_relocation::prepare(&source, &path)
                    }
                    Destination::Existing(config) => {
                        super::storage_relocation::prepare_selection(&source, &config)
                    }
                }
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                context.request_repaint();
            }) {
            Ok(_) => {
                self.helper = Some(rx);
                self.notice =
                    uu_i18n::text("正在检查配置迁移，助手就绪后将关闭并重新打开 Eclipse…").into();
            }
            Err(error) => self.notice = error.to_string(),
        }
    }
    /// Only an explicit helper READY allows the launcher to bypass close-to-tray.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        if let Some(result) = self.picker.as_ref().and_then(receive) {
            self.picker = None;
            match result {
                Ok(Some(path)) => {
                    self.existing = None;
                    self.draft = path.display().to_string();
                    self.notice.clear();
                }
                Ok(None) => {}
                Err(error) => self.notice = error,
            }
        }
        if let Some(result) = self.helper.as_ref().and_then(receive) {
            self.helper = None;
            match result {
                Ok(prepared) => {
                    prepared.release();
                    self.restarting = true;
                    return true;
                }
                Err(error) => {
                    self.notice = uu_i18n::format!("配置迁移未开始，当前目录保持不变：{error}")
                }
            }
        }
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        false
    }
    fn browse(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let parent = eframe::viewport_window(ctx);
        let mut dialog = rfd::AsyncFileDialog::new().set_title(uu_i18n::text("选择配置根目录"));
        if let Some(window) = &parent {
            dialog = dialog.set_parent(window.as_ref());
        }
        if !self.draft.is_empty() {
            dialog = dialog.set_directory(&self.draft);
        }
        let (tx, rx) = mpsc::channel();
        let context = ctx.clone();
        match std::thread::Builder::new()
            .name("eclipse-profile-folder".into())
            .spawn(move || {
                let _parent = parent;
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|runtime| {
                        runtime
                            .block_on(dialog.pick_folder())
                            .map(|file| file.path().to_owned())
                    })
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
                context.request_repaint();
            }) {
            Ok(_) => self.picker = Some(rx),
            Err(error) => self.notice = error.to_string(),
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.separator();
        ui.strong(uu_i18n::text("配置存储"));
        let Some(source) = self.source.clone() else {
            ui.label(uu_i18n::text("当前配置不可用"));
            return;
        };
        ui.label(uu_i18n::text("当前设置、设备和插件数据"));
        ui.add(egui::Label::new(source.data_dir.display().to_string()).wrap());
        ui.label(uu_i18n::text("当前日志和运行状态"));
        ui.add(egui::Label::new(source.state_dir.display().to_string()).wrap());
        ui.add_space(8.0);
        ui.label(uu_i18n::text("新的配置根目录"));
        ui.add_enabled_ui(!self.busy(), |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 90.0).max(120.0);
                if ui
                    .add(
                        super::theme::Input::new(&mut self.draft)
                            .desired_width(width)
                            .hint_text(uu_i18n::text("选择绝对目录，内含 data 和 state")),
                    )
                    .changed()
                {
                    self.existing = None;
                }
                if ui
                    .add(super::theme::secondary_button(uu_i18n::text("浏览…")))
                    .clicked()
                {
                    self.browse(ui.ctx());
                }
            });
            ui.horizontal_wrapped(|ui| {
                #[cfg(windows)]
                if ui
                    .add(super::theme::secondary_button(uu_i18n::text(
                        "恢复默认目录",
                    )))
                    .clicked()
                {
                    self.reset();
                }
                #[cfg(windows)]
                if ui
                    .add(super::theme::secondary_button(uu_i18n::text(
                        "使用旧版用户配置",
                    )))
                    .clicked()
                {
                    match uu_core::Config::legacy_user_profile() {
                        Ok(config) => {
                            self.draft = config.data_dir.display().to_string();
                            self.existing = Some(config);
                            self.notice.clear();
                        }
                        Err(error) => self.notice = format!("{error:#}"),
                    }
                }
                if ui
                    .add(super::theme::secondary_button(uu_i18n::text(
                        "打开配置目录",
                    )))
                    .clicked()
                {
                    self.notice = open_directory(&source.data_dir)
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                }
            });
        });
        ui.add(egui::Label::new(uu_i18n::text("修改目录后，点击下方“保存、迁移并重启”。设置、设备配对、插件数据和日志会一起迁移，原目录保留。请先退出使用同一配置的其它串流窗口。")).wrap());
        #[cfg(windows)]
        ui.add(egui::Label::new(uu_i18n::text("默认目录为程序旁的 eclipse-data。后台服务正在使用时，须等待串流和显示器操作结束；迁移服务可能需要管理员确认，不会直接断开现有会话。")).wrap());
        if let Some(existing) = &self.existing {
            ui.add(
                egui::Label::new(uu_i18n::format!(
                    "重启后使用已有配置：{directory}",
                    directory = existing.data_dir.display()
                ))
                .wrap(),
            );
            ui.add(
                egui::Label::new(uu_i18n::text(
                    "只切换本程序的配置位置，不复制或合并数据，现有目录和后台服务配置均保留。",
                ))
                .wrap(),
            );
        }
        if !self.notice.is_empty() {
            let color = if self.busy() {
                super::theme::palette(ui.visuals()).muted
            } else {
                super::theme::palette(ui.visuals()).danger
            };
            ui.add(egui::Label::new(egui::RichText::new(&self.notice).color(color)).wrap());
        }
    }
}
fn receive<T>(rx: &mpsc::Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match rx.try_recv() {
        Ok(value) => Some(value),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            Some(Err(uu_i18n::text("配置目录后台任务已停止，请重试").into()))
        }
    }
}
fn open_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(windows)]
    let mut command = Command::new("explorer.exe");
    #[cfg(not(windows))]
    let mut command = Command::new("xdg-open");
    command.arg(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> uu_core::Config {
        let root = std::env::temp_dir().join("eclipse-storage-ui-current");
        uu_core::Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        }
    }
    #[test]
    fn storage_draft_is_separate_from_live_config_and_cancel_restores_it() {
        let source = config();
        let mut state = Storage::new(Some(&source));
        assert!(!state.dirty());
        assert!(state.destination().unwrap().is_none());
        let destination = std::env::temp_dir().join("eclipse-storage-ui-new");
        state.draft = destination.display().to_string();
        assert!(state.dirty());
        assert!(
            matches!(state.destination().unwrap(), Some(Destination::Relocate(path)) if path == destination)
        );
        assert_eq!(state.source.as_ref().unwrap().data_dir, source.data_dir);
        state.discard();
        assert!(!state.dirty());
    }
    #[test]
    fn folder_cancel_and_failed_helper_keep_the_launcher_and_current_profile() {
        let source = config();
        let mut state = Storage::new(Some(&source));
        let old = state.draft.clone();
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(None)).unwrap();
        state.picker = Some(rx);
        let ctx = egui::Context::default();
        assert!(!state.poll(&ctx));
        assert_eq!(state.draft, old);
        let (tx, rx) = mpsc::channel();
        tx.send(Err("other stream remains".into())).unwrap();
        state.helper = Some(rx);
        assert!(!state.poll(&ctx));
        assert!(!state.restarting);
        assert!(!state.busy());
        assert!(state.notice.contains("other stream remains"));
        assert_eq!(state.source.unwrap().data_dir, source.data_dir);
    }
    #[test]
    fn legacy_storage_only_changes_when_a_new_absolute_root_is_chosen() {
        let source = uu_core::Config {
            data_dir: "user-data".into(),
            state_dir: "user-state".into(),
            signaling_host: String::new(),
        };
        let mut state = Storage::new(Some(&source));
        assert!(!state.dirty());
        state.draft = "relative-profile".into();
        assert!(state.destination().is_err());
    }
    #[test]
    fn existing_profile_choice_is_a_draft_and_cancel_never_migrates_or_changes_live_paths() {
        let source = config();
        let existing = uu_core::Config {
            data_dir: std::env::temp_dir().join("eclipse-storage-ui-legacy"),
            state_dir: std::env::temp_dir().join("eclipse-storage-ui-legacy/state"),
            signaling_host: String::new(),
        };
        let mut state = Storage::new(Some(&source));
        state.existing = Some(existing.clone());
        state.draft = existing.data_dir.display().to_string();
        assert!(state.dirty());
        assert!(
            matches!(state.destination().unwrap(), Some(Destination::Existing(value))
            if value.data_dir == existing.data_dir && value.state_dir == existing.state_dir)
        );
        assert_eq!(state.source.as_ref().unwrap().data_dir, source.data_dir);
        state.discard();
        assert!(state.destination().unwrap().is_none());
        assert!(state.existing.is_none());
        assert_eq!(state.source.as_ref().unwrap().data_dir, source.data_dir);
    }
}
