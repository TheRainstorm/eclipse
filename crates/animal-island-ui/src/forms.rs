//! Native egui editing and selection, with island-theme form surfaces.
use crate::tokens::{self, COMPACT_HEIGHT, CONTROL_HEIGHT, MOTION_SECONDS, PILL_RADIUS};
use egui::{
    self, InnerResponse, Response, Sense, Stroke, TextStyle, Ui, Widget, WidgetInfo, WidgetText,
    WidgetType,
};

fn height(compact: bool) -> f32 {
    if compact {
        COMPACT_HEIGHT
    } else {
        CONTROL_HEIGHT
    }
}

fn form_style(ui: &mut Ui, compact: bool) {
    let p = *tokens::tokens(ui);
    ui.spacing_mut().interact_size.y = height(compact);
    let enabled = ui.is_enabled();
    let v = ui.visuals_mut();
    v.extreme_bg_color = if enabled { p.control } else { p.disabled };
    v.override_text_color = Some(if enabled { p.text } else { p.muted });
    for state in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        state.rounding = egui::Rounding::same(PILL_RADIUS);
        state.fg_stroke = Stroke::new(1.0, p.text);
    }
    v.widgets.inactive.bg_fill = p.control;
    v.widgets.inactive.weak_bg_fill = p.control;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.control_border);
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.5, p.accent);
    v.widgets.active.bg_fill = p.control;
    v.widgets.active.weak_bg_fill = p.control;
    v.widgets.active.bg_stroke = Stroke::new(2.0, p.focus);
    v.widgets.open.bg_fill = p.control;
    v.widgets.open.weak_bg_fill = p.control;
    v.widgets.open.bg_stroke = Stroke::new(2.0, p.focus);
}

/// Single-line native TextEdit: cursor, selection, IME and clipboard stay in egui.
#[must_use = "add this widget with ui.add or ui.add_enabled"]
pub struct Input<'a> {
    value: &'a mut String,
    hint: WidgetText,
    width: Option<f32>,
    password: bool,
    compact: bool,
}
impl<'a> Input<'a> {
    pub fn new(value: &'a mut String) -> Self {
        Self {
            value,
            hint: WidgetText::default(),
            width: None,
            password: false,
            compact: false,
        }
    }
    pub fn hint_text(mut self, hint: impl Into<WidgetText>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn desired_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    pub fn password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}
impl Widget for Input<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        ui.scope(|ui| {
            form_style(ui, self.compact);
            // Inputs have no resting border or drop shadow; a small warm
            // underside appears only on hover, behind the native field.
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
            ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::NONE;
            let shadow = ui.painter().add(egui::Shape::Noop);
            let mut input = egui::TextEdit::singleline(self.value)
                .hint_text(self.hint)
                .password(self.password)
                .margin(egui::Margin::symmetric(16.0, 0.0))
                .vertical_align(egui::Align::Center)
                .min_size(egui::vec2(0.0, height(self.compact)));
            if let Some(width) = self.width {
                input = input.desired_width(width);
            }
            let response = ui.add(input);
            let outer = response.rect.expand2(egui::vec2(16.0, 0.0));
            if response.hovered() && !response.has_focus() && ui.is_enabled() {
                ui.painter().set(
                    shadow,
                    egui::epaint::RectShape::filled(
                        outer.translate(egui::vec2(0.0, 2.0)),
                        PILL_RADIUS,
                        tokens::tokens(ui).shadow,
                    ),
                );
            }
            if response.has_focus() && ui.is_enabled() {
                // Keep native selection colors intact: TextEdit uses the same
                // selection stroke for its built-in focus frame.
                ui.painter().rect_stroke(
                    outer,
                    PILL_RADIUS,
                    Stroke::new(2.0, tokens::tokens(ui).focus),
                );
            }
            response
        })
        .inner
    }
}

/// Checkbox semantics with an animated switch face. Sense::click keeps egui's
/// normal tab focus, Space/Enter and accessibility activation on one path.
#[must_use = "add this widget with ui.add or ui.add_enabled"]
pub struct Switch<'a> {
    value: &'a mut bool,
    label: WidgetText,
    compact: bool,
}
impl<'a> Switch<'a> {
    pub fn new(value: &'a mut bool, label: impl Into<WidgetText>) -> Self {
        Self {
            value,
            label: label.into(),
            compact: false,
        }
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}
impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = *tokens::tokens(ui);
        let track_size = if self.compact {
            egui::vec2(44.0, 24.0)
        } else {
            egui::vec2(52.0, 28.0)
        };
        let gap = ui.spacing().icon_spacing;
        let label = if self.label.is_empty() {
            None
        } else {
            Some(self.label.into_galley(
                ui,
                None,
                (ui.available_width() - track_size.x - gap).max(0.0),
                TextStyle::Button,
            ))
        };
        let label_size = label
            .as_ref()
            .map_or(egui::Vec2::ZERO, |label| label.size());
        let width = track_size.x
            + if label.is_some() {
                gap + label_size.x
            } else {
                0.0
            };
        let (rect, mut response) = ui.allocate_exact_size(
            egui::vec2(width, height(self.compact).max(label_size.y)),
            Sense::click(),
        );
        if ui.is_enabled() && response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Checkbox,
                ui.is_enabled(),
                *self.value,
                label.as_ref().map_or("", |label| label.text()),
            )
        });
        // egui requests repaints only while this 200 ms transition is active.
        let position = ui.ctx().animate_bool_with_time(
            response.id.with("island-switch"),
            *self.value,
            MOTION_SECONDS,
        );
        if ui.is_rect_visible(rect) {
            let track = egui::Rect::from_center_size(
                egui::pos2(rect.left() + track_size.x * 0.5, rect.center().y),
                track_size,
            );
            let enabled = ui.is_enabled();
            let fill = if !enabled {
                p.disabled
            } else {
                egui::Color32::from(egui::lerp(
                    egui::Rgba::from(p.switch_off)..=egui::Rgba::from(p.switch_on),
                    position,
                ))
            };
            let border = if response.has_focus() && enabled {
                Stroke::new(2.5, p.focus)
            } else if response.hovered() && enabled {
                Stroke::new(2.5, p.accent)
            } else {
                Stroke::new(2.5, p.control_border)
            };
            ui.painter().rect(track, PILL_RADIUS, fill, border);
            if enabled {
                let inset = track.shrink(2.5);
                let r = inset.height() * 0.5;
                let mut upper = Vec::with_capacity(18);
                for step in 0..=8 {
                    let angle =
                        std::f32::consts::PI + step as f32 * std::f32::consts::FRAC_PI_2 / 8.0;
                    upper.push(egui::pos2(
                        inset.left() + r + r * angle.cos(),
                        inset.center().y + r * angle.sin(),
                    ));
                }
                for step in 0..=8 {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + step as f32 * std::f32::consts::FRAC_PI_2 / 8.0;
                    upper.push(egui::pos2(
                        inset.right() - r + r * angle.cos(),
                        inset.center().y + r * angle.sin(),
                    ));
                }
                ui.painter().with_clip_rect(inset).add(egui::Shape::line(
                    upper,
                    Stroke::new(2.0, p.shadow.gamma_multiply(0.45)),
                ));
            }
            let radius = track.height() * 0.5 - 3.0;
            let x = egui::lerp(
                (track.left() + radius + 3.0)..=(track.right() - radius - 3.0),
                position,
            );
            ui.painter().circle(
                egui::pos2(x, track.center().y),
                radius,
                if enabled { p.control } else { p.border },
                Stroke::new(2.5, p.control_border),
            );
            if let Some(label) = label {
                ui.painter().galley(
                    egui::pos2(track.right() + gap, rect.center().y - label.size().y * 0.5),
                    label,
                    if enabled { p.text } else { p.muted },
                );
            }
        }
        response
    }
}

/// Native ComboBox popup, navigation and enabled state with a pill trigger.
#[must_use = "call show_ui to display the select"]
pub struct Select {
    combo: egui::ComboBox,
    compact: bool,
}
impl Select {
    pub fn from_id_salt(id: impl std::hash::Hash) -> Self {
        Self {
            combo: egui::ComboBox::from_id_salt(id),
            compact: false,
        }
    }
    pub fn selected_text(mut self, text: impl Into<WidgetText>) -> Self {
        self.combo = self.combo.selected_text(text);
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.combo = self.combo.width(width);
        self
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
    pub fn show_ui<R>(
        self,
        ui: &mut Ui,
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        ui.scope(|ui| {
            form_style(ui, self.compact);
            self.combo.show_ui(ui, contents)
        })
        .inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        mut body: impl FnMut(&mut Ui),
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 400.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| body(ui));
            },
        )
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn pointer(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn input_keeps_native_text_paste_and_ime_composition() {
        let ctx = egui::Context::default();
        let mut value = String::new();
        let mut id = egui::Id::NULL;
        let _ = frame(&ctx, 0.0, vec![], |ui| {
            id = ui
                .add(
                    Input::new(&mut value)
                        .hint_text("Type")
                        .desired_width(240.0),
                )
                .id;
        });
        ctx.memory_mut(|memory| memory.request_focus(id));
        let _ = frame(
            &ctx,
            0.1,
            vec![
                egui::Event::Text("x".into()),
                egui::Event::Paste("é".into()),
            ],
            |ui| {
                ui.add(
                    Input::new(&mut value)
                        .hint_text("Type")
                        .desired_width(240.0),
                );
            },
        );
        assert_eq!(value, "xé");
        let _ = frame(
            &ctx,
            0.2,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("ni".into())),
            ],
            |ui| {
                ui.add(Input::new(&mut value));
            },
        );
        let _ = frame(
            &ctx,
            0.3,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("你".into())),
                egui::Event::Ime(egui::ImeEvent::Disabled),
            ],
            |ui| {
                ui.add(Input::new(&mut value));
            },
        );
        assert_eq!(value, "xé你");
    }

    #[test]
    fn switch_uses_native_space_enter_and_disabled_pointer_semantics() {
        let ctx = egui::Context::default();
        let mut value = false;
        let mut id = egui::Id::NULL;
        let _ = frame(&ctx, 0.0, vec![], |ui| {
            id = ui.add(Switch::new(&mut value, "Sound")).id;
        });
        ctx.memory_mut(|memory| memory.request_focus(id));
        let _ = frame(&ctx, 0.1, vec![key(egui::Key::Space)], |ui| {
            assert!(ui.add(Switch::new(&mut value, "Sound")).changed());
        });
        assert!(value);
        let _ = frame(&ctx, 0.2, vec![key(egui::Key::Enter)], |ui| {
            assert!(ui.add(Switch::new(&mut value, "Sound")).changed());
        });
        assert!(!value);

        let disabled = egui::Context::default();
        let mut rect = egui::Rect::NOTHING;
        let _ = frame(&disabled, 0.0, vec![], |ui| {
            rect = ui.add_enabled(false, Switch::new(&mut value, "Sound")).rect;
        });
        let _ = frame(&disabled, 0.1, pointer(rect.center(), true), |ui| {
            ui.add_enabled(false, Switch::new(&mut value, "Sound"));
        });
        let _ = frame(&disabled, 0.2, pointer(rect.center(), false), |ui| {
            let response = ui.add_enabled(false, Switch::new(&mut value, "Sound"));
            assert!(!response.changed());
            assert!(!response.has_focus());
        });
        assert!(!value);
        assert_eq!(disabled.memory(|memory| memory.focused()), None);
    }

    #[test]
    fn settled_switch_does_not_keep_requesting_animation_repaints() {
        let ctx = egui::Context::default();
        let mut value = false;
        for time in [0.0, 0.1, 0.2] {
            let _ = frame(&ctx, time, vec![], |ui| {
                ui.add(Switch::new(&mut value, ""));
            });
        }
        value = true;
        let _ = frame(&ctx, 0.3, vec![], |ui| {
            ui.add(Switch::new(&mut value, ""));
        });
        let moving = frame(&ctx, 0.4, vec![], |ui| {
            ui.add(Switch::new(&mut value, ""));
        });
        assert_eq!(
            moving.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            std::time::Duration::ZERO
        );
        for time in [0.6, 0.7] {
            let _ = frame(&ctx, time, vec![], |ui| {
                ui.add(Switch::new(&mut value, ""));
            });
        }
        let settled = frame(&ctx, 0.8, vec![], |ui| {
            ui.add(Switch::new(&mut value, ""));
        });
        assert!(
            settled.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                > std::time::Duration::from_secs(1)
        );
    }
}
