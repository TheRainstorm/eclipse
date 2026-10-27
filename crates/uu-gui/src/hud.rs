//! Collect a bounded snapshot; layout and hover presentation share one model.
use eframe::egui::{self, Rect};
use std::{sync::atomic::Ordering::Relaxed,time::Instant};
use uu_core::client_settings::HudDetail;
use crate::hud_panel::{Counters,Layout,Metrics};

#[derive(Default)]
pub(super) struct Cache {
    key:Option<(u32,u32,u32,u32,u32,HudDetail)>,
    updated:Option<Instant>,
    metrics:Metrics,
    layout:Option<Layout>,
}
impl super::App {
    pub(super) fn paint_hud(&mut self,ui:&egui::Ui,rect:Rect) {
        let Some(controller)=&self.controller else{return;};
        let s=&controller.stats;
        let (w,h)=self.textures.get(&self.selected_track).map(|t|(t.size()[0] as u32,t.size()[1] as u32)).unwrap_or_else(||s.res());
        let width=(rect.width()-32.).max(1.);
        let detail=self.preferences.hud_detail;
        let key=(self.selected_track,width.to_bits(),ui.ctx().pixels_per_point().to_bits(),w,h,detail);
        if self.hud_cache.key!=Some(key)||self.hud_cache.updated.is_none_or(|t|t.elapsed().as_millis()>=500) {
            let native=s.native_timing.snapshot();
            let network=s.network_loss.lock().unwrap().clone().filter(|v|v.fresh());
            let host=native.host.and_then(|v|v.video);
            let updated=s.rtt_updated_ms.load(Relaxed);
            let fresh=updated!=0&&uu_core::control_wire::now_ms().saturating_sub(updated)<5000;
            let received=s.packets.load(Relaxed)>0;
            let decoded=s.decoded_total.load(Relaxed)>0;
            let ms=|v:u32|v as f32/1000.;
            let configured=s.configured_fps.load(Relaxed);
            let m=Metrics {
                size:[w,h],
                fps:[received.then(||s.src_fps()),decoded.then(||s.dec_fps()),self.fps.get(&self.selected_track).copied()],
                target_fps:if configured>0{configured}else{self.target_fps},
                source_hz:s.remote_screens.lock().unwrap().for_track(self.selected_track).map(|v|v.refresh_hz),
                codec:format!("{}{}",s.codec.lock().unwrap().rsplit('/').next().unwrap_or("").to_uppercase(),if s.native_hdr_response.lock().unwrap().is_some(){" 10-bit HDR"}else{""}),
                mode:s.native_quality_current.lock().unwrap().and_then(|v|v.rate_control).map_or_else(String::new,|r|r.label().into()),
                backend:s.backend(),
                bitrate:received.then(||s.bitrate_kbps.load(Relaxed) as f32/1000.),
                rtt:fresh.then(||[s.rtt_us.load(Relaxed) as f32/1000.,s.rtt_deviation_us.load(Relaxed) as f32/1000.]),
                rtt_distribution:if fresh{s.rtt_distribution_us.lock().unwrap().map(|v|v.map(|n|n as f64/1000.))}else{None},
                loss:network.as_ref().and_then(|v|v.loss_ppm).map(|v|v as f32/10000.),
                unrecovered_loss:received.then(||s.loss_ppm.load(Relaxed) as f32/10000.),
                network,
                process:[host.map(|v|ms(v.capture_us)),host.map(|v|ms(v.encode_us)),decoded.then(||s.decode_ms()),
                    (self.render_samples>0||self.render_ms>0.).then_some(self.render_ms)],
                queues:[host.map(|v|ms(v.send_wait_us)),s.frame_cadence.lock().unwrap().decode_queue_ms.map(|v|v[0] as f32),
                    controller.frames.delivery_wait_ms(self.selected_track)],
                counters:Counters {
                    lost:s.lost.load(Relaxed) as u64,nack:s.nack_requests.load(Relaxed) as u64,rtx:s.rtx_packets.load(Relaxed) as u64,
                    reordered:s.reordered.load(Relaxed) as u64,late:s.late_packets.load(Relaxed) as u64,
                    damaged:s.damaged_frames.load(Relaxed) as u64,errors:s.errors.load(Relaxed) as u64,
                    queue_drops:s.queue_drops.load(Relaxed) as u64,keyframes:s.keyframe_requests.load(Relaxed) as u64,
                    fec_packets:s.fec_packets.load(Relaxed) as u64,fec_recovered:s.fec_recovered.load(Relaxed) as u64,
                    fec_bytes:s.fec_bytes.load(Relaxed) as u64,fec_drops:s.fec_drops.load(Relaxed) as u64,
                    recovery:[s.recovery_receive_damage.load(Relaxed) as u64,s.recovery_decode_queue.load(Relaxed) as u64,
                        s.recovery_decoder_error.load(Relaxed) as u64,s.recovery_format_change.load(Relaxed) as u64],
                },
                input_ready:self.input_ready(),
                input_counts:[s.input_messages_sent.load(Relaxed) as u64,s.input_send_errors.load(Relaxed) as u64],
                input_timing:[native.client_input_queue_us.map(ms),native.input.map(|v|ms(v.confirmation_rtt_us)),
                    native.input.map(|v|ms(v.host_queue_us)),native.input.map(|v|ms(v.host_inject_us))],
                waiting_for_keyframe:s.waiting_for_keyframe.load(Relaxed)!=0,
                toggle_hint:format!("{} · Toggle HUD",self.preferences.hotkeys.label(uu_core::hotkeys::Action::Stats)),
            };
            self.hud_cache.layout=Some(Layout::new(ui,&m,detail,width));
            self.hud_cache.metrics=m;
            self.hud_cache.key=Some(key);self.hud_cache.updated=Some(Instant::now());
        }
        if let Some(layout)=&self.hud_cache.layout {
            layout.paint(ui,rect).on_hover_ui(|ui|crate::hud_panel::details(ui,&self.hud_cache.metrics));
        }
    }
}
