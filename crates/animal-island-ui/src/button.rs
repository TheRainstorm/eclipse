//! Pill buttons with native egui interaction and island-style elevation.
use crate::tokens::{self, Tokens, COMPACT_HEIGHT, CONTROL_HEIGHT, MOTION_SECONDS, PILL_RADIUS};
use egui::{
    pos2, vec2, Color32, Rect, Response, Sense, Stroke, TextStyle, Vec2, Widget, WidgetInfo,
    WidgetText, WidgetType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[must_use = "add the button with ui.add or ui.add_enabled"]
pub struct Button {
    text: WidgetText,
    kind: ButtonKind,
    small: bool,
    toolbar: bool,
    selected: bool,
    min_size: Vec2,
}
impl Button {
    pub fn new(text: impl Into<WidgetText>, kind: ButtonKind) -> Self {
        Self {
            text: text.into(),
            kind,
            small: false,
            toolbar: false,
            selected: false,
            min_size: Vec2::ZERO,
        }
    }
    pub fn small(self) -> Self {
        self.compact(true)
    }
    pub fn compact(mut self, compact: bool) -> Self {
        self.small = compact;
        self
    }
    /// Shared 40-point toolbar slot, 30-point face centered in both axes.
    pub fn toolbar(mut self) -> Self {
        self.toolbar = true;
        self.small = true;
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    /// Minimum face dimensions; the layout also reserves room for its shadow.
    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }
}

#[derive(Clone, Copy)]
struct Appearance {
    fill: Color32,
    text: Color32,
    border: Color32,
    shadow: Color32,
    depth: f32,
    blur: f32,
    offset: f32,
}
fn appearance(
    p: &Tokens,
    dark: bool,
    kind: ButtonKind,
    enabled: bool,
    hover: f32,
    press: f32,
) -> Appearance {
    if !enabled {
        return Appearance {
            fill: p.disabled,
            text: p.muted,
            border: p.border,
            shadow: Color32::TRANSPARENT,
            depth: 0.,
            blur: 0.,
            offset: 0.,
        };
    }
    let (fill, text, border, shadow, depth, blur) = match kind {
        ButtonKind::Primary => (p.base, p.text, p.base, p.shadow, 5. + hover, 0.),
        ButtonKind::Danger => {
            let base = if dark { p.danger_bg } else { p.danger };
            (
                base.lerp_to_gamma(p.danger_bg, hover * 0.12)
                    .lerp_to_gamma(base.gamma_multiply(0.88), press),
                if dark { p.danger } else { Color32::WHITE },
                base,
                base.gamma_multiply(0.74),
                5. + hover,
                0.,
            )
        }
        ButtonKind::Secondary => (
            p.control
                .lerp_to_gamma(p.hover, hover)
                .lerp_to_gamma(p.pressed, press),
            p.text.lerp_to_gamma(p.accent, hover.max(press)),
            p.control_border.lerp_to_gamma(p.accent, hover.max(press)),
            p.shadow.gamma_multiply(0.14 + 0.08 * hover * (1. - press)),
            2. + hover * (1. - press),
            4. + 6. * hover * (1. - press),
        ),
        ButtonKind::Ghost => (
            Color32::TRANSPARENT
                .lerp_to_gamma(p.hover, hover)
                .lerp_to_gamma(p.pressed, press),
            p.text.lerp_to_gamma(p.accent, hover.max(press)),
            Color32::TRANSPARENT,
            Color32::TRANSPARENT,
            0.,
            0.,
        ),
    };
    let raised = matches!(kind, ButtonKind::Primary | ButtonKind::Danger);
    Appearance {
        fill,
        text,
        border,
        shadow,
        blur,
        depth: if raised {
            egui::lerp(depth..=1., press)
        } else {
            depth
        },
        offset: if raised {
            egui::lerp(-hover..=2., press)
        } else {
            -hover * (1. - press)
        },
    }
}
impl Widget for Button {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let label = self.text.text().to_owned();
        let horizontal = if self.toolbar {
            8.
        } else if self.small {
            16.
        } else {
            20.
        };
        let text = if self.small {
            self.text.small()
        } else {
            self.text
        };
        let galley = text.strong().into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (ui.available_width() - 2. * horizontal).max(0.),
            TextStyle::Button,
        );
        let height = if self.small {
            COMPACT_HEIGHT
        } else {
            CONTROL_HEIGHT
        };
        let mut face_size = vec2(
            galley.size().x + 2. * horizontal,
            height.max(galley.size().y + 12.),
        )
        .max(self.min_size);
        // Reserve the same envelope for every kind so rows align and a thick
        // shadow cannot paint into the next control's hit target.
        if self.toolbar {
            face_size = face_size.max(Vec2::splat(crate::tokens::TOOLBAR_FACE));
            face_size.y = crate::tokens::TOOLBAR_FACE.max(self.min_size.y);
        }
        let envelope = if self.toolbar {
            vec2(10., 10.)
        } else {
            vec2(0., 6.)
        };
        let (rect, response) = ui.allocate_at_least(face_size + envelope, Sense::click());
        if !label.is_empty() {
            response
                .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), &label));
        }
        let enabled = ui.is_enabled();
        let hover = ui.ctx().animate_bool_with_time_and_easing(
            response.id.with("island-hover"),
            enabled && response.hovered(),
            MOTION_SECONDS,
            egui::emath::easing::cubic_out,
        );
        let down = response.is_pointer_button_down_on()
            || (response.has_focus()
                && ui.input(|i| i.key_down(egui::Key::Space) || i.key_down(egui::Key::Enter)));
        let press = ui.ctx().animate_bool_with_time_and_easing(
            response.id.with("island-press"),
            enabled && down,
            MOTION_SECONDS,
            egui::emath::easing::cubic_out,
        );
        if ui.is_rect_visible(rect) {
            let p = tokens::tokens(ui);
            let mut a = appearance(p, ui.visuals().dark_mode, self.kind, enabled, hover, press);
            if self.selected && self.kind == ButtonKind::Ghost && enabled {
                a.fill = p
                    .tint
                    .lerp_to_gamma(p.hover, hover)
                    .lerp_to_gamma(p.pressed, press);
            }
            let face = if self.toolbar {
                rect.shrink2(vec2(5., 5.))
            } else {
                Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 6.))
            };
            let face = face.translate(vec2(0., a.offset));
            if self.toolbar {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(response.id.with("toolbar-face"), face));
            }
            let radius = if self.small {
                12.
            } else {
                PILL_RADIUS.min(face.height() * 0.5)
            };
            if a.shadow != Color32::TRANSPARENT {
                ui.painter().add(
                    egui::epaint::Shadow {
                        offset: vec2(0., a.depth),
                        blur: a.blur,
                        spread: 0.,
                        color: a.shadow,
                    }
                    .as_shape(face, radius),
                );
            }
            ui.painter()
                .rect(face, radius, a.fill, Stroke::new(2., a.border));
            if enabled && response.has_focus() {
                ui.painter()
                    .rect_stroke(face.expand(3.), radius + 3., Stroke::new(2., p.accent));
            }
            ui.painter().galley_with_override_text_color(
                face.center() - galley.size() * 0.5,
                galley,
                a.text,
            );
        }
        if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn draw(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        enabled: bool,
    ) -> (egui::FullOutput, Response) {
        let mut response = None;
        let output = ctx.run(
            egui::RawInput {
                time: Some(time),
                events,
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600., 200.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    response = Some(ui.add_enabled(
                        enabled,
                        Button::new("Save", ButtonKind::Primary).min_size(vec2(150., 40.)),
                    ));
                });
            },
        );
        (output, response.unwrap())
    }
    #[test]
    fn thick_shadow_and_press_travel_are_exclusive_to_primary_kinds() {
        for p in [&tokens::LIGHT, &tokens::DARK] {
            for kind in [ButtonKind::Primary, ButtonKind::Danger] {
                let rest = appearance(p, false, kind, true, 0., 0.);
                let hover = appearance(p, false, kind, true, 1., 0.);
                let down = appearance(p, false, kind, true, 1., 1.);
                assert_eq!((rest.depth, hover.depth, down.depth), (5., 6., 1.));
                assert_eq!((rest.offset, hover.offset, down.offset), (0., -1., 2.));
                assert_eq!(rest.blur, 0.);
            }
            assert!(appearance(p, false, ButtonKind::Secondary, true, 1., 0.).blur > 0.);
            assert_eq!(
                appearance(p, false, ButtonKind::Ghost, true, 1., 0.).shadow,
                Color32::TRANSPARENT
            );
            for kind in [
                ButtonKind::Primary,
                ButtonKind::Secondary,
                ButtonKind::Ghost,
                ButtonKind::Danger,
            ] {
                let disabled = appearance(p, false, kind, false, 1., 1.);
                assert_eq!(disabled.shadow, Color32::TRANSPARENT);
                assert_eq!(disabled.offset, 0.);
            }
        }
    }
    #[test]
    fn native_pointer_keyboard_and_disabled_semantics_keep_the_hit_rect_stable() {
        let ctx = egui::Context::default();
        let (_, initial) = draw(&ctx, 0., vec![], true);
        let (_, settled) = draw(&ctx, 0.1, vec![], true);
        assert!(settled.rect.width() >= 150. && settled.rect.height() >= 40.);
        let at = settled.rect.center();
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let (_, down) = draw(
            &ctx,
            0.2,
            vec![egui::Event::PointerMoved(at), click(true)],
            true,
        );
        assert_eq!(down.rect, initial.rect);
        let (_, up) = draw(&ctx, 0.3, vec![click(false)], true);
        assert!(up.clicked());
        up.request_focus();
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let (_, activated) = draw(&ctx, 0.4, vec![enter.clone()], true);
        assert!(activated.clicked());
        let (_, disabled) = draw(&ctx, 0.5, vec![enter, click(true), click(false)], false);
        assert!(!disabled.clicked() && !disabled.enabled());
    }
    #[test]
    fn hover_and_press_animations_settle_without_an_idle_repaint_loop() {
        let ctx = egui::Context::default();
        draw(&ctx, 0., vec![], true);
        let (_, r) = draw(&ctx, 0.1, vec![], true);
        draw(
            &ctx,
            1.,
            vec![egui::Event::PointerMoved(r.rect.center())],
            true,
        );
        let mut output = draw(&ctx, 1.4, vec![], true).0;
        for n in 0..3 {
            output = draw(&ctx, 2. + n as f64 * 0.4, vec![], true).0;
        }
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                > Duration::from_millis(100)
        );
    }
    #[test]
    fn actual_button_text_uses_state_color_even_inside_a_colored_parent() {
        for dark in [false, true] {
            let p = if dark { &tokens::DARK } else { &tokens::LIGHT };
            for kind in [
                ButtonKind::Primary,
                ButtonKind::Secondary,
                ButtonKind::Ghost,
                ButtonKind::Danger,
            ] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                let output = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.visuals_mut().override_text_color = Some(Color32::from_rgb(255, 0, 255));
                        ui.add(Button::new(
                            egui::RichText::new("State-owned button")
                                .color(Color32::from_rgb(255, 0, 255)),
                            kind,
                        ));
                    });
                });
                let expected = match kind {
                    ButtonKind::Danger if dark => p.danger,
                    ButtonKind::Danger => Color32::WHITE,
                    _ => p.text,
                };
                let text_shapes: Vec<_> = output
                    .shapes
                    .into_iter()
                    .filter(|s| {
                        matches!(&s.shape,
                    egui::Shape::Text(t) if t.galley.text()=="State-owned button")
                    })
                    .collect();
                assert_eq!(text_shapes.len(), 1);
                let mesh_colors: Vec<_> = ctx
                    .tessellate(text_shapes, output.pixels_per_point)
                    .into_iter()
                    .flat_map(|p| match p.primitive {
                        egui::epaint::Primitive::Mesh(m) => {
                            m.vertices.into_iter().map(|v| v.color).collect()
                        }
                        _ => Vec::new(),
                    })
                    .collect();
                assert!(!mesh_colors.is_empty());
                assert!(
                    mesh_colors.iter().all(|c| *c == expected),
                    "{dark:?} {kind:?}"
                );
            }
        }
    }
}
