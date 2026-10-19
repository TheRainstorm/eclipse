//! Centered, content-sized stream statistics. Only display measured timings.
use eframe::egui::{self, Color32, FontId, Rect};
use std::sync::atomic::Ordering::Relaxed;

impl super::App {
    pub(super) fn paint_hud(&self, ui: &egui::Ui, rect: Rect) {
        let Some(controller) = &self.controller else { return };
        let s = &controller.stats;
        let (w, h) = s.res();
        let rd = self.fps.get(&self.selected_track).copied().unwrap_or(0.0);
        let codec = s.codec.lock().unwrap().trim_start_matches("video/").to_uppercase();
        let codec = if codec.is_empty() { "—" } else { &codec };
        let source = s.remote_screens.lock().unwrap().for_track(self.selected_track)
            .map(|s| format!(" · {} {}Hz", s.kind_label(), s.refresh_hz)).unwrap_or_default();
        let fresh = uu_core::control_wire::now_ms().saturating_sub(s.rtt_updated_ms.load(Relaxed)) < 5000;
        let network = if fresh {
            format!("{:.1}±{:.1}ms", s.rtt_us.load(Relaxed) as f32 / 1000.0, s.rtt_deviation_us.load(Relaxed) as f32 / 1000.0)
        } else { "—ms".into() };
        // Render is CPU preparation/submission time, excluding vsync wait.
        let text = format!(
            "{w}×{h}@{rd:.0}   {codec}   目标 {} FPS{source}   FPS {:.1} Rx · {:.1} De · {rd:.1} Rd\n\
             Network {network}   Loss {:.2}%   Bandwidth {:.2} Mbps | Render {:.2}ms · Decode {:.2}ms\n\
             丢包 {} · NACK {} · RTX {} · 乱序 {} · 迟到 {} · 坏帧 {} · 解码错误 {} · 丢队列 {} · 恢复请求 {}",
            self.target_fps, s.src_fps(), s.dec_fps(), s.loss_ppm.load(Relaxed) as f32 / 10000.0,
            s.bitrate_kbps.load(Relaxed) as f32 / 1000.0, self.render_ms, s.decode_ms(),
            s.lost.load(Relaxed), s.nack_requests.load(Relaxed), s.rtx_packets.load(Relaxed),
            s.reordered.load(Relaxed), s.late_packets.load(Relaxed), s.damaged_frames.load(Relaxed),
            s.errors.load(Relaxed), s.queue_drops.load(Relaxed), s.keyframe_requests.load(Relaxed));
        let mut job = egui::text::LayoutJob::simple(text, FontId::proportional(12.0),
            Color32::from_rgb(174, 238, 219), (rect.width() - 40.0).clamp(1.0, 920.0));
        job.halign = egui::Align::Center;
        if s.waiting_for_keyframe.load(Relaxed) != 0 {
            job.append("\n正在恢复参考帧", 0.0, egui::TextFormat {
                font_id: FontId::proportional(12.0),
                color: Color32::from_rgb(255, 197, 104), ..Default::default()
            });
        }
        let galley = ui.fonts(|f| f.layout_job(job));
        let bounds = Rect::from_min_size(
            egui::pos2(rect.center().x - galley.size().x / 2.0 - 8.0, rect.top() + 8.0),
            galley.size() + egui::vec2(16.0, 10.0));
        ui.painter().rect_filled(bounds, 6.0, Color32::from_black_alpha(180));
        ui.painter().galley(egui::pos2(rect.center().x, bounds.top() + 5.0), galley, Color32::WHITE);
        ui.interact(bounds, ui.id().with("hud-details"), egui::Sense::hover()).on_hover_text(format!(
            "{}\n输入通道：{} · 本地发送 {} 条 · 发送失败 {} 条\nCtrl+Alt+S：显示 / 隐藏统计\nNetwork：CONTROL 往返耗时均值 ± 标准差（最近 32 次）\nLoss：最近 2 秒超时未修复的 RTP 包比例，重复 RTX 不计入分母\n其余包、帧、请求计数为本次会话累计值\nRd：每秒提交的新画面数，不是显示器扫描次数\nRender：UI 准备及渲染提交 CPU 时间，不含 VSync 等待\nDecode：解码调用耗时，不含颜色转换", s.backend(), if self.input_ready() { "已打开" } else { "未打开" },
            s.input_messages_sent.load(Relaxed), s.input_send_errors.load(Relaxed)));
    }
}
