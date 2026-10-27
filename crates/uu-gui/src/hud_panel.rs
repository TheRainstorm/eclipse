//! Presentation shared by the live HUD and the isolated visual probe.
use eframe::egui::{self, Color32, FontId, Rect, RichText};
use std::sync::Arc;
use uu_core::client_settings::HudDetail;

#[derive(Clone,Default)]
pub struct Metrics {
    pub size:[u32;2],
    pub fps:[Option<f32>;3],
    pub target_fps:u32,
    pub source_hz:Option<u32>,
    pub codec:String,
    pub mode:String,
    pub backend:String,
    pub bitrate:Option<f32>,
    pub rtt:Option<[f32;2]>,
    pub rtt_distribution:Option<[f64;5]>,
    pub loss:Option<f32>,
    pub unrecovered_loss:Option<f32>,
    pub network:Option<uu_core::network_loss::Snapshot>,
    // Capture, encode, decode, UI preparation/render submission.
    pub process:[Option<f32>;4],
    // Producer send wait, decoder queue, mailbox-to-UI delivery.
    pub queues:[Option<f32>;3],
    pub counters:Counters,
    pub input_ready:bool,
    pub input_counts:[u64;2],
    // Client queue, confirmation RTT, host queue, injection.
    pub input_timing:[Option<f32>;4],
    pub waiting_for_keyframe:bool,
    pub toggle_hint:String,
}
#[derive(Clone,Default)]
pub struct Counters {
    pub lost:u64,pub nack:u64,pub rtx:u64,pub reordered:u64,pub late:u64,
    pub damaged:u64,pub errors:u64,pub queue_drops:u64,pub keyframes:u64,
    pub fec_packets:u64,pub fec_recovered:u64,pub fec_bytes:u64,pub fec_drops:u64,
    pub recovery:[u64;4],
}
#[derive(Debug)]
pub struct Estimate {pub known:f32,pub missing:Vec<&'static str>,pub samples:usize}
fn valid(value:Option<f32>)->Option<f32> {value.filter(|v|v.is_finite()&&*v>=0.)}
fn sum(values:&[Option<f32>])->Option<f32> {values.iter().copied().map(valid).sum()}
fn number(value:Option<f32>)->String {
    valid(value).map_or_else(||"—".into(),|v|if v<1. {format!("{v:.2}")}else{format!("{v:.1}")})
}
impl Metrics {
    pub fn estimate(&self)->Estimate {
        let parts=[("cap",self.process[0]),("enc",self.process[1]),("dec",self.process[2]),("rnd",self.process[3]),
            ("net",self.rtt.map(|v|v[0]/2.)),("send queue",self.queues[0]),("decode queue",self.queues[1]),("mailbox",self.queues[2])];
        let mut result=Estimate{known:0.,missing:Vec::new(),samples:0};
        for (name,value) in parts {if let Some(v)=valid(value){result.known+=v;result.samples+=1;}else{result.missing.push(name);}}
        result
    }
    pub fn total_label(&self)->String {
        let e=self.estimate();
        if e.samples==0 {"Est. Total —".into()}
        else if e.missing.is_empty(){format!("Est. Total ≈{} ms",number(Some(e.known)))}
        else{format!("Known ≈{} ms",number(Some(e.known)))}
    }
    pub fn fps_label(&self)->String {
        let values=self.fps.map(|v|valid(v).map_or_else(||"—".into(),|v|format!("{v:.1}")));
        format!("FPS[recv/dec/rnd] {}/{}/{}",values[0],values[1],values[2])
    }
    fn network_label(&self)->String {
        let rtt=self.rtt.map_or_else(||"—".into(),|v|format!("{} ± {} ms",number(Some(v[0])),number(Some(v[1]))));
        let loss=valid(self.loss).map_or_else(||"—".into(),|v|format!("{v:.2}%"));
        format!("RTT {rtt} · Loss {loss}")
    }
    fn stages_label(&self)->String {
        format!("enc/dec/rnd {}/{}/{} ms",number(self.process[1]),number(self.process[2]),number(self.process[3]))
    }
}
struct Placed {offset:egui::Vec2,galley:Arc<egui::Galley>}
pub struct Layout {placed:Vec<Placed>,pub size:egui::Vec2,compact:bool}
impl Layout {
    pub fn new(ui:&egui::Ui,m:&Metrics,mode:HudDetail,width:f32)->Self {
        let primary=Color32::from_rgb(239,245,252);
        let muted=Color32::from_rgb(181,198,218);
        let network=if m.loss.is_some_and(|v|v>1.) {Color32::from_rgb(255,195,108)}else{Color32::from_rgb(154,202,255)};
        let process=Color32::from_rgb(159,231,204);
        let codec=if m.codec.is_empty(){"—"}else{&m.codec};
        let resolution=if m.size.contains(&0){"—".into()}else{format!("{}x{}@{}",m.size[0],m.size[1],m.target_fps)};
        let video=format!("{resolution} · {}",m.fps_label());
        let rate=valid(m.bitrate).map_or_else(||"— Mbps".into(),|v|format!("{v:.1} Mbps"));
        let mut rows:Vec<Vec<(String,f32,Color32)>>=vec![vec![(video,15.,primary),(format!("{codec} · {rate}"),14.,primary)]];
        if mode==HudDetail::Compact {
            let color=Color32::from_rgb(0,235,214);
            rows[0]=vec![(resolution,14.,color),(codec.into(),14.,color),(m.fps_label(),14.,color),
                (m.network_label(),14.,color),(format!("Bandwidth {rate}"),14.,color),
                (m.stages_label(),14.,color),(m.total_label(),14.,color)];
        }else{
            let mode=if m.mode.is_empty(){String::new()}else{format!(" / {}",m.mode)};
            rows[0][1].0=format!("{codec}{mode} · {rate}");
            rows[0].push((decoder_label(&m.backend).into(),12.5,muted));
            rows.push(vec![(m.network_label(),13.,network),(format!("{} = Proc {} + Net ≈{} + Queue {} ms",m.total_label(),
                number(sum(&m.process)),number(m.rtt.map(|v|v[0]/2.)),number(sum(&m.queues))),13.,primary)]);
            rows.push(vec![(format!("Process  cap/enc/dec/rnd {}/{}/{}/{} ms",number(m.process[0]),number(m.process[1]),number(m.process[2]),number(m.process[3])),13.,process)]);
            rows.push(vec![(format!("Queue  send/dec/mailbox {}/{}/{} ms",number(m.queues[0]),number(m.queues[1]),number(m.queues[2])),12.5,muted),
                (format!("Target {} FPS{}",m.target_fps,m.source_hz.map_or_else(String::new,|hz|format!(" · Source {hz} Hz"))),12.5,muted)]);
            let c=&m.counters;
            if c.errors+c.damaged+c.queue_drops>0 {rows.push(vec![(format!("Session warnings  damaged {} · dec errors {} · queue drops {}",c.damaged,c.errors,c.queue_drops),12.5,Color32::from_rgb(255,195,108))]);}
        }
        if m.waiting_for_keyframe {rows.push(vec![("Recovering reference frame…".into(),12.5,Color32::from_rgb(255,195,108))]);}
        let width=width.max(1.);let mut placed=Vec::new();let mut x=0.;let mut y=0.;let mut row_h=0f32;let mut max_x=0f32;
        for (line,row) in rows.into_iter().enumerate() {
            if line>0 {x=0.;y+=row_h+6.;row_h=0.;}
            for (text,size,color) in row {
                let mut job=egui::text::LayoutJob::simple(text,FontId::proportional(size),color,width);
                job.wrap.break_anywhere=false;
                let galley=ui.fonts(|f|f.layout_job(job));
                let gap=if x>0. {16.}else{0.};
                if x>0.&&x+gap+galley.size().x>width {x=0.;y+=row_h+5.;row_h=0.;}
                else{x+=gap;}
                row_h=row_h.max(galley.size().y);max_x=max_x.max(x+galley.size().x);
                placed.push(Placed{offset:egui::vec2(x,y),galley:galley.clone()});x+=galley.size().x;
            }
        }
        Self{placed,size:egui::vec2(max_x,y+row_h),compact:mode==HudDetail::Compact}
    }
    pub fn paint(&self,ui:&egui::Ui,video:Rect)->egui::Response {
        let pad=if self.compact{egui::vec2(8.,5.)}else{egui::vec2(12.,9.)};
        let bounds=Rect::from_min_size(egui::pos2(video.center().x-self.size.x/2.-pad.x,video.top()+4.),self.size+pad*2.);
        ui.painter().rect_filled(bounds,if self.compact{0.}else{8.},Color32::from_black_alpha(205));
        if !self.compact{ui.painter().rect_stroke(bounds,8.,egui::Stroke::new(0.7,Color32::from_white_alpha(35)));}
        for p in &self.placed {ui.painter().galley(bounds.min+pad+p.offset,p.galley.clone(),Color32::WHITE);}
        ui.interact(bounds,ui.id().with("hud-details"),egui::Sense::hover())
    }
}
fn heading(ui:&mut egui::Ui,title:&str,subtitle:&str) {
    ui.add_space(5.);
    ui.horizontal(|ui|{ui.strong(title);ui.weak(subtitle);});
    ui.separator();
}
fn pair(ui:&mut egui::Ui,label:&str,value:impl ToString) {ui.label(label);ui.monospace(value.to_string());}
pub fn details(ui:&mut egui::Ui,m:&Metrics) {
    ui.set_max_width(540.);
    heading(ui,"Pipeline","window means · ms");
    ui.label(RichText::new(m.total_label()).strong());
    egui::Grid::new("hud-pipeline").spacing([14.,3.]).show(ui,|ui| {
        pair(ui,"cap / enc",format!("{} / {}",number(m.process[0]),number(m.process[1])));
        pair(ui,"dec / rnd",format!("{} / {}",number(m.process[2]),number(m.process[3])));ui.end_row();
        pair(ui,"send / dec queue",format!("{} / {}",number(m.queues[0]),number(m.queues[1])));
        pair(ui,"mailbox",number(m.queues[2]));ui.end_row();
    });
    let e=m.estimate();
    if !e.missing.is_empty(){ui.weak(format!("Missing: {}. Known is a partial subtotal.",e.missing.join(", ")));}
    heading(ui,"Network & repair","session totals");
    ui.label(m.network_label());
    if let Some(n)=&m.network {
        ui.weak(format!("Original RTP: recv {} · lost {} · reorder {} · dup {} · late {} · resync {}",n.original_received,n.original_lost,n.reordered,n.duplicates,n.late,n.resyncs));
    }
    ui.weak(format!("After repair: {}% unrecovered",valid(m.unrecovered_loss).map_or_else(||"—".into(),|v|format!("{v:.2}"))));
    if let Some(v)=m.rtt_distribution {ui.weak(format!("RTT last/min/median/P95/max: {:.1}/{:.1}/{:.1}/{:.1}/{:.1} ms",v[0],v[1],v[2],v[3],v[4]));}
    let c=&m.counters;
    egui::Grid::new("hud-counters").striped(true).spacing([14.,3.]).show(ui,|ui| {
        for (a,av,b,bv) in [("Unrecovered",c.lost,"NACK",c.nack),("RTX",c.rtx,"Repair reorder",c.reordered),
            ("FEC parity",c.fec_packets,"FEC recovered",c.fec_recovered),("FEC discarded",c.fec_drops,"Late",c.late),
            ("Damaged frames",c.damaged,"Dec errors",c.errors),("Queue drops",c.queue_drops,"Refresh requests",c.keyframes)] {
            pair(ui,a,av);pair(ui,b,bv);ui.end_row();
        }
    });
    ui.weak(format!("FEC payload {:.2} MiB · Recovery rx/queue/decoder/format: {}/{}/{}/{}",c.fec_bytes as f64/1048576.,c.recovery[0],c.recovery[1],c.recovery[2],c.recovery[3]));
    heading(ui,"Input","injection feedback");
    ui.label(format!("{} · sent {} · send errors {}",if m.input_ready{"Ready"}else{"Unavailable"},m.input_counts[0],m.input_counts[1]));
    ui.weak(format!("Client queue {} · Ack RTT {} · Host queue {} · Inject {} ms",number(m.input_timing[0]),number(m.input_timing[1]),number(m.input_timing[2]),number(m.input_timing[3])));
    heading(ui,"Definitions",&m.toggle_hint);
    ui.weak(format!("Decoder: {}",if m.backend.is_empty(){"initializing"}else{&m.backend}));
    ui.small("RTT: control-message round trip, mean ± stddev (32 samples), not ICMP ping. Net uses RTT/2 assuming symmetry.");
    ui.small("Loss: original RTP missing after the reorder grace period, before FEC/RTX. Repairs cannot erase this rate. After repair is the remaining loss. Duplicates are excluded; late original packets are separate from reordering.");
    ui.small("Est. Total sums the listed stage means; it is not input-to-display latency. Frame assembly, application response, VSync, composition and scanout are not measured.");
    ui.small("rnd: UI preparation and render submission CPU time, including driver stalls, excluding Present/VSync. dec: decoder calls; GPU completion may wait in rnd.");
    ui.small("Queues: producer send wait / before decode / publication to UI. recv/dec FPS are session rates; rnd is the selected view, not monitor scanout. Samples refresh every 500 ms; missing/stale data stays unknown.");
}

pub fn decoder_label(backend:&str)->&str {
    if backend.starts_with("vaapi") {"VAAPI HW"}
    else if backend.starts_with("d3d11va") {"D3D11 HW"}
    else if backend.starts_with("openh264") {"OpenH264 SW"}
    else if backend.starts_with("pyrowave") {if backend.contains("cpu-readback"){"PyroWave / CPU readback"}else{"PyroWave GPU"}}
    else if backend.is_empty(){"Decoder initializing"}
    else {backend.split(['(','[']).next().unwrap_or(backend).trim()}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abandoned_hardware_is_not_reported_as_the_active_decoder() {
        assert_eq!(decoder_label("openh264 [hardware vaapi abandoned: error]"),"OpenH264 SW");
        assert_eq!(decoder_label("d3d11va (Media Foundation hw: GPU)"),"D3D11 HW");
        assert_eq!(decoder_label("pyrowave-vulkan-cpu-readback"),"PyroWave / CPU readback");
    }
    #[test]
    fn estimated_total_distinguishes_missing_from_measured_zero() {
        let m=Metrics{process:[Some(1.),Some(1.6),Some(0.8),Some(5.4)],queues:[Some(0.7),Some(0.3),Some(3.)],rtt:Some([4.,0.6]),..Default::default()};
        assert!((m.estimate().known-14.8).abs()<0.001);assert!(m.estimate().missing.is_empty());
        assert_eq!(m.total_label(),"Est. Total ≈14.8 ms");
        let partial=Metrics{process:[None,Some(0.),None,None],..Default::default()};
        assert_eq!(partial.estimate().samples,1);assert!(!partial.estimate().missing.is_empty());assert!(partial.total_label().starts_with("Known"));
        assert_eq!(Metrics::default().total_label(),"Est. Total —");
    }
    #[test]
    fn fps_and_timing_triplets_have_consistent_order() {
        let m=Metrics{fps:[Some(60.),Some(59.9),Some(59.7)],process:[Some(8.),Some(1.2),Some(0.07),Some(2.3)],..Default::default()};
        assert_eq!(m.fps_label(),"FPS[recv/dec/rnd] 60.0/59.9/59.7");
        assert_eq!(m.stages_label(),"enc/dec/rnd 1.2/0.07/2.3 ms");
    }
    #[test]
    fn layouts_fit_wide_and_narrow_viewports_without_overlapping_groups() {
        let ctx=egui::Context::default();
        let _=ctx.run(Default::default(),|ctx|{egui::CentralPanel::default().show(ctx,|ui| {
            for mode in HudDetail::ALL {for width in [240.,640.,1600.] {
                let m=Metrics{size:[3840,2160],fps:[Some(120.);3],codec:"PYROWAVE".into(),..Default::default()};
                let layout=Layout::new(ui,&m,mode,width);
                assert!(layout.size.x<=width+0.1);
                for (i,p) in layout.placed.iter().enumerate() {for q in &layout.placed[i+1..] {
                    assert!(!Rect::from_min_size(p.offset.to_pos2(),p.galley.size()).intersects(Rect::from_min_size(q.offset.to_pos2(),q.galley.size())));
                }}
            }}
        });});
    }
}
