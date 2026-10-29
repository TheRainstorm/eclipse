//! Three minutes of bounded observations. Missing values and generations never join.
use std::{collections::VecDeque, sync::Arc, time::Instant};

pub const WINDOW_SECONDS: f64 = 180.;
const STEP_SECONDS: f64 = 0.5;
const MAX_SAMPLES: usize = 362;
const ESTIMATE_WINDOW_SECONDS:f64=3.;
const SERIES_COUNT:usize=8;
const ESTIMATE_SERIES:usize=7;
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct EstimatedTotal {
    pub mean_ms:f32,
    /// Variation of sampled estimates, not an E2E measurement error bound.
    pub std_ms:Option<f32>,
    pub samples:usize,
}
#[derive(Clone, Debug)]
pub struct Sample {
    pub seconds: f64,
    /// Same-frame video E2E estimate, capture, encode, decode, render, RTT/2, queues.
    pub values: [Option<f32>; 7],
    pub video_uncertainty_ms: Option<f32>,
    pub estimated_total:Option<EstimatedTotal>,
}
fn full_total(values:&[Option<f32>;7])->Option<f32> {
    values[1..].iter().copied().sum::<Option<f32>>().filter(|v|v.is_finite()&&*v>=0.)
}
fn series_value(sample:&Sample,series:usize)->Option<f32> {
    if series==ESTIMATE_SERIES {sample.estimated_total.map(|v|v.mean_ms)}else{sample.values[series]}
}
fn series_spread(sample:&Sample,series:usize)->Option<f32> {
    match series {0=>sample.video_uncertainty_ms,ESTIMATE_SERIES=>sample.estimated_total.and_then(|v|v.std_ms),_=>None}
}
pub struct History {
    origin: Instant,
    samples: VecDeque<Sample>,
    generation: Option<[u64;3]>,
}
impl Default for History {
    fn default() -> Self { Self { origin: Instant::now(), samples: VecDeque::new(), generation: None } }
}
impl History {
    pub fn record(&mut self, generation: [u64;3], values: [Option<f32>; 7], video_uncertainty_ms: Option<f32>) -> Arc<[Sample]> {
        self.record_at(self.origin.elapsed().as_secs_f64(), generation, values, video_uncertainty_ms);
        self.snapshot()
    }
    pub(crate) fn snapshot(&self)->Arc<[Sample]> {self.samples.iter().cloned().collect()}
    pub(crate) fn record_at(&mut self, now: f64, generation: [u64;3], values: [Option<f32>; 7], video_uncertainty_ms: Option<f32>) {
        if !now.is_finite() { return; }
        if self.generation!=Some(generation) {self.samples.clear();self.generation=Some(generation);}
        if self.samples.back().is_some_and(|s| now < s.seconds) { self.samples.clear(); }
        if self.samples.back().is_some_and(|s| now - s.seconds < STEP_SECONDS - 0.001) { return; }
        while self.samples.front().is_some_and(|s| now - s.seconds > WINDOW_SECONDS) { self.samples.pop_front(); }
        while self.samples.len() >= MAX_SAMPLES { self.samples.pop_front(); }
        let values=values.map(|v|v.filter(|v|v.is_finite() && *v>=0.));
        let estimated_total=full_total(&values).map(|total|{
            let mut count=1usize;let mut mean=f64::from(total);let mut m2=0.;let mut previous=now;
            for sample in self.samples.iter().rev() {
                if now-sample.seconds>ESTIMATE_WINDOW_SECONDS||previous-sample.seconds>STEP_SECONDS*2. {break;}
                let Some(value)=full_total(&sample.values) else{break;};
                count+=1;let delta=f64::from(value)-mean;mean+=delta/count as f64;m2+=delta*(f64::from(value)-mean);previous=sample.seconds;
            }
            EstimatedTotal{mean_ms:mean as f32,std_ms:(count>1).then(||(m2/count as f64).max(0.).sqrt() as f32),samples:count}
        });
        self.samples.push_back(Sample { seconds: now, values, video_uncertainty_ms: values[0].and(video_uncertainty_ms.filter(|v|v.is_finite() && *v>=0.)),estimated_total });
    }
}

pub fn chart(ui:&mut eframe::egui::Ui,samples:&[Sample],height:f32) {
    use eframe::egui::{self,Color32,FontId,Stroke};
    let names=[uu_i18n::text("端到端"),uu_i18n::text("采集"),uu_i18n::text("编码"),uu_i18n::text("解码"),uu_i18n::text("渲染"),"RTT/2",uu_i18n::text("排队"),uu_i18n::text("估计")];
    let colors=[Color32::WHITE,Color32::from_rgb(159,176,255),Color32::from_rgb(255,190,108),Color32::from_rgb(114,215,185),Color32::from_rgb(109,205,255),Color32::from_rgb(202,156,255),Color32::from_rgb(255,145,157),Color32::from_rgb(226,218,119)];
    let id=ui.id().with("latency-curve-series");let mut selected=ui.data_mut(|d|d.get_temp::<[bool;SERIES_COUNT]>(id)).unwrap_or([true,false,true,true,true,true,true,true]);
    ui.horizontal(|ui|{ui.strong(uu_i18n::text("最近 3 分钟"));ui.label(egui::RichText::new(uu_i18n::text("延迟 · ms")).small().weak());});
    egui::ScrollArea::horizontal().id_salt("latency-curve-legend").auto_shrink([false,true]).show(ui,|ui|{ui.horizontal(|ui|{for i in [0,7,1,2,3,4,5,6] {
        let text=egui::RichText::new(names[i]).size(11.).color(if selected[i]{colors[i]}else{Color32::from_gray(110)});
        let help=if i==ESTIMATE_SERIES {uu_i18n::text("估计曲线为Est.Total：最近3秒完整分量和采样的均值，±为这些估计值的标准差，不是同帧端到端或测量误差界。缺项留空，样本不足两次时波动未知。点击切换曲线。")}
            else{uu_i18n::text("端到端曲线为同帧测量的估计均值，竖线为误差范围；其它曲线是独立阶段窗口均值。未知不补零，测量代次改变后重新记录。点击切换曲线。")};
        let response=ui.add(egui::Button::new(text).selected(selected[i]).frame(false).wrap_mode(egui::TextWrapMode::Extend)).on_hover_text(help);
        if response.clicked(){selected[i]=!selected[i];}
    } });});ui.data_mut(|d|d.insert_temp(id,selected));
    let (bounds,response)=ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.),height.max(80.)),egui::Sense::hover());
    let plot=egui::Rect::from_min_max(bounds.min+egui::vec2(32.,8.),bounds.max-egui::vec2(8.,22.));
    if !plot.is_positive(){return;}
    let painter=ui.painter().with_clip_rect(bounds);
    painter.rect_filled(plot,4.,Color32::from_black_alpha(55));
    let end=samples.last().map_or(0.,|s|s.seconds);
    let max=samples.iter().flat_map(|s|(0..SERIES_COUNT).filter_map(|i|selected[i].then(||series_value(s,i)).flatten().map(|v|v+series_spread(s,i).unwrap_or(0.)))).fold(1_f32,f32::max)*1.15;
    for i in 0..=4 {let y=egui::lerp(plot.bottom()..=plot.top(),i as f32/4.);
        painter.hline(plot.x_range(),y,Stroke::new(0.5,Color32::from_white_alpha(25)));
        painter.text(egui::pos2(plot.left()-5.,y),egui::Align2::RIGHT_CENTER,format!("{:.1}",max*i as f32/4.),FontId::proportional(10.),Color32::from_gray(165));
    }
    for (fraction,label) in [(0.,"-3m"),(0.5,"-90s"),(1.,uu_i18n::text("现在"))] {let x=egui::lerp(plot.x_range(),fraction);painter.text(egui::pos2(x,plot.bottom()+6.),egui::Align2::CENTER_TOP,label,FontId::proportional(10.),Color32::from_gray(165));}
    for series in 0..SERIES_COUNT {if !selected[series]{continue;}let mut points=Vec::new();
        let flush=|points:&mut Vec<egui::Pos2>|{
            if points.len()>1 {
                if series==ESTIMATE_SERIES {painter.extend(egui::Shape::dashed_line(points,Stroke::new(1.5,colors[series]),6.,4.));points.clear();}
                else {painter.add(egui::Shape::line(std::mem::take(points),Stroke::new(if series==0{1.5}else{1.1},colors[series])));}
            } else {points.clear();}
        };
        let mut previous=None;
        for sample in samples {
            if previous.is_some_and(|at|sample.seconds-at>STEP_SECONDS*2.){flush(&mut points);}
            previous=Some(sample.seconds);
            if let Some(v)=series_value(sample,series) {
                let x=plot.right()-((end-sample.seconds)/WINDOW_SECONDS) as f32*plot.width();
                if let Some(error)=series_spread(sample,series) {
                    painter.line_segment([egui::pos2(x,plot.bottom()-(v-error).max(0.)/max*plot.height()),egui::pos2(x,plot.bottom()-(v+error)/max*plot.height())],Stroke::new(0.7,colors[series].gamma_multiply(0.22)));
                }
                points.push(egui::pos2(x,plot.bottom()-v/max*plot.height()));
            }
            else{flush(&mut points);}
        }flush(&mut points);
    }
    if samples.is_empty(){painter.text(plot.center(),egui::Align2::CENTER_CENTER,uu_i18n::text("连接后开始记录"),FontId::proportional(12.),Color32::from_gray(155));}
    if let Some(pointer)=response.hover_pos().filter(|p|plot.contains(*p)) {
        let wanted=end-(plot.right()-pointer.x) as f64/plot.width() as f64*WINDOW_SECONDS;
        if let Some(sample)=samples.iter().min_by(|a,b|(a.seconds-wanted).abs().total_cmp(&(b.seconds-wanted).abs())) {
            let x=plot.right()-((end-sample.seconds)/WINDOW_SECONDS) as f32*plot.width();painter.vline(x,plot.y_range(),Stroke::new(1.,Color32::from_white_alpha(120)));
            egui::Area::new(ui.id().with("latency-curve-tooltip")).order(egui::Order::Tooltip).interactable(false)
                .fixed_pos(pointer+egui::vec2(14.,14.)).constrain_to(ui.ctx().screen_rect().shrink(4.)).show(ui.ctx(),|ui|{
                egui::Frame::none().fill(Color32::from_black_alpha(238)).rounding(6.).inner_margin(8.)
                    .stroke(Stroke::new(1.,Color32::from_white_alpha(45))).show(ui,|ui|{
                ui.visuals_mut().override_text_color=Some(Color32::from_gray(220));ui.small(uu_i18n::format!("{:.1} 秒前",end-sample.seconds));for i in [0,7,1,2,3,4,5,6] {if selected[i]{
                let value=series_value(sample,i).map_or_else(||"—".into(),|v|if i==0||i==ESTIMATE_SERIES{series_spread(sample,i).map_or_else(||format!("≈{v:.3} ±—"),|error|format!("≈{v:.3} ±{error:.3}"))}else{format!("{v:.2}")});
                ui.colored_label(colors[i],format!("{}  {} ms",names[i],value));
            }}});});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimate_uses_complete_raw_sums_and_reports_window_variation_instead_of_error() {
        let mut h=History::default();
        for n in 0..3 {
            let mut values=[Some(n as f32+1.);7];values[0]=Some(1000.);
            h.record_at(n as f64*0.5,[1,0,1],values,Some(999.));
        }
        let total=h.samples.back().unwrap().estimated_total.unwrap();
        assert_eq!(total.samples,3);assert!((total.mean_ms-12.).abs()<0.001);
        assert!((total.std_ms.unwrap()-24f32.sqrt()).abs()<0.001);
        h.record_at(1.1,[1,0,1],[Some(100.);7],None);
        assert_eq!(h.samples.back().unwrap().estimated_total.unwrap(),total,"UI repaint is not a sample");
        // The old raw totals leave the three-second window; no averaging of
        // previous rolling means and no zero variation invented for one point.
        h.record_at(4.5,[1,0,1],[Some(4.);7],None);
        let total=h.samples.back().unwrap().estimated_total.unwrap();
        assert_eq!((total.mean_ms,total.std_ms,total.samples),(24.,None,1));
        let mut missing=[Some(1.);7];missing[3]=None;
        h.record_at(5.,[1,0,1],missing,None);assert!(h.samples.back().unwrap().estimated_total.is_none());
        let mut values=[Some(1.);7];values[0]=None;
        h.record_at(5.5,[1,0,1],values,None);
        assert_eq!(h.samples.back().unwrap().estimated_total.unwrap().samples,1,"missing components break the estimate window");
        h.record_at(6.,[1,0,1],[Some(2.);7],None);
        let total=h.samples.back().unwrap().estimated_total.unwrap();
        assert_eq!((total.mean_ms,total.std_ms,total.samples),(9.,Some(3.),2));
        h.record_at(6.5,[1,1,1],[Some(2.);7],None);
        assert_eq!(h.samples.back().unwrap().estimated_total.unwrap().samples,1);
        h.record_at(7.,[1,1,1],[Some(f32::MAX);7],None);
        assert!(h.samples.back().unwrap().estimated_total.is_none());
    }
    #[test]
    fn legend_stays_on_one_line_and_estimate_button_toggles_its_curve() {
        use eframe::egui::{self,Rect,Event,PointerButton};
        for width in [520.,300.] {
            let ctx=egui::Context::default();let view=Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,400.));
            let mut now=1.;let mut chart_id=None;
            let mut frame=|events| {
                now+=0.05;
                ctx.run(egui::RawInput{screen_rect:Some(view),time:Some(now),events,..Default::default()},|ctx|{
                    egui::CentralPanel::default().show(ctx,|ui|{chart_id=Some(ui.id().with("latency-curve-series"));chart(ui,&[],200.);});
                })
            };
            let mut output=frame(vec![]);for _ in 0..3{output=frame(vec![]);}
            let names=["端到端","估计","采集","编码","解码","渲染","RTT/2","排队"];
            let labels:Vec<_>=output.shapes.iter().filter_map(|s|if let egui::Shape::Text(t)=&s.shape{names.contains(&t.galley.job.text.as_str()).then_some(t)}else{None}).collect();
            assert_eq!(labels.len(),8);
            assert!(labels.iter().all(|t|(t.pos.y-labels[0].pos.y).abs()<0.1&&t.galley.rows.len()==1));
            let estimate=labels.iter().find(|t|t.galley.job.text=="估计").unwrap();let point=estimate.pos+estimate.galley.size()/2.;
            frame(vec![Event::PointerMoved(point)]);
            // Let egui finish the hover tooltip's invisible sizing pass before clicking.
            for _ in 0..3 {frame(vec![]);}
            frame(vec![Event::PointerButton{pos:point,button:PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
            frame(vec![Event::PointerButton{pos:point,button:PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            drop(frame);
            let selected=ctx.data(|d|d.get_temp::<[bool;SERIES_COUNT]>(chart_id.unwrap())).unwrap();
            assert!(!selected[ESTIMATE_SERIES],"{width}: estimate click at {point:?} did not toggle: {selected:?}");assert!(selected[0],"toggling estimate must not toggle E2E");
        }
    }
    #[test]
    fn window_is_bounded_and_unknown_is_not_zero() {
        let mut h=History::default();
        for n in 0..10000 {h.record_at(n as f64*0.5,[1,0,1],[Some(1.),None,Some(f32::NAN),Some(-1.),None,None,None],Some(0.5));}
        assert_eq!(h.samples.len(),361);
        assert!((h.samples.back().unwrap().seconds-h.samples.front().unwrap().seconds-180.).abs()<0.001);
        assert_eq!(h.samples.back().unwrap().values,[Some(1.),None,None,None,None,None,None]);
    }
    #[test]
    fn duplicate_repaints_do_not_create_samples_and_clock_reset_starts_fresh() {
        let mut h=History::default();h.record_at(10.,[1,0,1],[Some(1.);7],None);h.record_at(10.1,[1,0,1],[Some(2.);7],None);assert_eq!(h.samples.len(),1);
        h.record_at(0.,[1,0,1],[Some(3.);7],None);assert_eq!(h.samples.len(),1);assert_eq!(h.samples[0].values,[Some(3.);7]);
    }
    #[test]
    fn source_track_and_measurement_generations_reset_history_without_throttling() {
        let mut h=History::default();
        for (n,generation) in [[1,0,1],[2,0,1],[2,1,1],[2,1,2]].into_iter().enumerate() {
            h.record_at(n as f64*0.1,generation,[Some(n as f32);7],Some(0.2));
            assert_eq!(h.samples.len(),1);
            assert_eq!(h.samples[0].values,[Some(n as f32);7]);
        }
        h.record_at(1.,[2,1,2],[None;7],Some(0.2));
        assert_eq!(h.samples.back().unwrap().values,[None;7]);
        assert_eq!(h.samples.back().unwrap().video_uncertainty_ms,None);
    }
}
