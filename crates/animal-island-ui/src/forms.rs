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
    edit: egui::TextEdit<'a>,
    width: Option<f32>,
    compact: bool,
    shadow: bool,
}
impl<'a> Input<'a> {
    pub fn new(value: &'a mut String) -> Self {
        Self::from_text_edit(egui::TextEdit::singleline(value))
    }
    pub fn multiline(value: &'a mut String) -> Self {
        Self::from_text_edit(egui::TextEdit::multiline(value))
    }
    pub fn from_text_edit(edit: egui::TextEdit<'a>) -> Self {
        Self {
            edit,
            width: None,
            compact: false,
            shadow: false,
        }
    }
    pub fn hint_text(mut self, hint: impl Into<WidgetText>) -> Self {
        self.edit = self.edit.hint_text(hint);
        self
    }
    /// Desired outside width, including the pill's horizontal inset.
    pub fn desired_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    pub fn password(mut self, on: bool) -> Self {
        self.edit = self.edit.password(on);
        self
    }
    pub fn compact(mut self, on: bool) -> Self {
        self.compact = on;
        self
    }
    pub fn shadow(mut self, on: bool) -> Self {
        self.shadow = on;
        self
    }
    pub fn char_limit(mut self, limit: usize) -> Self {
        self.edit = self.edit.char_limit(limit);
        self
    }
    pub fn interactive(mut self, on: bool) -> Self {
        self.edit = self.edit.interactive(on);
        self
    }
    pub fn font(mut self, font: impl Into<egui::FontSelection>) -> Self {
        self.edit = self.edit.font(font);
        self
    }
    pub fn id(mut self, id: egui::Id) -> Self {
        self.edit = self.edit.id(id);
        self
    }
    pub fn desired_rows(mut self, rows: usize) -> Self {
        self.edit = self.edit.desired_rows(rows);
        self
    }
}
impl Widget for Input<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        ui.scope(|ui| {
            form_style(ui, self.compact);
            let p = *tokens::tokens(ui);
            let background = ui.painter().add(egui::Shape::Noop);
            let inset = egui::vec2(18., 8.);
            let mut edit = self
                .edit
                .frame(false)
                .margin(egui::Margin::symmetric(inset.x, inset.y))
                .vertical_align(egui::Align::Center)
                .min_size(egui::vec2(0., height(self.compact) - inset.y * 2.));
            if let Some(width) = self.width {
                edit = edit.desired_width((width.min(ui.available_width()) - inset.x * 2.).max(1.));
            }
            let mut response = ui.add(edit);
            // TextEdit owns selection/IME and reports its text rectangle. Its
            // reserved margin forms the pill, not an extra external allocation.
            let outer = response.rect.expand2(inset);
            response.rect = outer;
            let hover = ui.ctx().animate_bool_with_time(
                response.id.with("input-hover"),
                response.hovered() && ui.is_enabled(),
                MOTION_SECONDS,
            );
            let focus = ui.ctx().animate_bool_with_time(
                response.id.with("input-focus"),
                response.has_focus() && ui.is_enabled(),
                MOTION_SECONDS,
            );
            let enabled = ui.is_enabled();
            let fill = if enabled {
                p.control.lerp_to_gamma(p.hover, hover * 0.35)
            } else {
                p.disabled
            };
            let mut shapes = Vec::new();
            // Upstream defaults shadow=false; explicitly requested elevation
            // remains finite and never changes text layout or hit geometry.
            if self.shadow && enabled {
                shapes.push(
                    egui::epaint::Shadow {
                        offset: egui::vec2(0., 2. + hover),
                        blur: 4. + hover * 4.,
                        spread: 0.,
                        color: p.shadow.gamma_multiply(0.2),
                    }
                    .as_shape(outer, PILL_RADIUS)
                    .into(),
                );
            }
            shapes.push(egui::Shape::rect_filled(outer, PILL_RADIUS, fill));
            if focus > 0. {
                shapes.push(egui::Shape::rect_stroke(
                    outer.expand(2.),
                    PILL_RADIUS,
                    Stroke::new(2., p.focus.gamma_multiply(focus)),
                ));
            }
            ui.painter().set(background, egui::Shape::Vec(shapes));
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
    hint: WidgetText,
    full_width: bool,
    trailing: bool,
}
impl<'a> Switch<'a> {
    pub fn new(value: &'a mut bool, label: impl Into<WidgetText>) -> Self {
        Self {
            value,
            label: label.into(),
            compact: false,
            hint: WidgetText::default(),
            full_width: false,
            trailing: false,
        }
    }
    pub fn compact(mut self, on: bool) -> Self {
        self.compact = on;
        self
    }
    pub fn hint_text(mut self, hint: impl Into<WidgetText>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn full_width(mut self, on: bool) -> Self {
        self.full_width = on;
        self
    }
    pub fn trailing(mut self, on: bool) -> Self {
        self.trailing = on;
        self
    }
}
impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = *tokens::tokens(ui);
        let enabled = ui.is_enabled();
        let gap = 10.;
        let size = if self.compact {
            egui::vec2(38., 20.)
        } else {
            egui::vec2(52., 28.)
        };
        let hint = self.hint.into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            ui.available_width() * 0.4,
            TextStyle::Small,
        );
        let label = self.label.into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (ui.available_width() - size.x - hint.size().x - 3. * gap).max(1.),
            TextStyle::Button,
        );
        let natural = size.x
            + if label.is_empty() {
                0.
            } else {
                gap + label.size().x
            }
            + if hint.is_empty() {
                0.
            } else {
                gap + hint.size().x
            };
        let width = if self.full_width {
            ui.available_width()
        } else {
            natural
        };
        let (rect, mut response) = ui.allocate_exact_size(
            egui::vec2(width, height(self.compact).max(label.size().y)),
            Sense::click(),
        );
        if enabled && response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::Checkbox, enabled, *self.value, label.text())
        });
        let position = ui.ctx().animate_bool_with_time(
            response.id.with("island-switch"),
            *self.value,
            MOTION_SECONDS,
        );
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("switch-hover"),
            enabled && response.hovered(),
            MOTION_SECONDS,
        );
        let press = ui.ctx().animate_bool_with_time(
            response.id.with("switch-press"),
            enabled && response.is_pointer_button_down_on(),
            0.12,
        );
        if ui.is_rect_visible(rect) {
            if self.full_width {
                ui.painter().rect_filled(
                    rect,
                    12.,
                    egui::Color32::TRANSPARENT.lerp_to_gamma(p.tint, hover),
                );
            }
            let track = egui::Rect::from_center_size(
                egui::pos2(
                    if self.trailing {
                        rect.right() - size.x * 0.5 - 8.
                    } else {
                        rect.left() + size.x * 0.5
                    },
                    rect.center().y,
                ),
                size,
            );
            let on = p
                .switch_on
                .lerp_to_gamma(p.switch_on.gamma_multiply(0.94), hover.max(press));
            let fill = if enabled {
                p.switch_off.lerp_to_gamma(on, position)
            } else {
                p.disabled
            };
            let border = if enabled {
                p.control_border.lerp_to_gamma(p.switch_on_border, position)
            } else {
                p.border
            };
            ui.painter()
                .rect(track, PILL_RADIUS, fill, Stroke::new(2.5, border));
            // The CSS reference uses an inset upper shadow, never elevation.
            if enabled {
                let inset = track.shrink(2.5);
                let top = egui::Rect::from_min_size(
                    inset.min,
                    egui::vec2(inset.width(), inset.height() * 0.46),
                );
                ui.painter().with_clip_rect(top).rect_stroke(
                    inset.translate(egui::vec2(0., 1.5)),
                    PILL_RADIUS,
                    Stroke::new(2., p.shadow.gamma_multiply(0.28)),
                );
            }
            let radius = if self.compact { 7. } else { 10.5 };
            let center = egui::pos2(
                egui::lerp(
                    (track.left() + radius + 3.5)..=(track.right() - radius - 3.5),
                    position,
                ),
                track.center().y,
            );
            let handle = egui::Rect::from_center_size(
                center,
                egui::vec2(2. * radius + press * 2., 2. * radius),
            );
            ui.painter().rect(
                handle,
                PILL_RADIUS,
                if enabled { p.control } else { p.border },
                Stroke::new(2.5, border),
            );
            if response.has_focus() && enabled {
                ui.painter()
                    .rect_stroke(track.expand(3.), PILL_RADIUS, Stroke::new(2., p.focus));
            }
            let text_x = if self.trailing {
                rect.left() + 8.
            } else {
                track.right() + gap
            };
            ui.painter().galley_with_override_text_color(
                egui::pos2(text_x, rect.center().y - label.size().y * 0.5),
                label,
                if enabled { p.text } else { p.muted },
            );
            if !hint.is_empty() {
                let right = if self.trailing {
                    track.left() - gap
                } else {
                    rect.right() - 8.
                };
                ui.painter().galley_with_override_text_color(
                    egui::pos2(right - hint.size().x, rect.center().y - hint.size().y * 0.5),
                    hint,
                    p.muted,
                );
            }
        }
        if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
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
    pub fn from_combo(combo: egui::ComboBox) -> Self {
        Self {
            combo,
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
            let result = self.combo.show_ui(ui, |ui| {
                // A narrow arrow trigger is not the width of its menu. Keep
                // native Extend sizing and a usable minimum for shared rows.
                ui.set_min_width(ui.spacing().combo_width);
                contents(ui)
            });
            let response = &result.response;
            let p = *tokens::tokens(ui);
            let open = egui::ComboBox::is_open(ui.ctx(), response.id);
            let ring = ui.ctx().animate_bool_with_time(
                response.id.with("select-ring"),
                ui.is_enabled() && (response.hovered() || response.has_focus() || open),
                MOTION_SECONDS,
            );
            if ring > 0. {
                ui.painter().rect_stroke(
                    response.rect.expand(2.),
                    PILL_RADIUS,
                    Stroke::new(2., p.focus.gamma_multiply(ring)),
                );
            }
            result
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
    #[test]
    fn input_outer_width_char_limit_and_read_only_are_native() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let mut id = egui::Id::NULL;
        let _ = frame(&ctx, 0., vec![], |ui| {
            let r = ui.add(Input::new(&mut text).desired_width(240.).char_limit(3));
            assert!((r.rect.width() - 240.).abs() < 0.01);
            id = r.id;
        });
        ctx.memory_mut(|m| m.request_focus(id));
        let _ = frame(&ctx, 0.1, vec![egui::Event::Paste("abcdef".into())], |ui| {
            ui.add(Input::new(&mut text).desired_width(240.).char_limit(3));
        });
        assert_eq!(text, "abc");
        let _ = frame(&ctx, 0.2, vec![egui::Event::Paste("x".into())], |ui| {
            ui.add(Input::new(&mut text).interactive(false));
        });
        assert_eq!(text, "abc");
    }
}
