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

#[path = "theme/design_system.rs"]
mod design_system;
pub use design_system::*;

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
        ctx.set_theme(egui::Theme::Light);
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
