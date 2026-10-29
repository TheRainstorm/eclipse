//! Presentation shared by the live HUD and the isolated visual probe.
use eframe::egui::{self, Color32, FontId, Rect, RichText};
use std::sync::Arc;
use uu_core::client_settings::HudAppearance;
#[path="hud_history.rs"]
mod history;
pub use history::{History,Sample as HistorySample,EstimatedTotal};
#[path="hud_statistics.rs"]
mod statistics;
pub use statistics::{Statistics,WINDOWS};

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
    pub link: Option<uu_core::link_metrics::Snapshot>,
    pub statistics: [Counters;3],
    pub statistics_coverage: [f64;3],
    pub continuity_windows: [Option<uu_core::frame_continuity::Snapshot>;3],
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
    pub input_latency:Option<uu_core::video_e2e::InputSnapshot>,
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
    pub original_received:u64,
    pub original_lost:u64,
    pub duplicates:u64,
    pub resyncs:u64,
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
fn unit(value:f32,fallback:f32)->f32 {if value.is_finite(){value.clamp(0.,1.)}else{fallback}}
fn background_alpha(value:f32,fallback:f32)->u8 {(unit(value,fallback)*255.).round() as u8}
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
        format!("Est.Total {}",self.estimated_value())
    }
    fn estimated_total(&self)->Option<EstimatedTotal> {
        let e=self.estimate();
        if !e.missing.is_empty()||!e.known.is_finite(){return None;}
        self.history.last().and_then(|s|s.estimated_total).or(Some(EstimatedTotal{mean_ms:e.known,std_ms:None,samples:1}))
    }
    fn estimated_value(&self)->String {
        self.estimated_total().map_or_else(||"—".into(),|v|format!("≈{:.2} ±{} ms",v.mean_ms,v.std_ms.map_or_else(||"—".into(),|s|format!("{s:.2}"))))
    }
    fn throughput(&self) -> Option<f32> {
        let link=self.link?;
        valid(Some(valid(link.rx_mbps)?+valid(link.tx_mbps)?))
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
            format!("≈{:.2} ±{:.2} ms",value.mean_us as f64/1000.,value.uncertainty_us as f64/1000.))
    }
    pub fn diagnostic_text(&self,window:usize)->String {
        let window=window.min(2);
        let mut text=format!("Eclipse {} · {}\n",env!("CARGO_PKG_VERSION"),uu_i18n::text("串流诊断"));
        text+=&uu_i18n::format!("统计窗口 {} 分钟 · 已记录 {:.1} 秒\n",WINDOWS[window]/60,self.statistics_coverage[window]);
        for row in detail_rows_window(self,window) {
            let lines=row.values.chunk_by(|a,b|a.line==b.line).map(|line|line.iter().map(|v|format!("{} {}",v.label,v.value).trim().to_owned()).collect::<Vec<_>>().join(" · ")).collect::<Vec<_>>();
            text+=&format!("{}: {}\n",row.label,lines.join("\n  "));
        }
        text+=uu_i18n::text("视频端到端为区间估测；阶段为独立窗口均值，不能相减推算未测等待。帧周期是节拍参考，不额外加入E2E。\n");
        text
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
        uu_i18n::format!("端到端 {} · {}",self.video_value(),status)
    }
    fn input_response_label(&self) -> String {
        uu_i18n::format!("输入→后继画面 {} · 注入确认 {} ms",self.input_frame_value(),number(self.input_timing[1]))
    }
    fn input_frame_value(&self) -> String {
        self.input_latency.and_then(|s|s.input_to_frame).map_or_else(||"—".into(),|v|
            format!("≈{:.2} ±{:.2} ms",v.mean_us as f64/1000.,v.uncertainty_us as f64/1000.))
    }
    pub fn fps_label(&self) -> String {
        let values = self
            .fps
            .map(|v| valid(v).map_or_else(|| "—".into(), |v| format!("{v:.1}")));
        format!(
            "FPS · Rcv/Dec/Rnd {}/{}/{}",
            values[0], values[1], values[2]
        )
    }
    fn network_label(&self) -> String {
        let rtt = self.rtt.map_or_else(
            || "—".into(),
            |v| format!("{} ± {} ms", number(Some(v[0])), number(Some(v[1]))),
        );
        let loss = valid(self.loss).map_or_else(|| "—".into(), |v| format!("{v:.2}%"));
        format!("BW {} Mbps · RTT {rtt} · Loss {loss}",number(self.throughput()))
    }
    fn stages_label(&self) -> String {
        let total=self.estimated_value();
        format!(
            "Latency · Cap+Enc/Dec+Rnd {}/{} ms · Est. {total}",
            number(sum(&self.process[..2])),
            number(sum(&self.process[2..]))
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
}
impl Layout {
    pub fn new(ui: &egui::Ui, m: &Metrics, width: f32) -> Self {
        let primary = Color32::from_rgb(239, 245, 252);
        let muted = Color32::from_rgb(181, 198, 218);
        let network = Color32::from_rgb(154, 202, 255);
        let process = Color32::from_rgb(159, 231, 204);
        let codec = if m.codec.is_empty() { "—" } else { &m.codec };
        let resolution = if m.size.contains(&0) {
            "—".into()
        } else {
            format!("{}x{}@{}", m.size[0], m.size[1], m.target_fps)
        };
        let mut rows=vec![vec![
            (format!("{resolution} {codec}"),13.,primary),
            (m.fps_label(),13.,muted),
            (m.network_label(),13.,network),
            (m.stages_label(),13.,process),
            (format!("E2E · Video {} · Input→Frame {}",m.video_value(),m.input_frame_value()),13.,primary),
        ]];
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
        }
    }
    pub fn paint(&self, ui: &egui::Ui, video: Rect, appearance:HudAppearance) -> egui::Response {
        let pad = egui::vec2(8.,5.);
        let size=self.size+pad*2.;
        let y=(video.height()*unit(appearance.vertical_position,HudAppearance::default().vertical_position)).min((video.height()-size.y).max(0.));
        let bounds = Rect::from_min_size(
            egui::pos2(
                video.center().x - self.size.x / 2. - pad.x,
                video.top() + y,
            ),
            size,
        );
        ui.painter().rect_filled(
            bounds,
            7.,
            Color32::from_black_alpha(background_alpha(appearance.opacity,HudAppearance::default().opacity)),
        );
        {
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
struct DetailValue { label: &'static str, value: String, help: &'static str, group: u8, line: u8 }
struct DetailRow { label: &'static str, color: Color32, values: Vec<DetailValue> }
fn field(label:&'static str,value:impl ToString,help:&'static str)->DetailValue {DetailValue{label,value:value.to_string(),help,group:0,line:0}}
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
        field(uu_i18n::text("均值区间"),measured.map_or_else(||"—".into(),|v|format!("{:.2}–{:.2} ms",(v.mean_us.saturating_sub(v.uncertainty_us)/10) as f64/100.,v.mean_us.saturating_add(v.uncertainty_us).div_ceil(10) as f64/100.)),mean_help),
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
    let mut latency=vec![field("",m.stage_sum_label(),uu_i18n::text("Est.Total为最近3秒完整分量和采样的均值±标准差，每500ms记录一次，表示估计值波动，不是同帧测量或准确性误差界。采、编、解、渲、RTT/2与三项队列均有效才纳入；缺项留空，少于两次采样时±未知。未覆盖的采集前等待、GPU完成、呈现队列及物理响应不包含在内。"))];
    for (i,(label,help)) in [(uu_i18n::text("采"),uu_i18n::text("主机获取本帧桌面图像的时间。")),(uu_i18n::text("编"),uu_i18n::text("主机编码调用耗时，可包含硬件队列等待。")),(uu_i18n::text("解"),uu_i18n::text("客户端解码调用耗时。")),(uu_i18n::text("渲"),uu_i18n::text("客户端UI准备与渲染提交的CPU墙钟时间，可能包含驱动等待；不是GPU计时或扫描延迟。"))].into_iter().enumerate() {let mut v=field(label,format!("{} ms",number(m.process[i])),help);v.group=1;latency.push(v);}
    for (label,value,help) in [(uu_i18n::text("网"),m.rtt.map(|v|v[0]/2.),uu_i18n::text("RTT的一半，仅在往返对称时近似单程。RTT还可能包含应用调度、处理与队列等待。")),(uu_i18n::text("排"),sum(&m.queues),uu_i18n::text("主机发送等待、客户端解码队列和画面邮箱等待的合计。未知阶段不按零计算。"))]{let mut v=field(label,format!("{} ms",number(value)),help);v.group=2;latency.push(v);}
    for (i,(label,help)) in [
        (uu_i18n::text("排队 · 发"),uu_i18n::text("主机发送视频包时等待传输API完成的时间，可能包含套接字背压和调度。")),
        (uu_i18n::text("解"),uu_i18n::text("完整帧进入解码工作队列到开始解码的等待。")),
        (uu_i18n::text("邮箱"),uu_i18n::text("解码画面发布到UI领取之间的等待；受UI唤醒、绘制预算和负载影响。"))
    ].into_iter().enumerate() {
        let mut v=field(label,format!("{} ms",number(m.queues[i])),help);v.line=1;v.group=1;latency.push(v);
    }
    for (label,span,help) in [
        (uu_i18n::text("源至发完"),m.host_video.as_ref().and_then(|v|v.source_to_last_rtp),uu_i18n::text("主机同一QPC时钟内，源画面呈现到最后一个RTP包写入API返回的均值/P95。仅新源画面；包含采集前画面年龄、编码、排队和RTP发送，与视频阶段和重叠，不能再相加。不是网卡发完或客户端显示完成；超过5秒未更新则未知。")),
        (uu_i18n::text("RTP跨度"),m.host_video.as_ref().and_then(|v|v.rtp_write),uu_i18n::text("同一视频帧首个到最后一个RTP写入API返回的均值/P95；用于观察分包与传输调用耗时，区别于编码线程等待发送队列。它与源至发完重叠，不是线速或单程网络时间。"))] {
        let value=span.map_or_else(||"—".into(),|v|format!("{:.1}/{:.1} ms",v.mean_us as f64/1000.,v.p95_us as f64/1000.));
        let mut value=field(label,value,help);value.group=2;value.line=1;latency.push(value);
    }
    let link_help=uu_i18n::text("实际选中直连路径的本机网卡接收/发送吞吐，包含这张网卡的其它应用流量；不是视频码率、网卡标称速率或可用带宽。每秒在后台读取字节计数，路径变化或样本不足时未知。");
    let ping_help=uu_i18n::text("对实际选中直连媒体端点的ICMP echo往返，每秒一次，超时200ms；区别于应用CONTROL RTT。受防火墙和系统权限影响，不支持或无回复时未知。Windows回执精度为1ms，小于1ms显示<1ms；中继路径不冒充对端ping。");
    let ping=m.link.and_then(|v|v.ping_ms).map_or_else(||"—".into(),|v|if v<1.{"<1 ms".into()}else{format!("{v:.1} ms")});
    let mut network=vec![field("ICMP Ping",ping,ping_help),field(uu_i18n::text("链路吞吐"),format!("{} Mbps",number(m.throughput())),link_help),field("RX/TX",format!("{}/{} Mbps",number(m.link.and_then(|v|v.rx_mbps)),number(m.link.and_then(|v|v.tx_mbps))),link_help),field("RTT",m.rtt.map_or_else(||"—".into(),|v|format!("{}±{} ms",number(Some(v[0])),number(Some(v[1])))),uu_i18n::text("控制消息往返均值±标准差，最近32个样本。包含传输、排队和两端处理，不等于ICMP ping。")),field(uu_i18n::text("链路丢包"),pct(m.loss),uu_i18n::text("修复前的原始RTP缺包率。乱序宽限期后仍未到达的包计入，即使随后由FEC或RTX恢复也保留。它反映接收端看到的网络交付，不只代表物理线路。")),field(uu_i18n::text("修复后丢包"),pct(m.unrecovered_loss),uu_i18n::text("接收器超时后仍未恢复的RTP丢包率；FEC/重传成功会降低这个值。"))];
    if let Some(v)=m.rtt_distribution {network.push(field(uu_i18n::text("末/低/中/P95/高"),format!("{:.1}/{:.1}/{:.1}/{:.1}/{:.1} ms",v[0],v[1],v[2],v[3],v[4]),uu_i18n::text("最近一分钟内RTT的末值、最小值、中位数、95分位和最大值，用于观察波动。")));}
    let mut receive=vec![];
    if let Some(n)=&m.network {
        receive.push(field(uu_i18n::text("窗口缺/应"),format!("{}/{}",n.window_lost,n.window_expected),uu_i18n::text("当前原始包窗口中的缺失数与期望数，二者用于计算链路丢包率。")));
        receive.push(field(uu_i18n::text("累计收/缺"),format!("{}/{}",n.original_received,n.original_lost),uu_i18n::text("会话内收到的原始RTP包与在宽限期后认定缺失的原始包。")));
        receive.push(field(uu_i18n::text("乱/重/迟/重置"),format!("{}/{}/{}/{}",n.reordered,n.duplicates,n.late,n.resyncs),uu_i18n::text("原始包乱序、重复、超宽限期迟到和无法解释序号跳变后的统计重置次数。")));
    }else {receive.push(field(uu_i18n::text("原始包统计"),"—",uu_i18n::text("当前还没有新鲜、可解释的原始包窗口；不会伪造零丢包。")));}
    let mut fps=vec![field(uu_i18n::text("接收"),format!("{} FPS",number(m.fps[0])),uu_i18n::text("进入接收管线的完整视频帧率。")),field(uu_i18n::text("解码"),format!("{} FPS",number(m.fps[1])),uu_i18n::text("成功解码帧率。")),field(uu_i18n::text("渲染"),format!("{} FPS",number(m.fps[2])),uu_i18n::text("当前视图提交的新画面帧率；不是显示器扫描输出帧率。"))];
    fps.push(field(uu_i18n::text("源"),m.source_hz.map_or_else(||"—".into(),|v|format!("{v} Hz")),uu_i18n::text("主机报告的显示器模式刷新率，未知或旧主机显示缺省。")));
    fps.push(field(uu_i18n::text("串流帧周期"),if m.target_fps>0{format!("{:.2} ms",1000./m.target_fps as f32)}else{"—".into()},uu_i18n::text("1000/目标FPS，只表示采样节拍，不是额外测得的等待，也不会加进E2E。30fps为33.33ms，60fps为16.67ms；实际等待取决于源画面和采集相位。")));
    fps.push(field(uu_i18n::text("源帧周期"),m.source_hz.filter(|v|*v>0).map_or_else(||"—".into(),|hz|format!("{:.2} ms",1000./hz as f32)),uu_i18n::text("1000/主机显示器刷新率，是源显示节拍参考；不是客户端扫描或采集等待实测。")));
    let continuity_help=uu_i18n::text("仅统计前台可见串流提交的新画面。间隔超过3个预期帧周期且至少20ms记一次，预期帧率取目标与已知源刷新率较小值；持续停图只计一次，完成后保留60秒。失焦、最小化、切屏或帧率改变会重置比较基线。源空闲、丢帧、网络、解码或UI都可能产生长间隔，不能据此归因，也不等于物理屏幕卡顿。少于两帧时显示未知。");
    let mut continuity=Vec::new();
    continuity.push(field(uu_i18n::text("长间隔/60s"),m.continuity.as_ref().map_or_else(||"—".into(),|s|s.long_gaps.to_string()),continuity_help));
    if let Some(s)=&m.continuity {
        continuity.push(field(uu_i18n::text("最长/阈值"),format!("{:.1}/{:.1} ms",s.max_gap_ms,s.threshold_ms),continuity_help));
        continuity.push(field(uu_i18n::text("当前观测"),format!("{:.1} s",s.observed_seconds),continuity_help));
        if let Some(gap)=s.ongoing_gap_ms {continuity.push(field(uu_i18n::text("正在等待"),format!("{gap:.1} ms"),continuity_help));}
    }
    vec![
        row(uu_i18n::text("画面"),cyan,vec![field("",format!("{}×{}@{}",m.size[0],m.size[1],m.target_fps),uu_i18n::text("当前视频尺寸与目标帧率；不会改变远端桌面模式。")),field(uu_i18n::text("编码"),if m.codec.is_empty(){"—"}else{&m.codec},uu_i18n::text("实际协商的编码与色彩模式，区别于请求偏好。")),field(uu_i18n::text("码率"),format!("{} Mbps",number(m.bitrate)),uu_i18n::text("近期接收到的视频码率；不是目标码率或总链路带宽。"))]),
        row(uu_i18n::text("帧率"),cyan,fps),
        row(uu_i18n::text("编解码"),green,vec![
            field(uu_i18n::text("延迟"),uu_i18n::format!("| 采+编 {} ms 解+呈 {} ms",number(sum(&m.process[..2])),number(sum(&m.process[2..]))),uu_i18n::text("采集与编码、解码与UI呈现提交的阶段和。呈现提交为CPU耗时，不含系统显示等待；独立阶段窗口不能代替端到端测量。")),
            field(uu_i18n::text("编码器"),m.host_video.as_ref().map_or("—",|v|v.encoder.as_str()),uu_i18n::text("当前媒体代次中新鲜的主机遥测所报告的真实编码后端，不按显卡品牌猜测。")),
            field(uu_i18n::text("编码模式"),if m.mode.is_empty(){"—"}else{&m.mode},uu_i18n::text("主机已确认的当前码率控制模式。")),
            field(uu_i18n::text("解码器"),decoder_label(&m.backend),uu_i18n::text("客户端当前实际使用的解码后端。"))]),
        row(uu_i18n::text("网络"),if m.loss.is_some_and(|v|v>1.){orange}else{blue},network),
        row(uu_i18n::text("延迟"),green,latency),
        row(uu_i18n::text("端到端"),green,video),
        row(uu_i18n::text("统计"),plain,continuity),
        row(uu_i18n::text("恢复累计"),plain,vec![field(uu_i18n::text("FEC恢复/冗余"),format!("{}/{}",c.fec_recovered,c.fec_packets),uu_i18n::text("FEC恢复的包数与收到的冗余包数，会话累计。")),field(uu_i18n::text("冗余量"),format!("{:.2} MiB",c.fec_bytes as f64/1048576.),uu_i18n::text("会话收到的FEC冗余数据量。")),field(uu_i18n::text("FEC丢弃"),c.fec_drops,uu_i18n::text("无法使用或被丢弃的FEC数据。")),field("RTX",c.rtx,uu_i18n::text("收到的重传包数。")),field("NACK",c.nack,uu_i18n::text("发送的缺包重传请求数。"))]),
        row(uu_i18n::text("异常累计"),if c.errors+c.damaged+c.queue_drops>0{orange}else{plain},vec![field(uu_i18n::text("未恢复包"),c.lost,uu_i18n::text("会话累计超时未恢复包。")),field(uu_i18n::text("损帧"),c.damaged,uu_i18n::text("接收器丢弃的不完整或损坏视频帧。")),field(uu_i18n::text("解码错"),c.errors,uu_i18n::text("解码后端返回的错误次数。")),field(uu_i18n::text("队列丢帧"),c.queue_drops,uu_i18n::text("解码队列为保留新鲜画面而丢弃的帧数。")),field(uu_i18n::text("刷新"),c.keyframes,uu_i18n::text("请求关键帧的次数。")),field(uu_i18n::text("乱/迟"),format!("{}/{}",c.reordered,c.late),uu_i18n::text("修复接收器记录的乱序与迟到次数。"))]),
        row(uu_i18n::text("输入响应"),plain,input_fields(m)),
        row(uu_i18n::text("收包"),plain,receive),
        row(uu_i18n::text("恢复原因"),plain,vec![field(uu_i18n::text("接收/排队/解码/格式"),uu_i18n::format!("{}/{}/{}/{} 次",c.recovery[0],c.recovery[1],c.recovery[2],c.recovery[3]),uu_i18n::text("分别由接收损坏、解码队列溢出、解码错误和格式改变触发的恢复次数。"))]),
    ]
}
fn input_fields(m:&Metrics)->Vec<DetailValue> {
    let bound=|value:Option<uu_core::video_e2e::InputDelay>|value.map_or_else(||"—".into(),|v|format!("≈{:.2} ±{:.2} ms",v.mean_us as f64/1000.,v.uncertainty_us as f64/1000.));
    let latency=m.input_latency.unwrap_or_default();
    let mut fields=vec![
            field(uu_i18n::text("后继画面"),m.input_frame_value(),uu_i18n::text("同一次输入入队，到注入后已观测源图像的首显回执；包含输入、后继源等待及视频路径，沿用显示回执的估测方法与误差界。不能证明游戏已处理该输入，也不包含鼠标硬件或物理发光。")),
            field(uu_i18n::text("视觉响应"),"—",uu_i18n::text("尚无输入事件到对应画面显示的因果关联测量，视觉响应保持未知；注入确认、视频端到端和阶段和均不能代替它。")),
            field(uu_i18n::text("注入确认"),format!("{} ms",number(m.input_timing[1])),uu_i18n::text("抽样输入从客户端发出、主机系统接受注入，到确认返回客户端的往返时间；不表示应用已处理，也不表示对应画面已显示。")),
            field(uu_i18n::text("状态"),if m.input_ready{uu_i18n::text("已就绪")}else{uu_i18n::text("未就绪")},uu_i18n::text("当前窗口、主机授权和本地输入归属共同决定是否能远程输入。")),
            field(uu_i18n::text("发/错"),format!("{}/{}",m.input_counts[0],m.input_counts[1]),uu_i18n::text("客户端发送的输入消息数和发送错误数。")),
            field(uu_i18n::text("排队/主机队列/注入"),[m.input_timing[0],m.input_timing[2],m.input_timing[3]].map(number).join("/"),uu_i18n::text("输入在客户端的排队、主机队列与系统注入耗时，单位毫秒；这些阶段不构成视觉响应测量。")),
        ];
    for (label,value,help) in [
        (uu_i18n::text("排队+上行"),latency.enqueue_to_host,uu_i18n::text("同一实际输入从客户端入队到主机CONTROL收到的区间，包含客户端等待与上行；复用四时间戳校时，不假设RTT对半。±是均值的校时误差界，非波动标准差。旧主机或未校时保持未知。")),
        (uu_i18n::text("输入至注入"),latency.enqueue_to_injected,uu_i18n::text("同一输入从客户端入队到主机成功完成系统注入，包含上行、主机队列与注入。与排队+上行重叠，不再相加；不代表应用已处理。")),
        (uu_i18n::text("后继源等待"),latency.injected_to_source,uu_i18n::text("同一主机时钟下，注入完成至后继已观测源图像更新的等待，可能包含应用调度与帧节拍；桌面其它变化也会更新画面，不能当作游戏处理时间。缺少匹配首显则未知。")),
    ] { let mut value=field(label,bound(value),help);value.line=1;fields.push(value); }
    let mut count=field(uu_i18n::text("输入样本/画面匹配"),format!("{}/{}",latency.tracked_inputs,latency.input_to_frame.map_or(0,|v|v.samples)),uu_i18n::text("最近3秒收到有效注入回执的实际输入抽样与匹配画面数，至多每250ms抽样一次；输入空闲、换代或证据过期后未知，不借用其它输入样本。"));count.line=1;fields.push(count);
    if let Some(v)=latency.enqueue_to_host {
        let mut spread=field(uu_i18n::text("送达P95/σ"),format!("{:.2}/{:.2} ms",v.p95_us as f64/1000.,v.std_dev_us as f64/1000.),uu_i18n::text("送达区间中点的P95与总体标准差，描述样本波动；与±校时误差界分开。"));spread.line=1;fields.push(spread);
    }
    fields
}
fn detail_rows_window(m:&Metrics,window:usize)->Vec<DetailRow> {
    let mut scoped=m.clone();
    scoped.counters=m.statistics[window].clone();
    scoped.continuity=m.continuity_windows[window].clone();
    let mut rows=detail_rows(&scoped);
    let c=&scoped.counters;
    for row in &mut rows {
        if row.label==uu_i18n::text("恢复累计") {row.label=uu_i18n::text("恢复统计");}
        if row.label==uu_i18n::text("异常累计") {row.label=uu_i18n::text("异常统计");}
        if row.label==uu_i18n::text("统计") {
            for field in &mut row.values {
                if field.label==uu_i18n::text("长间隔/60s") {field.label=uu_i18n::text("长间隔");}
                field.help=uu_i18n::text("所选3/10/60分钟内的前台新画面提交统计，完成间隔按1秒时间桶保留；持续停图只计一次，后台时间不续算。按间隔发生时的帧率判定，当前最长/阈值不代表物理扫描。窗口边界精度1秒，恢复后至少两帧才可测。");
            }
            row.values.push(field(uu_i18n::text("已记录"),format!("{:.1} s",m.statistics_coverage[window]),uu_i18n::text("本客户端实际记录到的计数器时间跨度，不足所选窗口时不外推；重连、切轨道或计数器重置后重新记录。连续性只在前台活动时观测。")));
        }
        if row.label==uu_i18n::text("收包") {
            row.values=vec![field(uu_i18n::text("原始收/缺"),format!("{}/{}",c.original_received,c.original_lost),uu_i18n::text("所选窗口内原始RTP接收与缺失计数的差值；不是当前短窗口的丢包率。")),field(uu_i18n::text("乱/重/迟/重置"),format!("{}/{}/{}/{}",c.reordered,c.duplicates,c.late,c.resyncs),uu_i18n::text("所选窗口内的乱序、重复、迟到与统计重置次数。"))];
        }
        if matches!(row.label,l if l==uu_i18n::text("恢复统计")||l==uu_i18n::text("异常统计")||l==uu_i18n::text("恢复原因")) {
            for field in &mut row.values {field.help=uu_i18n::text("所选统计窗口内的计数器增量；仅覆盖已记录时段，未满窗口不外推。连接、媒体、轨道或计数器重置时重新记录。");}
        }
    }
    rows
}
#[derive(Default)]
pub struct DetailState { pub pinned:bool, pub window:usize, copied_until:f64 }
/// Height and width include the complete frame, in viewport logical points.
pub fn detail_limits(view: Rect) -> egui::Vec2 {
    egui::vec2(
        (view.width() - 12.).clamp(1., 1120.),
        (view.height() - 12.).clamp(1., 520.),
    )
}
fn data_rows(ui:&mut egui::Ui,m:&Metrics,window:usize,width:f32) {
    let rows=detail_rows_window(m,window);
    let title_width=rows.iter().map(|r|ui.fonts(|f|f.layout_no_wrap(r.label.into(),FontId::proportional(12.),r.color).size().x)).fold(50.,f32::max).min(106.).min(width*0.27);
    for row in rows {
        ui.horizontal_top(|ui|{
            ui.allocate_ui_with_layout(egui::vec2(title_width,0.),egui::Layout::top_down(egui::Align::Min),|ui|{
                ui.set_width(title_width);ui.add(egui::Label::new(RichText::new(row.label).size(12.).color(row.color).strong()).selectable(true));
            });
            ui.vertical(|ui|{
                let width=(width-title_width-8.).max(1.);ui.set_width(width);
                for line in row.values.chunk_by(|a,b|a.line==b.line) {
                    // Both columns start at the text line's top. horizontal_wrapped
                    // reserves an interactive row and centers short labels in it.
                    ui.allocate_ui_with_layout(egui::vec2(width,0.),egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),|ui|{
                        let mut group=0;
                        for v in line {
                            if v.group!=group&&group>0 {ui.label(RichText::new("│").size(12.).color(Color32::from_gray(90)));}group=v.group;
                            ui.add(egui::Label::new(RichText::new(format!("{} {}",v.label,v.value).trim()).size(12.).color(Color32::from_rgb(219,227,238))).selectable(true).wrap()).on_hover_text(v.help);
                        }
                    });
                }
            });
        });
        ui.add_space(4.);
    }
}
fn detail_contents(ui:&mut egui::Ui,m:&Metrics,width:f32,max_height:f32,state:&mut DetailState) {
    ui.set_width(width.max(1.));ui.set_max_height(max_height.max(1.));
    *ui.visuals_mut()=egui::Visuals::dark();ui.visuals_mut().panel_fill=Color32::TRANSPARENT;
    ui.spacing_mut().item_spacing=egui::vec2(7.,4.);
    let now=ui.input(|i|i.time);let top=ui.cursor().top();
    ui.horizontal_wrapped(|ui|{
        ui.label(RichText::new(uu_i18n::text("串流诊断")).size(13.).strong().color(Color32::WHITE));
        for (i,name) in ["3 min","10 min","60 min"].iter().enumerate(){ui.selectable_value(&mut state.window,i,*name);}
        if ui.selectable_label(state.pinned,uu_i18n::text(if state.pinned{"已固定"}else{"固定"})).clicked(){state.pinned=!state.pinned;}
        if ui.button(uu_i18n::text(if now<state.copied_until{"已复制"}else{"复制诊断"})).clicked(){ui.ctx().copy_text(m.diagnostic_text(state.window));state.copied_until=now+1.5;}
    });
    ui.separator();
    let remaining=(max_height-(ui.cursor().top()-top)-2.).max(1.);
    if width<820. {
        egui::ScrollArea::vertical().id_salt("hud-detail-scroll").max_height(remaining).min_scrolled_height(0.).auto_shrink([false,true]).show(ui,|ui|{
            let width=(width-10.).max(1.);data_rows(ui,m,state.window,width);ui.separator();history::chart(ui,&m.history,190.,state.window);
        });
    } else {
        let columns_width=(width-20.).max(1.);let chart_width=columns_width*0.4;let data_width=columns_width-chart_width;
        ui.horizontal_top(|ui|{
            ui.allocate_ui_with_layout(egui::vec2(data_width,remaining),egui::Layout::top_down(egui::Align::Min),|ui|{
                egui::ScrollArea::vertical().id_salt("hud-detail-scroll").max_height(remaining).min_scrolled_height(0.).auto_shrink([false,true]).show(ui,|ui|data_rows(ui,m,state.window,(data_width-10.).max(1.)));
            });
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(chart_width,remaining),egui::Layout::top_down(egui::Align::Min),|ui|{
                ui.set_max_width(chart_width);egui::ScrollArea::vertical().id_salt("hud-chart-scroll").max_height(remaining).min_scrolled_height(0.).show(ui,|ui|history::chart(ui,&m.history,(remaining-65.).max(80.),state.window));
            });
        });
    }
}
/// Shared by the production hover card and the generated visual probe.
pub fn details(ui: &mut egui::Ui, m: &Metrics) {
    let limits = detail_limits(ui.ctx().screen_rect());
    detail_contents(ui, m, limits.x - 20., limits.y - 20., &mut DetailState::default());
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
) -> Rect { details_at_with_state(ui,m,view,anchor,previous,&mut DetailState::default(),HudAppearance::default()) }
pub fn details_at_with_state(ui:&egui::Ui,m:&Metrics,view:Rect,anchor:Rect,previous:Option<Rect>,state:&mut DetailState,appearance:HudAppearance)->Rect {
    let mut limits = detail_limits(view);
    let available=(view.bottom()-6.-anchor.bottom()-4.).max(anchor.top()-view.top()-10.).max(1.);
    limits.y=limits.y.min(available);
    let size = egui::vec2(
        limits.x,
        previous.map_or(limits.y, |r| r.height()).min(limits.y),
    );
    let pos = detail_origin(view, anchor, size);
    egui::Area::new(ui.id().with("hud-hover-card"))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .movable(false)
        .fixed_pos(pos)
        .constrain_to(view.shrink(6.))
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(Color32::from_black_alpha(background_alpha(appearance.diagnostic_opacity,HudAppearance::default().diagnostic_opacity)))
                .rounding(9.)
                .stroke(egui::Stroke::new(1.,Color32::from_white_alpha(35)))
                .inner_margin(10.)
                .show(ui, |ui| {
                    detail_contents(ui, m, limits.x - 22., limits.y - 22.,state)
                });
        })
        .response
        .rect
}
#[derive(Default)]
pub struct HoverCard {
    since: Option<f64>,
    dismiss_at: Option<f64>,
    pub bounds: Option<Rect>,
    selected: bool,
    state:DetailState,
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
        appearance:HudAppearance,
    ) -> bool {
        let (now, pointer, focused) = ui
            .ctx()
            .input(|i| (i.time, i.pointer.hover_pos(), i.focused));
        if !focused {self.selected=false;}
        if (!focused&&!self.state.pinned) || view.width() < 48. || view.height() < 48. {
            self.selected=false;self.since=None;self.bounds=None;self.dismiss_at=None;
            return false;
        }
        // Use the current pointer and our own regions, rather than the previous
        // pass's layer hover result. The foreground card can cover the HUD while
        // Area visibility/hit testing settles, which must not restart its lifetime.
        let over_source=focused&&pointer.is_some_and(|p|source.rect.contains(p));
        let over_card = focused&&pointer.is_some_and(|p| self.bounds.is_some_and(|r| r.contains(p)));
        if focused&&ui.ctx().input(|i|i.key_pressed(egui::Key::Escape)) {self.selected=false;self.state.pinned=false;}
        if focused&&ui.ctx().input(|i|i.pointer.any_pressed()) {self.selected=over_card;}
        // A pending hover needs continuous presence. Only an already visible
        // card gets the bridge grace period; otherwise it can open after exit
        // and flash briefly before the dismissal timer expires.
        if self.bounds.is_none()&&!over_source&&!self.state.pinned {
            self.since=None;self.dismiss_at=None;
            return false;
        }
        if over_source || over_card || self.selected || self.state.pinned {
            self.dismiss_at=None;
            self.since.get_or_insert(now);
        } else if self.since.is_some() {
            let deadline=*self.dismiss_at.get_or_insert(now+0.18);
            if now>=deadline {
                self.since=None;self.bounds=None;self.selected=false;self.dismiss_at=None;
                return false;
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(deadline-now));
        }
        let Some(since) = self.since else {
            return false;
        };
        if !self.state.pinned&&self.bounds.is_none() && now - since < 0.25 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(
                    (0.25 - (now - since)).max(0.),
                ));
            return false;
        }
        let bounds = details_at_with_state(ui, m, view, source.rect, self.bounds,&mut self.state,appearance);
        self.bounds = Some(bounds);
        focused&&pointer.is_some_and(|p| bounds.contains(p))
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
    fn diagnostic_time_selection_changes_the_chart_window() {
        let ctx=egui::Context::default();let mut state=DetailState::default();let mut time=0.;
        let mut frame=|events| {
            time+=0.05;
            let output=ctx.run(egui::RawInput{time:Some(time),screen_rect:Some(Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.))),events,..Default::default()},|ctx|{
                egui::CentralPanel::default().show(ctx,|ui|detail_contents(ui,&Metrics::default(),1100.,500.,&mut state));
            });
            output.shapes.into_iter().filter_map(|s|if let egui::Shape::Text(t)=s.shape {Some((t.galley.job.text.clone(),t.pos+t.galley.size()/2.))}else{None}).collect::<std::collections::HashMap<_,_>>()
        };
        let mut labels=frame(vec![]);for _ in 0..3{labels=frame(vec![]);}
        for (button,title,axis) in [("10 min","最近 10 分钟","-10m"),("60 min","最近 60 分钟","-60m"),("3 min","最近 3 分钟","-3m")] {
            let point=labels[button];frame(vec![egui::Event::PointerMoved(point)]);
            frame(vec![egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
            labels=frame(vec![egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            assert!(labels.contains_key(title)&&labels.contains_key(axis),"{button} must change chart title and axis: {labels:?}");
        }
    }
    #[test]
    fn appearance_changes_apply_to_cached_layout_and_keep_hud_inside_the_picture() {
        let ctx=egui::Context::default();let view=Rect::from_min_size(egui::pos2(0.,0.),egui::vec2(1920.,1080.));
        let _=ctx.run(egui::RawInput{screen_rect:Some(view),..Default::default()},|ctx|{
            egui::CentralPanel::default().show(ctx,|ui|{
                let layout=Layout::new(ui,&Metrics::default(),view.width()-32.);
                for position in [0.,0.5,1.,HudAppearance::default().vertical_position] {
                    let appearance=HudAppearance{vertical_position:position,..Default::default()};
                    let bounds=layout.paint(ui,view,appearance).rect;
                    assert!((bounds.top()-(view.height()*position).min(view.height()-bounds.height())).abs()<0.01);
                    assert!(view.contains_rect(bounds));
                    if position==HudAppearance::default().vertical_position {assert!((bounds.top()-36.).abs()<0.01);}
                }
            });
        });
        for opacity in [0.,0.2,0.75,1.] {
            let output=ctx.run(egui::RawInput{screen_rect:Some(view),..Default::default()},|ctx|{
                egui::CentralPanel::default().show(ctx,|ui|{
                    Layout::new(ui,&Metrics::default(),view.width()-32.).paint(ui,view,HudAppearance{opacity,..Default::default()});
                });
            });
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Rect(r) if r.fill==Color32::from_black_alpha((opacity*255.).round() as u8))));
        }
    }
    #[test]
    fn hover_exit_closes_once_without_reappearing_and_reentry_starts_a_new_delay() {
        for size in [egui::vec2(1280.,720.),egui::vec2(640.,360.)] {
            let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,size);
            let mut card=HoverCard::default();let mut now=0.;
            let m=Metrics{size:[1920,1080],target_fps:60,history:(0..8).map(|i|HistorySample{seconds:i as f64,values:[Some(1.);7],video_uncertainty_ms:Some(0.1),estimated_total:None,input_frame:None}).collect(),..Default::default()};
            let frame=|card:&mut HoverCard,now:f64,p:egui::Pos2| {
                ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),events:vec![egui::Event::PointerMoved(p)],..Default::default()},|ctx|{
                    egui::CentralPanel::default().show(ctx,|ui|{
                        let source=ui.interact(Rect::from_min_size(egui::pos2(12.,12.),egui::vec2(160.,24.)),egui::Id::new("source"),egui::Sense::hover());
                        card.show(ui,&source,&m,view,HudAppearance::default());
                    });
                })
            };
            // Leaving before the opening delay must never finish that pending
            // opening during the grace interval.
            for _ in 0..4 {now+=0.05;frame(&mut card,now,egui::pos2(30.,20.));}
            for _ in 0..40 {
                now+=1./120.;frame(&mut card,now,egui::pos2(size.x-2.,size.y-2.));
                assert!(card.bounds.is_none(),"pending hover opened after exit");
            }
            for _ in 0..12 {now+=0.05;frame(&mut card,now,egui::pos2(30.,20.));}
            let bounds=card.bounds.unwrap();
            // Move through the toolbar, chart and scrollable data, then back
            // across the small bridge to the HUD. None of these closes it.
            for p in [bounds.min+egui::vec2(40.,15.),bounds.center(),bounds.right_center()-egui::vec2(30.,0.),egui::pos2(30.,38.),egui::pos2(30.,20.)] {
                now+=0.03;frame(&mut card,now,p);assert!(card.bounds.is_some());
            }
            let mut hidden=false;let mut visible_frames=0;
            for _ in 0..100 {
                now+=1./120.;
                let output=frame(&mut card,now,egui::pos2(size.x-2.,size.y-2.));
                let visible=output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="串流诊断"));
                if visible {assert!(!hidden,"card flashed after it had closed");visible_frames+=1;}else{hidden=true;}
            }
            assert!(hidden&&visible_frames>0);assert!(card.bounds.is_none());
            now+=0.01;frame(&mut card,now,egui::pos2(30.,20.));assert!(card.bounds.is_none());
            for _ in 0..8 {now+=0.05;frame(&mut card,now,egui::pos2(30.,20.));}
            assert!(card.bounds.is_some());
        }
    }
    #[test]
    fn compact_network_uses_interface_throughput_and_estimate_requires_all_components() {
        let mut m=Metrics{bitrate:Some(99.),link:Some(uu_core::link_metrics::Snapshot{rx_mbps:Some(12.),tx_mbps:Some(0.4),ping_ms:None}),rtt:Some([1.5,0.32]),loss:Some(0.),process:[Some(1.);4],queues:[Some(0.);3],..Default::default()};
        assert_eq!(m.network_label(),"BW 12.4 Mbps · RTT 1.5 ± 0.32 ms · Loss 0.00%");
        assert!(m.stages_label().ends_with("Est. ≈4.75 ±— ms"));
        m.queues[2]=None;assert!(m.stages_label().ends_with("Est. —"));
        m.link.as_mut().unwrap().tx_mbps=None;assert!(m.network_label().starts_with("BW — Mbps"));
        let rows=detail_rows(&m);
        assert_eq!(rows.iter().take(6).map(|r|r.label).collect::<Vec<_>>(),["画面","帧率","编解码","网络","延迟","端到端"]);
        let latency=&rows[4].values;assert_eq!(latency.iter().filter(|v|v.line==1).count(),5);
    }
    #[test]
    fn toolbar_buttons_pin_switch_window_and_copy_through_real_pointer_events() {
        let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));
        let mut state=DetailState::default();let mut previous=None;let mut now=1.;
        let mut frame=|state:&mut DetailState,events:Vec<egui::Event>| {
            now+=0.05;
            ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),events,..Default::default()},|ctx|{
                egui::CentralPanel::default().show(ctx,|ui|{previous=Some(details_at_with_state(ui,&Metrics::default(),view,Rect::from_min_size(egui::pos2(20.,28.),egui::vec2(500.,24.)),previous,state,HudAppearance::default()));});
            })
        };
        let mut output=frame(&mut state,vec![]);for _ in 0..3{output=frame(&mut state,vec![]);}
        for label in ["固定","10 min","复制诊断"] {
            let point=output.shapes.iter().find_map(|s|if let egui::Shape::Text(t)=&s.shape {(t.galley.job.text==label).then_some(t.pos+t.galley.size()/2.)}else{None}).unwrap();
            frame(&mut state,vec![egui::Event::PointerMoved(point)]);
            frame(&mut state,vec![egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
            output=frame(&mut state,vec![egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            if label=="固定"{assert!(state.pinned);}
            if label=="10 min"{assert_eq!(state.window,1);}
            if label=="复制诊断"{assert!(output.platform_output.copied_text.contains("统计窗口 10 分钟"));}
        }
    }
    #[test]
    fn pinned_card_survives_pointer_leave_and_releases_keyboard_on_picture_or_focus_loss() {
        let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));
        let mut card=HoverCard::default();card.state.pinned=true;let mut now=0.;
        let mut frame=|card:&mut HoverCard,focused,pressed| {
            now+=0.3;let point=egui::pos2(1270.,710.);
            let mut events=vec![egui::Event::PointerMoved(point)];
            if pressed {events.push(egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()});}
            ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),focused,events,..Default::default()},|ctx|{
                egui::CentralPanel::default().show(ctx,|ui|{
                    let source=ui.interact(Rect::from_min_size(egui::pos2(12.,12.),egui::vec2(160.,24.)),egui::Id::new("source"),egui::Sense::hover());
                    card.show(ui,&source,&Metrics::default(),view,HudAppearance::default());
                });
            })
        };
        for _ in 0..4{frame(&mut card,true,false);}
        assert!(card.bounds.is_some());assert!(!card.keyboard_owned());
        card.selected=true;frame(&mut card,true,true);
        assert!(!card.keyboard_owned());assert!(card.state.pinned);assert!(card.bounds.is_some());
        card.selected=true;let output=frame(&mut card,false,false);
        assert!(!card.keyboard_owned());assert!(card.bounds.is_some());assert!(card.state.pinned);
        assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="串流诊断")),"pinned content must still be painted without focus");
        for _ in 0..6 {frame(&mut card,false,false);assert!(card.bounds.is_some());assert!(!card.keyboard_owned());}
        frame(&mut card,true,false);assert!(card.bounds.is_some());assert!(!card.keyboard_owned(),"focus return must not reclaim keyboard selection");
    }
    #[test]
    fn report_copies_the_selected_counter_window_and_distinguishes_period_from_latency() {
        let mut m=Metrics{target_fps:30,statistics_coverage:[180.,270.,270.],..Default::default()};
        m.counters.nack=999;m.statistics[0].nack=2;m.statistics[1].nack=7;
        let short=m.diagnostic_text(0);let long=m.diagnostic_text(1);
        assert!(short.contains("NACK 2"));assert!(long.contains("NACK 7"));assert!(!long.contains("NACK 999"));
        assert!(long.contains("33.33 ms"));assert!(long.contains("Est.Total —"));assert!(long.contains("视觉响应 —"));
        assert!(!long.contains("127.0.0.1"));
    }
    #[test]
    fn clicking_detail_owns_keyboard_until_outside_escape_or_focus_loss() {
        let ctx=egui::Context::default();let mut card=HoverCard::default();
        let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));let mut now=0.;
        let mut frame=|card:&mut HoverCard,events:Vec<egui::Event>,focused:bool| {
            now+=0.2;
            let _=ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),events,focused,..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|{
                let source=ui.interact(Rect::from_min_size(egui::pos2(12.,12.),egui::vec2(160.,24.)),egui::Id::new("source"),egui::Sense::hover());
                card.show(ui,&source,&Metrics::default(),view,HudAppearance::default());
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
        let latency=&rows.iter().find(|r|r.label=="延迟").unwrap().values;
        assert_eq!(latency.iter().filter(|v|v.line==0&&v.group==1).count(),4);
        assert_eq!(latency.iter().filter(|v|v.line==0&&v.group==2).count(),2);
        for width in [1280.,640.,320.] {
            let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,720.));
            if let Ok(path)=std::env::var("ECLIPSE_HUD_TEST_FONT") {
                let mut fonts=egui::FontDefinitions::default();
                fonts.font_data.insert("cjk".into(),egui::FontData::from_owned(std::fs::read(path).unwrap()));
                fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0,"cjk".into());
                ctx.set_fonts(fonts);
            }
            let mut checked=0;
            for _ in 0..3 {
                let output=ctx.run(egui::RawInput{screen_rect:Some(view),..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|details(ui,&m));});
                let texts:Vec<_>=output.shapes.iter().filter_map(|s|if let egui::Shape::Text(t)=&s.shape {Some(t)}else{None}).collect();
                assert!(texts.iter().any(|t|t.galley.job.text=="串流诊断"));
                checked=0;
                for row in &rows {
                    let first=&row.values[0];let value=format!("{} {}",first.label,first.value).trim().to_owned();
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
            for frame in 0..70 {
                let mut events=Vec::new();
                if frame>=3 {
                    if let Some(bounds)=bounds {
                        events.push(egui::Event::PointerMoved(bounds.min+egui::vec2(40.,80.)));
                        if frame<64 {events.push(egui::Event::MouseWheel {unit:egui::MouseWheelUnit::Point,delta:egui::vec2(0.,-60.),modifiers:Default::default()});}
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
        assert_eq!(m.stage_sum_label(), "Est.Total ≈14.80 ±— ms");
        assert_eq!(m.total_label(), "端到端 — · 未知");
        assert_eq!(m.video_mean_ms(),None);
        assert_eq!(m.video_uncertainty_ms(),None);
        let partial = Metrics {
            process: [None, Some(0.), None, None],
            ..Default::default()
        };
        assert_eq!(partial.estimate().samples, 1);
        assert!(!partial.estimate().missing.is_empty());
        assert_eq!(partial.stage_sum_label(),"Est.Total —");
        assert_eq!(partial.total_label(), "端到端 — · 未知");
        assert_eq!(Metrics::default().stage_sum_label(), "Est.Total —");
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
            assert_eq!(m.total_label(),"端到端 ≈21.16 ±1.00 ms · 系统显示回执");
            assert_eq!(m.input_response_label(),"输入→后继画面 — · 注入确认 2.2 ms");
        });
        uu_i18n::with_language(uu_i18n::Language::English,||{
            assert_eq!(m.total_label(),"E2E ≈21.16 ±1.00 ms · System display receipt");
            assert_eq!(m.input_response_label(),"Input→Subsequent frame — · Injection ack 2.2 ms");
            let rows=detail_rows(&m);
            assert!(rows.iter().any(|row|row.label=="Latency"));
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
            let values=&rows.iter().find(|row|row.label=="端到端").unwrap().values;
            assert!(values.iter().any(|v|v.help.contains("多显示器")&&v.help.contains("经验估测")));
            assert!(values.iter().any(|v|v.help.contains("不是平台保证的硬界")));
        });
        uu_i18n::with_language(uu_i18n::Language::English,||{
            assert!(m.total_label().ends_with("DXGI receipt estimate"));
            let rows=detail_rows(&m);
            let values=&rows.iter().find(|row|row.label=="E2E").unwrap().values;
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
            (uu_i18n::Language::Chinese,"端到端","历史淘汰","单帧最大半宽","源/显/观测丢弃"),
            (uu_i18n::Language::English,"E2E","History evictions","Max frame half-width","Src/disp/obs drops")
        ] {
            uu_i18n::with_language(language,||{
                assert!(m.total_label().contains("≈15.00 ±5.00 ms"));
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
                assert_eq!(m.total_label(),format!("E2E — · {label}"));
                assert_eq!(m.video_mean_ms(),None);
            }
            let zero=Measurement {samples:1,mean_us:0,p50_us:0,p95_us:0,uncertainty_us:0,max_sample_uncertainty_us:0,method:DisplayMethod::RefreshInterval};
            let m=Metrics {video_e2e:Some(e2e_snapshot(Some(zero),None)),..Default::default()};
            assert_eq!(m.video_mean_ms(),Some(0.));
            assert_eq!(m.total_label(),"E2E ≈0.00 ±0.00 ms · Refresh interval");
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
        assert_eq!(m.fps_label(), "FPS · Rcv/Dec/Rnd 60.0/59.9/59.7");
        assert_eq!(m.stages_label(), "Latency · Cap+Enc/Dec+Rnd 9.2/2.4 ms · Est. —");
    }
    #[test]
    fn layouts_fit_wide_and_narrow_viewports_without_overlapping_groups() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                {
                    for width in [240., 640., 1600.] {
                        let m = Metrics {
                            size: [3840, 2160],
                            fps: [Some(120.); 3],
                            codec: "PYROWAVE".into(),
                            ..Default::default()
                        };
                        let layout = Layout::new(ui, &m, width);
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
