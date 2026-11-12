//! A ball and its panel have separate native lifetimes and share no stream resources.
use eframe::egui::{self, Pos2, Rect, Vec2};
use uu_plugin_sdk::ui::{icon_button, Icon};

const BALL: f32 = 64.;
const PANEL: Vec2 = egui::vec2(420., 480.);

#[derive(Clone, Copy, Default)]
pub(crate) struct Summary {
    pub connections: usize,
    pub paused: bool,
}

#[derive(Default)]
pub(crate) struct Floating {
    open: bool,
    expanded: bool,
    focus: bool,
    placement: Option<Rect>,
}
impl Floating {
    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        summary: Summary,
        mut contents: impl FnMut(&mut egui::Ui),
    ) {
        if !self.open {
            return;
        }
        if self.placement.is_none() {
            let work = crate::window::native_geometry(ctx)
                .map(|g| g.work)
                .or_else(|| {
                    ctx.input(|i| {
                        i.viewport()
                            .monitor_size
                            .map(|size| Rect::from_min_size(Pos2::ZERO, size))
                    })
                });
            if let Some(work) = work {
                self.placement = Some(Rect::from_min_size(
                    egui::pos2(work.right() - BALL - 20., work.top() + work.height() * 0.3),
                    Vec2::splat(BALL),
                ));
            }
        }
        let id = egui::ViewportId::from_hash_of(if self.expanded {
            "host-session-panel"
        } else {
            "host-session-ball"
        });
        let size = if self.expanded {
            PANEL
        } else {
            Vec2::splat(BALL)
        };
        let mut builder = egui::ViewportBuilder::default()
            .with_title(uu_i18n::text("悬浮会话中心 · Eclipse"))
            .with_inner_size(size)
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_resizable(self.expanded)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_active(self.expanded);
        if self.expanded {
            builder = builder.with_min_inner_size([320., 240.]);
        } else {
            builder = builder.with_min_inner_size(size).with_max_inner_size(size);
        }
        if let Some(rect) = self.placement {
            builder = builder.with_position(rect.min).with_inner_size(rect.size());
        }
        if std::mem::take(&mut self.focus) {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
            if self.expanded {
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            }
        }
        ctx.show_viewport_immediate(id, builder, |ctx, _| {
            if ctx.input(|input| input.viewport().close_requested()) {
                self.close();
                return;
            }
            let geometry = crate::window::native_geometry(ctx);
            let (outer, work) = geometry
                .map(|g| (Some(g.outer), Some(g.work)))
                .unwrap_or_else(|| {
                    ctx.input(|i| {
                        (
                            i.viewport().outer_rect,
                            i.viewport()
                                .monitor_size
                                .map(|size| Rect::from_min_size(Pos2::ZERO, size)),
                        )
                    })
                });
            let before = self.expanded;
            if self.expanded {
                crate::window::resize_border(ctx, false);
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::none()
                            .fill(crate::theme::palette(&ctx.style().visuals).base)
                            .stroke(egui::Stroke::new(
                                1.,
                                crate::theme::palette(&ctx.style().visuals).border,
                            ))
                            .rounding(12.)
                            .inner_margin(12.),
                    )
                    .show(ctx, |ui| {
                        self.header(ui, summary.connections);
                        ui.add_space(8.);
                        egui::ScrollArea::vertical().show(ui, |ui| contents(ui));
                    });
            } else {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        let response = ball(ui, summary);
                        if response.drag_started() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                        if response.clicked() {
                            self.expanded = true;
                        }
                    });
            }
            if self.open && before != self.expanded {
                let outer = outer.unwrap_or_else(|| Rect::from_min_size(Pos2::new(40., 80.), size));
                let desired = placement(outer, work, self.expanded);
                self.placement = Some(desired);
                // The parent declares the other viewport on its next pass.
                // A fixed-size ball never leaves stale size constraints on the panel.
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
            if !self.open {
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(750));
    }
    fn header(&mut self, ui: &mut egui::Ui, connections: usize) {
        ui.horizontal(|ui| {
            let title = ui.add_sized(
                egui::vec2((ui.available_width() - 76.).max(1.), 30.),
                egui::Label::new(
                    egui::RichText::new(uu_i18n::format!("会话中心 · {} 个连接", connections))
                        .strong(),
                )
                .truncate()
                .sense(egui::Sense::drag()),
            );
            if title.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Close, uu_i18n::text("关闭悬浮球")).clicked() {
                    self.close();
                }
                if icon_button(ui, Icon::Min, uu_i18n::text("收起为悬浮球")).clicked() {
                    self.expanded = false;
                }
            });
        });
    }
    fn close(&mut self) {
        self.open = false;
        self.expanded = false;
        self.placement = None;
    }
}

pub(crate) fn ball(ui: &mut egui::Ui, summary: Summary) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BALL), egui::Sense::click_and_drag());
    let p = crate::theme::palette(ui.visuals());
    let center = rect.center();
    ui.painter().circle_filled(
        center,
        25.,
        if response.hovered() { p.hover } else { p.card },
    );
    ui.painter().circle_stroke(
        center,
        25.,
        egui::Stroke::new(1.5, if summary.paused { p.warning } else { p.accent }),
    );
    // Eclipse's simple overlapping circles stay sharp at every desktop DPI.
    ui.painter()
        .circle_filled(center - egui::vec2(3., 0.), 12., p.accent);
    ui.painter()
        .circle_filled(center + egui::vec2(5., -3.), 10., p.card);
    if summary.connections > 0 {
        let badge = center + egui::vec2(18., 18.);
        ui.painter().circle_filled(badge, 10., p.primary);
        ui.painter().text(
            badge,
            egui::Align2::CENTER_CENTER,
            summary.connections.to_string(),
            egui::FontId::proportional(11.),
            p.primary_text,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            uu_i18n::text("展开会话中心"),
        )
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn placement(outer: Rect, work: Option<Rect>, expanded: bool) -> Rect {
    let size = if expanded { PANEL } else { Vec2::splat(BALL) };
    let min = Pos2::new(outer.right() - size.x, outer.top());
    if let Some(work) = work {
        let size = size.min(work.size());
        let min = Pos2::new(
            min.x.clamp(work.left(), work.right() - size.x),
            min.y.clamp(work.top(), work.bottom() - size.y),
        );
        Rect::from_min_size(min, size)
    } else {
        Rect::from_min_size(min, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_and_collapse_stay_on_the_same_monitor_including_negative_origins() {
        for work in [
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1920., 1040.)),
            Rect::from_min_size(egui::pos2(-1600., -300.), egui::vec2(1600., 900.)),
        ] {
            for anchor in [work.min, work.max - Vec2::splat(BALL)] {
                let ball = Rect::from_min_size(anchor, Vec2::splat(BALL));
                let panel = placement(ball, Some(work), true);
                assert!(work.contains_rect(panel));
                let collapsed = placement(panel, Some(work), false);
                assert!(work.contains_rect(collapsed));
                assert_eq!(placement(collapsed, Some(work), true), panel);
            }
        }
    }
}
