//! Compact Moonlight-style performance strip. Unavailable host timings stay N/A.
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
        let fresh = uu_core::control_wire::now_ms().saturating_sub(s.rtt_updated_ms.load(Relaxed)) < 5000;
        let network = if fresh {
            format!("{:.1}±{:.1}ms", s.rtt_us.load(Relaxed) as f32 / 1000.0, s.rtt_deviation_us.load(Relaxed) as f32 / 1000.0)
        } else { "—ms".into() };
        // eframe measures UI preparation + rendering, excluding vsync wait.
        // This is CPU submission time, not a GPU timestamp or display latency.
        let text = format!("{w}×{h}@{rd:.0}   {codec}   FPS {:.1} Rx · {:.1} De · {rd:.1} Rd   Network {network}   Loss {:.2}%   Bandwidth {:.2} Mbps | Render {:.2}ms · Decode {:.2}ms · Encode N/A",
            s.src_fps(), s.dec_fps(), s.loss_ppm.load(Relaxed) as f32 / 10000.0,
            s.bitrate_kbps.load(Relaxed) as f32 / 1000.0, self.render_ms, s.decode_ms());
        let pos = rect.left_top() + egui::vec2(5.0, 3.0);
        let galley = ui.painter().layout(text, FontId::proportional(12.0),
            Color32::from_rgb(174, 238, 219), (rect.width() - 10.0).max(1.0));
        let size = galley.size();
        ui.painter().rect_filled(Rect::from_min_size(rect.left_top(), egui::vec2(rect.width(), size.y + 6.0)), 0.0,
            Color32::from_black_alpha(180));
        ui.painter().galley(pos, galley, Color32::WHITE);
        if s.waiting_for_keyframe.load(Relaxed) != 0 {
            ui.painter().text(pos + egui::vec2(0.0, size.y + 5.0), egui::Align2::LEFT_TOP,
                format!("正在恢复参考帧 · 请求 {} · 坏帧 {} · RTX {}", s.keyframe_requests.load(Relaxed), s.damaged_frames.load(Relaxed), s.rtx_packets.load(Relaxed)),
                FontId::proportional(11.0), Color32::from_rgb(255, 197, 104));
        }
    }
}
