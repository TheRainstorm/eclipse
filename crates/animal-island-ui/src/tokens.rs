//! Shared semantic colors and geometry. No process-global theme state.
use egui::{Color32 as C, Ui};

pub const CONTROL_HEIGHT: f32 = 40.0;
pub const COMPACT_HEIGHT: f32 = 32.0;
pub const CARD_RADIUS: f32 = 20.0;
pub const PILL_RADIUS: f32 = 50.0;
pub const MOTION_SECONDS: f32 = 0.2;

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub base: C,
    pub card: C,
    pub overlay: C,
    pub sidebar: C,
    pub text: C,
    pub muted: C,
    pub border: C,
    pub control_border: C,
    pub control: C,
    pub hover: C,
    pub pressed: C,
    pub tint: C,
    pub accent: C,
    pub primary: C,
    pub primary_hover: C,
    pub primary_pressed: C,
    pub disabled: C,
    pub success: C,
    pub warning: C,
    pub danger: C,
    pub success_bg: C,
    pub warning_bg: C,
    pub danger_bg: C,
    pub info_bg: C,
    pub shadow: C,
    pub focus: C,
    pub switch_on: C,
    pub switch_off: C,
}
// Core light colors follow the reference's cream, warm brown and mint tokens.
// Muted text/status accents are deepened where needed for readable small text.
pub const LIGHT: Tokens = Tokens {
    base: C::from_rgb(248, 248, 240),
    card: C::from_rgb(247, 243, 223),
    overlay: C::from_rgb(255, 253, 247),
    sidebar: C::from_rgb(240, 232, 216),
    text: C::from_rgb(114, 93, 66),
    muted: C::from_rgb(120, 105, 84),
    border: C::from_rgb(212, 201, 180),
    control_border: C::from_rgb(158, 145, 124),
    control: C::from_rgb(255, 253, 247),
    hover: C::from_rgb(230, 249, 246),
    pressed: C::from_rgb(224, 241, 224),
    tint: C::from_rgb(230, 249, 246),
    accent: C::from_rgb(17, 119, 107),
    primary: C::from_rgb(25, 200, 185),
    primary_hover: C::from_rgb(61, 212, 198),
    primary_pressed: C::from_rgb(17, 168, 155),
    disabled: C::from_rgb(236, 232, 220),
    success: C::from_rgb(61, 112, 31),
    warning: C::from_rgb(122, 87, 10),
    danger: C::from_rgb(165, 54, 54),
    success_bg: C::from_rgb(226, 242, 212),
    warning_bg: C::from_rgb(255, 243, 200),
    danger_bg: C::from_rgb(255, 230, 224),
    info_bg: C::from_rgb(230, 249, 246),
    shadow: C::from_rgb(189, 174, 160),
    focus: C::from_rgb(255, 204, 0),
    switch_on: C::from_rgb(134, 214, 122),
    switch_off: C::from_rgb(212, 201, 180),
};
// A warm night interpretation; the reference itself specifies a light theme.
pub const DARK: Tokens = Tokens {
    base: C::from_rgb(37, 34, 29),
    card: C::from_rgb(49, 45, 37),
    overlay: C::from_rgb(57, 52, 43),
    sidebar: C::from_rgb(43, 39, 32),
    text: C::from_rgb(247, 236, 211),
    muted: C::from_rgb(203, 188, 161),
    border: C::from_rgb(87, 78, 61),
    control_border: C::from_rgb(155, 137, 108),
    control: C::from_rgb(64, 58, 47),
    hover: C::from_rgb(68, 75, 58),
    pressed: C::from_rgb(47, 62, 49),
    tint: C::from_rgb(44, 65, 54),
    accent: C::from_rgb(113, 223, 190),
    primary: C::from_rgb(25, 200, 185),
    primary_hover: C::from_rgb(61, 212, 198),
    primary_pressed: C::from_rgb(17, 168, 155),
    disabled: C::from_rgb(54, 49, 41),
    success: C::from_rgb(174, 218, 131),
    warning: C::from_rgb(244, 208, 116),
    danger: C::from_rgb(255, 163, 146),
    success_bg: C::from_rgb(48, 65, 39),
    warning_bg: C::from_rgb(72, 57, 30),
    danger_bg: C::from_rgb(78, 43, 37),
    info_bg: C::from_rgb(44, 65, 54),
    shadow: C::from_rgb(25, 23, 19),
    focus: C::from_rgb(255, 211, 74),
    switch_on: C::from_rgb(113, 178, 96),
    switch_off: C::from_rgb(104, 91, 70),
};
pub fn palette(visuals: &egui::Visuals) -> &'static Tokens {
    if visuals.dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}
pub fn tokens(ui: &Ui) -> &'static Tokens {
    palette(ui.visuals())
}

/// Standalone native style; applications can reuse this without any widgets.
pub fn style(theme: egui::Theme) -> egui::Style {
    let mut style = egui::Style::default();
    let mut v = match theme {
        egui::Theme::Light => egui::Visuals::light(),
        egui::Theme::Dark => egui::Visuals::dark(),
    };
    let p = palette(&v);
    let widget = |fill, border, text| egui::style::WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: egui::Stroke::new(1.5, border),
        fg_stroke: egui::Stroke::new(1.0, text),
        rounding: egui::Rounding::same(PILL_RADIUS),
        expansion: 0.0,
    };
    v.panel_fill = p.base;
    v.window_fill = p.overlay;
    v.extreme_bg_color = p.base;
    v.faint_bg_color = p.tint;
    v.hyperlink_color = p.accent;
    v.override_text_color = None;
    v.error_fg_color = p.danger;
    v.warn_fg_color = p.warning;
    v.selection.bg_fill = p.focus;
    v.selection.stroke = egui::Stroke::new(1.0, C::from_rgb(43, 38, 32));
    v.widgets.noninteractive = widget(p.card, p.border, p.text);
    v.widgets.inactive = widget(p.control, p.control_border, p.text);
    v.widgets.hovered = widget(p.hover, p.accent, p.text);
    v.widgets.active = widget(p.pressed, p.focus, p.text);
    v.widgets.open = widget(p.tint, p.accent, p.text);
    v.window_rounding = egui::Rounding::same(CARD_RADIUS);
    v.menu_rounding = egui::Rounding::same(16.0);
    v.window_stroke = egui::Stroke::new(1.5, p.border);
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    style.visuals = v;
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.item_spacing = egui::vec2(8.0, 12.0);
    style.spacing.button_padding = egui::vec2(18.0, 8.0);
    for (kind, size) in [
        (egui::TextStyle::Heading, 24.0),
        (egui::TextStyle::Body, 14.0),
        (egui::TextStyle::Button, 14.0),
        (egui::TextStyle::Small, 12.0),
        (egui::TextStyle::Monospace, 14.0),
    ] {
        style
            .text_styles
            .insert(kind, egui::FontId::new(size, crate::fonts::family()));
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contrast(a: C, b: C) -> f32 {
        let luminance = |c: C| {
            let linear = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn both_palettes_keep_body_status_and_selected_text_readable() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let style = style(theme);
            let p = palette(&style.visuals);
            for surface in [p.base, p.card, p.overlay] {
                assert!(contrast(p.text, surface) >= 4.5, "{theme:?} body");
                assert!(contrast(p.muted, surface) >= 4.5, "{theme:?} secondary");
            }
            for (fg, bg) in [
                (p.success, p.success_bg),
                (p.warning, p.warning_bg),
                (p.danger, p.danger_bg),
                (p.accent, p.info_bg),
            ] {
                assert!(contrast(fg, bg) >= 4.5, "{theme:?} status {fg:?}");
            }
            assert!(
                contrast(
                    style.visuals.selection.stroke.color,
                    style.visuals.selection.bg_fill
                ) >= 4.5
            );
        }
    }
}
