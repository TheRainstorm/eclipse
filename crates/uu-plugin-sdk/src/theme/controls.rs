//! Shared native editing, choice and menu controls for all application pages.
use super::{is_animal, island, palette, CONTROL_HEIGHT};
use eframe::egui::{
    self, Color32, Id, InnerResponse, Response, Sense, Stroke, TextStyle, Ui, Widget, WidgetInfo,
    WidgetText, WidgetType,
};

/// Native TextEdit state, cursor, selection, paste and IME remain owned by egui.
#[must_use = "add with ui.add"]
pub struct Input<'a> {
    edit: egui::TextEdit<'a>,
    width: Option<f32>,
    compact: bool,
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
        }
    }
    pub fn hint_text(mut self, hint: impl Into<WidgetText>) -> Self {
        self.edit = self.edit.hint_text(hint);
        self
    }
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
    pub fn id(mut self, id: Id) -> Self {
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
        if is_animal(ui.visuals()) {
            let mut input = island::Input::from_text_edit(self.edit).compact(self.compact);
            if let Some(width) = self.width {
                input = input.desired_width(width);
            }
            return ui.add(input);
        }
        let p = *palette(ui.visuals());
        ui.scope(|ui| {
            ui.visuals_mut().extreme_bg_color = p.control;
            let mut edit = self
                .edit
                .margin(egui::Margin::symmetric(8., 6.))
                .min_size(egui::vec2(
                    0.,
                    if self.compact { 24. } else { CONTROL_HEIGHT } - 12.,
                ));
            if let Some(width) = self.width {
                edit = edit.desired_width((width.min(ui.available_width()) - 16.).max(1.));
            }
            let response = ui.add(edit);
            let ring = ui.ctx().animate_bool_with_time(
                response.id.with("input-ring"),
                ui.is_enabled() && response.has_focus(),
                0.15,
            );
            if ring > 0. {
                ui.painter().rect_stroke(
                    response.rect.expand2(egui::vec2(8., 6.)).expand(1.),
                    8.,
                    Stroke::new(1.5, p.accent.gamma_multiply(ring)),
                );
            }
            response
        })
        .inner
    }
}
pub fn text_input(ui: &mut Ui, value: &mut String, hint: &str, width: f32) -> Response {
    ui.add(Input::new(value).hint_text(hint).desired_width(width))
}

/// Keeps ComboBox's native popup, keyboard navigation and accessibility.
#[must_use = "call show_ui"]
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
    pub fn compact(mut self, on: bool) -> Self {
        self.compact = on;
        self
    }
    pub fn truncate(mut self) -> Self {
        self.combo = self.combo.truncate();
        self
    }
    pub fn wrap(mut self) -> Self {
        self.combo = self.combo.wrap();
        self
    }
    pub fn show_ui<R>(
        self,
        ui: &mut Ui,
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        if is_animal(ui.visuals()) {
            return island::Select::from_combo(self.combo)
                .compact(self.compact)
                .show_ui(ui, contents);
        }
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = if self.compact { 24. } else { CONTROL_HEIGHT };
            let p = *palette(ui.visuals());
            let result = self.combo.show_ui(ui, |ui| {
                // Trigger width may intentionally be arrow-only. Popup items
                // retain native intrinsic sizing independently of that width.
                ui.set_min_width(ui.spacing().combo_width);
                contents(ui)
            });
            let response = &result.response;
            let open = egui::ComboBox::is_open(ui.ctx(), response.id);
            let ring = ui.ctx().animate_bool_with_time(
                response.id.with("select-ring"),
                ui.is_enabled() && (response.has_focus() || open),
                0.15,
            );
            if ring > 0. {
                ui.painter().rect_stroke(
                    response.rect.expand(1.),
                    8.,
                    Stroke::new(1.5, p.accent.gamma_multiply(ring)),
                );
            }
            result
        })
        .inner
    }
}
pub fn number_input(ui: &mut Ui, value: egui::DragValue<'_>) -> Response {
    ui.scope(|ui| {
        if is_animal(ui.visuals()) {
            let p = *island::tokens::tokens(ui);
            ui.spacing_mut().button_padding = egui::vec2(14., 8.);
            ui.spacing_mut().interact_size.y = island::tokens::COMPACT_HEIGHT;
            let v = ui.visuals_mut();
            v.extreme_bg_color = p.control;
            for state in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                state.rounding = egui::Rounding::same(16.);
            }
        }
        ui.add(value)
    })
    .inner
}

fn simple_switch(
    ui: &mut Ui,
    value: &mut bool,
    text: WidgetText,
    hint: &str,
    compact: bool,
    menu: bool,
) -> Response {
    let p = *palette(ui.visuals());
    let enabled = ui.is_enabled();
    let gap = 8.;
    let track_size = egui::vec2(36., 20.);
    let hint = WidgetText::from(hint).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        ui.available_width() * 0.4,
        TextStyle::Small,
    );
    let label = text.into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (ui.available_width() - track_size.x - hint.size().x - gap * 3.).max(1.),
        TextStyle::Button,
    );
    let width = if menu {
        ui.available_width()
    } else {
        track_size.x
            + if label.is_empty() {
                0.
            } else {
                gap + label.size().x
            }
    };
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(width, if compact { 28. } else { CONTROL_HEIGHT }),
        Sense::click(),
    );
    if enabled && response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *value, label.text()));
    let position =
        ui.ctx()
            .animate_bool_with_time(response.id.with("switch-position"), *value, 0.18);
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("switch-hover"),
        enabled && response.hovered(),
        0.15,
    );
    if ui.is_rect_visible(rect) {
        if menu {
            ui.painter()
                .rect_filled(rect, 6., Color32::TRANSPARENT.lerp_to_gamma(p.hover, hover));
        }
        let center = egui::pos2(
            if menu {
                rect.right() - 8. - track_size.x * 0.5
            } else {
                rect.left() + track_size.x * 0.5
            },
            rect.center().y,
        );
        let track = egui::Rect::from_center_size(center, track_size);
        let fill = if enabled {
            p.control.lerp_to_gamma(p.primary, position)
        } else {
            p.disabled
        };
        ui.painter().rect(
            track,
            10.,
            fill,
            Stroke::new(
                1.,
                if enabled {
                    p.control_border.lerp_to_gamma(p.primary, position)
                } else {
                    p.border
                },
            ),
        );
        let thumb = egui::pos2(
            egui::lerp((track.left() + 10.)..=(track.right() - 10.), position),
            track.center().y,
        );
        ui.painter().circle(
            thumb,
            7.,
            if enabled { Color32::WHITE } else { p.muted },
            Stroke::new(0.5, p.border),
        );
        if response.has_focus() && enabled {
            ui.painter()
                .rect_stroke(track.expand(3.), 13., Stroke::new(1.5, p.accent));
        }
        let x = if menu {
            rect.left() + 8.
        } else {
            track.right() + gap
        };
        ui.painter().galley_with_override_text_color(
            egui::pos2(x, rect.center().y - label.size().y * 0.5),
            label,
            if enabled { p.text } else { p.muted },
        );
        if !hint.is_empty() {
            ui.painter().galley_with_override_text_color(
                egui::pos2(
                    track.left() - gap - hint.size().x,
                    rect.center().y - hint.size().y * 0.5,
                ),
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
fn switch_impl(
    ui: &mut Ui,
    value: &mut bool,
    label: WidgetText,
    hint: &str,
    compact: bool,
    menu: bool,
) -> Response {
    if is_animal(ui.visuals()) {
        ui.add(
            island::Switch::new(value, label)
                .compact(compact)
                .hint_text(hint)
                .full_width(menu)
                .trailing(menu),
        )
    } else {
        simple_switch(ui, value, label, hint, compact, menu)
    }
}
pub fn checkbox(ui: &mut Ui, value: &mut bool, label: impl Into<WidgetText>) -> Response {
    switch_impl(ui, value, label.into(), "", false, false)
}
pub fn switch(ui: &mut Ui, value: &mut bool, label: impl Into<WidgetText>) -> Response {
    checkbox(ui, value, label)
}
pub fn compact_switch(ui: &mut Ui, value: &mut bool, label: impl Into<WidgetText>) -> Response {
    switch_impl(ui, value, label.into(), "", true, false)
}
pub fn menu_toggle(ui: &mut Ui, value: &mut bool, label: impl Into<WidgetText>) -> Response {
    menu_toggle_with_hint(ui, value, label, "")
}
pub fn menu_toggle_with_hint(
    ui: &mut Ui,
    value: &mut bool,
    label: impl Into<WidgetText>,
    hint: &str,
) -> Response {
    switch_impl(ui, value, label.into(), hint, true, true)
}

pub fn radio_value<T: PartialEq>(
    ui: &mut Ui,
    value: &mut T,
    alternative: T,
    label: impl Into<WidgetText>,
) -> Response {
    let selected = *value == alternative;
    let label = label.into();
    let mut response = if is_animal(ui.visuals()) {
        ui.add(island::Radio::new(selected, label))
    } else {
        ui.radio(selected, label)
    };
    let by_key = island::choices::selection_by_key(
        ui,
        &response,
        std::any::type_name::<(island::Radio, T)>(),
    );
    if ui.is_enabled() && (response.clicked() || by_key) && *value != alternative {
        *value = alternative;
        response.mark_changed();
    }
    response
}
/// Foreground for a normally enabled tab face. Disabled callers use muted.
pub fn tab_text_color(visuals: &egui::Visuals, selected: bool) -> Color32 {
    let p = palette(visuals);
    if is_animal(visuals) && selected {
        if visuals.dark_mode {
            p.base
        } else {
            Color32::WHITE
        }
    } else {
        p.text
    }
}
/// Face-only adapter for draggable/closable tab headers. Caller owns ID,
/// interaction and layout; this function never creates another hit target.
pub fn paint_tab(
    ui: &mut Ui,
    response: &Response,
    selected: bool,
    label: impl Into<WidgetText>,
) -> egui::Rect {
    let label = label.into();
    if is_animal(ui.visuals()) {
        return island::choices::paint_tab(ui, response, selected, label);
    }
    let p = *palette(ui.visuals());
    let enabled = ui.is_enabled();
    let face = response.rect;
    let galley = label.into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (face.width() - 24.).max(1.),
        TextStyle::Button,
    );
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("tab-hover"),
        enabled && response.hovered(),
        0.15,
    );
    let down = ui.ctx().animate_bool_with_time(
        response.id.with("tab-down"),
        enabled && response.is_pointer_button_down_on(),
        0.12,
    );
    if ui.is_rect_visible(face) {
        let base = if selected {
            p.tint
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(
            face,
            6.,
            if enabled {
                base.lerp_to_gamma(p.hover, hover)
                    .lerp_to_gamma(p.pressed, down)
            } else {
                p.disabled
            },
        );
        if enabled && response.has_focus() {
            ui.painter()
                .rect_stroke(face.shrink(1.), 6., Stroke::new(1.5, p.accent));
        }
        ui.painter().galley_with_override_text_color(
            face.center() - galley.size() * 0.5,
            galley,
            if enabled { p.text } else { p.muted },
        );
    }
    face
}
pub fn selectable_tab(ui: &mut Ui, selected: bool, label: impl Into<WidgetText>) -> Response {
    let label = label.into();
    if is_animal(ui.visuals()) {
        return ui.add(island::Tab::new(selected, label));
    }
    let galley = label.clone().into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (ui.available_width() - 24.).max(1.),
        TextStyle::Button,
    );
    let (_, response) = ui.allocate_at_least(
        egui::vec2(galley.size().x + 24., CONTROL_HEIGHT),
        Sense::click(),
    );
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label.text(),
        )
    });
    paint_tab(ui, &response, selected, label);
    response
}
pub fn tab_value<T: PartialEq>(
    ui: &mut Ui,
    value: &mut T,
    alternative: T,
    label: impl Into<WidgetText>,
) -> Response {
    let mut response = selectable_tab(ui, *value == alternative, label);
    let key =
        island::choices::selection_by_key(ui, &response, std::any::type_name::<(island::Tab, T)>());
    if ui.is_enabled() && (response.clicked() || key) && *value != alternative {
        *value = alternative;
        response.mark_changed();
    }
    response
}

/// Full-width option row: optional vector icon, stable gutters, visible hint,
/// native click/focus and submenu arrow all share one response.
pub fn menu_control(
    ui: &mut Ui,
    label: impl Into<WidgetText>,
    icon: Option<crate::ui::Icon>,
    selected: bool,
    arrow: bool,
    hint: &str,
    active: bool,
) -> Response {
    ui.scope(|ui| {
        let p = *palette(ui.visuals());
        let animal = is_animal(ui.visuals());
        let enabled = ui.is_enabled();
        // Popup density is independent of the larger controls on settings pages.
        let h: f32 = 30.;
        let inset = 8.;
        let gutter = 24.;
        let hint = WidgetText::from(hint).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            ui.available_width() * 0.4,
            TextStyle::Small,
        );
        // Native ComboBox deliberately uses Extend so an arrow-only trigger
        // can grow its popup to its items. Fixed menus still truncate normally.
        let extend = ui.wrap_mode() == egui::TextWrapMode::Extend;
        let text = label.into().into_galley(
            ui,
            Some(if extend {
                egui::TextWrapMode::Extend
            } else {
                egui::TextWrapMode::Truncate
            }),
            (ui.available_width()
                - inset * 2.
                - gutter
                - hint.size().x
                - if arrow { 20. } else { 0. })
            .max(1.),
            TextStyle::Button,
        );
        let intrinsic =
            text.size().x + hint.size().x + inset * 2. + gutter + if arrow { 20. } else { 0. };
        let width = if extend {
            ui.available_width().max(intrinsic)
        } else {
            ui.available_width()
        };
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, h.max(text.size().y + 8.)), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, text.text())
        });
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("menu-hover"),
            enabled && response.hovered(),
            0.16,
        );
        let press = ui.ctx().animate_bool_with_time(
            response.id.with("menu-press"),
            enabled && response.is_pointer_button_down_on(),
            0.12,
        );
        if ui.is_rect_visible(rect) {
            let fill = if selected || active {
                p.tint
            } else {
                Color32::TRANSPARENT
            }
            .lerp_to_gamma(p.hover, hover)
            .lerp_to_gamma(p.pressed, press);
            ui.painter()
                .rect_filled(rect, if animal { 12. } else { 6. }, fill);
            if response.has_focus() && enabled {
                ui.painter().rect_stroke(
                    rect.shrink(1.),
                    if animal { 12. } else { 6. },
                    Stroke::new(1.5, p.accent),
                );
            }
            let fg = if enabled {
                ui.visuals().override_text_color.unwrap_or(p.text)
            } else {
                p.muted
            };
            let glyph = egui::Rect::from_center_size(
                egui::pos2(rect.left() + inset + 8., rect.center().y),
                egui::Vec2::splat(18.),
            );
            if let Some(icon) = icon {
                crate::ui::icon(ui.painter(), glyph, icon, fg);
            } else if selected {
                let c = glyph.center();
                ui.painter().add(egui::Shape::line(
                    vec![
                        c + egui::vec2(-5., 0.),
                        c + egui::vec2(-1., 4.),
                        c + egui::vec2(6., -4.),
                    ],
                    Stroke::new(2., p.accent),
                ));
            }
            ui.painter().galley_with_override_text_color(
                egui::pos2(
                    rect.left() + inset + gutter,
                    rect.center().y - text.size().y * 0.5,
                ),
                text,
                fg,
            );
            let right = rect.right() - inset - if arrow { 18. } else { 0. };
            if !hint.is_empty() {
                ui.painter().galley_with_override_text_color(
                    egui::pos2(right - hint.size().x, rect.center().y - hint.size().y * 0.5),
                    hint,
                    p.muted,
                );
            }
            if arrow {
                let c = egui::pos2(rect.right() - inset - 4., rect.center().y);
                ui.painter().add(egui::Shape::line(
                    vec![
                        c + egui::vec2(-3., -4.),
                        c + egui::vec2(1., 0.),
                        c + egui::vec2(-3., 4.),
                    ],
                    Stroke::new(1.5, fg),
                ));
            }
        }
        response
    })
    .inner
}
pub fn menu_row(ui: &mut Ui, selected: bool, label: impl Into<WidgetText>) -> Response {
    menu_row_with_hint(ui, selected, label, "")
}
pub fn menu_row_with_hint(
    ui: &mut Ui,
    selected: bool,
    label: impl Into<WidgetText>,
    hint: &str,
) -> Response {
    menu_control(ui, label, None, selected, false, hint, false)
}
pub fn selectable_label(ui: &mut Ui, selected: bool, label: impl Into<WidgetText>) -> Response {
    menu_row(ui, selected, label)
}
pub fn selectable_value<T: PartialEq>(
    ui: &mut Ui,
    value: &mut T,
    alternative: T,
    label: impl Into<WidgetText>,
) -> Response {
    let mut response = selectable_label(ui, *value == alternative, label);
    if ui.is_enabled() && response.clicked() && *value != alternative {
        *value = alternative;
        response.mark_changed();
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, ButtonKind, ColorTheme, COLOR_THEMES};
    fn frame(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        mut body: impl FnMut(&mut Ui),
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700., 400.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| body(ui));
            },
        )
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
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    #[test]
    fn unopened_menu_rows_accept_pointer_and_keyboard_and_parent_disable_still_wins() {
        for color in COLOR_THEMES {
            let ctx = egui::Context::default();
            theme::apply_color_theme(&ctx, color);
            let mut rect = egui::Rect::NOTHING;
            let mut id = Id::NULL;
            let mut clicks = 0;
            let mut draw = |time, events| {
                frame(&ctx, time, events, |ui| {
                    let r = menu_control(
                        ui,
                        "Open",
                        Some(crate::ui::Icon::Settings),
                        false,
                        false,
                        "Ctrl+O",
                        false,
                    );
                    rect = r.rect;
                    id = r.id;
                    clicks += usize::from(r.clicked());
                    let disabled = ui
                        .add_enabled_ui(false, |ui| menu_row(ui, false, "Disabled"))
                        .inner;
                    assert!(!disabled.clicked());
                })
            };
            let _ = draw(0., vec![]);
            drop(draw);
            let at = rect.center();
            for (t, down) in [(0.1, true), (0.2, false)] {
                let _ = frame(&ctx, t, pointer(at, down), |ui| {
                    clicks += usize::from(
                        menu_control(
                            ui,
                            "Open",
                            Some(crate::ui::Icon::Settings),
                            false,
                            false,
                            "Ctrl+O",
                            false,
                        )
                        .clicked(),
                    );
                });
            }
            assert_eq!(clicks, 1, "pointer {color:?}");
            ctx.memory_mut(|m| m.request_focus(id));
            let _ = frame(&ctx, 0.3, vec![key(egui::Key::Enter)], |ui| {
                assert!(
                    menu_control(
                        ui,
                        "Open",
                        Some(crate::ui::Icon::Settings),
                        false,
                        false,
                        "Ctrl+O",
                        false
                    )
                    .clicked(),
                    "keyboard {color:?}"
                );
            });
        }
    }
    #[test]
    fn every_color_family_uses_switch_motion_keyboard_and_quiet_idle() {
        for color in COLOR_THEMES {
            let ctx = egui::Context::default();
            theme::apply_color_theme(&ctx, color);
            let mut on = false;
            let mut id = Id::NULL;
            let _ = frame(&ctx, 0., vec![], |ui| {
                id = checkbox(ui, &mut on, "HDR").id;
            });
            ctx.memory_mut(|m| m.request_focus(id));
            let _ = frame(&ctx, 0.1, vec![key(egui::Key::Space)], |ui| {
                assert!(checkbox(ui, &mut on, "HDR").changed());
            });
            assert!(on);
            // Release focus before checking only widget animation repainting.
            ctx.memory_mut(|m| m.surrender_focus(id));
            let moving = frame(&ctx, 0.2, vec![], |ui| {
                checkbox(ui, &mut on, "HDR");
            });
            assert_eq!(
                moving.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                std::time::Duration::ZERO
            );
            for t in [1., 1.1, 1.2] {
                let _ = frame(&ctx, t, vec![], |ui| {
                    checkbox(ui, &mut on, "HDR");
                });
            }
            let quiet = frame(&ctx, 1.3, vec![], |ui| {
                checkbox(ui, &mut on, "HDR");
            });
            assert!(
                quiet.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                    > std::time::Duration::from_secs(1),
                "idle {color:?}"
            );
        }
    }
    #[test]
    fn toolbar_icon_and_button_share_the_exact_outer_slot_and_visual_center() {
        for color in COLOR_THEMES {
            let ctx = egui::Context::default();
            theme::apply_color_theme(&ctx, color);
            let _ = frame(&ctx, 0., vec![], |ui| {
                ui.horizontal(|ui| {
                    let icon = crate::ui::icon_button(ui, crate::ui::Icon::Settings, "Settings");
                    let button = ui.add(theme::toolbar_button("Save", ButtonKind::Primary));
                    let ghost = ui.add(theme::toolbar_button("Quiet", ButtonKind::Ghost));
                    for response in [&icon, &button, &ghost] {
                        assert_eq!(response.rect.height(), theme::TOOLBAR_HEIGHT);
                        assert_eq!(
                            theme::toolbar_face_rect(ui, response).height(),
                            theme::TOOLBAR_FACE
                        );
                    }
                    assert_eq!(
                        theme::toolbar_face_rect(ui, &icon).center().y,
                        theme::toolbar_face_rect(ui, &button).center().y
                    );
                    assert_eq!(
                        theme::toolbar_face_rect(ui, &ghost).center().y,
                        theme::toolbar_face_rect(ui, &button).center().y
                    );
                });
            });
        }
    }
    #[test]
    fn legacy_cyan_deserializes_only_the_color_field_and_reencodes_light_blue() {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Config {
            color_theme: ColorTheme,
            fps: u32,
            name: String,
        }
        let config: Config =
            serde_json::from_str(r#"{"color_theme":"cyan","fps":165,"name":"Kept"}"#).unwrap();
        assert_eq!(config.color_theme, ColorTheme::LightBlue);
        assert_eq!(config.fps, 165);
        assert_eq!(config.name, "Kept");
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["color_theme"], "light_blue");
        assert_eq!(value["fps"], 165);
    }
    #[test]
    fn arrow_only_select_expands_items_and_clicks_the_visible_text() {
        for color in COLOR_THEMES {
            let ctx = egui::Context::default();
            theme::apply_color_theme(&ctx, color);
            let label = "Existing profile with its complete descriptive name";
            let mut selected = 0u8;
            let mut arrow = egui::Rect::NOTHING;
            let mut row = egui::Rect::NOTHING;
            let mut draw = |time, events| {
                frame(&ctx, time, events, |ui| {
                    arrow = Select::from_id_salt("arrow-only-select")
                        .selected_text("")
                        .width(28.)
                        .show_ui(ui, |ui| {
                            let response = selectable_value(ui, &mut selected, 1, label);
                            row = response.rect;
                        })
                        .response
                        .rect;
                })
            };
            let _ = draw(0., vec![]);
            drop(draw);
            let at = arrow.center();
            for (time, down) in [(0.1, true), (0.2, false)] {
                let _ = frame(&ctx, time, pointer(at, down), |ui| {
                    Select::from_id_salt("arrow-only-select")
                        .selected_text("")
                        .width(28.)
                        .show_ui(ui, |ui| {
                            row = selectable_value(ui, &mut selected, 1, label).rect;
                        });
                });
            }
            let output = frame(&ctx, 0.25, vec![], |ui| {
                Select::from_id_salt("arrow-only-select")
                    .selected_text("")
                    .width(28.)
                    .show_ui(ui, |ui| {
                        row = selectable_value(ui, &mut selected, 1, label).rect;
                    });
            });
            let text = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == label => Some(t),
                    _ => None,
                })
                .expect("visible popup item text");
            let center = text.pos + text.galley.size() * 0.5;
            assert!(
                row.contains(center),
                "{color:?} text outside item hit rectangle"
            );
            assert!(
                row.width() > arrow.width() + 80.,
                "popup must be independent of 28-point trigger"
            );
            for (time, down) in [(0.3, true), (0.4, false)] {
                let _ = frame(&ctx, time, pointer(center, down), |ui| {
                    Select::from_id_salt("arrow-only-select")
                        .selected_text("")
                        .width(28.)
                        .show_ui(ui, |ui| {
                            selectable_value(ui, &mut selected, 1, label);
                        });
                });
            }
            assert_eq!(selected, 1, "visible item click {color:?}");
        }
    }
}
