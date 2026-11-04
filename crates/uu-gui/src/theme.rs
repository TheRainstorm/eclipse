//! Shared host/extension UI palette and accessibility rules.
pub use uu_plugin_sdk::theme::*;

use eframe::egui::{self, ThemePreference};

pub const DEFAULT_PREFERENCE: ThemePreference = ThemePreference::Light;
pub const PREFERENCES: [ThemePreference; 3] = [
    ThemePreference::Light,
    ThemePreference::Dark,
    ThemePreference::System,
];

// Keep storage local to the application. Enabling egui's serde feature merely
// for this field would unnecessarily change the shared SDK dependency graph.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(remote = "ThemePreference")]
pub enum PreferenceStorage {
    Dark,
    Light,
    System,
}

pub fn preference_label(preference: ThemePreference) -> &'static str {
    uu_i18n::text(match preference {
        ThemePreference::Light => "浅色",
        ThemePreference::Dark => "深色",
        ThemePreference::System => "跟随系统",
    })
}

pub fn color_theme_label(color: ColorTheme) -> &'static str {
    uu_i18n::text(match color {
        ColorTheme::Blue => "默认蓝",
        ColorTheme::Cyan => "青蓝",
    })
}

/// Keep the application choice independent of the current platform appearance.
/// egui resolves System from each platform ThemeChanged event; unsupported
/// platforms use the same light fallback as a fresh application configuration.
pub fn apply_preference(ctx: &egui::Context, preference: ThemePreference) {
    if update_preference(ctx, preference) { repaint_known_windows(ctx); }
}
/// Mode and color family are independent global application preferences.
/// Apply both before waking shared streaming/management viewports once.
pub fn apply_appearance(ctx: &egui::Context, preference: ThemePreference, color: ColorTheme) {
    let color_changed = apply_color_theme(ctx, color);
    let mode_changed = update_preference(ctx, preference);
    if color_changed || mode_changed { repaint_known_windows(ctx); }
}
fn update_preference(ctx: &egui::Context, preference: ThemePreference) -> bool {
    ctx.options_mut(|options| {
        let changed =
            options.theme_preference != preference || options.fallback_theme != egui::Theme::Light;
        options.theme_preference = preference;
        options.fallback_theme = egui::Theme::Light;
        changed
    })
}
fn repaint_known_windows(ctx: &egui::Context) {
    let viewports = ctx.input(|input| input.raw.viewports.keys().copied().collect::<Vec<_>>());
    for viewport in viewports { ctx.request_repaint_of(viewport); }
}

/// Appearance-only observer for standalone windows that do not own an App.
/// Its configuration always belongs to the current process/user; service data
/// and the identity whose permissions are being managed are not theme sources.
#[cfg(any(windows, test))]
#[derive(Default)]
pub struct PreferenceWatch {
    path: Option<std::path::PathBuf>,
    settings: super::settings::ApplicationWatch,
}
#[cfg(any(windows, test))]
impl PreferenceWatch {
    pub fn load_current(ctx: &egui::Context) -> Self {
        Self::for_path(ctx, super::settings::Settings::path())
    }
    fn for_path(ctx: &egui::Context, path: Option<std::path::PathBuf>) -> Self {
        let (preference, color) = path
            .as_deref()
            .and_then(|path| super::settings::Settings::load(path).ok())
            .map_or((DEFAULT_PREFERENCE, ColorTheme::default()), |settings| (settings.theme_preference, settings.color_theme));
        apply_appearance(ctx, preference, color);
        Self {
            path,
            settings: Default::default(),
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(settings) = self
            .path
            .as_deref()
            .and_then(|path| self.settings.changed(path))
        {
            apply_appearance(ctx, settings.theme_preference, settings.color_theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_platform_events_and_explicit_choices_remain_fixed() {
        let ctx = egui::Context::default();
        apply(&ctx);
        apply_preference(&ctx, ThemePreference::System);
        for system in [egui::Theme::Dark, egui::Theme::Light, egui::Theme::Dark] {
            let _ = ctx.run(
                egui::RawInput {
                    system_theme: Some(system),
                    ..Default::default()
                },
                |ctx| {
                    assert_eq!(ctx.theme(), system);
                    assert_eq!(ctx.style().visuals.dark_mode, system == egui::Theme::Dark);
                    assert_eq!(
                        palette(&ctx.style().visuals).base,
                        if system == egui::Theme::Dark {
                            DARK.base
                        } else {
                            LIGHT.base
                        }
                    );
                    assert_eq!(ctx.style().spacing.interact_size.y, CONTROL_HEIGHT);
                },
            );
        }
        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            apply_preference(&ctx, preference);
            for system in [egui::Theme::Light, egui::Theme::Dark] {
                let _ = ctx.run(
                    egui::RawInput {
                        system_theme: Some(system),
                        ..Default::default()
                    },
                    |ctx| {
                        assert_eq!(
                            ctx.theme(),
                            if preference == ThemePreference::Light {
                                egui::Theme::Light
                            } else {
                                egui::Theme::Dark
                            }
                        );
                    },
                );
            }
        }
    }

    #[test]
    fn system_mode_and_color_family_are_independent_for_each_context() {
        let cyan = egui::Context::default();
        let blue = egui::Context::default();
        apply(&cyan); apply(&blue);
        apply_appearance(&cyan, ThemePreference::System, ColorTheme::Cyan);
        apply_appearance(&blue, ThemePreference::System, ColorTheme::Blue);
        for mode in [egui::Theme::Dark, egui::Theme::Light, egui::Theme::Dark] {
            let _ = cyan.run(egui::RawInput { system_theme:Some(mode), ..Default::default() }, |ctx| {
                assert_eq!(ctx.theme(), mode);
                assert_eq!(current_color_theme(ctx), ColorTheme::Cyan);
                assert_eq!(palette(&ctx.style().visuals).accent, if mode==egui::Theme::Dark { CYAN_DARK.accent } else { CYAN_LIGHT.accent });
            });
            let _ = blue.run(egui::RawInput { system_theme:Some(mode), ..Default::default() }, |ctx| {
                assert_eq!(current_color_theme(ctx), ColorTheme::Blue);
                assert_eq!(palette(&ctx.style().visuals).accent, if mode==egui::Theme::Dark { DARK.accent } else { LIGHT.accent });
            });
        }
        apply_appearance(&cyan, ThemePreference::Light, ColorTheme::Cyan);
        assert_eq!(cyan.theme(), egui::Theme::Light);
        assert_eq!(blue.options(|o| o.theme_preference), ThemePreference::System);
        assert_eq!(current_color_theme(&blue), ColorTheme::Blue);
    }

    #[test]
    fn light_default_and_unknown_system_share_the_light_palette() {
        let ctx = egui::Context::default();
        apply(&ctx);
        apply_preference(&ctx, DEFAULT_PREFERENCE);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        apply_preference(&ctx, ThemePreference::System);
        let _ = ctx.run(Default::default(), |ctx| {
            assert_eq!(ctx.theme(), egui::Theme::Light);
            assert_eq!(palette(&ctx.style().visuals).base, LIGHT.base);
        });
        // Extensions register their shared styles without replacing the choice.
        apply(&ctx);
        assert_eq!(
            ctx.options(|options| options.theme_preference),
            ThemePreference::System
        );
    }

    #[test]
    fn changing_the_global_choice_wakes_each_known_window_once() {
        use std::sync::{Arc, Mutex};
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let child = egui::ViewportId::from_hash_of("theme-stream-window");
        for _ in 0..4 {
            let mut input = egui::RawInput::default();
            input.viewports.entry(child).or_default();
            let _ = ctx.run(input.clone(), |ctx| {
                ctx.show_viewport_deferred(child, Default::default(), |_, _| {})
            });
            input.viewport_id = child;
            let _ = ctx.run(input, |_| {});
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        ctx.set_request_repaint_callback(move |info| {
            observed.lock().unwrap().push(info.viewport_id)
        });
        let mut input = egui::RawInput::default();
        input.viewports.entry(child).or_default();
        let _ = ctx.run(input, |ctx| {
            ctx.show_viewport_deferred(child, Default::default(), |_, _| {});
            calls.lock().unwrap().clear();
            apply_preference(ctx, ThemePreference::Dark);
            let actual = calls.lock().unwrap().clone();
            assert_eq!(actual.len(), 2);
            assert!(actual.contains(&egui::ViewportId::ROOT));
            assert!(actual.contains(&child));
            calls.lock().unwrap().clear();
            apply_preference(ctx, ThemePreference::Dark);
            assert!(
                calls.lock().unwrap().is_empty(),
                "unchanged preferences must not poll native windows"
            );
        });
    }

    #[test]
    fn standalone_window_loads_and_observes_the_same_saved_theme() {
        let root =
            std::env::temp_dir().join(format!("eclipse-theme-watch-{}", uuid::Uuid::new_v4()));
        let path = root.join("gui-settings.json");
        let mut settings = super::super::settings::Settings::default();
        settings.theme_preference = ThemePreference::Dark;
        settings.color_theme = ColorTheme::Cyan;
        settings.save(&path).unwrap();
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut watch = PreferenceWatch::for_path(&ctx, Some(path.clone()));
        assert_eq!(
            ctx.theme(),
            egui::Theme::Dark,
            "management windows load the saved choice before their first frame"
        );
        assert_eq!(current_color_theme(&ctx), ColorTheme::Cyan);
        settings.theme_preference = ThemePreference::System;
        settings.color_theme = ColorTheme::Blue;
        settings.save(&path).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while ctx.options(|options| options.theme_preference) != ThemePreference::System || current_color_theme(&ctx) != ColorTheme::Blue {
            watch.poll(&ctx);
            assert!(
                std::time::Instant::now() < deadline,
                "global appearance update was not delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = ctx.run(
            egui::RawInput {
                system_theme: Some(egui::Theme::Light),
                ..Default::default()
            },
            |ctx| {
                assert_eq!(ctx.theme(), egui::Theme::Light);
            },
        );
        drop(watch);
        std::fs::remove_dir_all(root).unwrap();
    }
}
