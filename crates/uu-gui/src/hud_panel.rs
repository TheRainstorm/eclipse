//! Presentation shared by the live HUD and the isolated visual probe.
use eframe::egui::{self, Color32, FontId, Rect, RichText};
use std::sync::Arc;
use uu_core::client_settings::HudDetail;
#[path="hud_history.rs"]
mod history;
pub use history::{History,Sample as HistorySample};

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
    pub history: Arc<[HistorySample]>,
    pub continuity: Option<uu_core::frame_continuity::Snapshot>,
    pub host_video:Option<uu_core::frame_trace::VideoLatency>,
    pub video_e2e:Option<uu_core::video_e2e::Snapshot>,
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
    /// Diagnostic sum of independent stage windows; never the primary latency.
    pub fn stage_sum_label(&self) -> String {
        let e = self.estimate();
        if e.samples == 0 {
            uu_i18n::text("阶段小计 —").into()
        } else if e.missing.is_empty() {
            uu_i18n::format!("阶段小计 ≈{} ms", number(Some(e.known)))
        } else {
            uu_i18n::format!("已知阶段小计 ≈{} ms", number(Some(e.known)))
        }
    }
    fn video_measurement(&self) -> Option<uu_core::video_e2e::Measurement> {
        self.video_e2e.and_then(|snapshot|snapshot.measurement).filter(|value|value.samples>0)
    }
    pub fn video_mean_ms(&self) -> Option<f32> {
        self.video_measurement().map(|value|value.mean_us as f32/1000.)
    }
    pub fn video_uncertainty_ms(&self) -> Option<f32> {
        self.video_measurement().map(|value|value.uncertainty_us as f32/1000.)
    }
    fn video_value(&self) -> String {
        self.video_measurement().map_or_else(||"—".into(),|value|
            format!("≈{:.3} ±{:.3} ms",value.mean_us as f64/1000.,value.uncertainty_us as f64/1000.))
    }
    fn video_status(&self) -> &'static str {
        use uu_core::video_e2e::UnknownReason;
        uu_i18n::text(match self.video_e2e.and_then(|snapshot|snapshot.unknown) {
            Some(UnknownReason::UnsupportedHost)=>"不支持",
            Some(UnknownReason::SourceTimestampUnavailable)=>"源时间未知",
            Some(UnknownReason::ClockUnsynchronized)=>"正在校时",
            Some(UnknownReason::DisplayTimestampUnavailable)=>"显示时间未知",
            Some(UnknownReason::NoCurrentSample)=>"等待当前样本",
            None=>"未知",
        })
    }
    fn video_method(&self) -> &'static str {
        use uu_core::video_e2e::DisplayMethod;
        uu_i18n::text(match self.video_measurement().map(|value|value.method) {
            Some(DisplayMethod::PlatformDisplayed)=>"系统显示回执",
            Some(DisplayMethod::RefreshInterval)=>"刷新区间",
            Some(DisplayMethod::DxgiReceiptInterval)=>"DXGI回执估测",
            None=>"未知",
        })
    }
    pub fn total_label(&self) -> String {
        let status=if self.video_measurement().is_some(){self.video_method()}else{self.video_status()};
        uu_i18n::format!("视频端到端 {} · {}",self.video_value(),status)
    }
    fn input_response_label(&self) -> String {
        uu_i18n::format!("输入响应 — · 注入确认 {} ms",number(self.input_timing[1]))
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
            "Enc/Dec/Rnd {}/{}/{} ms",
            number(self.process[1]),
            number(self.process[2]),
            number(self.process[3])
        )
    }
    fn continuity_label(&self)->String {
        self.continuity.as_ref().map_or_else(||"Stalls(60s) —".into(),|s|format!("Stalls(60s) {} · Max gap {:.1} ms",s.long_gaps,s.max_gap_ms))
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
                (format!("Bandwidth {rate}"), 14., color),
                (m.network_label(), 14., color),
                (m.total_label(), 14., color),
                (m.input_response_label(), 14., color),
                (m.continuity_label(), 14., color),
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
                (m.total_label(), 13., primary),
            ]);
            rows.push(vec![(m.input_response_label(),12.5,muted)]);
            rows.push(vec![(m.continuity_label(),12.5,muted)]);
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
// Each category is a compact, wrapping data row with an aligned title.
struct DetailValue { label: &'static str, value: String, help: &'static str, group: u8 }
struct DetailRow { label: &'static str, color: Color32, values: Vec<DetailValue> }
fn field(label:&'static str,value:impl ToString,help:&'static str)->DetailValue {DetailValue{label,value:value.to_string(),help,group:0}}
fn row(label:&'static str,color:Color32,values:Vec<DetailValue>)->DetailRow {
    DetailRow{label,color,values}
}
fn detail_rows(m:&Metrics)->Vec<DetailRow> {
    let plain=Color32::from_rgb(216,224,232);let cyan=Color32::from_rgb(0,235,214);
    let green=Color32::from_rgb(143,225,184);let blue=Color32::from_rgb(139,194,255);let orange=Color32::from_rgb(255,190,108);
    let pct=|v:Option<f32>|valid(v).map_or_else(||"—".into(),|v|format!("{v:.2}%"));let c=&m.counters;
    let measured=m.video_measurement();
    let empirical=measured.is_some_and(|value|value.method==uu_core::video_e2e::DisplayMethod::DxgiReceiptInterval);
    let video_help=if empirical {uu_i18n::text("按同帧身份将源时间关联至DXGI已报告的PresentCount，显示区间取该图像最早提交前至首次回执查询后。多显示器或其它全屏应用可能使DXGI统计不可靠；这是经验估测，不是平台保证的硬界，也不是鼠标到像素延迟。")}
        else {uu_i18n::text("按同一源画面身份关联主机源呈现和客户端系统显示回执。四时间戳只约束时钟偏移区间，不假定上下行对称；数值是带误差的估测，不是物理像素响应或鼠标到屏幕延迟。缺少任一端点或校时数据时保持未知，绝不回退阶段和。")};
    let mean_help=uu_i18n::text("最近3秒同一批延迟区间的上下界分别取均值，±为该均值的向外舍入界，包含校时和显示区间误差；不要求样本独立，也不把共同校时误差按样本数消除。DXGI方法仍受统计可信假设限制，中点不是精确时间。");
    let percentile_help=uu_i18n::text("同一批单帧延迟区间中点的P50/P95估测；不是未知实际端点的精确分位数。主均值的±不适用于这两个分位数，最大单帧区间误差另列。DXGI方法仍受统计可信假设限制。");
    let mut video=vec![
        field(uu_i18n::text("均值估测"),m.video_value(),mean_help),
        field("P50/P95",measured.map_or_else(||"—".into(),|v|format!("≈{:.3}/{:.3} ms",v.p50_us as f64/1000.,v.p95_us as f64/1000.)),percentile_help),
        field(uu_i18n::text("方法"),if measured.is_some(){m.video_method()}else{m.video_status()},video_help),
        field(uu_i18n::text("样本"),measured.map_or_else(||"—".into(),|v|v.samples.to_string()),video_help),
    ];
    if let Some(value)=measured {
        video.push(field(uu_i18n::text("单帧最大半宽"),format!("±{:.3} ms",value.max_sample_uncertainty_us as f64/1000.),uu_i18n::text("当前样本中最大的单帧区间半宽，包含时钟、显示和微秒舍入；与主项的整批均值误差分别计算。DXGI方法的范围仅在统计可信假设下成立。")));
    }
    if let Some(snapshot)=m.video_e2e {
        video.push(field(uu_i18n::text("漂移假设"),format!("{} ppm",snapshot.relative_drift_ppm),uu_i18n::text("两端时钟换算后的相对漂移工程上限，用于扩大时间区间；不是平台保证的精度。样本过期后停止估测。")));
        video.push(field(uu_i18n::text("源/显/观测丢弃"),format!("{}/{}/{}",snapshot.source_frames,snapshot.displayed_frames,snapshot.observations_dropped),uu_i18n::text("源帧、系统显示回执及真实丢弃的观测累计数；丢弃包含钩子或队列丢失及非法元数据，不含历史容量淘汰，也不是媒体丢帧数。")));
        video.push(field(uu_i18n::text("历史淘汰"),snapshot.history_evicted,uu_i18n::text("PTS关联、接收记录、源首显证明及延迟样本达到容量上限后移除的历史条目累计数。同一帧可能占多个表；这是保留历史的维护，不表示丢观测或媒体丢帧。")));
    }
    let mut latency=vec![field("",m.stage_sum_label(),uu_i18n::text("已知视频阶段窗口均值之和，并非同一帧的完整测量。包含采集、编码、解码、UI提交、RTT/2及已测队列；不含输入设备与去程、应用响应、等待下次采集、未测装帧等待、GPU完成、呈现队列、合成、扫描及像素响应。RTT/2仅是对称假设，不能当作输入到显示延迟。"))];
    for (i,(label,help)) in [(uu_i18n::text("采"),uu_i18n::text("主机获取本帧桌面图像的时间。")),(uu_i18n::text("编"),uu_i18n::text("主机编码调用耗时，可包含硬件队列等待。")),(uu_i18n::text("解"),uu_i18n::text("客户端解码调用耗时。")),(uu_i18n::text("渲"),uu_i18n::text("客户端UI准备与渲染提交的CPU墙钟时间，可能包含驱动等待；不是GPU计时或扫描延迟。"))].into_iter().enumerate() {let mut v=field(label,format!("{} ms",number(m.process[i])),help);v.group=1;latency.push(v);}
    for (label,value,help) in [(uu_i18n::text("网"),m.rtt.map(|v|v[0]/2.),uu_i18n::text("RTT的一半，仅在往返对称时近似单程。RTT还可能包含应用调度、处理与队列等待。")),(uu_i18n::text("排"),sum(&m.queues),uu_i18n::text("主机发送等待、客户端解码队列和画面邮箱等待的合计。未知阶段不按零计算。"))]{let mut v=field(label,format!("{} ms",number(value)),help);v.group=2;latency.push(v);}
    for (label,span,help) in [
        (uu_i18n::text("源至发完"),m.host_video.as_ref().and_then(|v|v.source_to_last_rtp),uu_i18n::text("主机同一QPC时钟内，源画面呈现到最后一个RTP包写入API返回的均值/P95。仅新源画面；包含采集前画面年龄、编码、排队和RTP发送，与视频阶段和重叠，不能再相加。不是网卡发完或客户端显示完成；超过5秒未更新则未知。")),
        (uu_i18n::text("RTP跨度"),m.host_video.as_ref().and_then(|v|v.rtp_write),uu_i18n::text("同一视频帧首个到最后一个RTP写入API返回的均值/P95；用于观察分包与传输调用耗时，区别于编码线程等待发送队列。它与源至发完重叠，不是线速或单程网络时间。"))] {
        let value=span.map_or_else(||"—".into(),|v|format!("{:.1}/{:.1} ms",v.mean_us as f64/1000.,v.p95_us as f64/1000.));
        let mut value=field(label,value,help);value.group=3;latency.push(value);
    }
    let mut network=vec![field("RTT",m.rtt.map_or_else(||"—".into(),|v|format!("{}±{} ms",number(Some(v[0])),number(Some(v[1])))),uu_i18n::text("控制消息往返均值±标准差，最近32个样本。包含传输、排队和两端处理，不等于ICMP ping。")),field(uu_i18n::text("链路丢包"),pct(m.loss),uu_i18n::text("修复前的原始RTP缺包率。乱序宽限期后仍未到达的包计入，即使随后由FEC或RTX恢复也保留。它反映接收端看到的网络交付，不只代表物理线路。")),field(uu_i18n::text("修复后丢包"),pct(m.unrecovered_loss),uu_i18n::text("接收器超时后仍未恢复的RTP丢包率；FEC/重传成功会降低这个值。"))];
    if let Some(v)=m.rtt_distribution {network.push(field(uu_i18n::text("末/低/中/P95/高"),format!("{:.1}/{:.1}/{:.1}/{:.1}/{:.1} ms",v[0],v[1],v[2],v[3],v[4]),uu_i18n::text("最近一分钟内RTT的末值、最小值、中位数、95分位和最大值，用于观察波动。")));}
    let mut receive=vec![];
    if let Some(n)=&m.network {
        receive.push(field(uu_i18n::text("窗口缺/应"),format!("{}/{}",n.window_lost,n.window_expected),uu_i18n::text("当前原始包窗口中的缺失数与期望数，二者用于计算链路丢包率。")));
        receive.push(field(uu_i18n::text("累计收/缺"),format!("{}/{}",n.original_received,n.original_lost),uu_i18n::text("会话内收到的原始RTP包与在宽限期后认定缺失的原始包。")));
        receive.push(field(uu_i18n::text("乱/重/迟/重置"),format!("{}/{}/{}/{}",n.reordered,n.duplicates,n.late,n.resyncs),uu_i18n::text("原始包乱序、重复、超宽限期迟到和无法解释序号跳变后的统计重置次数。")));
    }else {receive.push(field(uu_i18n::text("原始包统计"),"—",uu_i18n::text("当前还没有新鲜、可解释的原始包窗口；不会伪造零丢包。")));}
    let mut fps=vec![field(uu_i18n::text("接收"),format!("{} FPS",number(m.fps[0])),uu_i18n::text("进入接收管线的完整视频帧率。")),field(uu_i18n::text("解码"),format!("{} FPS",number(m.fps[1])),uu_i18n::text("成功解码帧率。")),field(uu_i18n::text("渲染"),format!("{} FPS",number(m.fps[2])),uu_i18n::text("当前视图提交的新画面帧率；不是显示器扫描输出帧率。"))];
    fps.push(field(uu_i18n::text("源"),m.source_hz.map_or_else(||"—".into(),|v|format!("{v} Hz")),uu_i18n::text("主机报告的显示器模式刷新率，未知或旧主机显示缺省。")));
    let continuity_help=uu_i18n::text("仅统计前台可见串流提交的新画面。间隔超过3个预期帧周期且至少20ms记一次，预期帧率取目标与已知源刷新率较小值；持续停图只计一次，完成后保留60秒。失焦、最小化、切屏或帧率改变会重置比较基线。源空闲、丢帧、网络、解码或UI都可能产生长间隔，不能据此归因，也不等于物理屏幕卡顿。少于两帧时显示未知。");
    fps.push(field(uu_i18n::text("长间隔/60s"),m.continuity.as_ref().map_or_else(||"—".into(),|s|s.long_gaps.to_string()),continuity_help));
    if let Some(s)=&m.continuity {
        fps.push(field(uu_i18n::text("最长/阈值"),format!("{:.1}/{:.1} ms",s.max_gap_ms,s.threshold_ms),continuity_help));
        fps.push(field(uu_i18n::text("当前观测"),format!("{:.1} s",s.observed_seconds),continuity_help));
        if let Some(gap)=s.ongoing_gap_ms {fps.push(field(uu_i18n::text("正在等待"),format!("{gap:.1} ms"),continuity_help));}
    }
    vec![
        row(uu_i18n::text("画面"),cyan,vec![field("",format!("{}×{}@{}",m.size[0],m.size[1],m.target_fps),uu_i18n::text("当前视频尺寸与目标帧率；不会改变远端桌面模式。")),field(uu_i18n::text("编码"),if m.codec.is_empty(){"—"}else{&m.codec},uu_i18n::text("实际协商的编码与色彩模式，区别于请求偏好。")),field(uu_i18n::text("解码器"),decoder_label(&m.backend),uu_i18n::text("客户端当前实际使用的解码后端。")),field(uu_i18n::text("码率"),format!("{} Mbps",number(m.bitrate)),uu_i18n::text("近期接收到的视频码率；不是目标码率或总链路带宽。"))]),
        row(uu_i18n::text("帧率"),cyan,fps),
        row(uu_i18n::text("视频端到端"),green,video),
        row(uu_i18n::text("阶段小计"),green,latency),
        row(uu_i18n::text("网络"),if m.loss.is_some_and(|v|v>1.){orange}else{blue},network),
        row(uu_i18n::text("排队"),green,vec![field(uu_i18n::text("发"),format!("{} ms",number(m.queues[0])),uu_i18n::text("主机发送视频包时等待传输API完成的时间，可能包含套接字背压和调度。")),field(uu_i18n::text("解"),format!("{} ms",number(m.queues[1])),uu_i18n::text("完整帧进入解码工作队列到开始解码的等待。")),field(uu_i18n::text("邮箱"),format!("{} ms",number(m.queues[2])),uu_i18n::text("解码画面发布到UI领取之间的等待；受UI唤醒、绘制预算和负载影响。")),field(uu_i18n::text("合计"),format!("{} ms",number(sum(&m.queues))),uu_i18n::text("发送、解码、邮箱等待的均值之和；缺样本时显示未知。"))]),
        row(uu_i18n::text("恢复累计"),plain,vec![field(uu_i18n::text("FEC恢复/冗余"),format!("{}/{}",c.fec_recovered,c.fec_packets),uu_i18n::text("FEC恢复的包数与收到的冗余包数，会话累计。")),field(uu_i18n::text("冗余量"),format!("{:.2} MiB",c.fec_bytes as f64/1048576.),uu_i18n::text("会话收到的FEC冗余数据量。")),field(uu_i18n::text("FEC丢弃"),c.fec_drops,uu_i18n::text("无法使用或被丢弃的FEC数据。")),field("RTX",c.rtx,uu_i18n::text("收到的重传包数。")),field("NACK",c.nack,uu_i18n::text("发送的缺包重传请求数。"))]),
        row(uu_i18n::text("异常累计"),if c.errors+c.damaged+c.queue_drops>0{orange}else{plain},vec![field(uu_i18n::text("未恢复包"),c.lost,uu_i18n::text("会话累计超时未恢复包。")),field(uu_i18n::text("损帧"),c.damaged,uu_i18n::text("接收器丢弃的不完整或损坏视频帧。")),field(uu_i18n::text("解码错"),c.errors,uu_i18n::text("解码后端返回的错误次数。")),field(uu_i18n::text("队列丢帧"),c.queue_drops,uu_i18n::text("解码队列为保留新鲜画面而丢弃的帧数。")),field(uu_i18n::text("刷新"),c.keyframes,uu_i18n::text("请求关键帧的次数。")),field(uu_i18n::text("乱/迟"),format!("{}/{}",c.reordered,c.late),uu_i18n::text("修复接收器记录的乱序与迟到次数。"))]),
        row(uu_i18n::text("输入响应"),plain,vec![
            field(uu_i18n::text("视觉响应"),"—",uu_i18n::text("尚无输入事件到对应画面显示的因果关联测量，视觉响应保持未知；注入确认、视频端到端和阶段和均不能代替它。")),
            field(uu_i18n::text("注入确认"),format!("{} ms",number(m.input_timing[1])),uu_i18n::text("抽样输入从客户端发出、主机系统接受注入，到确认返回客户端的往返时间；不表示应用已处理，也不表示对应画面已显示。")),
            field(uu_i18n::text("状态"),if m.input_ready{uu_i18n::text("已就绪")}else{uu_i18n::text("未就绪")},uu_i18n::text("当前窗口、主机授权和本地输入归属共同决定是否能远程输入。")),
            field(uu_i18n::text("发/错"),format!("{}/{}",m.input_counts[0],m.input_counts[1]),uu_i18n::text("客户端发送的输入消息数和发送错误数。")),
            field(uu_i18n::text("排队/主机队列/注入"),[m.input_timing[0],m.input_timing[2],m.input_timing[3]].map(number).join("/"),uu_i18n::text("输入在客户端的排队、主机队列与系统注入耗时，单位毫秒；这些阶段不构成视觉响应测量。")),
        ]),
        row(uu_i18n::text("收包"),plain,receive),
        row(uu_i18n::text("恢复原因"),plain,vec![field(uu_i18n::text("接收/排队/解码/格式"),uu_i18n::format!("{}/{}/{}/{} 次",c.recovery[0],c.recovery[1],c.recovery[2],c.recovery[3]),uu_i18n::text("分别由接收损坏、解码队列溢出、解码错误和格式改变触发的恢复次数。"))]),
    ]
}
/// Height and width include the complete frame, in viewport logical points.
pub fn detail_limits(view: Rect) -> egui::Vec2 {
    egui::vec2(
        (view.width() - 12.).clamp(1., 1120.),
        (view.height() - 12.).clamp(1., 310.),
    )
}
fn detail_contents(ui:&mut egui::Ui,m:&Metrics,width:f32,max_height:f32) {
    ui.set_width(width.max(1.));ui.set_max_height(max_height.max(1.));
    *ui.visuals_mut()=egui::Visuals::dark();ui.spacing_mut().item_spacing=egui::vec2(5.,3.);
    let remaining=max_height.max(1.);let chart_width=(width*0.37).clamp(90.,400.);let data_width=(width-chart_width-18.).max(1.);
    ui.horizontal_top(|ui|{
        ui.allocate_ui_with_layout(egui::vec2(data_width,remaining),egui::Layout::top_down(egui::Align::Min),|ui|{
            egui::ScrollArea::vertical().id_salt("hud-detail-scroll").max_height(remaining).min_scrolled_height(0.).auto_shrink([false,true]).show(ui,|ui|{
                let rows=detail_rows(m);
                let title_width=rows.iter().map(|r|ui.fonts(|fonts|fonts.layout_no_wrap(r.label.into(),FontId::proportional(12.),r.color).size().x)).fold(50.,f32::max).min(110.).min(data_width*0.30);
                for row in rows{
                    ui.horizontal_top(|ui|{
                        ui.allocate_ui_with_layout(egui::vec2(title_width,0.),egui::Layout::top_down(egui::Align::Min),|ui|{
                            ui.set_width(title_width);
                            ui.add(egui::Label::new(RichText::new(row.label).size(12.).color(row.color).strong()).selectable(true));
                        });
                        ui.vertical(|ui|{ui.set_width((data_width-title_width-7.).max(1.));ui.horizontal_wrapped(|ui|{
                            let mut group=0;
                            for v in row.values {
                                if v.group!=group&&group>0{ui.label(RichText::new("│").size(12.).color(Color32::from_gray(100)));}group=v.group;
                                ui.add(egui::Label::new(RichText::new(format!("{} {}",v.label,v.value)).size(12.).color(row.color)).selectable(true).wrap()).on_hover_text(v.help);
                            }
                        });});
                    });
                }
            });
        });
        ui.separator();
        ui.allocate_ui_with_layout(egui::vec2(chart_width,remaining),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_max_width(chart_width);egui::ScrollArea::vertical().id_salt("hud-chart-scroll").max_height(remaining).min_scrolled_height(0.).show(ui,|ui|history::chart(ui,&m.history,(remaining-52.).max(80.)));});
    });
}
/// Shared by the production hover card and the generated visual probe.
pub fn details(ui: &mut egui::Ui, m: &Metrics) {
    let limits = detail_limits(ui.ctx().screen_rect());
    detail_contents(ui, m, limits.x - 16., limits.y - 16.);
}
fn detail_origin(view: Rect, anchor: Rect, size: egui::Vec2) -> egui::Pos2 {
    let safe = view.shrink(6.);
    let below=anchor.bottom()+4.;
    let y=if below+size.y<=safe.bottom(){below}else if anchor.top()-4.-size.y>=safe.top(){anchor.top()-4.-size.y}else{below.clamp(safe.top(),(safe.bottom()-size.y).max(safe.top()))};
    egui::pos2(
        anchor
            .left()
            .clamp(safe.left(), (safe.right() - size.x).max(safe.left())),
        y,
    )
}
pub fn details_at(
    ui: &egui::Ui,
    m: &Metrics,
    view: Rect,
    anchor: Rect,
    previous: Option<Rect>,
) -> Rect {
    let mut limits = detail_limits(view);
    let available=(view.bottom()-6.-anchor.bottom()-4.).max(anchor.top()-view.top()-10.).max(1.);
    limits.y=limits.y.min(available);
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
    selected: bool,
}
impl HoverCard {
    pub fn keyboard_owned(&self)->bool {self.selected}

    /// Hover alone keeps keyboard ownership remote. Clicking this local card
    /// owns text selection/copy until a click outside, Escape or focus loss.
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
        if ui.ctx().input(|i|i.key_pressed(egui::Key::Escape)) {self.selected=false;}
        if ui.ctx().input(|i|i.pointer.any_pressed()) {self.selected=over_card;}
        if source.hovered() || over_card || self.selected {
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
    fn clicking_detail_owns_keyboard_until_outside_escape_or_focus_loss() {
        let ctx=egui::Context::default();let mut card=HoverCard::default();
        let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));let mut now=0.;
        let mut frame=|card:&mut HoverCard,events:Vec<egui::Event>,focused:bool| {
            now+=0.2;
            let _=ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),events,focused,..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|{
                let source=ui.interact(Rect::from_min_size(egui::pos2(12.,12.),egui::vec2(160.,24.)),egui::Id::new("source"),egui::Sense::hover());
                card.show(ui,&source,&Metrics::default(),view);
            });});
        };
        let move_to=|p|vec![egui::Event::PointerMoved(p)];
        for _ in 0..6{frame(&mut card,move_to(egui::pos2(30.,20.)),true);}
        assert!(card.bounds.is_some());assert!(!card.keyboard_owned(),"hover keeps keyboard remote");
        let point=card.bounds.unwrap().center();
        let press=|p,pressed|vec![egui::Event::PointerMoved(p),egui::Event::PointerButton{pos:p,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}];
        frame(&mut card,press(point,true),true);frame(&mut card,press(point,false),true);
        assert!(card.keyboard_owned());
        frame(&mut card,move_to(egui::pos2(1270.,700.)),true);assert!(card.keyboard_owned(),"selection stays available for Ctrl+C after dragging outside");
        frame(&mut card,press(egui::pos2(1270.,700.),true),true);assert!(!card.keyboard_owned());
        frame(&mut card,press(egui::pos2(1270.,700.),false),true);
        for _ in 0..6{frame(&mut card,move_to(egui::pos2(30.,20.)),true);}
        let point=card.bounds.unwrap().center();frame(&mut card,press(point,false),true);frame(&mut card,press(point,true),true);assert!(card.keyboard_owned());
        frame(&mut card,vec![egui::Event::Key{key:egui::Key::Escape,physical_key:None,pressed:true,repeat:false,modifiers:Default::default()}],true);assert!(!card.keyboard_owned());
        frame(&mut card,press(point,false),true);frame(&mut card,press(point,true),true);assert!(card.keyboard_owned());
        frame(&mut card,vec![],false);assert!(!card.keyboard_owned());assert!(card.bounds.is_none());
    }
    #[test]
    fn details_keep_explained_fields_and_align_category_with_the_first_value() {
        let m=Metrics {codec:"PYROWAVE SDR".into(),process:[Some(1.);4],rtt:Some([2.,0.2]),loss:Some(0.4),unrecovered_loss:Some(0.),..Default::default()};
        let rows=detail_rows(&m);
        assert!(rows.iter().flat_map(|r|&r.values).all(|v|!v.help.is_empty()));
        let latency=&rows.iter().find(|r|r.label=="阶段小计").unwrap().values;
        assert_eq!(latency.iter().filter(|v|v.group==1).count(),4);
        assert_eq!(latency.iter().filter(|v|v.group==2).count(),2);
        for width in [1280.,640.,320.] {
            let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,720.));
            let mut checked=0;
            for _ in 0..3 {
                let output=ctx.run(egui::RawInput{screen_rect:Some(view),..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|details(ui,&m));});
                let texts:Vec<_>=output.shapes.iter().filter_map(|s|if let egui::Shape::Text(t)=&s.shape {Some(t)}else{None}).collect();
                assert!(!texts.iter().any(|t|matches!(t.galley.job.text.as_str(),"串流诊断"|"复制详情")));
                checked=0;
                for row in &rows {
                    let first=&row.values[0];let value=format!("{} {}",first.label,first.value);
                    if let (Some(label),Some(value))=(texts.iter().find(|t|t.galley.job.text==row.label),texts.iter().find(|t|t.galley.job.text==value)){
                        assert!((label.pos.y-value.pos.y).abs()<=1.,"{width}: {} is not aligned: {:?} {:?}",row.label,label.pos,value.pos);
                        checked+=1;
                    }
                }
            }
            assert!(checked>=3,"not enough visible rows at {width}");
        }
    }
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
            let mut bounds: Option<Rect> = None;
            let mut final_category_visible=false;
            for frame in 0..14 {
                let mut events=Vec::new();
                if frame>=3 {
                    if let Some(bounds)=bounds {
                        events.push(egui::Event::PointerMoved(bounds.min+egui::vec2(40.,40.)));
                        if frame<7 {events.push(egui::Event::MouseWheel {unit:egui::MouseWheelUnit::Point,delta:egui::vec2(0.,-1200.),modifiers:Default::default()});}
                    }
                }
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(view),
                        time:Some(frame as f64/30.),
                        events,
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
                final_category_visible|=output.shapes.iter().any(|shape|match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text=="恢复原因"=>shape.clip_rect.contains_rect(Rect::from_min_size(text.pos,text.galley.size())),
                    _=>false
                });
            }
            assert!(final_category_visible,"the final category must be reachable by scrolling at {size:?}");
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
    fn stage_sum_never_substitutes_missing_same_frame_measurement() {
        let m = Metrics {
            process: [Some(1.), Some(1.6), Some(0.8), Some(5.4)],
            queues: [Some(0.7), Some(0.3), Some(3.)],
            rtt: Some([4., 0.6]),
            ..Default::default()
        };
        assert!((m.estimate().known - 14.8).abs() < 0.001);
        assert!(m.estimate().missing.is_empty());
        assert_eq!(m.stage_sum_label(), "阶段小计 ≈14.8 ms");
        assert_eq!(m.total_label(), "视频端到端 — · 未知");
        assert_eq!(m.video_mean_ms(),None);
        assert_eq!(m.video_uncertainty_ms(),None);
        let partial = Metrics {
            process: [None, Some(0.), None, None],
            ..Default::default()
        };
        assert_eq!(partial.estimate().samples, 1);
        assert!(!partial.estimate().missing.is_empty());
        assert!(partial.stage_sum_label().starts_with("已知阶段小计"));
        assert_eq!(partial.total_label(), "视频端到端 — · 未知");
        assert_eq!(Metrics::default().stage_sum_label(), "阶段小计 —");
    }
    fn e2e_snapshot(measurement:Option<uu_core::video_e2e::Measurement>,unknown:Option<uu_core::video_e2e::UnknownReason>)->uu_core::video_e2e::Snapshot {
        uu_core::video_e2e::Snapshot {generation:1,relative_drift_ppm:1000,measurement,unknown,source_frames:120,displayed_frames:24,observations_dropped:0,history_evicted:0}
    }
    #[test]
    fn primary_estimate_keeps_uncertainty_method_and_input_response_separate_in_both_languages() {
        use uu_core::video_e2e::{DisplayMethod,Measurement};
        let m=Metrics {
            video_e2e:Some(e2e_snapshot(Some(Measurement {samples:24,mean_us:21_163,p50_us:20_000,p95_us:26_000,uncertainty_us:999,max_sample_uncertainty_us:1500,method:DisplayMethod::PlatformDisplayed}),None)),
            input_timing:[Some(0.1),Some(2.2),Some(0.2),Some(0.1)],
            ..Default::default()
        };
        uu_i18n::with_language(uu_i18n::Language::Chinese,||{
            assert_eq!(m.total_label(),"视频端到端 ≈21.163 ±0.999 ms · 系统显示回执");
            assert_eq!(m.input_response_label(),"输入响应 — · 注入确认 2.2 ms");
        });
        uu_i18n::with_language(uu_i18n::Language::English,||{
            assert_eq!(m.total_label(),"Video E2E ≈21.163 ±0.999 ms · System display receipt");
            assert_eq!(m.input_response_label(),"Input response — · Injection ack 2.2 ms");
            let rows=detail_rows(&m);
            assert!(rows.iter().any(|row|row.label=="Stage sum"));
            let input=&rows.iter().find(|row|row.label=="Input response").unwrap().values;
            assert!(input.iter().any(|value|value.label=="Visible response"&&value.value=="—"));
        });
        assert_eq!(m.video_mean_ms(),Some(21.163));
        assert_eq!(m.video_uncertainty_ms(),Some(0.999));
    }
    #[test]
    fn dxgi_method_exposes_empirical_quality_and_multiple_monitor_limitation() {
        use uu_core::video_e2e::{DisplayMethod,Measurement};
        let m=Metrics {video_e2e:Some(e2e_snapshot(Some(Measurement {samples:2,
            mean_us:21_000,p50_us:20_000,p95_us:22_000,uncertainty_us:10_000,max_sample_uncertainty_us:15_000,
            method:DisplayMethod::DxgiReceiptInterval}),None)),..Default::default()};
        uu_i18n::with_language(uu_i18n::Language::Chinese,||{
            assert!(m.total_label().ends_with("DXGI回执估测"));
            let rows=detail_rows(&m);
            let values=&rows.iter().find(|row|row.label=="视频端到端").unwrap().values;
            assert!(values.iter().any(|v|v.help.contains("多显示器")&&v.help.contains("经验估测")));
            assert!(values.iter().any(|v|v.help.contains("不是平台保证的硬界")));
        });
        uu_i18n::with_language(uu_i18n::Language::English,||{
            assert!(m.total_label().ends_with("DXGI receipt estimate"));
            let rows=detail_rows(&m);
            let values=&rows.iter().find(|row|row.label=="Video E2E").unwrap().values;
            assert!(values.iter().any(|v|v.help.contains("empirical")&&v.help.contains("multiple monitors")));
        });
    }
    #[test]
    fn mean_bound_single_frame_bound_and_history_evictions_have_separate_fields() {
        use uu_core::video_e2e::{DisplayMethod,Measurement};
        let mut snapshot=e2e_snapshot(Some(Measurement {samples:2,
            mean_us:15_000,p50_us:10_000,p95_us:20_000,uncertainty_us:5_000,
            max_sample_uncertainty_us:10_000,method:DisplayMethod::DxgiReceiptInterval}),None);
        snapshot.history_evicted=42;
        let m=Metrics {video_e2e:Some(snapshot),..Default::default()};
        for (language,row,history,max_frame,discard) in [
            (uu_i18n::Language::Chinese,"视频端到端","历史淘汰","单帧最大半宽","源/显/观测丢弃"),
            (uu_i18n::Language::English,"Video E2E","History evictions","Max frame half-width","Src/disp/obs drops")
        ] {
            uu_i18n::with_language(language,||{
                assert!(m.total_label().contains("≈15.000 ±5.000 ms"));
                let rows=detail_rows(&m);
                let fields=&rows.iter().find(|r|r.label==row).unwrap().values;
                assert!(fields.iter().any(|f|f.label==history&&f.value=="42"));
                assert!(fields.iter().any(|f|f.label==max_frame&&f.value=="±10.000 ms"));
                assert!(fields.iter().any(|f|f.label==discard&&f.value=="120/24/0"));
            });
        }
    }
    #[test]
    fn unknown_causes_stay_explicit_and_a_measured_zero_is_available() {
        use uu_core::video_e2e::{DisplayMethod,Measurement,UnknownReason};
        uu_i18n::with_language(uu_i18n::Language::English,||{
            for (reason,label) in [
                (UnknownReason::UnsupportedHost,"Unsupported"),
                (UnknownReason::SourceTimestampUnavailable,"Source time unknown"),
                (UnknownReason::ClockUnsynchronized,"Synchronizing clocks"),
                (UnknownReason::DisplayTimestampUnavailable,"Display time unknown"),
                (UnknownReason::NoCurrentSample,"Waiting for a current sample"),
            ] {
                let m=Metrics {video_e2e:Some(e2e_snapshot(None,Some(reason))),process:[Some(1.);4],queues:[Some(1.);3],rtt:Some([2.,0.1]),..Default::default()};
                assert_eq!(m.total_label(),format!("Video E2E — · {label}"));
                assert_eq!(m.video_mean_ms(),None);
            }
            let zero=Measurement {samples:1,mean_us:0,p50_us:0,p95_us:0,uncertainty_us:0,max_sample_uncertainty_us:0,method:DisplayMethod::RefreshInterval};
            let m=Metrics {video_e2e:Some(e2e_snapshot(Some(zero),None)),..Default::default()};
            assert_eq!(m.video_mean_ms(),Some(0.));
            assert_eq!(m.total_label(),"Video E2E ≈0.000 ±0.000 ms · Refresh interval");
            let invalid=Metrics {video_e2e:Some(e2e_snapshot(Some(Measurement {samples:0,..zero}),None)),..Default::default()};
            assert_eq!(invalid.video_mean_ms(),None);
        });
    }
    #[test]
    fn fps_and_timing_triplets_have_consistent_order() {
        let m = Metrics {
            fps: [Some(60.), Some(59.9), Some(59.7)],
            process: [Some(8.), Some(1.2), Some(0.07), Some(2.3)],
            ..Default::default()
        };
        assert_eq!(m.fps_label(), "FPS[recv/dec/rnd] 60.0/59.9/59.7");
        assert_eq!(m.stages_label(), "Enc/Dec/Rnd 1.2/0.07/2.3 ms");
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
