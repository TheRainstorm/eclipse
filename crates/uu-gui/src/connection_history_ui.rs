//! One history view for foreground sharing and the installed host. The view
//! retains snapshots only; it never keeps a stopped host's writer alive.
use eframe::egui;
use std::{path::PathBuf, sync::mpsc, time::{Duration, Instant}};
use uu_core::connection_history::{History, Outcome, Record, Snapshot, Source};

#[derive(Default)]
pub(super) struct State {
    pub snapshot: Option<Snapshot>,
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
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut refresh = false;
        ui.horizontal(|ui| {
            ui.strong(uu_i18n::text("连接历史（最近 200 次）"));
            if ui.add(crate::theme::secondary_button(uu_i18n::text("刷新")).small()).clicked() {
                self.next_read = None; refresh = true;
            }
        });
        ui.weak(uu_i18n::text("最近连接在前；同一设备的不同屏幕分别记录。"));
        if let Some(error) = self.error.as_deref().or_else(|| self.snapshot.as_ref().and_then(|s| s.error.as_deref())) {
            ui.colored_label(crate::theme::palette(ui.visuals()).danger,
                uu_i18n::format!("连接历史暂不可读写：{error}"));
        }
        match &self.snapshot {
            None => { ui.weak(uu_i18n::text("正在读取连接历史…")); }
            Some(snapshot) if snapshot.records.is_empty() => { ui.weak(uu_i18n::text("还没有连接记录。已授权设备发起串流后会显示在这里。")); }
            Some(snapshot) => {
                // A fixed row height keeps all 200 records inexpensive to view.
                let height = 2.0 * ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
                egui::ScrollArea::vertical().id_salt("host-connection-history").max_height(360.0)
                    .show_rows(ui, height, snapshot.records.len(), |ui, range| {
                        for index in range { row(ui, &snapshot.records[index]); }
                    });
            }
        }
        refresh
    }
}
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(timestamp(record.connected_at.unwrap_or(record.requested_at)));
            ui.label(outcome(record));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&record.name).strong()).truncate());
            });
        });
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
