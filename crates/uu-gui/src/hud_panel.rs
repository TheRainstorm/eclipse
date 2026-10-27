//! Presentation shared by the live HUD and the isolated visual probe.
use eframe::egui::{self, Color32, FontId, Rect, RichText};
use std::sync::Arc;
use uu_core::client_settings::HudDetail;

#[derive(Clone, Default)]
pub struct Metrics {
    pub size: [u32; 2],
    pub fps: [Option<f32>; 3],
    pub target_fps: u32,
    pub source_hz: Option<u32>,
    pub codec: String,
    pub mode: String,
    pub backend: String,
    pub bitrate: Option<f32>,
    pub rtt: Option<[f32; 2]>,
    pub rtt_distribution: Option<[f64; 5]>,
    pub loss: Option<f32>,
    pub unrecovered_loss: Option<f32>,
    pub network: Option<uu_core::network_loss::Snapshot>,
    // Capture, encode, decode, UI preparation/render submission.
    pub process: [Option<f32>; 4],
    // Producer send wait, decoder queue, mailbox-to-UI delivery.
    pub queues: [Option<f32>; 3],
    pub counters: Counters,
    pub input_ready: bool,
    pub input_counts: [u64; 2],
    // Client queue, confirmation RTT, host queue, injection.
    pub input_timing: [Option<f32>; 4],
    pub waiting_for_keyframe: bool,
    pub toggle_hint: String,
}
#[derive(Clone, Default)]
pub struct Counters {
    pub lost: u64,
    pub nack: u64,
    pub rtx: u64,
    pub reordered: u64,
    pub late: u64,
    pub damaged: u64,
    pub errors: u64,
    pub queue_drops: u64,
    pub keyframes: u64,
    pub fec_packets: u64,
    pub fec_recovered: u64,
    pub fec_bytes: u64,
    pub fec_drops: u64,
    pub recovery: [u64; 4],
}
#[derive(Debug)]
pub struct Estimate {
    pub known: f32,
    pub missing: Vec<&'static str>,
    pub samples: usize,
}
fn valid(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v >= 0.)
}
fn sum(values: &[Option<f32>]) -> Option<f32> {
    values.iter().copied().map(valid).sum()
}
fn number(value: Option<f32>) -> String {
    valid(value).map_or_else(
        || "—".into(),
        |v| {
            if v < 1. {
                format!("{v:.2}")
            } else {
                format!("{v:.1}")
            }
        },
    )
}
impl Metrics {
    pub fn estimate(&self) -> Estimate {
        let parts = [
            ("cap", self.process[0]),
            ("enc", self.process[1]),
            ("dec", self.process[2]),
            ("rnd", self.process[3]),
            ("net", self.rtt.map(|v| v[0] / 2.)),
            ("send queue", self.queues[0]),
            ("decode queue", self.queues[1]),
            ("mailbox", self.queues[2]),
        ];
        let mut result = Estimate {
            known: 0.,
            missing: Vec::new(),
            samples: 0,
        };
        for (name, value) in parts {
            if let Some(v) = valid(value) {
                result.known += v;
                result.samples += 1;
            } else {
                result.missing.push(name);
            }
        }
        result
    }
    pub fn total_label(&self) -> String {
        let e = self.estimate();
        if e.samples == 0 {
            "Est. Total —".into()
        } else if e.missing.is_empty() {
            format!("Est. Total ≈{} ms", number(Some(e.known)))
        } else {
            format!("Known ≈{} ms", number(Some(e.known)))
        }
    }
    pub fn fps_label(&self) -> String {
        let values = self
            .fps
            .map(|v| valid(v).map_or_else(|| "—".into(), |v| format!("{v:.1}")));
        format!(
            "FPS[recv/dec/rnd] {}/{}/{}",
            values[0], values[1], values[2]
        )
    }
    fn network_label(&self) -> String {
        let rtt = self.rtt.map_or_else(
            || "—".into(),
            |v| format!("{} ± {} ms", number(Some(v[0])), number(Some(v[1]))),
        );
        let loss = valid(self.loss).map_or_else(|| "—".into(), |v| format!("{v:.2}%"));
        format!("RTT {rtt} · Loss {loss}")
    }
    fn stages_label(&self) -> String {
        format!(
            "enc/dec/rnd {}/{}/{} ms",
            number(self.process[1]),
            number(self.process[2]),
            number(self.process[3])
        )
    }
}
struct Placed {
    offset: egui::Vec2,
    galley: Arc<egui::Galley>,
}
pub struct Layout {
    placed: Vec<Placed>,
    pub size: egui::Vec2,
    compact: bool,
}
impl Layout {
    pub fn new(ui: &egui::Ui, m: &Metrics, mode: HudDetail, width: f32) -> Self {
        let primary = Color32::from_rgb(239, 245, 252);
        let muted = Color32::from_rgb(181, 198, 218);
        let network = if m.loss.is_some_and(|v| v > 1.) {
            Color32::from_rgb(255, 195, 108)
        } else {
            Color32::from_rgb(154, 202, 255)
        };
        let process = Color32::from_rgb(159, 231, 204);
        let codec = if m.codec.is_empty() { "—" } else { &m.codec };
        let resolution = if m.size.contains(&0) {
            "—".into()
        } else {
            format!("{}x{}@{}", m.size[0], m.size[1], m.target_fps)
        };
        let video = format!("{resolution} · {}", m.fps_label());
        let rate = valid(m.bitrate).map_or_else(|| "— Mbps".into(), |v| format!("{v:.1} Mbps"));
        let mut rows: Vec<Vec<(String, f32, Color32)>> = vec![vec![
            (video, 15., primary),
            (format!("{codec} · {rate}"), 14., primary),
        ]];
        if mode == HudDetail::Compact {
            let color = Color32::from_rgb(0, 235, 214);
            rows[0] = vec![
                (resolution, 14., color),
                (codec.into(), 14., color),
                (m.fps_label(), 14., color),
                (m.network_label(), 14., color),
                (format!("Bandwidth {rate}"), 14., color),
                (m.stages_label(), 14., color),
                (format!("net/que ≈{}/{} ms",number(m.rtt.map(|v|v[0]/2.)),number(sum(&m.queues))),14.,color),
                (m.total_label(), 14., color),
            ];
        } else {
            let mode = if m.mode.is_empty() {
                String::new()
            } else {
                format!(" / {}", m.mode)
            };
            rows[0][1].0 = format!("{codec}{mode} · {rate}");
            rows[0].push((decoder_label(&m.backend).into(), 12.5, muted));
            rows.push(vec![
                (m.network_label(), 13., network),
                (
                    format!(
                        "{} = Proc {} + Net ≈{} + Queue {} ms",
                        m.total_label(),
                        number(sum(&m.process)),
                        number(m.rtt.map(|v| v[0] / 2.)),
                        number(sum(&m.queues))
                    ),
                    13.,
                    primary,
                ),
            ]);
            rows.push(vec![(
                format!(
                    "Process  cap/enc/dec/rnd {}/{}/{}/{} ms",
                    number(m.process[0]),
                    number(m.process[1]),
                    number(m.process[2]),
                    number(m.process[3])
                ),
                13.,
                process,
            )]);
            rows.push(vec![
                (
                    format!(
                        "Queue  send/dec/mailbox {}/{}/{} ms",
                        number(m.queues[0]),
                        number(m.queues[1]),
                        number(m.queues[2])
                    ),
                    12.5,
                    muted,
                ),
                (
                    format!(
                        "Target {} FPS{}",
                        m.target_fps,
                        m.source_hz
                            .map_or_else(String::new, |hz| format!(" · Source {hz} Hz"))
                    ),
                    12.5,
                    muted,
                ),
            ]);
            let c = &m.counters;
            if c.errors + c.damaged + c.queue_drops > 0 {
                rows.push(vec![(
                    format!(
                        "Session warnings  damaged {} · dec errors {} · queue drops {}",
                        c.damaged, c.errors, c.queue_drops
                    ),
                    12.5,
                    Color32::from_rgb(255, 195, 108),
                )]);
            }
        }
        if m.waiting_for_keyframe {
            rows.push(vec![(
                "Recovering reference frame…".into(),
                12.5,
                Color32::from_rgb(255, 195, 108),
            )]);
        }
        let width = width.max(1.);
        let mut placed = Vec::new();
        let mut x = 0.;
        let mut y = 0.;
        let mut row_h = 0f32;
        let mut max_x = 0f32;
        for (line, row) in rows.into_iter().enumerate() {
            if line > 0 {
                x = 0.;
                y += row_h + 6.;
                row_h = 0.;
            }
            for (text, size, color) in row {
                let mut job =
                    egui::text::LayoutJob::simple(text, FontId::proportional(size), color, width);
                job.wrap.break_anywhere = false;
                let galley = ui.fonts(|f| f.layout_job(job));
                let gap = if x > 0. { 16. } else { 0. };
                if x > 0. && x + gap + galley.size().x > width {
                    x = 0.;
                    y += row_h + 5.;
                    row_h = 0.;
                } else {
                    x += gap;
                }
                row_h = row_h.max(galley.size().y);
                max_x = max_x.max(x + galley.size().x);
                placed.push(Placed {
                    offset: egui::vec2(x, y),
                    galley: galley.clone(),
                });
                x += galley.size().x;
            }
        }
        Self {
            placed,
            size: egui::vec2(max_x, y + row_h),
            compact: mode == HudDetail::Compact,
        }
    }
    pub fn paint(&self, ui: &egui::Ui, video: Rect) -> egui::Response {
        let pad = if self.compact {
            egui::vec2(8., 5.)
        } else {
            egui::vec2(12., 9.)
        };
        let bounds = Rect::from_min_size(
            egui::pos2(
                video.center().x - self.size.x / 2. - pad.x,
                video.top() + 4.,
            ),
            self.size + pad * 2.,
        );
        ui.painter().rect_filled(
            bounds,
            if self.compact { 0. } else { 8. },
            Color32::from_black_alpha(205),
        );
        if !self.compact {
            ui.painter().rect_stroke(
                bounds,
                8.,
                egui::Stroke::new(0.7, Color32::from_white_alpha(35)),
            );
        }
        for p in &self.placed {
            ui.painter().galley(
                bounds.min + pad + p.offset,
                p.galley.clone(),
                Color32::WHITE,
            );
        }
        ui.interact(bounds, ui.id().with("hud-details"), egui::Sense::hover())
    }
}
// Ten categories, each rendered as a compact, wrapping data row.
struct DetailRow {
    label: &'static str,
    text: String,
    color: Color32,
}
fn detail_rows(m: &Metrics) -> Vec<DetailRow> {
    let plain = Color32::from_rgb(216, 224, 232);
    let cyan = Color32::from_rgb(0, 235, 214);
    let green = Color32::from_rgb(143, 225, 184);
    let blue = Color32::from_rgb(139, 194, 255);
    let orange = Color32::from_rgb(255, 190, 108);
    let pct = |v: Option<f32>| valid(v).map_or_else(|| "—".into(), |v| format!("{v:.2}%"));
    let c = &m.counters;
    let e = m.estimate();
    let total = if e.samples == 0 {
        "预估 —".into()
    } else {
        format!(
            "{} ≈{} ms",
            if e.missing.is_empty() {
                "预估"
            } else {
                "已知小计"
            },
            number(Some(e.known))
        )
    };
    let fps = m
        .fps
        .map(|v| valid(v).map_or_else(|| "—".into(), |v| format!("{v:.1}")));
    let rtt = m.rtt.map_or_else(
        || "RTT —".into(),
        |v| format!("RTT {}±{} ms", number(Some(v[0])), number(Some(v[1]))),
    );
    let distribution = m.rtt_distribution.map_or_else(String::new, |v| {
        format!(
            " · 末/低/中/P95/高 {:.1}/{:.1}/{:.1}/{:.1}/{:.1}",
            v[0], v[1], v[2], v[3], v[4]
        )
    });
    let missing = if e.missing.is_empty() {
        String::new()
    } else {
        format!(
            " · 缺 {}",
            e.missing
                .iter()
                .map(|v| match *v {
                    "cap" => "采集",
                    "enc" => "编码",
                    "dec" => "解码",
                    "rnd" => "渲染",
                    "net" => "网络",
                    "send queue" => "发送排队",
                    "decode queue" => "解码排队",
                    _ => "邮箱",
                })
                .collect::<Vec<_>>()
                .join("/")
        )
    };
    let received = m.network.as_ref().map_or_else(
        || "原始包统计 —".into(),
        |n| {
            format!(
                "窗口缺/应 {}/{} · 累计收/缺 {}/{} · 乱/重/迟/重置 {}/{}/{}/{}",
                n.window_lost,
                n.window_expected,
                n.original_received,
                n.original_lost,
                n.reordered,
                n.duplicates,
                n.late,
                n.resyncs
            )
        },
    );
    vec![
        DetailRow {
            label: "画面",
            text: format!(
                "{}×{}@{} · {} · {} · {} Mbps",
                m.size[0],
                m.size[1],
                m.target_fps,
                if m.codec.is_empty() { "—" } else { &m.codec },
                decoder_label(&m.backend),
                number(m.bitrate)
            ),
            color: cyan,
        },
        DetailRow {
            label: "帧率",
            text: format!(
                "接收 {} / 解码 {} / 渲染 {} FPS{}",
                fps[0],
                fps[1],
                fps[2],
                m.source_hz
                    .map_or_else(String::new, |v| format!(" · 源 {v} Hz"))
            ),
            color: cyan,
        },
        DetailRow {
            label: "延迟",
            text: format!(
                "{total} · 采/编/解/渲/网/排 {}/{}/{}/{}/{}/{} ms{missing}",
                number(m.process[0]),
                number(m.process[1]),
                number(m.process[2]),
                number(m.process[3]),
                number(m.rtt.map(|v|v[0]/2.)),
                number(sum(&m.queues))
            ),
            color: green,
        },
        DetailRow {
            label: "网络",
            text: format!(
                "{rtt} · 原丢 {} / 残丢 {}{distribution}",
                pct(m.loss),
                pct(m.unrecovered_loss)
            ),
            color: if m.loss.is_some_and(|v| v > 1.) {
                orange
            } else {
                blue
            },
        },
        DetailRow {
            label: "排队",
            text: format!(
                "发/解/邮箱 {}/{}/{} ms · 合计 {} ms",
                number(m.queues[0]),
                number(m.queues[1]),
                number(m.queues[2]),
                number(sum(&m.queues))
            ),
            color: green,
        },
        DetailRow {
            label: "恢复累计",
            text: format!(
                "FEC 恢复/冗余 {}/{} · {:.2} MiB / 丢弃 {} · RTX {} · NACK {}",
                c.fec_recovered,
                c.fec_packets,
                c.fec_bytes as f64 / 1048576.,
                c.fec_drops,
                c.rtx,
                c.nack
            ),
            color: plain,
        },
        DetailRow {
            label: "异常累计",
            text: format!(
                "残包 {} · 损帧 {} · 解错 {} · 丢帧 {} · 刷新 {} · 修复乱/迟 {}/{}",
                c.lost, c.damaged, c.errors, c.queue_drops, c.keyframes, c.reordered, c.late
            ),
            color: if c.errors + c.damaged + c.queue_drops > 0 {
                orange
            } else {
                plain
            },
        },
        DetailRow {
            label: "输入",
            text: format!(
                "{} · 发/错 {}/{} · 排队/确认/主机队列/注入 {}/{}/{}/{} ms",
                if m.input_ready {
                    "已就绪"
                } else {
                    "未就绪"
                },
                m.input_counts[0],
                m.input_counts[1],
                number(m.input_timing[0]),
                number(m.input_timing[1]),
                number(m.input_timing[2]),
                number(m.input_timing[3])
            ),
            color: plain,
        },
        DetailRow {
            label: "收包",
            text: received,
            color: plain,
        },
        DetailRow {
            label: "恢复原因",
            text: format!(
                "接收/排队/解码/格式 {}/{}/{}/{} 次",
                c.recovery[0], c.recovery[1], c.recovery[2], c.recovery[3]
            ),
            color: plain,
        },
    ]
}
/// Height and width include the complete frame, in viewport logical points.
pub fn detail_limits(view: Rect) -> egui::Vec2 {
    egui::vec2(
        (view.width() - 12.).clamp(1., 680.),
        (view.height() - 12.).clamp(1., 280.),
    )
}
fn detail_contents(ui: &mut egui::Ui, m: &Metrics, width: f32, max_height: f32) {
    ui.set_width(width.max(1.));
    // An auto-sized Area remembers its previous size. Give the scroll area
    // the current viewport budget, so it can grow to include new rows.
    ui.set_max_height(max_height.max(1.));
    ui.spacing_mut().item_spacing = egui::vec2(7., 3.);
    *ui.visuals_mut() = egui::Visuals::dark();
    egui::ScrollArea::vertical()
        .id_salt("hud-detail-scroll")
        .max_height(max_height.max(1.))
        .min_scrolled_height(0.)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for row in detail_rows(m) {
                ui.horizontal_top(|ui| {
                    ui.add_sized(
                        [50., 16.],
                        egui::Label::new(
                            RichText::new(row.label).size(12.).color(row.color).strong(),
                        ),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(row.text).size(12.).color(row.color)).wrap(),
                    );
                });
            }
        });
}
/// Shared by the production hover card and the generated visual probe.
pub fn details(ui: &mut egui::Ui, m: &Metrics) {
    let limits = detail_limits(ui.ctx().screen_rect());
    detail_contents(ui, m, limits.x - 16., limits.y - 16.);
}
fn detail_origin(view: Rect, anchor: Rect, size: egui::Vec2) -> egui::Pos2 {
    let safe = view.shrink(6.);
    egui::pos2(
        anchor
            .left()
            .clamp(safe.left(), (safe.right() - size.x).max(safe.left())),
        (anchor.bottom() + 4.).clamp(safe.top(), (safe.bottom() - size.y).max(safe.top())),
    )
}
pub fn details_at(
    ui: &egui::Ui,
    m: &Metrics,
    view: Rect,
    anchor: Rect,
    previous: Option<Rect>,
) -> Rect {
    let limits = detail_limits(view);
    let size = egui::vec2(
        limits.x,
        previous.map_or(260., |r| r.height()).min(limits.y),
    );
    let pos = detail_origin(view, anchor, size);
    egui::Area::new(ui.id().with("hud-hover-card"))
        .order(egui::Order::Tooltip)
        .movable(false)
        .fixed_pos(pos)
        .constrain_to(view.shrink(6.))
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(Color32::from_black_alpha(228))
                .rounding(3.)
                .inner_margin(8.)
                .show(ui, |ui| {
                    detail_contents(ui, m, limits.x - 16., limits.y - 16.)
                });
        })
        .response
        .rect
}
#[derive(Default)]
pub struct HoverCard {
    since: Option<f64>,
    last_inside: f64,
    pub bounds: Option<Rect>,
}
impl HoverCard {
    /// Hover does not transfer keyboard control. Only pointer events over the
    /// scrollable card are local; a short bridge permits moving into the card.
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        source: &egui::Response,
        m: &Metrics,
        view: Rect,
    ) -> bool {
        let (now, pointer, focused) = ui
            .ctx()
            .input(|i| (i.time, i.pointer.hover_pos(), i.focused));
        if !focused || view.width() < 48. || view.height() < 48. {
            *self = Self::default();
            return false;
        }
        let over_card = pointer.is_some_and(|p| self.bounds.is_some_and(|r| r.contains(p)));
        if source.hovered() || over_card {
            self.last_inside = now;
            self.since.get_or_insert(now);
        } else if now - self.last_inside > 0.18 {
            *self = Self::default();
            return false;
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(180));
        }
        let Some(since) = self.since else {
            return false;
        };
        if self.bounds.is_none() && now - since < 0.25 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(
                    (0.25 - (now - since)).max(0.),
                ));
            return false;
        }
        let bounds = details_at(ui, m, view, source.rect, self.bounds);
        self.bounds = Some(bounds);
        pointer.is_some_and(|p| bounds.contains(p))
    }
}

pub fn decoder_label(backend: &str) -> &str {
    if backend.starts_with("vaapi") {
        "VAAPI HW"
    } else if backend.starts_with("d3d11va") {
        "D3D11 HW"
    } else if backend.starts_with("openh264") {
        "OpenH264 SW"
    } else if backend.starts_with("pyrowave") {
        if backend.contains("cpu-readback") {
            "PyroWave / CPU readback"
        } else {
            "PyroWave GPU"
        }
    } else if backend.is_empty() {
        "Decoder initializing"
    } else {
        backend.split(['(', '[']).next().unwrap_or(backend).trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_detail_card_fits_windowed_viewports_and_has_scrollable_content() {
        for size in [
            egui::vec2(1280., 720.),
            egui::vec2(640., 360.),
            egui::vec2(320., 180.),
        ] {
            let ctx = egui::Context::default();
            let view = Rect::from_min_size(egui::pos2(0., 0.), size);
            let m = Metrics {
                size: [3840, 2160],
                fps: [Some(120.); 3],
                codec: "AV1 10-bit HDR".into(),
                backend: "d3d11va".into(),
                ..Default::default()
            };
            let mut bounds = None;
            let mut final_category_visible=false;
            for _ in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(view),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::none())
                            .show(ctx, |ui| {
                                bounds = Some(details_at(
                                    ui,
                                    &m,
                                    view,
                                    Rect::from_min_size(egui::pos2(6., 6.), egui::vec2(120., 26.)),
                                    bounds,
                                ));
                            });
                    },
                );
                final_category_visible=output.shapes.iter().any(|shape|match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text=="恢复原因"=>shape.clip_rect.contains_rect(Rect::from_min_size(text.pos,text.galley.size())),
                    _=>false
                });
            }
            if size.y>=300. {assert!(final_category_visible,"the final category must fit without hidden clipping at {size:?}");}
            let bounds = bounds.unwrap();
            assert!(
                view.contains_rect(bounds),
                "{size:?}: card {bounds:?} escaped viewport {view:?}"
            );
            assert!(bounds.height() <= detail_limits(view).y + 1.);
        }
    }
    #[test]
    fn abandoned_hardware_is_not_reported_as_the_active_decoder() {
        assert_eq!(
            decoder_label("openh264 [hardware vaapi abandoned: error]"),
            "OpenH264 SW"
        );
        assert_eq!(
            decoder_label("d3d11va (Media Foundation hw: GPU)"),
            "D3D11 HW"
        );
        assert_eq!(
            decoder_label("pyrowave-vulkan-cpu-readback"),
            "PyroWave / CPU readback"
        );
    }
    #[test]
    fn estimated_total_distinguishes_missing_from_measured_zero() {
        let m = Metrics {
            process: [Some(1.), Some(1.6), Some(0.8), Some(5.4)],
            queues: [Some(0.7), Some(0.3), Some(3.)],
            rtt: Some([4., 0.6]),
            ..Default::default()
        };
        assert!((m.estimate().known - 14.8).abs() < 0.001);
        assert!(m.estimate().missing.is_empty());
        assert_eq!(m.total_label(), "Est. Total ≈14.8 ms");
        let partial = Metrics {
            process: [None, Some(0.), None, None],
            ..Default::default()
        };
        assert_eq!(partial.estimate().samples, 1);
        assert!(!partial.estimate().missing.is_empty());
        assert!(partial.total_label().starts_with("Known"));
        assert_eq!(Metrics::default().total_label(), "Est. Total —");
    }
    #[test]
    fn fps_and_timing_triplets_have_consistent_order() {
        let m = Metrics {
            fps: [Some(60.), Some(59.9), Some(59.7)],
            process: [Some(8.), Some(1.2), Some(0.07), Some(2.3)],
            ..Default::default()
        };
        assert_eq!(m.fps_label(), "FPS[recv/dec/rnd] 60.0/59.9/59.7");
        assert_eq!(m.stages_label(), "enc/dec/rnd 1.2/0.07/2.3 ms");
    }
    #[test]
    fn layouts_fit_wide_and_narrow_viewports_without_overlapping_groups() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for mode in HudDetail::ALL {
                    for width in [240., 640., 1600.] {
                        let m = Metrics {
                            size: [3840, 2160],
                            fps: [Some(120.); 3],
                            codec: "PYROWAVE".into(),
                            ..Default::default()
                        };
                        let layout = Layout::new(ui, &m, mode, width);
                        assert!(layout.size.x <= width + 0.1);
                        for (i, p) in layout.placed.iter().enumerate() {
                            for q in &layout.placed[i + 1..] {
                                assert!(!Rect::from_min_size(p.offset.to_pos2(), p.galley.size())
                                    .intersects(Rect::from_min_size(
                                        q.offset.to_pos2(),
                                        q.galley.size()
                                    )));
                            }
                        }
                    }
                }
            });
        });
    }
}
