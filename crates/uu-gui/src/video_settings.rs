//! Flat video selectors: editor mode is UI state, saved values stay unchanged.
use eframe::egui::{self, Ui};
use uu_core::client_settings::{ClientSettings, COMMON_FRAME_RATES, OTHER_FRAME_RATES, RESOLUTION_PRESETS, LOCAL_RESOLUTION, CUSTOM_RESOLUTION};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Rate { Remote, Local, Common(u32), Custom, Other }

#[derive(Clone, Copy, Debug, PartialEq)]
enum Resolution { Remote, Local, Common(usize), Custom, Other }

fn rate_choice(s: &ClientSettings) -> Rate {
    if s.follow_local_fps { Rate::Local }
    else if s.follow_remote_fps { Rate::Remote }
    else if COMMON_FRAME_RATES.contains(&s.fps) { Rate::Common(s.fps) }
    else if OTHER_FRAME_RATES.contains(&s.fps) { Rate::Other }
    else { Rate::Custom }
}

fn resolution_choice(s: &ClientSettings) -> Resolution {
    match s.resolution {
        0 => Resolution::Remote,
        LOCAL_RESOLUTION => Resolution::Local,
        1..=3 => Resolution::Common(s.resolution),
        _ if RESOLUTION_PRESETS.iter().any(|(size, _)| *size == s.custom_resolution) => Resolution::Other,
        _ => Resolution::Custom,
    }
}

fn select(ui: &Ui, id: &str, label: impl Into<egui::WidgetText>) -> crate::theme::Select {
    crate::theme::Select::from_id_salt(ui.make_persistent_id(id))
        .width(210.0_f32.min(ui.available_width()))
        .height(320.0).selected_text(label)
}

pub(super) fn frame_rate(ui: &mut Ui, s: &mut ClientSettings, max_fps: u32) {
    let stamp = (s.fps, s.follow_remote_fps, s.follow_local_fps);
    let id = ui.make_persistent_id("frame-rate-editor");
    let mut choice = ui.data_mut(|d| d.get_temp::<((u32, bool, bool), Rate)>(id))
        .filter(|(seen, _)| *seen == stamp).map_or_else(|| rate_choice(s), |(_, choice)| choice);
    ui.horizontal_wrapped(|ui| {
        let label = match choice {
            Rate::Remote => uu_i18n::text("保持远端帧率").to_owned(),
            Rate::Local => uu_i18n::text("保持本地帧率").to_owned(),
            Rate::Common(fps) => format!("{fps} FPS"),
            Rate::Custom => uu_i18n::text("自定义帧数").to_owned(),
            Rate::Other => uu_i18n::text("其它帧数").to_owned(),
        };
        let before = choice;
        select(ui, "frame-rate-mode", label).show_ui(ui, |ui| {
            crate::theme::selectable_value(ui, &mut choice, Rate::Remote, uu_i18n::text("保持远端帧率"));
            crate::theme::selectable_value(ui, &mut choice, Rate::Local, uu_i18n::text("保持本地帧率"));
            ui.separator();
            for fps in COMMON_FRAME_RATES.into_iter().filter(|fps| *fps <= max_fps) {
                crate::theme::selectable_value(ui, &mut choice, Rate::Common(fps), format!("{fps} FPS"));
            }
            crate::theme::selectable_value(ui, &mut choice, Rate::Custom, uu_i18n::text("自定义帧数"));
            crate::theme::selectable_value(ui, &mut choice, Rate::Other, uu_i18n::text("其它帧数"));
        });
        // Browsing a form never changes a saved mode or silently clamps its value.
        if choice != before {
            s.follow_remote_fps = choice == Rate::Remote;
            s.follow_local_fps = choice == Rate::Local;
            if let Rate::Common(fps) = choice { s.fps = fps; }
        }
        match choice {
            Rate::Custom => {
                crate::theme::number_input(ui, egui::DragValue::new(&mut s.fps)
                    .range(5..=max_fps.clamp(5, 240)).clamp_existing_to_range(false).suffix(" FPS"));
            }
            Rate::Other => {
                select(ui, "frame-rate-preset", format!("{} FPS", s.fps)).show_ui(ui, |ui| {
                    for fps in OTHER_FRAME_RATES.into_iter().filter(|fps| *fps <= max_fps) {
                        crate::theme::selectable_value(ui, &mut s.fps, fps, format!("{fps} FPS"));
                    }
                });
            }
            _ => {},
        }
    });
    ui.data_mut(|d| d.insert_temp(id, ((s.fps, s.follow_remote_fps, s.follow_local_fps), choice)));
}

pub(super) fn resolution(ui: &mut Ui, s: &mut ClientSettings) {
    let stamp = (s.resolution, s.custom_resolution);
    let id = ui.make_persistent_id("resolution-editor");
    let mut choice = ui.data_mut(|d| d.get_temp::<((usize, [u32; 2]), Resolution)>(id))
        .filter(|(seen, _)| *seen == stamp).map_or_else(|| resolution_choice(s), |(_, choice)| choice);
    ui.horizontal_wrapped(|ui| {
        let label = match choice {
            Resolution::Remote => uu_i18n::text("保持远端分辨率"),
            Resolution::Local => uu_i18n::text("保持本地分辨率"),
            Resolution::Common(1) => "1920 × 1080",
            Resolution::Common(2) => "2560 × 1440",
            Resolution::Common(_) => "3840 × 2160",
            Resolution::Custom => uu_i18n::text("自定义分辨率"),
            Resolution::Other => uu_i18n::text("其它分辨率"),
        };
        let before = choice;
        select(ui, "resolution-mode", label).show_ui(ui, |ui| {
            for (value, label) in [
                (Resolution::Remote, uu_i18n::text("保持远端分辨率")),
                (Resolution::Local, uu_i18n::text("保持本地分辨率")),
                (Resolution::Common(1), "1920 × 1080"),
                (Resolution::Common(2), "2560 × 1440"),
                (Resolution::Common(3), "3840 × 2160"),
                (Resolution::Custom, uu_i18n::text("自定义分辨率")),
                (Resolution::Other, uu_i18n::text("其它分辨率")),
            ] {
                crate::theme::selectable_value(ui, &mut choice, value, label);
            }
        });
        if choice != before {
            s.resolution = match choice {
                Resolution::Remote => 0,
                Resolution::Local => LOCAL_RESOLUTION,
                Resolution::Common(index) => index,
                Resolution::Custom | Resolution::Other => CUSTOM_RESOLUTION,
            };
        }
        match choice {
            Resolution::Other => {
                let label = RESOLUTION_PRESETS.iter().find(|(size, _)| *size == s.custom_resolution)
                    .map_or_else(|| format!("{} × {}", s.custom_resolution[0], s.custom_resolution[1]), |(_, label)| (*label).to_owned());
                select(ui, "resolution-preset", label).show_ui(ui, |ui| {
                    for (size, label) in RESOLUTION_PRESETS {
                        crate::theme::selectable_value(ui, &mut s.custom_resolution, *size, *label);
                    }
                });
            }
            Resolution::Custom => {
                ui.horizontal(|ui| {
                    for axis in 0..2 {
                        if axis == 1 { ui.label("×"); }
                        crate::theme::number_input(ui, egui::DragValue::new(&mut s.custom_resolution[axis])
                            .range(2..=8192).clamp_existing_to_range(false).speed(2.).max_decimals(0));
                    }
                });
            }
            _ => {},
        }
    });
    ui.data_mut(|d| d.insert_temp(id, ((s.resolution, s.custom_resolution), choice)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Form { ctx: egui::Context, settings: ClientSettings, resolution: bool, both: bool, time: f64, max_fps: u32 }
    impl Form {
        fn new(resolution: bool, color: crate::theme::ColorTheme) -> Self {
            let ctx = egui::Context::default();
            crate::theme::apply_color_theme(&ctx, color);
            Self { ctx, settings: ClientSettings::default(), resolution, both: false, time: 0., max_fps: 240 }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> HashMap<String, egui::Rect> {
            self.time += 0.05;
            let output = self.ctx.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(560., 480.))),
                events, focused: true, time: Some(self.time), ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if self.both {
                        egui::Grid::new("video-fields").spacing([24., 14.]).show(ui, |ui| {
                            ui.label("目标帧率"); frame_rate(ui, &mut self.settings, self.max_fps); ui.end_row();
                            ui.label("输出分辨率"); resolution(ui, &mut self.settings); ui.end_row();
                        });
                    }
                    else if self.resolution { resolution(ui, &mut self.settings); }
                    else { frame_rate(ui, &mut self.settings, self.max_fps); }
                });
            });
            fn collect(shape: &egui::Shape, clip: egui::Rect, text: &mut HashMap<String, egui::Rect>) {
                match shape {
                    egui::Shape::Text(value) => {
                        let rect = egui::Rect::from_min_size(value.pos, value.galley.size());
                        if clip.contains(rect.center()) { text.insert(value.galley.text().to_owned(), rect); }
                    }
                    egui::Shape::Vec(shapes) => for shape in shapes { collect(shape, clip, text); },
                    _ => {},
                }
            }
            let mut text = HashMap::new();
            for shape in output.shapes { collect(&shape.shape, shape.clip_rect, &mut text); }
            self.ctx.memory(|memory| {
                for layer in memory.areas().visible_layer_ids() {
                    if layer.order == egui::Order::Foreground {
                        if let Some(rect) = memory.area_rect(layer.id) {
                            assert!(rect.width() <= 320., "popup expanded beyond its selector: {rect:?}");
                        }
                    }
                }
            });
            text
        }
        fn click(&mut self, label: &str) {
            self.frame(vec![]);
            let text = self.frame(vec![]);
            let pos = text.get(label).unwrap_or_else(|| panic!("missing visible {label}; labels={:?}", text.keys())).center();
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default()
                }]);
            }
            self.frame(vec![]);
        }
    }

    #[test]
    fn frame_rate_presets_and_custom_are_separate_flat_selectors() {
        uu_i18n::with_language(uu_i18n::Language::Chinese, || {
            for color in crate::theme::COLOR_THEMES {
                let mut form = Form::new(false, color);
                form.click("保持远端帧率"); form.click("其它帧数");
                form.click("60 FPS"); form.click("24 FPS");
                assert_eq!(form.settings.fps, 24);
                assert!(!form.settings.follow_remote_fps && !form.settings.follow_local_fps);
                form.click("其它帧数"); form.click("60 FPS");
                form.click("60 FPS"); form.click("自定义帧数");
                assert_eq!(form.settings.fps, 60);
                for _ in 0..3 { assert!(form.frame(vec![]).contains_key("自定义帧数")); }
            }
        });
    }

    #[test]
    fn resolution_preset_and_custom_edit_preserve_the_same_saved_size() {
        uu_i18n::with_language(uu_i18n::Language::Chinese, || {
            for color in crate::theme::COLOR_THEMES {
                let mut form = Form::new(true, color);
                form.click("保持远端分辨率"); form.click("其它分辨率");
                form.click("1920 × 1080"); form.click("360p · 640 × 360");
                assert_eq!(form.settings.resolution, CUSTOM_RESOLUTION);
                assert_eq!(form.settings.resolved_bounds(Default::default()), Some([640, 360]));
                assert!(!form.frame(vec![]).contains_key("×"), "preset is not a custom input editor");
                form.click("其它分辨率"); form.click("自定义分辨率");
                assert!(form.frame(vec![]).contains_key("×"));
                assert_eq!(form.settings.custom_resolution, [640, 360]);
                form.click("自定义分辨率"); form.click("保持本地分辨率");
                assert_eq!(form.settings.resolution, LOCAL_RESOLUTION);
            }
        });
    }

    #[test]
    fn opening_forms_preserves_nonpreset_values_even_above_the_current_rate_cap() {
        let mut form = Form::new(false, crate::theme::ColorTheme::SkyBlue);
        form.max_fps = 90;
        form.settings.fps = 177;
        form.settings.follow_remote_fps = false;
        form.settings.resolution = CUSTOM_RESOLUTION;
        form.settings.custom_resolution = [3458, 1450];
        let before = form.settings.clone();
        for _ in 0..3 { form.frame(vec![]); }
        form.resolution = true;
        for _ in 0..3 { form.frame(vec![]); }
        assert_eq!(form.settings, before);
    }

    #[test]
    fn custom_resolution_dimensions_stay_together_beside_a_custom_frame_rate() {
        for color in crate::theme::COLOR_THEMES {
            let mut form = Form::new(true, color);
            form.both = true;
            form.settings.follow_remote_fps = false;
            form.settings.fps = 177;
            form.settings.resolution = CUSTOM_RESOLUTION;
            form.settings.custom_resolution = [3458, 1450];
            for _ in 0..4 { form.frame(vec![]); }
            let text = form.frame(vec![]);
            let row = text["×"].center().y;
            for label in ["3458", "1450"] {
                assert!((text[label].center().y-row).abs() < 2., "width and height split across rows: {text:?}");
            }
        }
    }
}
