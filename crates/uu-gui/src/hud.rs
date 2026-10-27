//! Collect a bounded snapshot; layout and hover presentation share one model.
use eframe::egui::{self, Rect};
use std::{sync::atomic::Ordering::Relaxed,time::Instant};
use uu_core::client_settings::HudDetail;
use crate::hud_panel::{Counters,Layout,Metrics};

#[derive(Default)]
pub(super) struct Cache {
    key:Option<(u32,u32,u32,u32,u32,HudDetail,u64)>,
    updated:Option<Instant>,
    metrics:Metrics,
    layout:Option<Layout>,
    hover:crate::hud_panel::HoverCard,
    history:crate::hud_panel::History,
}
impl Cache {
    pub(super) fn keyboard_owned(&self)->bool {self.hover.keyboard_owned()}
    pub(super) fn invalidate(&mut self) {self.key=None;self.updated=None;self.hover=Default::default();}
}
impl super::App {
    pub(super) fn sample_hud(&mut self) {
        let Some(controller)=&self.controller else{return;};
        let s=&controller.stats;
        let (w,h)=self.textures.get(&self.selected_track).map(|t|(t.size()[0] as u32,t.size()[1] as u32)).unwrap_or_else(||s.res());
        if self.hud_cache.updated.is_none_or(|t|t.elapsed().as_millis()>=500) {
            let native=s.native_timing.snapshot();
            let network=s.network_loss.lock().unwrap().clone().filter(|v|v.fresh());
            let host=native.host.and_then(|v|v.video);
            let updated=s.rtt_updated_ms.load(Relaxed);
            let fresh=updated!=0&&uu_core::control_wire::now_ms().saturating_sub(updated)<5000;
            let received=s.packets.load(Relaxed)>0;
            let decoded=s.decoded_total.load(Relaxed)>0;
            let ms=|v:u32|v as f32/1000.;
            let configured=s.configured_fps.load(Relaxed);
            let mut m=Metrics {
                size:[w,h],
                fps:[received.then(||s.src_fps()),decoded.then(||s.dec_fps()),self.fps.get(&self.selected_track).copied()],
                target_fps:if configured>0{configured}else{self.target_fps},
                source_hz:s.remote_screens.lock().unwrap().for_track(self.selected_track).map(|v|v.refresh_hz).filter(|hz|*hz>1),
                codec:{let codec=s.codec.lock().unwrap().rsplit('/').next().unwrap_or("").to_uppercase();if codec.is_empty(){String::new()}else{format!("{codec} {}",if s.native_hdr_response.lock().unwrap().is_some(){"10-bit HDR"}else{"SDR"})}},
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
                history:Default::default(),
            };
            m.history=self.hud_cache.history.record([
                m.estimate().missing.is_empty().then(||m.estimate().known),m.process[0],m.process[1],m.process[2],m.process[3],m.rtt.map(|v|v[0]/2.),m.queues.iter().copied().collect::<Option<Vec<_>>>().map(|v|v.iter().sum())
            ]);
            self.hud_cache.metrics=m;
            self.hud_cache.key=None;self.hud_cache.updated=Some(Instant::now());
        }
    }
    pub(super) fn paint_hud(&mut self,ui:&egui::Ui,rect:Rect)->bool {
        if self.controller.is_none(){return false;}
        self.sample_hud();
        let [w,h]=self.hud_cache.metrics.size;
        let width=(rect.width()-32.).max(1.);let detail=self.preferences.hud_detail;
        let key=(self.selected_track,width.to_bits(),ui.ctx().pixels_per_point().to_bits(),w,h,detail,uu_i18n::epoch());
        if self.hud_cache.key!=Some(key) {
            self.hud_cache.layout=Some(Layout::new(ui,&self.hud_cache.metrics,detail,width));self.hud_cache.key=Some(key);
        }
        if let Some(layout)=&self.hud_cache.layout {
            let response=layout.paint(ui,rect);
            return self.hud_cache.hover.show(ui,&response,&self.hud_cache.metrics,rect);
        }
        false
    }
}
