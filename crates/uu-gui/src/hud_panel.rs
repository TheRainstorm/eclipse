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
            "Enc/Dec/Rnd {}/{}/{} ms",
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
                (format!("Bandwidth {rate}"), 14., color),
                (m.network_label(), 14., color),
                (m.stages_label(), 14., color),
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
struct DetailValue { label: &'static str, value: String, help: &'static str, group: u8 }
struct DetailRow { label: &'static str, text: String, color: Color32, values: Vec<DetailValue> }
fn field(label:&'static str,value:impl ToString,help:&'static str)->DetailValue {DetailValue{label,value:value.to_string(),help,group:0}}
fn row(label:&'static str,color:Color32,values:Vec<DetailValue>)->DetailRow {
    DetailRow{label,text:values.iter().map(|v|format!("{} {}",v.label,v.value)).collect::<Vec<_>>().join(" · "),color,values}
}
fn detail_rows(m:&Metrics)->Vec<DetailRow> {
    let plain=Color32::from_rgb(216,224,232);let cyan=Color32::from_rgb(0,235,214);
    let green=Color32::from_rgb(143,225,184);let blue=Color32::from_rgb(139,194,255);let orange=Color32::from_rgb(255,190,108);
    let pct=|v:Option<f32>|valid(v).map_or_else(||"—".into(),|v|format!("{v:.2}%"));let c=&m.counters;
    let e=m.estimate();let total=if e.samples==0{"—".into()}else{format!("≈{} ms",number(Some(e.known)))};
    let mut latency=vec![field(if e.missing.is_empty(){uu_i18n::text("预估")}else{uu_i18n::text("已知小计")},total,uu_i18n::text("已知阶段窗口均值之和。包含采集、编码、解码、渲染、RTT/2及排队；不含应用响应、合成与扫描，不能当作实测端到端延迟。"))];
    for (i,(label,help)) in [(uu_i18n::text("采"),uu_i18n::text("主机获取本帧桌面图像的时间。")),(uu_i18n::text("编"),uu_i18n::text("主机编码调用耗时，可包含硬件队列等待。")),(uu_i18n::text("解"),uu_i18n::text("客户端解码调用耗时。")),(uu_i18n::text("渲"),uu_i18n::text("客户端UI准备与渲染提交的CPU墙钟时间，可能包含驱动等待；不是GPU计时或扫描延迟。"))].into_iter().enumerate() {let mut v=field(label,format!("{} ms",number(m.process[i])),help);v.group=1;latency.push(v);}
    for (label,value,help) in [(uu_i18n::text("网"),m.rtt.map(|v|v[0]/2.),uu_i18n::text("RTT的一半，仅在往返对称时近似单程。RTT还可能包含应用调度、处理与队列等待。")),(uu_i18n::text("排"),sum(&m.queues),uu_i18n::text("主机发送等待、客户端解码队列和画面邮箱等待的合计。未知阶段不按零计算。"))]{let mut v=field(label,format!("{} ms",number(value)),help);v.group=2;latency.push(v);}
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
    vec![
        row(uu_i18n::text("画面"),cyan,vec![field("",format!("{}×{}@{}",m.size[0],m.size[1],m.target_fps),uu_i18n::text("当前视频尺寸与目标帧率；不会改变远端桌面模式。")),field(uu_i18n::text("编码"),if m.codec.is_empty(){"—"}else{&m.codec},uu_i18n::text("实际协商的编码与色彩模式，区别于请求偏好。")),field(uu_i18n::text("解码器"),decoder_label(&m.backend),uu_i18n::text("客户端当前实际使用的解码后端。")),field(uu_i18n::text("码率"),format!("{} Mbps",number(m.bitrate)),uu_i18n::text("近期接收到的视频码率；不是目标码率或总链路带宽。"))]),
        row(uu_i18n::text("帧率"),cyan,fps),row(uu_i18n::text("延迟"),green,latency),
        row(uu_i18n::text("网络"),if m.loss.is_some_and(|v|v>1.){orange}else{blue},network),
        row(uu_i18n::text("排队"),green,vec![field(uu_i18n::text("发"),format!("{} ms",number(m.queues[0])),uu_i18n::text("主机发送视频包时等待传输API完成的时间，可能包含套接字背压和调度。")),field(uu_i18n::text("解"),format!("{} ms",number(m.queues[1])),uu_i18n::text("完整帧进入解码工作队列到开始解码的等待。")),field(uu_i18n::text("邮箱"),format!("{} ms",number(m.queues[2])),uu_i18n::text("解码画面发布到UI领取之间的等待；受UI唤醒、绘制预算和负载影响。")),field(uu_i18n::text("合计"),format!("{} ms",number(sum(&m.queues))),uu_i18n::text("发送、解码、邮箱等待的均值之和；缺样本时显示未知。"))]),
        row(uu_i18n::text("恢复累计"),plain,vec![field(uu_i18n::text("FEC恢复/冗余"),format!("{}/{}",c.fec_recovered,c.fec_packets),uu_i18n::text("FEC恢复的包数与收到的冗余包数，会话累计。")),field(uu_i18n::text("冗余量"),format!("{:.2} MiB",c.fec_bytes as f64/1048576.),uu_i18n::text("会话收到的FEC冗余数据量。")),field(uu_i18n::text("FEC丢弃"),c.fec_drops,uu_i18n::text("无法使用或被丢弃的FEC数据。")),field("RTX",c.rtx,uu_i18n::text("收到的重传包数。")),field("NACK",c.nack,uu_i18n::text("发送的缺包重传请求数。"))]),
        row(uu_i18n::text("异常累计"),if c.errors+c.damaged+c.queue_drops>0{orange}else{plain},vec![field(uu_i18n::text("未恢复包"),c.lost,uu_i18n::text("会话累计超时未恢复包。")),field(uu_i18n::text("损帧"),c.damaged,uu_i18n::text("接收器丢弃的不完整或损坏视频帧。")),field(uu_i18n::text("解码错"),c.errors,uu_i18n::text("解码后端返回的错误次数。")),field(uu_i18n::text("队列丢帧"),c.queue_drops,uu_i18n::text("解码队列为保留新鲜画面而丢弃的帧数。")),field(uu_i18n::text("刷新"),c.keyframes,uu_i18n::text("请求关键帧的次数。")),field(uu_i18n::text("乱/迟"),format!("{}/{}",c.reordered,c.late),uu_i18n::text("修复接收器记录的乱序与迟到次数。"))]),
        row(uu_i18n::text("输入"),plain,vec![field(uu_i18n::text("状态"),if m.input_ready{uu_i18n::text("已就绪")}else{uu_i18n::text("未就绪")},uu_i18n::text("当前窗口、主机授权和本地输入归属共同决定是否能远程输入。")),field(uu_i18n::text("发/错"),format!("{}/{}",m.input_counts[0],m.input_counts[1]),uu_i18n::text("客户端发送的输入消息数和发送错误数。")),field(uu_i18n::text("排队/确认/主机队列/注入"),m.input_timing.map(|v|number(v)).join("/"),uu_i18n::text("输入在客户端的排队、确认往返、主机队列与系统注入耗时，单位毫秒；确认不代表画面响应。"))]),
        row(uu_i18n::text("收包"),plain,receive),
        row(uu_i18n::text("恢复原因"),plain,vec![field(uu_i18n::text("接收/排队/解码/格式"),uu_i18n::format!("{}/{}/{}/{} 次",c.recovery[0],c.recovery[1],c.recovery[2],c.recovery[3]),uu_i18n::text("分别由接收损坏、解码队列溢出、解码错误和格式改变触发的恢复次数。"))]),
    ]
}
fn detail_text(m:&Metrics)->String {detail_rows(m).iter().map(|r|format!("{}  {}",r.label,r.text)).collect::<Vec<_>>().join("\n")}
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
    ui.horizontal(|ui|{ui.strong(uu_i18n::text("串流诊断"));ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
        if ui.small_button(uu_i18n::text("复制详情")).on_hover_text(uu_i18n::text("复制当前全部诊断数值。也可拖选单个数值后按Ctrl+C；点击画面恢复远程键盘。")).clicked(){ui.ctx().copy_text(detail_text(m));}
    });});
    let remaining=(max_height-27.).max(1.);let chart_width=(width*0.37).clamp(90.,400.);let data_width=(width-chart_width-18.).max(1.);
    ui.horizontal_top(|ui|{
        ui.allocate_ui_with_layout(egui::vec2(data_width,remaining),egui::Layout::top_down(egui::Align::Min),|ui|{
            egui::ScrollArea::vertical().id_salt("hud-detail-scroll").max_height(remaining).min_scrolled_height(0.).auto_shrink([false,true]).show(ui,|ui|{
                let rows=detail_rows(m);
                let title_width=rows.iter().map(|r|ui.fonts(|fonts|fonts.layout_no_wrap(r.label.into(),FontId::proportional(12.),r.color).size().x)).fold(50.,f32::max).min(110.).min(data_width*0.30);
                for row in rows{
                    ui.horizontal_top(|ui|{
                        ui.add_sized([title_width,16.],egui::Label::new(RichText::new(row.label).size(12.).color(row.color).strong()));
                        ui.vertical(|ui|{ui.set_width((data_width-title_width-7.).max(1.));ui.horizontal_wrapped(|ui|{
                            let mut group=0;
                            for v in row.values {
                                if v.group!=group&&group>0{ui.add(egui::Separator::default().vertical().spacing(6.));}group=v.group;
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
    fn details_offer_explained_fields_and_copy_all_values() {
        let m=Metrics {codec:"PYROWAVE SDR".into(),process:[Some(1.);4],rtt:Some([2.,0.2]),loss:Some(0.4),unrecovered_loss:Some(0.),..Default::default()};
        let rows=detail_rows(&m);
        assert!(rows.iter().flat_map(|r|&r.values).all(|v|!v.help.is_empty()));
        let latency=&rows.iter().find(|r|r.label=="延迟").unwrap().values;
        assert_eq!(latency.iter().filter(|v|v.group==1).count(),4);
        assert_eq!(latency.iter().filter(|v|v.group==2).count(),2);
        let text=detail_text(&m);assert!(text.contains("链路丢包 0.40%"));assert!(text.contains("修复后丢包 0.00%"));
        let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));
        let mut button=None;
        for _ in 0..3 {let output=ctx.run(egui::RawInput{screen_rect:Some(view),..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|details(ui,&m));});
            for shape in output.shapes {if let egui::Shape::Text(t)=shape.shape {if t.galley.job.text=="复制详情"{button=Some(Rect::from_min_size(t.pos,t.galley.size()).center());}}}
        }
        let point=button.unwrap();
        let frame=|pressed|ctx.run(egui::RawInput{screen_rect:Some(view),events:vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}],..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|details(ui,&m));});
        frame(true);let copied=frame(false);
        assert_eq!(copied.platform_output.copied_text,text);
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
