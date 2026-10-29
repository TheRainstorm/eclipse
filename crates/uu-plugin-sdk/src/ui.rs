//! Shared application colors and UI controls for independent extensions.
use eframe::egui::{self,Color32 as C,Pos2,Rect,Response,RichText,Sense,Stroke,Vec2};
pub const BLUE: C = crate::theme::ACCENT;
pub const INK: C = crate::theme::INK;
pub const MUTED: C = crate::theme::MUTED;
pub const SIDEBAR: C = crate::theme::SIDEBAR;

#[derive(Clone, Copy)]
pub enum Icon {
    Desktop,
    Globe,
    Extension,
    Picture,
    Exit,
    Grid,
    Settings,
    Refresh,
    Close,
    Min,
    Max,
    Play,
    Folder,
    Terminal,
    Link,
    Person,
    Star,
    Clock,
    Info,
    Volume,
    Muted,
    Keyboard,
    Encoding,
}
pub fn icon(p: &egui::Painter, rect: Rect, kind: Icon, color: C) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    let pt = |x: f32, y: f32| Pos2::new(r.left() + x, r.top() + y);
    let stroke = Stroke::new(1.5, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], stroke);
    };
    match kind {
        Icon::Keyboard => {
            p.rect_stroke(Rect::from_min_max(pt(1.0,4.0),pt(17.0,14.0)),2.0,stroke);
            for y in [7.0,10.0] {for x in [4.0,7.0,10.0,13.0] {p.circle_filled(pt(x,y),0.7,color);}}
            line((6.0,12.0),(12.0,12.0));
        }
        Icon::Encoding => {
            p.rect_stroke(Rect::from_min_max(pt(4.0,4.0),pt(14.0,14.0)),2.0,stroke);
            for v in [6.0,9.0,12.0] {line((v,1.0),(v,4.0));line((v,14.0),(v,17.0));line((1.0,v),(4.0,v));line((14.0,v),(17.0,v));}
        }
        Icon::Volume | Icon::Muted => {
            p.add(egui::Shape::closed_line(vec![pt(2.0, 7.0), pt(5.0, 7.0), pt(9.0, 3.0), pt(9.0, 15.0), pt(5.0, 11.0), pt(2.0, 11.0)], stroke));
            if matches!(kind, Icon::Muted) {
                line((12.0, 6.0), (17.0, 12.0));
                line((12.0, 12.0), (17.0, 6.0));
            } else {
                for radius in [4.0, 7.0] {
                    let points = (0..=8).map(|step| {
                        let angle = -0.8 + step as f32 * 0.2;
                        pt(9.0, 9.0) + egui::vec2(angle.cos(), angle.sin()) * radius
                    }).collect();
                    p.add(egui::Shape::line(points, stroke));
                }
            }
        }
        Icon::Globe => {
            p.circle_stroke(r.center(), 8.0, stroke);
            line((1.0, 9.0), (17.0, 9.0));
            for axis in [false, true] {
                let points = (0..=32).map(|i| {
                    let angle = i as f32 * std::f32::consts::TAU / 32.0;
                    if axis { pt(9.0 + 7.5 * angle.cos(), 9.0 + 3.4 * angle.sin()) }
                    else { pt(9.0 + 3.4 * angle.cos(), 9.0 + 7.5 * angle.sin()) }
                }).collect();
                p.add(egui::Shape::closed_line(points, stroke));
            }
        }
        Icon::Extension => {p.rect_stroke(r.shrink(2.),3.,stroke);line((5.,9.),(13.,9.));line((9.,5.),(9.,13.));},
        Icon::Info => {
            p.circle_stroke(r.center(), 7.0, stroke);
            p.circle_filled(pt(9.0, 5.0), 0.9, color);
            line((9.0, 8.0), (9.0, 13.0));
        }
        Icon::Exit => {
            line((8.0, 2.0), (2.0, 2.0));
            line((2.0, 2.0), (2.0, 16.0));
            line((2.0, 16.0), (8.0, 16.0));
            line((7.0, 9.0), (17.0, 9.0));
            line((13.0, 5.0), (17.0, 9.0));
            line((17.0, 9.0), (13.0, 13.0));
        }
        Icon::Picture => {
            p.rect_stroke(Rect::from_min_max(pt(1.0, 2.0), pt(17.0, 16.0)), 2.0, stroke);
            p.circle_stroke(pt(6.0, 6.0), 1.5, stroke);
            line((2.0, 14.0), (8.0, 9.0));
            line((8.0, 9.0), (11.0, 12.0));
            line((11.0, 12.0), (14.0, 9.0));
            line((14.0, 9.0), (17.0, 12.0));
        }
        Icon::Desktop => {
            p.rect_stroke(
                Rect::from_min_max(pt(1.0, 2.0), pt(17.0, 13.0)),
                2.0,
                stroke,
            );
            line((9.0, 13.0), (9.0, 16.0));
            line((5.0, 17.0), (13.0, 17.0));
        }
        Icon::Grid => {
            for x in [1.0, 10.0] {
                for y in [1.0, 10.0] {
                    p.rect_stroke(Rect::from_min_size(pt(x, y), Vec2::splat(6.0)), 1.0, stroke);
                }
            }
        }
        Icon::Settings => {
            let mut points = Vec::with_capacity(32);
            for tooth in 0..8 {
                for (offset, radius) in [(-0.35, 6.0), (-0.20, 8.0), (0.20, 8.0), (0.35, 6.0)] {
                    let angle = (tooth as f32 + offset) * std::f32::consts::TAU / 8.0;
                    points.push(r.center() + egui::vec2(angle.cos(), angle.sin()) * radius);
                }
            }
            p.add(egui::Shape::closed_line(points, stroke));
            p.circle_stroke(r.center(), 2.6, stroke);
        }
        Icon::Refresh => {
            p.circle_stroke(r.center(), 6.0, stroke);
            line((15.0, 2.0), (15.0, 7.0));
            line((15.0, 7.0), (10.0, 7.0));
        }
        Icon::Close => {
            line((3.0, 3.0), (15.0, 15.0));
            line((15.0, 3.0), (3.0, 15.0));
        }
        Icon::Min => line((3.0, 9.0), (15.0, 9.0)),
        Icon::Max => {
            p.rect_stroke(
                Rect::from_min_max(pt(3.0, 3.0), pt(15.0, 15.0)),
                0.0,
                stroke,
            );
        }
        Icon::Play => {
            p.rect_stroke(r.shrink(1.0), 3.0, stroke);
            p.add(egui::Shape::closed_line(
                vec![pt(7.0, 5.0), pt(13.0, 9.0), pt(7.0, 13.0)],
                stroke,
            ));
        }
        Icon::Folder => {
            p.rect_stroke(
                Rect::from_min_max(pt(1.0, 5.0), pt(17.0, 16.0)),
                2.0,
                stroke,
            );
            line((1.0, 5.0), (1.0, 2.0));
            line((1.0, 2.0), (7.0, 2.0));
            line((7.0, 2.0), (10.0, 5.0));
        }
        Icon::Terminal => {
            p.rect_stroke(r.shrink(1.0), 3.0, stroke);
            line((4.0, 6.0), (7.0, 9.0));
            line((7.0, 9.0), (4.0, 12.0));
            line((10.0, 12.0), (14.0, 12.0));
        }
        Icon::Link => {
            p.rect_stroke(Rect::from_min_max(pt(1.0, 3.0), pt(6.0, 15.0)), 2.0, stroke);
            p.rect_stroke(
                Rect::from_min_max(pt(12.0, 3.0), pt(17.0, 15.0)),
                2.0,
                stroke,
            );
            line((6.0, 9.0), (12.0, 9.0));
        }
        Icon::Person => {
            p.circle_stroke(pt(9.0, 5.0), 3.0, stroke);
            p.rect_stroke(
                Rect::from_min_max(pt(3.0, 11.0), pt(15.0, 17.0)),
                3.0,
                stroke,
            );
        }
        Icon::Clock => {
            p.circle_stroke(r.center(), 7.0, stroke);
            line((9.0, 4.0), (9.0, 9.0));
            line((9.0, 9.0), (13.0, 11.0));
        }
        Icon::Star => {
            let points = (0..10)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
                    let radius = if i % 2 == 0 { 8.0 } else { 3.5 };
                    r.center() + egui::vec2(a.cos(), a.sin()) * radius
                })
                .collect();
            p.add(egui::Shape::closed_line(points, stroke));
        }
    }
}
pub fn icon_button(ui: &mut egui::Ui, kind: Icon, tip: &str) -> Response {
    icon_button_selected(ui, kind, tip, false)
}
pub fn icon_button_selected(ui: &mut egui::Ui, kind: Icon, tip: &str, selected: bool) -> Response {
    icon_button_tinted(ui, kind, tip, selected, ui.visuals().text_color())
}

pub fn icon_button_tinted(
    ui: &mut egui::Ui,
    kind: Icon,
    tip: &str,
    selected: bool,
    tint: C,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if response.hovered() || selected {
        ui.painter().rect_filled(
            rect,
            6.0,
            if selected {
                C::from_rgb(222, 235, 249)
            } else {
                ui.visuals().widgets.hovered.bg_fill
            },
        );
    }
    icon(ui.painter(), rect, kind, if selected { BLUE } else { tint });
    response.on_hover_text(tip)
}

pub fn icon_label_button(ui: &mut egui::Ui, kind: Icon, label: &str) -> Response {
    let text = ui.painter().layout_no_wrap(label.into(), egui::FontId::proportional(14.0), ui.visuals().text_color());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0 + text.size().x, 30.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, ui.visuals().widgets.hovered.bg_fill);
    }
    icon(ui.painter(), Rect::from_center_size(rect.left_center() + egui::vec2(14.0, 0.0), Vec2::splat(18.0)), kind, ui.visuals().text_color());
    ui.painter().galley(egui::pos2(rect.left() + 30.0, rect.center().y - text.size().y / 2.0), text, ui.visuals().text_color());
    response
}
