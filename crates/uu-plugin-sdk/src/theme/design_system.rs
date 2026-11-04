//! Theme-aware tokens and controls. Compatibility constants live in the parent.
use eframe::egui::{self, Color32 as C, Response, RichText, Stroke, Widget, WidgetText};
use super::island;

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
/// Application color family, independent of Light/Dark/System appearance.
/// Keep this value in each host Context, never in process-global mutable state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorTheme {
    #[default]
    Blue,
    Cyan,
    Animal,
}
pub const COLOR_THEMES: [ColorTheme; 3] = [ColorTheme::Blue, ColorTheme::Cyan, ColorTheme::Animal];
pub const CYAN_LIGHT: Palette = Palette {
    base: C::from_rgb(242, 251, 252),
    overlay: C::from_rgb(248, 253, 254),
    sidebar: C::from_rgb(233, 247, 249),
    text: C::from_rgb(22, 47, 58),
    muted: C::from_rgb(75, 104, 116),
    border: C::from_rgb(203, 221, 226),
    control_border: C::from_rgb(117, 142, 152),
    hover: C::from_rgb(221, 243, 247),
    pressed: C::from_rgb(192, 232, 239),
    tint: C::from_rgb(221, 243, 247),
    accent: C::from_rgb(14, 116, 144),
    primary: C::from_rgb(14, 116, 144),
    primary_hover: C::from_rgb(12, 122, 142),
    primary_pressed: C::from_rgb(21, 94, 117),
    disabled: C::from_rgb(230, 241, 244),
    info_bg: C::from_rgb(221, 243, 247),
    ..LIGHT
};
pub const CYAN_DARK: Palette = Palette {
    base: C::from_rgb(11, 31, 37),
    card: C::from_rgb(17, 43, 52),
    overlay: C::from_rgb(23, 56, 66),
    sidebar: C::from_rgb(12, 36, 43),
    text: C::from_rgb(230, 247, 250),
    muted: C::from_rgb(166, 196, 205),
    border: C::from_rgb(44, 86, 96),
    control_border: C::from_rgb(114, 151, 162),
    control: C::from_rgb(23, 56, 66),
    hover: C::from_rgb(33, 73, 85),
    pressed: C::from_rgb(14, 42, 51),
    tint: C::from_rgb(13, 62, 75),
    accent: C::from_rgb(103, 232, 249),
    primary: C::from_rgb(14, 116, 144),
    primary_hover: C::from_rgb(12, 122, 142),
    primary_pressed: C::from_rgb(21, 94, 117),
    disabled: C::from_rgb(23, 49, 58),
    info_bg: C::from_rgb(13, 62, 75),
    ..DARK
};
const fn island_palette(p: island::Tokens) -> Palette {
    Palette {
        base:p.base, card:p.card, overlay:p.overlay, sidebar:p.sidebar,
        text:p.text, muted:p.muted, border:p.border, control_border:p.control_border,
        control:p.control, hover:p.hover, pressed:p.pressed, tint:p.tint,
        accent:p.accent, primary:p.primary, primary_hover:p.primary_hover,
        primary_pressed:p.primary_pressed, disabled:p.disabled,
        success:p.success, warning:p.warning, danger:p.danger,
        success_bg:p.success_bg, warning_bg:p.warning_bg, danger_bg:p.danger_bg,
        info_bg:p.info_bg,
    }
}
pub const ANIMAL_LIGHT: Palette = island_palette(island::LIGHT);
pub const ANIMAL_DARK: Palette = island_palette(island::DARK);
fn colors(theme: egui::Theme, color: ColorTheme) -> &'static Palette {
    match (theme, color) {
        (egui::Theme::Light, ColorTheme::Blue) => &LIGHT,
        (egui::Theme::Dark, ColorTheme::Blue) => &DARK,
        (egui::Theme::Light, ColorTheme::Cyan) => &CYAN_LIGHT,
        (egui::Theme::Dark, ColorTheme::Cyan) => &CYAN_DARK,
        (egui::Theme::Light, ColorTheme::Animal) => &ANIMAL_LIGHT,
        (egui::Theme::Dark, ColorTheme::Animal) => &ANIMAL_DARK,
    }
}
fn visual_family(visuals: &egui::Visuals) -> ColorTheme {
    // The host-generated hyperlink accent is a semantic color-family token.
    // Visuals clones and widget-local state changes keep it, so existing host
    // and extension callers resolve their own context's palette without TLS or
    // process-global state. Local controls must not repurpose this global token.
    if visuals.hyperlink_color == ANIMAL_LIGHT.accent || visuals.hyperlink_color == ANIMAL_DARK.accent {
        ColorTheme::Animal
    } else if visuals.hyperlink_color == CYAN_LIGHT.accent || visuals.hyperlink_color == CYAN_DARK.accent {
        ColorTheme::Cyan
    } else {
        ColorTheme::Blue
    }
}
pub fn is_animal(visuals: &egui::Visuals) -> bool { visual_family(visuals) == ColorTheme::Animal }
pub fn palette(visuals: &egui::Visuals) -> &'static Palette {
    colors(if visuals.dark_mode { egui::Theme::Dark } else { egui::Theme::Light }, visual_family(visuals))
}

fn color_theme_id() -> egui::Id { egui::Id::new("eclipse.color-theme.v1") }
pub fn current_color_theme(ctx: &egui::Context) -> ColorTheme {
    ctx.data(|data| data.get_temp(color_theme_id()).unwrap_or_default())
}
/// Apply a color family to both mode styles. Returns whether it changed.
/// Calling `apply` again from an extension preserves this context-owned choice.
pub fn apply_color_theme(ctx: &egui::Context, color: ColorTheme) -> bool {
    let changed = ctx.data_mut(|data| {
        let previous = data.get_temp::<ColorTheme>(color_theme_id());
        data.insert_temp(color_theme_id(), color);
        previous != Some(color)
    });
    if changed { apply(ctx); }
    changed
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
    visuals_with_color(theme, ColorTheme::Blue)
}
pub fn visuals_with_color(theme: egui::Theme, color: ColorTheme) -> egui::Visuals {
    if color == ColorTheme::Animal { return island::tokens::style(theme).visuals; }
    let mut v = match theme {
        egui::Theme::Dark => egui::Visuals::dark(),
        egui::Theme::Light => egui::Visuals::light(),
    };
    let p = colors(theme, color);
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
    let color = current_color_theme(ctx);
    ctx.data_mut(|data| data.insert_temp(color_theme_id(), color));
    if color == ColorTheme::Animal { island::fonts::ensure(ctx); }
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        if color == ColorTheme::Animal {
            // Keep the library's geometry, named font, native selection and
            // control styles together; host compatibility tokens stay 8/32.
            ctx.set_style_of(theme, island::tokens::style(theme));
            continue;
        }
        let mut style = egui::Style {
            visuals: visuals_with_color(theme, color),
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
    if is_animal(visuals) { return island::Card::frame(visuals); }
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
    Danger,
}
/// Native egui button interaction with shared state colors. `Button::fill`
/// would override hovered/pressed fills, so appearance is applied via Visuals.
#[must_use = "add this widget with ui.add or ui.add_enabled"]
pub struct Button {
    text: WidgetText,
    kind: ButtonKind,
    small: bool,
    min_size: egui::Vec2,
}
impl Button {
    pub fn new(text: impl Into<WidgetText>, kind: ButtonKind) -> Self {
        Self {
            text: text.into(),
            kind,
            small: false,
            min_size: egui::Vec2::ZERO,
        }
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn min_size(mut self, size: egui::Vec2) -> Self {
        self.min_size = size;
        self
    }
}
fn button_visuals(v: &mut egui::Visuals, p: &Palette, kind: ButtonKind, enabled: bool) {
    let text = match kind {
        ButtonKind::Primary => C::WHITE,
        ButtonKind::Danger => p.danger,
        _ => p.text,
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
        ButtonKind::Danger => {
            v.widgets.inactive = widget(p.control, p.danger, text);
            v.widgets.hovered = widget(p.danger_bg, p.danger, text);
            v.widgets.active = widget(p.pressed, p.danger, text);
        }
    }
    v.widgets.active.bg_stroke.width = 1.5;
}
impl Widget for Button {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        if is_animal(ui.visuals()) {
            let kind = match self.kind {
                ButtonKind::Primary => island::ButtonKind::Primary,
                ButtonKind::Secondary => island::ButtonKind::Secondary,
                ButtonKind::Ghost => island::ButtonKind::Ghost,
                ButtonKind::Danger => island::ButtonKind::Danger,
            };
            return ui.add(island::Button::new(self.text, kind).compact(self.small).min_size(self.min_size));
        }
        let p = *palette(ui.visuals());
        let enabled = ui.is_enabled();
        ui.scope(|ui| {
            button_visuals(ui.visuals_mut(), &p, self.kind, enabled);
            let button = egui::Button::new(self.text).min_size(self.min_size);
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
pub fn danger_button(text: impl Into<WidgetText>) -> Button {
    Button::new(text, ButtonKind::Danger)
}
/// Use the shared switch only for Animal; legacy themes keep native behavior.
pub fn checkbox(ui: &mut egui::Ui, value: &mut bool, label: impl Into<WidgetText>) -> Response {
    if is_animal(ui.visuals()) { ui.add(island::Switch::new(value, label)) }
    else { ui.checkbox(value, label) }
}
/// Keep native text/IME ownership while selecting the library's Animal styling.
pub fn text_input(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> Response {
    if is_animal(ui.visuals()) { ui.add(island::Input::new(value).hint_text(hint).desired_width(width)) }
    else { ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(width)) }
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
    if is_animal(ui.visuals()) {
        let tone = match tone {
            Tone::Neutral => island::containers::Tone::Neutral,
            Tone::Info => island::containers::Tone::Info,
            Tone::Success => island::containers::Tone::Success,
            Tone::Warning => island::containers::Tone::Warning,
            Tone::Danger => island::containers::Tone::Danger,
        };
        return ui.add(island::Tag::new(text, tone));
    }
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
        for (theme, color) in [egui::Theme::Dark, egui::Theme::Light].into_iter()
            .flat_map(|theme| COLOR_THEMES.map(|color| (theme, color))) {
            let v = visuals_with_color(theme, color);
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
            if color != ColorTheme::Animal {
                for fill in [p.primary, p.primary_hover, p.primary_pressed] {
                    assert!(contrast(C::WHITE, fill) >= 4.5);
                }
            } else {
                // Animal's real primary is warm ink on cream, not white on
                // the mint semantic accent. Its native selection comes from
                // the library style and is checked above using actual colors.
                let tokens = if theme == egui::Theme::Dark { island::DARK } else { island::LIGHT };
                assert!(contrast(tokens.text, tokens.base) >= 4.5);
            }
        }
    }
    #[test]
    fn context_color_choices_survive_extensions_modes_and_visuals_clones() {
        let blue = egui::Context::default();
        let cyan = egui::Context::default();
        apply(&blue);
        apply_color_theme(&cyan, ColorTheme::Cyan);
        for mode in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            blue.set_theme(mode); cyan.set_theme(mode);
            // Extensions can register the shared controls at any time.
            apply(&cyan); apply(&blue);
            assert_eq!(palette(&blue.style().visuals).primary, colors(mode, ColorTheme::Blue).primary);
            assert_eq!(palette(&cyan.style().visuals).base, colors(mode, ColorTheme::Cyan).base);
            assert_eq!(current_color_theme(&blue), ColorTheme::Blue);
            assert_eq!(current_color_theme(&cyan), ColorTheme::Cyan);
            for kind in [ButtonKind::Primary, ButtonKind::Secondary, ButtonKind::Ghost] {
                let mut local = cyan.style().visuals.clone();
                let p = *palette(&local);
                button_visuals(&mut local, &p, kind, true);
                assert_eq!(palette(&local).accent, p.accent, "local widget states preserve the family token");
                button_visuals(&mut local, &p, kind, false);
                assert_eq!(palette(&local).accent, p.accent);
            }
        }
        assert!(!apply_color_theme(&cyan, ColorTheme::Cyan));
        assert!(apply_color_theme(&cyan, ColorTheme::Blue));
        assert_eq!(current_color_theme(&blue), ColorTheme::Blue);
        assert_eq!(palette(&cyan.style().visuals).primary, LIGHT.primary);
    }
    #[test]
    fn animal_uses_the_library_style_and_preserves_legacy_theme_geometry() {
        let ctx = egui::Context::default();
        apply(&ctx);
        for mode in [egui::Theme::Light, egui::Theme::Dark] {
            ctx.set_theme(mode);
            apply_color_theme(&ctx, ColorTheme::Animal);
            apply(&ctx);
            let actual = ctx.style();
            let expected = island::tokens::style(mode);
            assert!(is_animal(&actual.visuals));
            assert_eq!(actual.visuals, expected.visuals);
            assert_eq!(actual.spacing.interact_size.y, island::tokens::CONTROL_HEIGHT);
            assert_eq!(actual.visuals.window_rounding, egui::Rounding::same(island::tokens::CARD_RADIUS));
            assert_eq!(actual.text_styles[&egui::TextStyle::Body].family, island::fonts::family());
            assert_eq!(card(&actual.visuals).rounding, island::Card::frame(&actual.visuals).rounding);
            assert!(contrast(actual.visuals.selection.stroke.color, actual.visuals.selection.bg_fill) >= 4.5);
        }
        for color in [ColorTheme::Blue, ColorTheme::Cyan] {
            apply_color_theme(&ctx, color);
            assert_eq!(ctx.style().spacing.interact_size.y, CONTROL_HEIGHT);
            assert_eq!(ctx.style().visuals.window_rounding, egui::Rounding::same(RADIUS));
            assert!(!is_animal(&ctx.style().visuals));
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
