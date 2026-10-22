//! Shared colors and control states for the desktop application.
use eframe::egui::{self, Color32 as C};

pub const ACCENT: C = C::from_rgb(37, 99, 235);
pub const INK: C = C::from_rgb(23, 37, 61);
pub const MUTED: C = C::from_rgb(83, 100, 124);
pub const CANVAS: C = C::from_rgb(245, 248, 252);
pub const SURFACE: C = C::WHITE;
pub const SIDEBAR: C = C::from_rgb(239, 245, 253);
pub const TINT: C = C::from_rgb(234, 242, 255);
pub const BORDER: C = C::from_rgb(221, 230, 242);
pub const CONTROL_BORDER: C = C::from_rgb(129, 147, 171);
pub const DANGER: C = C::from_rgb(180, 35, 54);
pub const SUCCESS: C = C::from_rgb(22, 112, 73);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = SURFACE;
    visuals.faint_bg_color = TINT;
    visuals.hyperlink_color = ACCENT;
    visuals.error_fg_color = DANGER;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, C::WHITE);
    visuals.widgets.noninteractive.fg_stroke.color = INK;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.bg_fill = SURFACE;
    visuals.widgets.inactive.weak_bg_fill = SURFACE;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, CONTROL_BORDER);
    visuals.widgets.inactive.fg_stroke.color = INK;
    visuals.widgets.hovered.bg_fill = TINT;
    visuals.widgets.hovered.weak_bg_fill = TINT;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke.color = INK;
    visuals.widgets.active.bg_fill = TINT;
    visuals.widgets.active.weak_bg_fill = TINT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, ACCENT);
    // egui also uses active foreground as its default strong text color.
    visuals.widgets.active.fg_stroke.color = INK;
    visuals.widgets.open = visuals.widgets.hovered;
    visuals.window_rounding = egui::Rounding::same(12.0);
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.rounding = egui::Rounding::same(7.0);
        }
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 32.0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contrast(a: C, b: C) -> f32 {
        fn luminance(c: C) -> f32 {
            let linear = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn text_selection_and_control_boundaries_remain_legible() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let style = ctx.style();
        let v = &style.visuals;
        assert!(contrast(v.strong_text_color(), v.panel_fill) >= 4.5);
        assert!(contrast(v.text_color(), v.window_fill) >= 4.5);
        assert!(contrast(MUTED, CANVAS) >= 4.5);
        assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) >= 4.5);
        assert!(contrast(v.widgets.inactive.bg_stroke.color, SURFACE) >= 3.0);
    }
}
