//! One history view for foreground sharing and the installed host. The view
//! retains snapshots only; it never keeps a stopped host's writer alive.
use eframe::egui;
use std::{path::PathBuf, sync::mpsc, time::{Duration, Instant}};
use uu_core::connection_history::{History, Outcome, Record, Snapshot, Source};

#[derive(Default)]
pub(super) struct State {
    pub snapshot: Option<Snapshot>,
    pub open: bool,
    page: usize,
    error: Option<String>,
    read: Option<mpsc::Receiver<Result<Snapshot, String>>>,
    next_read: Option<Instant>,
}
impl State {
    pub fn revision(&self) -> Option<u64> { self.snapshot.as_ref().map(|s| s.revision) }
    pub fn update(&mut self, result: Result<Option<Snapshot>, String>) {
        match result {
            Ok(snapshot) => { if let Some(snapshot) = snapshot { self.snapshot = Some(snapshot); } self.error = None; }
            Err(error) => self.error = Some(error),
        }
    }
    /// Non-owning reads also work before starting sharing and after shutdown.
    pub fn local(&mut self, path: PathBuf, live: Option<&History>, ctx: &egui::Context) {
        if let Some(history) = live {
            self.read = None;
            self.update(Ok(history.changed_since(self.revision())));
            self.next_read = None;
            return;
        }
        if let Some(result) = self.read.as_ref().and_then(|read| read.try_recv().ok()) {
            self.read = None;
            self.update(result.map(Some));
            self.next_read = Some(Instant::now() + Duration::from_secs(5));
        }
        if self.read.is_none() && self.next_read.is_none_or(|at| Instant::now() >= at) {
            let (send, receive) = mpsc::channel();
            let context = ctx.clone();
            std::thread::spawn(move || {
                let _ = send.send(History::read(&path).map(Snapshot::inactive).map_err(|error| format!("{error:#}")));
                context.request_repaint();
            });
            self.read = Some(receive);
        }
        ctx.request_repaint_after(Duration::from_secs(5));
    }
    pub fn button(&mut self, ui:&mut egui::Ui)->bool {
        if ui.add(crate::theme::secondary_button(uu_i18n::text("连接历史"))).clicked() {
            self.open=true;self.page=0;self.next_read=None;true
        }else{false}
    }
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut refresh = false;
        ui.horizontal(|ui| {
            if ui.add(crate::theme::secondary_button(uu_i18n::text("返回会话"))).clicked(){self.open=false;}
            ui.strong(uu_i18n::text("连接历史（最近 200 次）"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                if ui.add(crate::theme::secondary_button(uu_i18n::text("刷新")).small()).clicked(){self.next_read=None;refresh=true;}
            });
        });
        ui.weak(uu_i18n::text("最近连接在前；同一设备的不同屏幕分别记录。"));
        if let Some(error) = self.error.as_deref().or_else(|| self.snapshot.as_ref().and_then(|s| s.error.as_deref())) {
            ui.colored_label(crate::theme::palette(ui.visuals()).danger,uu_i18n::format!("连接历史暂不可读写：{error}"));
        }
        let records:Vec<_>=self.snapshot.iter().flat_map(|s|s.records.iter()).filter(|record|finished(record)).collect();
        let pages=records.len().div_ceil(20).max(1);
        self.page=self.page.min(pages-1);
        if self.snapshot.is_none(){ui.weak(uu_i18n::text("正在读取连接历史…"));}
        else if records.is_empty(){ui.weak(uu_i18n::text("还没有已结束的连接记录。"));}
        else {
            ui.add_space(12.);
            for record in records.iter().skip(self.page*20).take(20) {
                row(ui,record);ui.separator();
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(self.page>0,crate::theme::secondary_button(uu_i18n::text("上一页"))).clicked(){self.page-=1;}
                ui.label(uu_i18n::format!("第 {} / {} 页",self.page+1,pages));
                if ui.add_enabled(self.page+1<pages,crate::theme::secondary_button(uu_i18n::text("下一页"))).clicked(){self.page+=1;}
            });
        }
        refresh
    }
}
fn finished(record:&Record)->bool {matches!(record.outcome,Outcome::Disconnected|Outcome::Failed|Outcome::Interrupted)}
fn outcome(record: &Record) -> &'static str {
    uu_i18n::text(match record.outcome {
        Outcome::Connecting => "连接中", Outcome::Connected => "串流中",
        Outcome::Disconnected => "已断开", Outcome::Failed if record.connected_at.is_none() => "未能建立连接",
        Outcome::Failed => "异常结束", Outcome::Interrupted => "主机中断",
    })
}
fn timestamp(ms: u64) -> String {
    let Ok(value) = time::OffsetDateTime::from_unix_timestamp((ms / 1000).min(i64::MAX as u64) as i64) else { return "—".into(); };
    let offset = time::UtcOffset::current_local_offset().ok();
    let value = value.to_offset(offset.unwrap_or(time::UtcOffset::UTC));
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}", value.year(), u8::from(value.month()), value.day(),
        value.hour(), value.minute(), value.second(), if offset.is_some() { "" } else { " UTC" })
}
fn row(ui: &mut egui::Ui, record: &Record) {
    ui.push_id(&record.id, |ui| {
        ui.horizontal(|ui| {ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(timestamp(record.connected_at.unwrap_or(record.requested_at)));
            ui.label(outcome(record));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&record.name).strong()).truncate());
            });
        });});
        let source = uu_i18n::text(match record.source { Source::Ip => "IP 连接", Source::Internet => "连接服务", Source::Other => "其它入口" });
        let address = record.address.map(|ip| ip.to_string()).unwrap_or_else(|| "—".into());
        let response = ui.weak(format!("{source} · {address}"));
        response.on_hover_ui(|ui| {
            ui.label(uu_i18n::format!("发起：{}", timestamp(record.requested_at)));
            if let Some(at) = record.connected_at { ui.label(uu_i18n::format!("建立：{}", timestamp(at))); }
            if let Some(at) = record.ended_at { ui.label(uu_i18n::format!("结束：{}", timestamp(at))); }
            if record.outcome == Outcome::Interrupted { ui.weak(uu_i18n::text("主机上次未正常结束，无法确定断开时间。")); }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_history_has_twenty_rows_per_page_and_excludes_live_connections() {
        let records=(0..43).map(|index|Record{id:index.to_string(),peer:"generated".into(),name:format!("History {index}"),source:Source::Ip,address:None,requested_at:1,connected_at:Some(2),ended_at:Some(3),outcome:if index<3{Outcome::Connected}else{Outcome::Disconnected}}).collect();
        let mut state=State{snapshot:Some(Snapshot{revision:1,records,error:None}),..Default::default()};
        let ctx=egui::Context::default();
        let draw=|state:&mut State| {
            let output=ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1200.,3000.))),..Default::default()},|ctx|{egui::CentralPanel::default().show(ctx,|ui|{state.show(ui);});});
            output.shapes.into_iter().filter_map(|shape|match shape.shape{egui::Shape::Text(text) if text.galley.text().starts_with("History ")=>Some(text.galley.text().to_owned()),_=>None}).collect::<Vec<_>>()
        };
        let first=draw(&mut state);assert_eq!(first.len(),20);assert_eq!(first[0],"History 3");
        state.page=1;let second=draw(&mut state);assert_eq!(second.len(),20);assert_eq!(second[0],"History 23");
        state.snapshot.as_mut().unwrap().records.truncate(5);let short=draw(&mut state);assert_eq!(state.page,0);assert_eq!(short.len(),2);
    }
}
