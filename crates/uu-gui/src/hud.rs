//! Centered, content-sized stream statistics. Only display measured timings.
use eframe::egui::{self, Color32, FontId, Rect};
use std::sync::atomic::Ordering::Relaxed;

#[derive(Default)]
pub(super) struct Cache {
    key: Option<(u32, u32, u32, u32, u32)>,
    updated: Option<std::time::Instant>,
    galley: Option<std::sync::Arc<egui::Galley>>,
    detail: String,
}

impl super::App {
    pub(super) fn paint_hud(&mut self, ui: &egui::Ui, rect: Rect) {
        let Some(controller) = &self.controller else {
            return;
        };
        let wrap = (rect.width() - 40.0).clamp(1.0, 920.0);
        let resolution = self
            .textures
            .get(&self.selected_track)
            .map(|t| (t.size()[0] as u32, t.size()[1] as u32))
            .unwrap_or_else(|| controller.stats.res());
        let key = (
            self.selected_track,
            wrap.to_bits(),
            ui.ctx().pixels_per_point().to_bits(),
            resolution.0,
            resolution.1,
        );
        let refresh = self.hud_cache.key != Some(key)
            || self.hud_cache.updated.map_or(true, |at| {
                at.elapsed() >= std::time::Duration::from_millis(500)
            });
        if refresh {
            let s = &controller.stats;
            let (w, h) = self
                .textures
                .get(&self.selected_track)
                .map(|t| (t.size()[0] as u32, t.size()[1] as u32))
                .unwrap_or_else(|| s.res());
            let rd = self.fps.get(&self.selected_track).copied().unwrap_or(0.0);
            let codec = s
                .codec
                .lock()
                .unwrap()
                .trim_start_matches("video/")
                .to_uppercase();
            let codec = if codec.is_empty() { "—" } else { &codec };
            // Report the active decoder, including a later software fallback,
            // rather than the build features or the user's hardware preference.
            let backend = s.backend();
            let decoder = decoder_label(&backend);
            let source = s
                .remote_screens
                .lock()
                .unwrap()
                .for_track(self.selected_track)
                .map(|s| format!(" · {} {}Hz", s.kind_label(), s.refresh_hz))
                .unwrap_or_default();
            let fresh = uu_core::control_wire::now_ms()
                .saturating_sub(s.rtt_updated_ms.load(Relaxed))
                < 5000;
            let network = if fresh {
                format!(
                    "{:.1}±{:.1}ms",
                    s.rtt_us.load(Relaxed) as f32 / 1000.0,
                    s.rtt_deviation_us.load(Relaxed) as f32 / 1000.0
                )
            } else {
                "—ms".into()
            };
            let rtt_distribution = if fresh {
                *s.rtt_distribution_us.lock().unwrap()
            } else {
                None
            };
            let rtt_tail = rtt_distribution
                .map(|d| format!(" · P95 {:.1}ms", d[3] as f32 / 1000.0))
                .unwrap_or_default();
            let rtt_detail = rtt_distribution
                .map(|d| {
                    format!(
                        "\n控制 RTT 最新/最小/中位/P95/最大：{:.1}/{:.1}/{:.1}/{:.1}/{:.1}ms",
                        d[0] as f32 / 1000.0,
                        d[1] as f32 / 1000.0,
                        d[2] as f32 / 1000.0,
                        d[3] as f32 / 1000.0,
                        d[4] as f32 / 1000.0
                    )
                })
                .unwrap_or_default();
            let configured = s.configured_fps.load(Relaxed);
            let target_fps = if configured > 0 {
                configured
            } else {
                self.target_fps
            };
            let wait = controller.frames.delivery_wait_ms(self.selected_track);
            let queue = wait
                .map(|v| format!("{v:.1}ms"))
                .unwrap_or_else(|| "—".into());
            let native = s.native_timing.snapshot();
            let host_video = native.host.and_then(|host| host.video);
            let decode_queue = s
                .frame_cadence
                .lock()
                .unwrap()
                .decode_queue_ms
                .map(|v| v[0] as f32);
            let mut stages = Vec::new();
            if let Some(host) = host_video {
                stages.push(format!("Capture {:.2}ms", host.capture_us as f32 / 1000.));
                stages.push(format!("Encode {:.2}ms", host.encode_us as f32 / 1000.));
            }
            stages.push(format!("Decode {:.2}ms", s.decode_ms()));
            stages.push(format!("Render CPU {:.2}ms", self.render_ms));
            let mut queues = Vec::new();
            if let Some(host) = host_video {
                queues.push(format!("发送 {:.2}ms", host.send_wait_us as f32 / 1000.));
            }
            if let Some(ms) = decode_queue {
                queues.push(format!("解码前 {ms:.2}ms"));
            }
            if wait.is_some() {
                queues.push(format!("待绘制 {queue}"));
            }
            let rate = s
                .native_quality_current
                .lock()
                .unwrap()
                .and_then(|v| v.rate_control)
                .map(|r| format!(" / {}", r.label()))
                .unwrap_or_default();
            let title = format!("{w} × {h}   ·   {codec}{rate}   ·   {decoder}");
            let fps_line = format!(
                "帧率    接收 {:.1}  ·  解码 {:.1}  ·  绘制 {rd:.1}  /  目标 {target_fps}{source}",
                s.src_fps(),
                s.dec_fps()
            );
            let network_line = format!(
                "网络    RTT {network}  ·  {:.2} Mbps  ·  丢包 {:.2}%",
                s.bitrate_kbps.load(Relaxed) as f32 / 1000.,
                s.loss_ppm.load(Relaxed) as f32 / 10000.
            );
            let counters=format!("丢包 {} · NACK {} · RTX {} · 乱序 {} · 迟到 {} · 坏帧 {} · 解码错误 {} · 丢队列 {} · 恢复请求 {}",s.lost.load(Relaxed),s.nack_requests.load(Relaxed),s.rtx_packets.load(Relaxed),s.reordered.load(Relaxed),s.late_packets.load(Relaxed),s.damaged_frames.load(Relaxed),s.errors.load(Relaxed),s.queue_drops.load(Relaxed),s.keyframe_requests.load(Relaxed));
            let recovery = format!(
                "恢复触发：接收损坏 {} · 解码积压 {} · 解码器错误 {} · 格式切换 {}",
                s.recovery_receive_damage.load(Relaxed),
                s.recovery_decode_queue.load(Relaxed),
                s.recovery_decoder_error.load(Relaxed),
                s.recovery_format_change.load(Relaxed)
            );
            let fec = if s.fec_enabled.load(Relaxed) || s.fec_packets.load(Relaxed) > 0 {
                format!(
                    "\nFEC XOR：校验 {} 包 · 恢复 {} 包 · 内容 {:.2} MiB · 无效/丢弃 {}",
                    s.fec_packets.load(Relaxed),
                    s.fec_recovered.load(Relaxed),
                    s.fec_bytes.load(Relaxed) as f64 / 1048576.,
                    s.fec_drops.load(Relaxed)
                )
            } else {
                String::new()
            };
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap;
            job.halign = egui::Align::Min;
            let mut append = |text: String, size: f32, color: Color32| {
                job.append(
                    &text,
                    0.,
                    egui::TextFormat {
                        font_id: FontId::proportional(size),
                        color,
                        ..Default::default()
                    },
                )
            };
            append(title, 14., Color32::from_rgb(236, 242, 248));
            append(
                format!("\n处理    {}", stages.join("  ·  ")),
                12.5,
                Color32::from_rgb(159, 231, 204),
            );
            append(
                format!("\n{network_line}\n{fps_line}"),
                12.,
                Color32::from_rgb(190, 208, 226),
            );
            if !queues.is_empty() {
                append(
                    format!("\n排队    {}", queues.join("  ·  ")),
                    12.,
                    Color32::from_rgb(168, 184, 202),
                );
            }
            if s.errors.load(Relaxed) > 0
                || s.damaged_frames.load(Relaxed) > 0
                || s.queue_drops.load(Relaxed) > 0
            {
                append(
                    format!(
                        "\n异常    坏帧 {}  ·  解码错误 {}  ·  队列丢帧 {}",
                        s.damaged_frames.load(Relaxed),
                        s.errors.load(Relaxed),
                        s.queue_drops.load(Relaxed)
                    ),
                    12.,
                    Color32::from_rgb(255, 197, 104),
                );
            }
            if s.waiting_for_keyframe.load(Relaxed) != 0 {
                job.append(
                    "\n正在恢复参考帧",
                    0.0,
                    egui::TextFormat {
                        font_id: FontId::proportional(12.0),
                        color: Color32::from_rgb(255, 197, 104),
                        ..Default::default()
                    },
                );
            }
            self.hud_cache.galley = Some(ui.fonts(|f| f.layout_job(job)));
            self.hud_cache.key = Some(key);
            self.hud_cache.updated = Some(std::time::Instant::now());
            self.hud_cache.detail=format!("{}\n输入通道：{} · 本地发送 {} 条 · 发送失败 {} 条\nCtrl+Alt+S：显示 / 隐藏统计\n统计每 500ms 更新；视频仍逐帧呈现\n处理行按采集、编码、解码、渲染排列，各项是不同阶段的窗口均值，不是同一帧的端到端测量，不相加为总延迟\n控制 RTT：CONTROL 应用消息往返均值 ± 标准差（最近 32 次），包含协议/排队/对端响应，不等于 ICMP ping\n待绘制：解码图像发布到 UI 取走的等待，最近 2 秒最多 120 次交付；被覆盖图像不计，超过 2 秒无图像不显示\n丢包率：最近 500ms 超时未修复的 RTP 包比例，重复 RTX 不计入分母\n接收、解码和绘制分别统计；绘制帧率不代表显示器扫描次数\nRender CPU：整轮 UI 准备及渲染提交时间，不含交换缓冲的 VSync 等待；驱动同步仍可能阻塞提交，不是纯 GPU 时间\nDecode：解码调用耗时；读回路径包含显存读回，GPU 共享路径的后续完成等待计入 Render\n主机采集、编码和发送等待来自最近 2 秒最多 120 次采样，超过 5 秒无更新不显示\n以下计数为本次会话累计：\n{counters}\n{recovery}{fec}",s.backend(),if self.input_ready(){"已打开"}else{"未打开"},s.input_messages_sent.load(Relaxed),s.input_send_errors.load(Relaxed));
            if let Some(input) = native.input {
                self.hud_cache.detail.push_str(&format!("\n输入确认 {:.1}ms RTT · 主机排队 {:.2}ms · 注入 {:.2}ms；确认表示主机完成注入，不表示应用已响应或画面已显示",input.confirmation_rtt_us as f32/1000.,input.host_queue_us as f32/1000.,input.host_inject_us as f32/1000.));
            }

            if let Some(us) = native.client_input_queue_us {
                self.hud_cache
                    .detail
                    .push_str(&format!("\n客户端输入队列：{:.2}ms", us as f32 / 1000.));
            }
            self.hud_cache.detail.push_str(&rtt_detail);
            self.hud_cache.detail.push_str(&rtt_tail);
            self.hud_cache.detail.push_str("\nP95：约95%的样本不超过此值；±为标准差，少数尖峰可使标准差大于均值，并非负延迟范围");
        }
        let Some(galley) = self.hud_cache.galley.clone() else {
            return;
        };
        let bounds = Rect::from_min_size(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0 - 12.0,
                rect.top() + 8.0,
            ),
            galley.size() + egui::vec2(24.0, 18.0),
        );
        ui.painter()
            .rect_filled(bounds, 8.0, Color32::from_black_alpha(205));
        ui.painter()
            .galley(bounds.min + egui::vec2(12.0, 9.0), galley, Color32::WHITE);
        ui.interact(bounds, ui.id().with("hud-details"), egui::Sense::hover())
            .on_hover_text(&self.hud_cache.detail);
    }
}

fn decoder_label(backend: &str) -> &str {
    if backend.starts_with("vaapi") {
        "VAAPI 硬解"
    } else if backend.starts_with("d3d11va") {
        "D3D11 硬解"
    } else if backend.starts_with("openh264") {
        "OpenH264 软解"
    } else if backend.is_empty() {
        "解码初始化中"
    } else {
        backend
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn abandoned_hardware_in_a_fallback_note_is_not_reported_as_active() {
        assert_eq!(
            super::decoder_label("vaapi (cros-codecs hw, /dev/dri/renderD128)"),
            "VAAPI 硬解"
        );
        assert_eq!(
            super::decoder_label("d3d11va (Media Foundation hw: GPU)"),
            "D3D11 硬解"
        );
        assert_eq!(
            super::decoder_label("openh264 [hardware vaapi abandoned: error]"),
            "OpenH264 软解"
        );
        assert_eq!(super::decoder_label(""), "解码初始化中");
    }
}
