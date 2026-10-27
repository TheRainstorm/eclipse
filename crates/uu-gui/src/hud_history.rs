//! Three minutes of bounded, monotonic pipeline samples. Missing stages stay missing.
use std::{collections::VecDeque, sync::Arc, time::Instant};

pub const WINDOW_SECONDS: f64 = 180.;
const STEP_SECONDS: f64 = 0.5;
const MAX_SAMPLES: usize = 362;
#[derive(Clone, Debug)]
pub struct Sample {
    pub seconds: f64,
    /// Total, capture, encode, decode, render, estimated network, queue sum.
    pub values: [Option<f32>; 7],
}
pub struct History {
    origin: Instant,
    samples: VecDeque<Sample>,
}
impl Default for History {
    fn default() -> Self { Self { origin: Instant::now(), samples: VecDeque::new() } }
}
impl History {
    pub fn record(&mut self, values: [Option<f32>; 7]) -> Arc<[Sample]> {
        self.record_at(self.origin.elapsed().as_secs_f64(), values);
        self.samples.iter().cloned().collect()
    }
    fn record_at(&mut self, now: f64, values: [Option<f32>; 7]) {
        if !now.is_finite() { return; }
        if self.samples.back().is_some_and(|s| now < s.seconds) { self.samples.clear(); }
        if self.samples.back().is_some_and(|s| now - s.seconds < STEP_SECONDS - 0.001) { return; }
        while self.samples.front().is_some_and(|s| now - s.seconds > WINDOW_SECONDS) { self.samples.pop_front(); }
        while self.samples.len() >= MAX_SAMPLES { self.samples.pop_front(); }
        self.samples.push_back(Sample { seconds: now, values: values.map(|v| v.filter(|v|v.is_finite() && *v>=0.)) });
    }
}

pub fn chart(ui:&mut eframe::egui::Ui,samples:&[Sample],height:f32) {
    use eframe::egui::{self,Color32,FontId,Stroke};
    let names=["合计","采集","编码","解码","渲染","网络","排队"];
    let colors=[Color32::WHITE,Color32::from_rgb(159,176,255),Color32::from_rgb(255,190,108),Color32::from_rgb(114,215,185),Color32::from_rgb(109,205,255),Color32::from_rgb(202,156,255),Color32::from_rgb(255,145,157)];
    let id=ui.id().with("latency-curve-series");let mut selected=ui.data_mut(|d|d.get_temp::<[bool;7]>(id)).unwrap_or([true,false,true,true,true,true,true]);
    ui.horizontal(|ui|{ui.strong("最近 3 分钟");ui.label(egui::RichText::new("延迟 · ms").small().weak());});
    ui.horizontal_wrapped(|ui|{for i in 0..7 {
        let text=egui::RichText::new(names[i]).size(11.).color(if selected[i]{colors[i]}else{Color32::from_gray(110)});
        if ui.selectable_label(selected[i],text).on_hover_text("点击显示或隐藏这条曲线；数值为采样窗口均值，缺失样本不补零").clicked(){selected[i]=!selected[i];}
    }});ui.data_mut(|d|d.insert_temp(id,selected));
    let (bounds,response)=ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.),height.max(80.)),egui::Sense::hover());
    let plot=egui::Rect::from_min_max(bounds.min+egui::vec2(32.,8.),bounds.max-egui::vec2(8.,22.));
    if !plot.is_positive(){return;}
    let painter=ui.painter().with_clip_rect(bounds);
    painter.rect_filled(plot,4.,Color32::from_black_alpha(55));
    let end=samples.last().map_or(0.,|s|s.seconds);
    let max=samples.iter().flat_map(|s|s.values.iter().enumerate().filter_map(|(i,v)|selected[i].then_some(*v).flatten())).fold(1_f32,f32::max)*1.15;
    for i in 0..=4 {let y=egui::lerp(plot.bottom()..=plot.top(),i as f32/4.);
        painter.hline(plot.x_range(),y,Stroke::new(0.5,Color32::from_white_alpha(25)));
        painter.text(egui::pos2(plot.left()-5.,y),egui::Align2::RIGHT_CENTER,format!("{:.1}",max*i as f32/4.),FontId::proportional(10.),Color32::from_gray(165));
    }
    for (fraction,label) in [(0.,"-3m"),(0.5,"-90s"),(1.,"现在")] {let x=egui::lerp(plot.x_range(),fraction);painter.text(egui::pos2(x,plot.bottom()+6.),egui::Align2::CENTER_TOP,label,FontId::proportional(10.),Color32::from_gray(165));}
    for series in 0..7 {if !selected[series]{continue;}let mut points=Vec::new();
        let flush=|points:&mut Vec<egui::Pos2>|{if points.len()>1{painter.add(egui::Shape::line(std::mem::take(points),Stroke::new(if series==0{1.5}else{1.1},colors[series])));}else{points.clear();}};
        for sample in samples {
            if let Some(v)=sample.values[series] {points.push(egui::pos2(plot.right()-((end-sample.seconds)/WINDOW_SECONDS) as f32*plot.width(),plot.bottom()-v/max*plot.height()));}
            else{flush(&mut points);}
        }flush(&mut points);
    }
    if samples.is_empty(){painter.text(plot.center(),egui::Align2::CENTER_CENTER,"连接后开始记录",FontId::proportional(12.),Color32::from_gray(155));}
    if let Some(pointer)=response.hover_pos().filter(|p|plot.contains(*p)) {
        let wanted=end-(plot.right()-pointer.x) as f64/plot.width() as f64*WINDOW_SECONDS;
        if let Some(sample)=samples.iter().min_by(|a,b|(a.seconds-wanted).abs().total_cmp(&(b.seconds-wanted).abs())) {
            let x=plot.right()-((end-sample.seconds)/WINDOW_SECONDS) as f32*plot.width();painter.vline(x,plot.y_range(),Stroke::new(1.,Color32::from_white_alpha(120)));
            response.on_hover_ui(|ui| {ui.small(format!("{:.1} 秒前",end-sample.seconds));for i in 0..7 {if selected[i]{ui.colored_label(colors[i],format!("{}  {} ms",names[i],sample.values[i].map_or_else(||"—".into(),|v|format!("{v:.2}"))));}}});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_is_bounded_and_unknown_is_not_zero() {
        let mut h=History::default();
        for n in 0..10000 {h.record_at(n as f64*0.5,[Some(1.),None,Some(f32::NAN),Some(-1.),None,None,None]);}
        assert_eq!(h.samples.len(),361);
        assert!((h.samples.back().unwrap().seconds-h.samples.front().unwrap().seconds-180.).abs()<0.001);
        assert_eq!(h.samples.back().unwrap().values,[Some(1.),None,None,None,None,None,None]);
    }
    #[test]
    fn duplicate_repaints_do_not_create_samples_and_clock_reset_starts_fresh() {
        let mut h=History::default();h.record_at(10.,[Some(1.);7]);h.record_at(10.1,[Some(2.);7]);assert_eq!(h.samples.len(),1);
        h.record_at(0.,[Some(3.);7]);assert_eq!(h.samples.len(),1);assert_eq!(h.samples[0].values,[Some(3.);7]);
    }
}
