//! Optional help belongs to the local application, never to a connection grant.
use eframe::egui;

fn show(ctx: &egui::Context, open: &mut bool) -> bool {
    let mut close = false;
    let mut local_test = false;
    egui::Window::new(uu_i18n::text("新手教程"))
        .id(egui::Id::new("getting-started"))
        .open(open)
        .collapsible(false)
        .default_width(520.)
        .max_height((ctx.screen_rect().height() - 80.).max(180.))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(uu_i18n::text("只有一台电脑也能测试"));
                ui.label(uu_i18n::text("本机串流测试通过回环连接验证采集、编解码和显示，无需另一台电脑或开启共享服务。"));
                if ui.add(super::theme::primary_button(uu_i18n::text("本机串流测试"))).clicked() {
                    local_test = true;
                    close = true;
                }
                ui.add_space(14.);
                ui.heading(uu_i18n::text("连接你的第一台电脑"));
                ui.label(uu_i18n::text("1. 在对方电脑开启 Eclipse 服务　2. 添加对方 IP 地址"));
                ui.label(uu_i18n::text("3. 首次连接完成设备配对　4. 点击开始串流"));
                ui.add_space(6.);
                ui.label(uu_i18n::text("安全连接密码为可选设置；也可以使用配对码或已授权公钥。"));
                ui.weak(uu_i18n::text("配对方式和安全连接密码可在服务管理的配对页设置。"));
                ui.add_space(14.);
                ui.label(uu_i18n::text("可在设置 → 常规中随时打开新手教程。"));
                if ui.add(super::theme::secondary_button(uu_i18n::text("关闭教程"))).clicked() {
                    close = true;
                }
            });
        });
    if close { *open = false; }
    local_test
}

impl super::App {
    pub(super) fn onboarding_ui(&mut self, ctx: &egui::Context) {
        if !self.onboarding_open { return; }
        let local_test = show(ctx, &mut self.onboarding_open);
        if !self.onboarding_open {
            self.preferences.onboarding_dismissed = true;
            self.settings_draft.onboarding_dismissed = true;
            let hint = uu_i18n::text("可在设置 → 常规中随时打开新手教程。");
            self.workspace.feedback.report(super::ui_feedback::Scope::Devices, super::ui_feedback::Kind::Info, hint);
            self.set_settings_notice(hint);
            if let Some(path) = super::settings::Settings::path() {
                if super::settings::Settings::dismiss_onboarding(&path).is_err() {
                    self.workspace.feedback.error(super::ui_feedback::Scope::Devices,
                        uu_i18n::text("教程已关闭，但偏好未能保存；原设置文件已保留。"));
                }
            }
        }
        if local_test {
            self.workspace_actions(vec![super::workspace_ui::Action::SelfStream], ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    #[test]
    fn dismissal_preserves_saved_preferences_and_cannot_be_undone_by_a_stale_window() {
        let root = std::env::temp_dir().join(format!("eclipse-guide-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        assert!(!Settings::load(&path).unwrap().onboarding_dismissed);
        let mut saved = Settings::default();
        saved.volume = 27;
        saved.vsync = true;
        saved.hud = false;
        saved.native_clipboard = false;
        saved.theme_preference = egui::ThemePreference::Dark;
        saved.save(&path).unwrap();
        Settings::dismiss_onboarding(&path).unwrap();
        let reopened = Settings::load(&path).unwrap();
        assert!(reopened.onboarding_dismissed);
        assert_eq!(reopened.client, saved.client);
        assert_eq!(reopened.theme_preference, saved.theme_preference);
        // A different, older window saves its own changes after the close.
        saved.volume = 33;
        saved.save(&path).unwrap();
        let reopened = Settings::load(&path).unwrap();
        assert!(reopened.onboarding_dismissed);
        assert_eq!(reopened.volume, 33);
        std::fs::write(&path, b"broken preferences").unwrap();
        assert!(Settings::dismiss_onboarding(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken preferences");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn guide_can_close_reopen_and_launch_a_single_computer_test() {
        fn frame(ctx: &egui::Context, open: &mut bool, events: Vec<egui::Event>) -> (bool, Vec<(String, egui::Pos2)>) {
            let mut launch = false;
            let output = ctx.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 900.))),
                events, ..Default::default()
            }, |ctx| launch = show(ctx, open));
            fn texts(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => out.push((t.galley.text().to_owned(), t.pos + t.galley.size() * 0.5)),
                    egui::Shape::Vec(v) => for s in v {texts(s, out)}, _ => {}
                }
            }
            let mut values = vec![];
            for s in output.shapes {texts(&s.shape, &mut values);}
            (launch, values)
        }
        let ctx = egui::Context::default();
        super::super::theme::apply(&ctx);
        for label in ["关闭教程", "本机串流测试"] {
            let mut open = true;
            frame(&ctx, &mut open, vec![]);
            let (_, labels) = frame(&ctx, &mut open, vec![]);
            let pos = labels.iter().find(|(s, _)| s == label).expect("tutorial action").1;
            let click = |pressed| egui::Event::PointerButton {pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default()};
            assert!(!frame(&ctx, &mut open, vec![egui::Event::PointerMoved(pos), click(true)]).0);
            let (launch, _) = frame(&ctx, &mut open, vec![click(false)]);
            assert!(!open);
            assert_eq!(launch, label == "本机串流测试");
        }
    }
}
