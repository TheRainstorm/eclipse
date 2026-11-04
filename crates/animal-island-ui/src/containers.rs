//! Cream cards, semantic pills and folded swallowtail headings.
use crate::tokens::{self, Tokens, CARD_RADIUS, PILL_RADIUS};
use egui::{
    pos2, vec2, Color32, Frame, InnerResponse, Margin, Pos2, Rect, Response, Sense, Shape, Stroke,
    TextStyle, Visuals, Widget, WidgetInfo, WidgetText, WidgetType,
};

/// Status is also expressed by the label; color never carries the only meaning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}
fn colors(p: &Tokens, tone: Tone) -> (Color32, Color32) {
    match tone {
        Tone::Neutral => (p.muted, p.sidebar),
        Tone::Info => (p.accent, p.info_bg),
        Tone::Success => (p.success, p.success_bg),
        Tone::Warning => (p.warning, p.warning_bg),
        Tone::Danger => (p.danger, p.danger_bg),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Card {
    padding: Margin,
}
impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}
impl Card {
    pub fn new() -> Self {
        Self {
            padding: Margin::symmetric(24., 16.),
        }
    }
    /// A card's depth comes from its warm surface and fine border, not shadow.
    pub fn frame(visuals: &Visuals) -> Frame {
        let p = tokens::palette(visuals);
        Frame::none()
            .fill(p.card)
            .rounding(CARD_RADIUS)
            .stroke(Stroke::new(1., p.border))
            .inner_margin(Margin::symmetric(24., 16.))
            .shadow(egui::epaint::Shadow::NONE)
    }
    pub fn inner_margin(mut self, padding: impl Into<Margin>) -> Self {
        self.padding = padding.into();
        self
    }
    pub fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> InnerResponse<R> {
        let text = tokens::tokens(ui).text;
        Self::frame(ui.visuals())
            .inner_margin(self.padding)
            .show(ui, |ui| {
                ui.visuals_mut().override_text_color = Some(text);
                add_contents(ui)
            })
    }
}

#[must_use = "add the tag with ui.add or call show"]
pub struct Tag {
    text: WidgetText,
    tone: Tone,
    small: bool,
}
impl Tag {
    pub fn new(text: impl Into<WidgetText>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            small: false,
        }
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn show(self, ui: &mut egui::Ui) -> Response {
        ui.add(self)
    }
}
impl Widget for Tag {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let label = self.text.text().to_owned();
        let text = if self.small {
            self.text.small()
        } else {
            self.text
        };
        let padding = if self.small { 10. } else { 12. };
        let galley = text.strong().into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (ui.available_width() - padding * 2.).max(0.),
            TextStyle::Button,
        );
        let height: f32 = if self.small { 24. } else { 32. };
        let (rect, response) = ui.allocate_exact_size(
            vec2(
                galley.size().x + padding * 2.,
                height.max(galley.size().y + 8.),
            ),
            Sense::hover(),
        );
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), &label));
        if ui.is_rect_visible(rect) {
            let (fg, bg) = colors(tokens::tokens(ui), self.tone);
            ui.painter().rect(
                rect,
                PILL_RADIUS.min(rect.height() * 0.5),
                bg,
                Stroke::new(1., fg.gamma_multiply(0.24)),
            );
            ui.painter().galley_with_override_text_color(
                rect.center() - galley.size() * 0.5,
                galley,
                fg,
            );
        }
        response
    }
}

/// A decorative heading with a front, two notched tails, and folded corners.
/// Its entire geometry remains inside the returned layout rectangle.
#[must_use = "add the ribbon with ui.add or call show"]
pub struct Ribbon {
    text: WidgetText,
    tone: Tone,
    size: f32,
}
impl Ribbon {
    pub fn new(text: impl Into<WidgetText>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Success,
            size: 24.,
        }
    }
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
    pub fn size(mut self, size: f32) -> Self {
        if size.is_finite() {
            self.size = size.clamp(12., 72.);
        }
        self
    }
    pub fn show(self, ui: &mut egui::Ui) -> Response {
        ui.add(self)
    }
}
fn tail(painter: &egui::Painter, rect: Rect, left: bool, color: Color32) {
    let outer = if left { rect.left() } else { rect.right() };
    let inner = if left { rect.right() } else { rect.left() };
    let notch = egui::lerp(outer..=inner, 0.3);
    let points = [
        pos2(outer, rect.top()),
        pos2(inner, rect.top()),
        pos2(inner, rect.bottom()),
        pos2(outer, rect.bottom()),
        pos2(notch, rect.center().y),
    ];
    // Explicit triangles preserve the concave swallowtail cutout. A convex
    // polygon tessellator would fill over the V-shaped notch.
    for indices in [[1, 2, 3], [1, 3, 4], [1, 4, 0]] {
        painter.add(Shape::convex_polygon(
            indices.map(|i| points[i]).to_vec(),
            color,
            Stroke::NONE,
        ));
    }
}
impl Widget for Ribbon {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let label = self.text.text().to_owned();
        let em = self.size;
        let mut font = TextStyle::Heading.resolve(ui.style());
        font.size = em;
        let galley = self.text.strong().into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (ui.available_width() - 4.4 * em).max(0.),
            font,
        );
        let width = (galley.size().x + 3.2 * em).max(3.4 * em);
        let height = (2. * em).max(galley.size().y + 0.4 * em);
        let (rect, response) =
            ui.allocate_exact_size(vec2(width + 1.2 * em, height + 0.4 * em), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), &label));
        if ui.is_rect_visible(rect) {
            let (fg, bg) = colors(tokens::tokens(ui), self.tone);
            let face = Rect::from_min_size(rect.min + vec2(0.6 * em, 0.), vec2(width, height));
            let left = Rect::from_min_size(
                pos2(rect.left(), face.bottom() - 1.3 * em),
                vec2(1.7 * em, 1.7 * em),
            );
            let right = Rect::from_min_size(pos2(rect.right() - 1.7 * em, left.top()), left.size());
            tail(ui.painter(), left, true, bg.gamma_multiply(0.80));
            tail(ui.painter(), right, false, bg.gamma_multiply(0.80));
            let fold = bg.gamma_multiply(0.58);
            for sign in [-1., 1.] {
                let x = if sign < 0. { face.left() } else { face.right() };
                let points: [Pos2; 3] = [
                    pos2(x - sign * 0.15 * em, face.bottom() - 0.05 * em),
                    pos2(x - sign * 1.10 * em, face.bottom() - 0.05 * em),
                    pos2(x - sign * 0.15 * em, face.bottom() + 0.4 * em),
                ];
                ui.painter()
                    .add(Shape::convex_polygon(points.to_vec(), fold, Stroke::NONE));
            }
            ui.painter().rect_filled(face, 0.2 * em, bg);
            ui.painter().galley_with_override_text_color(
                face.center() - galley.size() * 0.5 + vec2(0., 0.055 * em),
                galley,
                fg,
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cards_keep_host_state_and_tags_and_ribbons_are_read_only() {
        for visuals in [Visuals::light(), Visuals::dark()] {
            let frame = Card::frame(&visuals);
            assert_eq!(frame.shadow, egui::epaint::Shadow::NONE);
            assert_eq!(frame.rounding, egui::Rounding::same(20.));
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals);
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let original = ui.visuals().override_text_color;
                    let card = Card::new().show(ui, |ui| {
                        ui.label("A reusable card");
                        42
                    });
                    assert_eq!(card.inner, 42);
                    assert_eq!(ui.visuals().override_text_color, original);
                    for tone in [
                        Tone::Neutral,
                        Tone::Info,
                        Tone::Success,
                        Tone::Warning,
                        Tone::Danger,
                    ] {
                        let response = ui.add(Tag::new("Status", tone).small());
                        assert!(!response.sense.click);
                    }
                    let ribbon = ui.add(Ribbon::new("Island tools").tone(Tone::Warning));
                    assert!(!ribbon.sense.click);
                });
            });
        }
    }
    #[test]
    fn ribbon_preserves_notched_tails_as_separate_geometry_inside_its_bounds() {
        let ctx = egui::Context::default();
        let mut bounds = Rect::NOTHING;
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                bounds = ui.add(Ribbon::new("Heading")).rect;
            });
        });
        let polygons: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| {
                if let Shape::Path(path) = &s.shape {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            polygons.len(),
            8,
            "six tail triangles and two folded corners"
        );
        for path in polygons {
            for point in &path.points {
                assert!(bounds.expand(0.01).contains(*point));
            }
        }
    }
    #[test]
    fn actual_status_and_ribbon_text_override_parent_and_rich_text_colors() {
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                Visuals::dark()
            } else {
                Visuals::light()
            });
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    Card::new().show(ui, |ui| {
                        ui.visuals_mut().override_text_color = Some(Color32::from_rgb(255, 0, 255));
                        ui.add(Tag::new(
                            egui::RichText::new("Danger status").color(Color32::from_rgb(255, 0, 255)),
                            Tone::Danger,
                        ));
                        ui.add(
                            Ribbon::new(
                                egui::RichText::new("Warning heading").color(Color32::from_rgb(255, 0, 255)),
                            )
                            .tone(Tone::Warning),
                        );
                    });
                });
            });
            let p = if dark { &tokens::DARK } else { &tokens::LIGHT };
            for (label, expected) in [("Danger status", p.danger), ("Warning heading", p.warning)] {
                let shapes: Vec<_> = output
                    .shapes
                    .iter()
                    .filter(|s| matches!(&s.shape, Shape::Text(t) if t.galley.text()==label))
                    .cloned()
                    .collect();
                assert_eq!(shapes.len(), 1);
                let mesh_colors: Vec<_> = ctx
                    .tessellate(shapes, output.pixels_per_point)
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
                    "{dark:?} {label}"
                );
            }
        }
    }
}
