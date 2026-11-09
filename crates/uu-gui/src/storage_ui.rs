//! Explain the actual store used by this process, without switching a live
//! process's identity or copying credentials while other windows can write.
use eframe::egui;
use std::{io, path::Path, process::Command};

pub fn show(ui: &mut egui::Ui) {
    ui.separator();
    ui.strong(uu_i18n::text("配置存储"));
    let config = match uu_core::Config::from_env() {
        Ok(config) => config,
        Err(error) => { ui.colored_label(ui.visuals().error_fg_color, error.to_string()); return; },
    };
    ui.label(uu_i18n::text(if config.profile_dir().is_some() { "独立配置（便携模式）" } else { "当前用户配置" }));
    ui.label(uu_i18n::text("设置、设备和插件数据"));
    ui.add(egui::Label::new(config.data_dir.display().to_string()).wrap());
    ui.label(uu_i18n::text("日志和运行状态"));
    ui.add(egui::Label::new(config.state_dir.display().to_string()).wrap());
    let id = ui.id().with("storage-open-error");
    let mut error = ui.ctx().data_mut(|data| data.get_temp::<String>(id)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        if ui.add(super::theme::secondary_button(uu_i18n::text("打开配置目录"))).clicked() {
            error = open_directory(&config.data_dir).err().map(|e| e.to_string()).unwrap_or_default();
        }
        if ui.add(super::theme::secondary_button(uu_i18n::text("打开程序目录"))).clicked() {
            error = std::env::current_exe().and_then(|p| {
                open_directory(p.parent().ok_or_else(|| io::Error::other("application directory unavailable"))?)
            }).err().map(|e| e.to_string()).unwrap_or_default();
        }
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, error.clone()));
    if !error.is_empty() { ui.colored_label(ui.visuals().error_fg_color, error); }
    ui.add(egui::Label::new(uu_i18n::text("体验首次使用：退出该目录的程序，在程序旁新建空的 eclipse-data 文件夹，然后重新打开。各程序目录的配置独立，原用户配置会保留。")).wrap());
    ui.add(egui::Label::new(uu_i18n::text("迁移便携配置时，退出此配置的所有窗口，再复制整个程序目录；升级时保留 eclipse-data 文件夹。")).wrap());
    #[cfg(windows)]
    ui.add(egui::Label::new(uu_i18n::text("每份配置都有独立的服务端设置和配对，随 data/server 一起保存。一台电脑同时运行一个服务端；在服务管理中切换配置，有串流时请先断开。复制目录前先停止本配置的服务。")).wrap());
}

fn open_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(windows)]
    let mut command = Command::new("explorer.exe");
    #[cfg(not(windows))]
    let mut command = Command::new("xdg-open");
    command.arg(path);
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    command.spawn().map(|_| ())
}
