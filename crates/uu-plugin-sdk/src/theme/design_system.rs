//! Theme-aware tokens and controls. Compatibility constants live in the parent.
use eframe::egui::{self, Color32 as C, Response, RichText, Stroke, Widget, WidgetText};

pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_6: f32 = 24.0;
pub const RADIUS: f32 = 8.0;
pub const CONTROL_HEIGHT: f32 = 32.0;
pub const FONT_TITLE: f32 = 24.0;
pub const FONT_SECTION: f32 = 16.0;
pub const FONT_BODY: f32 = 14.0;
pub const FONT_META: f32 = 12.0;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
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
}

pub const DARK: Palette = Palette {
    base: C::from_rgb(17, 21, 28),
    card: C::from_rgb(27, 34, 45),
    overlay: C::from_rgb(37, 47, 61),
    sidebar: C::from_rgb(23, 29, 39),
    text: C::from_rgb(232, 237, 245),
    muted: C::from_rgb(168, 181, 198),
    border: C::from_rgb(52, 65, 82),
    control_border: C::from_rgb(120, 139, 164),
    control: C::from_rgb(37, 47, 61),
    hover: C::from_rgb(50, 65, 86),
    pressed: C::from_rgb(25, 33, 46),
    tint: C::from_rgb(29, 48, 76),
    accent: C::from_rgb(141, 183, 255),
    primary: C::from_rgb(37, 99, 235),
    primary_hover: C::from_rgb(42, 107, 238),
    primary_pressed: C::from_rgb(29, 78, 216),
    disabled: C::from_rgb(32, 40, 52),
    success: C::from_rgb(98, 213, 162),
    warning: C::from_rgb(242, 198, 109),
    danger: C::from_rgb(255, 141, 155),
    success_bg: C::from_rgb(23, 56, 45),
    warning_bg: C::from_rgb(57, 47, 29),
    danger_bg: C::from_rgb(62, 37, 45),
    info_bg: C::from_rgb(29, 48, 76),
};
pub const LIGHT: Palette = Palette {
    base: C::from_rgb(245, 248, 252),
    card: C::WHITE,
    overlay: C::from_rgb(250, 252, 255),
    sidebar: C::from_rgb(239, 245, 253),
    text: C::from_rgb(23, 37, 61),
    muted: C::from_rgb(83, 100, 124),
    border: C::from_rgb(221, 230, 242),
    control_border: C::from_rgb(129, 147, 171),
    control: C::WHITE,
    hover: C::from_rgb(234, 242, 255),
    pressed: C::from_rgb(212, 228, 253),
    tint: C::from_rgb(234, 242, 255),
    accent: C::from_rgb(37, 99, 235),
    primary: C::from_rgb(37, 99, 235),
    primary_hover: C::from_rgb(42, 107, 238),
    primary_pressed: C::from_rgb(29, 78, 216),
    disabled: C::from_rgb(234, 239, 246),
    success: C::from_rgb(22, 112, 73),
    warning: C::from_rgb(133, 82, 0),
    danger: C::from_rgb(180, 35, 54),
    success_bg: C::from_rgb(230, 245, 236),
    warning_bg: C::from_rgb(255, 245, 217),
    danger_bg: C::from_rgb(255, 235, 238),
    info_bg: C::from_rgb(234, 242, 255),
};
pub fn palette(visuals: &egui::Visuals) -> &'static Palette {
    if visuals.dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}
fn widget(fill: C, border: C, text: C) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(1.0, border),
        fg_stroke: Stroke::new(1.0, text),
        rounding: egui::Rounding::same(RADIUS),
        expansion: 0.0,
    }
}
pub fn visuals(theme: egui::Theme) -> egui::Visuals {
    let mut v = match theme {
        egui::Theme::Dark => egui::Visuals::dark(),
        egui::Theme::Light => egui::Visuals::light(),
    };
    let p = palette(&v);
    v.panel_fill = p.base;
    v.window_fill = p.overlay;
    v.extreme_bg_color = p.base;
    v.faint_bg_color = p.tint;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.danger;
    v.warn_fg_color = p.warning;
    v.selection.bg_fill = p.primary;
    v.selection.stroke = Stroke::new(1.0, C::WHITE);
    v.widgets.noninteractive = widget(p.card, p.border, p.text);
    v.widgets.inactive = widget(p.control, p.control_border, p.text);
    v.widgets.hovered = widget(p.hover, p.accent, p.text);
    v.widgets.active = widget(p.pressed, p.accent, p.text);
    v.widgets.active.bg_stroke.width = 1.5;
    v.widgets.open = widget(p.tint, p.accent, p.text);
    v.window_rounding = egui::Rounding::same(RADIUS);
    v.menu_rounding = egui::Rounding::same(RADIUS);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v
}
/// Configure both theme styles without changing the host's theme preference.
pub fn apply(ctx: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let mut style = egui::Style {
            visuals: visuals(theme),
            ..Default::default()
        };
        for (kind, size) in [
            (egui::TextStyle::Heading, FONT_TITLE),
            (egui::TextStyle::Body, FONT_BODY),
            (egui::TextStyle::Button, FONT_BODY),
            (egui::TextStyle::Small, FONT_META),
        ] {
            style
                .text_styles
                .insert(kind, egui::FontId::proportional(size));
        }
        style.spacing.item_spacing = egui::vec2(SPACE_2, SPACE_2);
        style.spacing.button_padding = egui::vec2(SPACE_3, SPACE_2);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        ctx.set_style_of(theme, style);
    }
}
pub fn card(visuals: &egui::Visuals) -> egui::Frame {
    let p = palette(visuals);
    egui::Frame::none()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .rounding(RADIUS)
        .inner_margin(SPACE_4)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}
/// Native egui button interaction with shared state colors. `Button::fill`
/// would override hovered/pressed fills, so appearance is applied via Visuals.
#[must_use = "add this widget with ui.add or ui.add_enabled"]
pub struct Button {
    text: WidgetText,
    kind: ButtonKind,
    small: bool,
}
impl Button {
    pub fn new(text: impl Into<WidgetText>, kind: ButtonKind) -> Self {
        Self {
            text: text.into(),
            kind,
            small: false,
        }
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
}
fn button_visuals(v: &mut egui::Visuals, p: &Palette, kind: ButtonKind, enabled: bool) {
    let text = if kind == ButtonKind::Primary {
        C::WHITE
    } else {
        p.text
    };
    v.override_text_color = Some(if enabled { text } else { p.muted });
    if !enabled {
        let disabled = widget(p.disabled, p.border, p.muted);
        v.widgets.noninteractive = disabled;
        v.widgets.inactive = disabled;
        v.widgets.hovered = disabled;
        v.widgets.active = disabled;
        return;
    }
    match kind {
        ButtonKind::Primary => {
            v.widgets.inactive = widget(p.primary, p.primary, text);
            v.widgets.hovered = widget(p.primary_hover, p.accent, text);
            v.widgets.active = widget(p.primary_pressed, p.accent, text);
        }
        ButtonKind::Secondary => {
            v.widgets.inactive = widget(p.control, p.control_border, text);
            v.widgets.hovered = widget(p.hover, p.accent, text);
            v.widgets.active = widget(p.pressed, p.accent, text);
        }
        ButtonKind::Ghost => {
            v.widgets.inactive = widget(p.control, C::TRANSPARENT, text);
            v.widgets.inactive.weak_bg_fill = C::TRANSPARENT;
            v.widgets.hovered = widget(p.hover, p.border, text);
            v.widgets.active = widget(p.pressed, p.accent, text);
        }
    }
    v.widgets.active.bg_stroke.width = 1.5;
}
impl Widget for Button {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let p = *palette(ui.visuals());
        let enabled = ui.is_enabled();
        ui.scope(|ui| {
            button_visuals(ui.visuals_mut(), &p, self.kind, enabled);
            let button = egui::Button::new(self.text);
            ui.add(if self.small { button.small() } else { button })
        })
        .inner
    }
}
pub fn primary_button(text: impl Into<WidgetText>) -> Button {
    Button::new(text, ButtonKind::Primary)
}
pub fn secondary_button(text: impl Into<WidgetText>) -> Button {
    Button::new(text, ButtonKind::Secondary)
}
pub fn ghost_button(text: impl Into<WidgetText>) -> Button {
    Button::new(text, ButtonKind::Ghost)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}
impl Palette {
    pub fn status(self, tone: Tone) -> (C, C) {
        match tone {
            Tone::Neutral => (self.muted, self.control),
            Tone::Info => (self.accent, self.info_bg),
            Tone::Success => (self.success, self.success_bg),
            Tone::Warning => (self.warning, self.warning_bg),
            Tone::Danger => (self.danger, self.danger_bg),
        }
    }
}
/// Read-only status. Meaning stays in the label rather than color alone.
pub fn pill(ui: &mut egui::Ui, text: &str, tone: Tone) -> Response {
    let (foreground, background) = palette(ui.visuals()).status(tone);
    egui::Frame::none()
        .fill(background)
        .rounding(20.0)
        .inner_margin(egui::Margin::symmetric(SPACE_2, SPACE_1))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).size(FONT_META).color(foreground)).wrap())
        })
        .inner
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
    fn both_themes_keep_text_controls_and_status_legible() {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let v = visuals(theme);
            let p = palette(&v);
            for surface in [p.base, p.card, p.overlay] {
                assert!(contrast(p.text, surface) >= 4.5);
                assert!(contrast(p.muted, surface) >= 4.5);
            }
            assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) >= 4.5);
            assert!(contrast(p.control_border, p.control) >= 3.0);
            for tone in [
                Tone::Neutral,
                Tone::Info,
                Tone::Success,
                Tone::Warning,
                Tone::Danger,
            ] {
                let (fg, bg) = p.status(tone);
                assert!(contrast(fg, bg) >= 4.5, "{theme:?} {tone:?}");
            }
            for fill in [p.primary, p.primary_hover, p.primary_pressed] {
                assert!(contrast(C::WHITE, fill) >= 4.5);
            }
        }
    }
    #[test]
    fn theme_switch_keeps_geometry_and_preference() {
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Dark);
        apply(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            ctx.set_theme(theme);
            let style = ctx.style();
            assert_eq!(style.visuals.dark_mode, theme == egui::Theme::Dark);
            assert_eq!(style.spacing.interact_size.y, CONTROL_HEIGHT);
            assert_eq!(style.text_styles[&egui::TextStyle::Body].size, FONT_BODY);
        }
    }
    #[test]
    fn button_states_are_distinct_and_disabled_is_neutral() {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            for kind in [
                ButtonKind::Primary,
                ButtonKind::Secondary,
                ButtonKind::Ghost,
            ] {
                let mut v = visuals(theme);
                let p = *palette(&v);
                button_visuals(&mut v, &p, kind, true);
                assert_ne!(
                    v.widgets.inactive.weak_bg_fill,
                    v.widgets.hovered.weak_bg_fill
                );
                assert_ne!(
                    v.widgets.hovered.weak_bg_fill,
                    v.widgets.active.weak_bg_fill
                );
                button_visuals(&mut v, &p, kind, false);
                assert_eq!(v.widgets.inactive.bg_fill, p.disabled);
                assert_eq!(v.widgets.active, v.widgets.inactive);
            }
        }
    }
    #[test]
    fn native_button_click_and_disabled_share_real_input_rules() {
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Dark);
        apply(&ctx);
        let mut height = 0.;
        let mut clicks = 0;
        let mut disabled_clicked = false;
        let mut draw = |events: Vec<egui::Event>| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400., 200.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.add(primary_button("Connect"));
                        height = response.rect.height();
                        clicks += usize::from(response.clicked());
                        disabled_clicked |= ui
                            .add_enabled(false, primary_button("Unavailable"))
                            .clicked();
                    });
                },
            )
        };
        let _ = draw(vec![]);
        let at = egui::pos2(35., 24.);
        for pressed in [true, false] {
            let _ = draw(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
        }
        // Click the disabled second button using the same physical sequence.
        let at = egui::pos2(35., 64.);
        for pressed in [true, false] {
            let output = draw(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
            assert_ne!(
                output.platform_output.cursor_icon,
                egui::CursorIcon::PointingHand
            );
        }
        assert_eq!(clicks, 1);
        assert!(!disabled_clicked);
        assert!(height >= CONTROL_HEIGHT);
    }
    #[test]
    fn primary_uses_native_keyboard_activation() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut clicks = 0;
        for frame in 0..3 {
            let events = if frame == 2 {
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]
            } else {
                vec![]
            };
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.add(primary_button("Save"));
                        if frame == 0 {
                            response.request_focus();
                        }
                        clicks += usize::from(response.clicked());
                    });
                },
            );
        }
        assert_eq!(clicks, 1);
    }
}
