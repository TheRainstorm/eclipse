//! Native focus/click choices with finite selection motion.
use crate::tokens::{self, COMPACT_HEIGHT, CONTROL_HEIGHT, MOTION_SECONDS};
use egui::{
    pos2, vec2, Color32, Response, Sense, Stroke, TextStyle, Ui, Widget, WidgetInfo, WidgetText,
    WidgetType,
};

#[derive(Clone, Default)]
struct Group {
    pass: u64,
    previous: Vec<(egui::Id, bool)>,
    current: Vec<(egui::Id, bool)>,
    target: Option<egui::Id>,
}
/// One keyboard owner per immediate UI choice group. Only IDs are retained;
/// application values and callbacks remain with the caller.
pub fn selection_by_key(ui: &Ui, response: &Response, kind: &'static str) -> bool {
    let key = ui
        .id()
        .with((ui.ctx().viewport_id(), "choice-navigation", kind));
    let pass = ui.ctx().cumulative_pass_nr();
    let mut group = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Group>(key).unwrap_or_default());
    if group.pass != pass {
        group.pass = pass;
        group.previous = std::mem::take(&mut group.current);
        group.target = None;
        if ui.is_enabled() {
            let enabled: Vec<_> = group
                .previous
                .iter()
                .filter(|(_, on)| *on)
                .map(|(id, _)| *id)
                .collect();
            if let Some(index) = ui
                .memory(|m| m.focused())
                .and_then(|id| enabled.iter().position(|v| *v == id))
            {
                let direction = ui.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                    {
                        Some(1)
                    } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                    {
                        Some(-1)
                    } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                        Some(-2)
                    } else if i.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                        Some(2)
                    } else {
                        None
                    }
                });
                if let Some(direction) = direction {
                    let target = match direction {
                        -2 => 0,
                        2 => enabled.len() - 1,
                        1 => (index + 1) % enabled.len(),
                        _ => (index + enabled.len() - 1) % enabled.len(),
                    };
                    group.target = Some(enabled[target]);
                    ui.memory_mut(|m| m.request_focus(enabled[target]));
                }
            }
        }
    }
    group.current.push((response.id, response.enabled()));
    let choose = response.enabled() && group.target == Some(response.id);
    ui.ctx().data_mut(|d| d.insert_temp(key, group));
    choose
}

#[must_use = "add with ui.add"]
pub struct Radio {
    selected: bool,
    text: WidgetText,
    compact: bool,
}
impl Radio {
    pub fn new(selected: bool, text: impl Into<WidgetText>) -> Self {
        Self {
            selected,
            text: text.into(),
            compact: false,
        }
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}
#[derive(Clone, Copy)]
struct Splash {
    selected: bool,
    started: Option<f64>,
}
impl Widget for Radio {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = *tokens::tokens(ui);
        let enabled = ui.is_enabled();
        let label = self.text.text().to_owned();
        let size = if self.compact { 18. } else { 22. };
        let height = if self.compact {
            COMPACT_HEIGHT
        } else {
            CONTROL_HEIGHT
        };
        let gutter = height;
        let galley = self.text.into_galley(
            ui,
            None,
            (ui.available_width() - gutter).max(0.),
            TextStyle::Button,
        );
        let (rect, response) = ui.allocate_exact_size(
            vec2(gutter + galley.size().x, height.max(galley.size().y)),
            Sense::click(),
        );
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::RadioButton, enabled, self.selected, &label)
        });
        let now = ui.input(|i| i.time);
        let state_id = response.id.with("radio-splash");
        let mut state = ui.ctx().data_mut(|d| {
            d.get_temp::<Splash>(state_id).unwrap_or(Splash {
                selected: self.selected,
                started: None,
            })
        });
        if self.selected && !state.selected && enabled {
            state.started = Some(now);
        }
        if !self.selected || !enabled {
            state.started = None;
        }
        state.selected = self.selected;
        let splash = state
            .started
            .map(|at| ((now - at) / 0.6).clamp(0., 1.) as f32);
        if splash.is_some_and(|t| t < 1.) {
            ui.ctx().request_repaint();
        } else {
            state.started = None;
        }
        ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
        let check = ui.ctx().animate_bool_with_time_and_easing(
            response.id.with("radio-check"),
            self.selected,
            MOTION_SECONDS,
            egui::emath::easing::cubic_out,
        );
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("radio-hover"),
            response.hovered() && enabled,
            MOTION_SECONDS,
        );
        if ui.is_rect_visible(rect) {
            let center = pos2(rect.left() + gutter * 0.5, rect.center().y);
            if let Some(t) = splash.filter(|t| *t < 1.) {
                for n in 0..6 {
                    let angle =
                        (n as f32 / 6.) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                    let at = center + vec2(angle.cos(), angle.sin()) * (size * 0.4 + t * 7.);
                    ui.painter().circle_filled(
                        at,
                        (2.4 * (1. - t)).max(0.),
                        p.primary.gamma_multiply((1. - t) * 0.75),
                    );
                }
            }
            let fill = if !enabled {
                p.disabled
            } else if self.selected {
                p.accent
            } else {
                p.control
            };
            ui.painter().circle(
                center,
                size * 0.5,
                fill,
                Stroke::new(
                    2.,
                    if self.selected {
                        p.accent
                    } else {
                        p.control_border.lerp_to_gamma(p.accent, hover)
                    },
                ),
            );
            if enabled && response.has_focus() {
                ui.painter()
                    .circle_stroke(center, size * 0.5 + 3., Stroke::new(2., p.focus));
            }
            if check > 0. {
                let a = center + vec2(-size * 0.23, 0.);
                let b = center + vec2(-size * 0.04, size * 0.19);
                let c = center + vec2(size * 0.27, -size * 0.20);
                let first = a + (b - a) * (check * 2.5).min(1.);
                ui.painter().line_segment(
                    [a, first],
                    Stroke::new(
                        2.5,
                        if !enabled {
                            p.muted
                        } else if ui.visuals().dark_mode {
                            p.base
                        } else {
                            Color32::WHITE
                        },
                    ),
                );
                if check > 0.4 {
                    ui.painter().line_segment(
                        [b, b + (c - b) * ((check - 0.4) / 0.6).min(1.)],
                        Stroke::new(
                            2.5,
                            if !enabled {
                                p.muted
                            } else if ui.visuals().dark_mode {
                                p.base
                            } else {
                                Color32::WHITE
                            },
                        ),
                    );
                }
            }
            ui.painter().galley_with_override_text_color(
                pos2(
                    rect.left() + gutter,
                    rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                if enabled { p.text } else { p.muted },
            );
        }
        if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
    }
}

#[must_use = "add with ui.add"]
pub struct Tab {
    selected: bool,
    text: WidgetText,
    compact: bool,
}
impl Tab {
    pub fn new(selected: bool, text: impl Into<WidgetText>) -> Self {
        Self {
            selected,
            text: text.into(),
            compact: false,
        }
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}
impl Widget for Tab {
    fn ui(self, ui: &mut Ui) -> Response {
        let text = self.text;
        let galley = text.clone().strong().into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (ui.available_width() - 32.).max(0.),
            TextStyle::Button,
        );
        let h = if self.compact {
            COMPACT_HEIGHT
        } else {
            CONTROL_HEIGHT
        };
        let (_, response) =
            ui.allocate_at_least(vec2(galley.size().x + 32., h + 4.), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::SelectableLabel,
                ui.is_enabled(),
                self.selected,
                text.text(),
            )
        });
        paint_tab(ui, &response, self.selected, text);
        if ui.is_enabled() {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
    }
}
/// Paint a page-tab face into an existing click/drag response. Does not allocate
/// an ID, add a second hit target, or take ownership of the caller's drag state.
pub fn paint_tab(
    ui: &mut Ui,
    response: &Response,
    selected: bool,
    text: impl Into<WidgetText>,
) -> egui::Rect {
    let p = *tokens::tokens(ui);
    let enabled = ui.is_enabled();
    let rect = response.rect;
    let galley = text.into().strong().into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (rect.width() - 32.).max(0.),
        TextStyle::Button,
    );
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("tab-hover"),
        enabled && response.hovered(),
        MOTION_SECONDS,
    );
    let motion = ui.ctx().animate_bool_with_time_and_easing(
        response.id.with("tab-selected"),
        selected,
        MOTION_SECONDS,
        egui::emath::easing::cubic_out,
    );
    let down = ui.ctx().animate_bool_with_time(
        response.id.with("tab-down"),
        enabled && response.is_pointer_button_down_on(),
        0.12,
    );
    let face = egui::Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 4.))
        .translate(vec2(0., down));
    if ui.is_rect_visible(rect) {
        if selected && enabled {
            ui.painter().rect_filled(
                face.translate(vec2(0., 3. * motion)),
                18.,
                p.shadow.gamma_multiply(0.55),
            );
        }
        let fill = if !enabled {
            p.disabled
        } else if selected {
            p.accent
        } else {
            Color32::TRANSPARENT.lerp_to_gamma(p.tint, hover)
        };
        ui.painter().rect_filled(face, 18., fill);
        if enabled && response.has_focus() {
            ui.painter()
                .rect_stroke(face.expand(2.), 20., Stroke::new(2., p.focus));
        }
        if selected && enabled {
            ui.painter().line_segment(
                [
                    pos2(face.left() + 12., face.bottom() - 5.),
                    pos2(
                        face.left() + 12. + (face.width() - 24.) * motion,
                        face.bottom() - 5.,
                    ),
                ],
                Stroke::new(2., p.primary),
            );
        }
        ui.painter().galley_with_override_text_color(
            face.center() - galley.size() * 0.5,
            galley,
            if !enabled {
                p.muted
            } else if selected {
                if ui.visuals().dark_mode {
                    p.base
                } else {
                    Color32::WHITE
                }
            } else {
                p.text
            },
        );
    }
    face
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn frame(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        value: &mut u8,
        tabs: bool,
    ) -> (egui::FullOutput, Vec<Response>) {
        let mut responses = Vec::new();
        let out = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(700., 300.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for i in 0..3 {
                            let mut r = if tabs {
                                ui.add_enabled(i != 1, Tab::new(*value == i, format!("Tab {i}")))
                            } else {
                                ui.add_enabled(
                                    i != 1,
                                    Radio::new(*value == i, format!("Radio {i}")),
                                )
                            };
                            let by_key =
                                selection_by_key(ui, &r, if tabs { "tabs" } else { "radio" });
                            if r.enabled() && (r.clicked() || by_key) && *value != i {
                                *value = i;
                                r.mark_changed();
                            }
                            responses.push(r);
                        }
                    });
                });
            },
        );
        (out, responses)
    }
    #[test]
    fn choices_use_real_focus_arrow_home_end_and_space_without_selecting_disabled() {
        for tabs in [false, true] {
            let ctx = egui::Context::default();
            let mut value = 0;
            let (_, r) = frame(&ctx, 0., vec![], &mut value, tabs);
            ctx.memory_mut(|m| m.request_focus(r[0].id));
            // A second pass records the focus before a real key event arrives.
            let _ = frame(&ctx, 0.1, vec![], &mut value, tabs);
            let _ = frame(
                &ctx,
                0.2,
                vec![key(egui::Key::ArrowRight)],
                &mut value,
                tabs,
            );
            assert_eq!(value, 2, "arrow skips disabled {tabs}");
            assert_eq!(ctx.memory(|m| m.focused()), Some(r[2].id));
            let _ = frame(&ctx, 0.3, vec![key(egui::Key::Home)], &mut value, tabs);
            assert_eq!(value, 0);
            let _ = frame(&ctx, 0.4, vec![key(egui::Key::End)], &mut value, tabs);
            assert_eq!(value, 2);
            ctx.memory_mut(|m| m.request_focus(r[0].id));
            let _ = frame(&ctx, 0.5, vec![key(egui::Key::Space)], &mut value, tabs);
            assert_eq!(value, 0);
            for (time, down) in [(0.6, true), (0.7, false)] {
                let at = r[2].rect.center();
                let events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: down,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                let _ = frame(&ctx, time, events, &mut value, tabs);
            }
            assert_eq!(value, 2, "real pointer selects enabled {tabs}");
            for (time, down) in [(0.8, true), (0.9, false)] {
                let at = r[1].rect.center();
                let events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: down,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                let _ = frame(&ctx, time, events, &mut value, tabs);
            }
            assert_eq!(value, 2, "real pointer cannot select disabled {tabs}");
        }
    }
    #[test]
    fn circle_check_and_tab_shapes_use_real_semantic_text_colors() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let ctx = egui::Context::default();
            crate::apply(&ctx);
            ctx.set_theme(theme);
            let p = if theme == egui::Theme::Light {
                tokens::LIGHT
            } else {
                tokens::DARK
            };
            let out = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.visuals_mut().override_text_color = Some(Color32::from_rgb(255, 0, 255));
                    ui.add(Radio::new(
                        true,
                        egui::RichText::new("Selected radio").color(Color32::from_rgb(255, 0, 255)),
                    ));
                    ui.add(Tab::new(
                        true,
                        egui::RichText::new("Selected tab").color(Color32::from_rgb(255, 0, 255)),
                    ));
                });
            });
            assert!(out.shapes.iter().any(
                |s| matches!(&s.shape,egui::Shape::Circle(c) if c.radius==11.&&c.fill==p.accent)
            ));
            for (label, expected) in [
                ("Selected radio", p.text),
                (
                    "Selected tab",
                    if theme == egui::Theme::Dark {
                        p.base
                    } else {
                        Color32::WHITE
                    },
                ),
            ] {
                let shapes = out
                    .shapes
                    .iter()
                    .filter(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.text()==label))
                    .cloned()
                    .collect();
                let colors: Vec<_> = ctx
                    .tessellate(shapes, out.pixels_per_point)
                    .into_iter()
                    .flat_map(|s| match s.primitive {
                        egui::epaint::Primitive::Mesh(m) => {
                            m.vertices.into_iter().map(|v| v.color).collect()
                        }
                        _ => Vec::new(),
                    })
                    .collect();
                assert!(!colors.is_empty());
                assert!(colors.iter().all(|c| *c == expected), "actual {label}");
            }
        }
    }
    #[test]
    fn selection_splash_and_tab_motion_settle_without_idle_repaints() {
        for tabs in [false, true] {
            let ctx = egui::Context::default();
            let mut value = 0;
            for t in [0., 0.1, 0.2] {
                let _ = frame(&ctx, t, vec![], &mut value, tabs);
            }
            value = 2;
            let _ = frame(&ctx, 0.3, vec![], &mut value, tabs);
            let (moving, _) = frame(&ctx, 0.4, vec![], &mut value, tabs);
            assert_eq!(
                moving.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                std::time::Duration::ZERO
            );
            for t in [1., 1.1, 1.2] {
                let _ = frame(&ctx, t, vec![], &mut value, tabs);
            }
            let (settled, _) = frame(&ctx, 1.3, vec![], &mut value, tabs);
            assert!(
                settled.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                    > std::time::Duration::from_secs(1)
            );
        }
    }
}
