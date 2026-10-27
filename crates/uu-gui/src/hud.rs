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
            let partial = if fresh {
                wait.map(|q| {
                    format!(
                        "≈{:.1}ms",
                        s.rtt_us.load(Relaxed) as f32 / 1000.0 + s.decode_ms() + q + self.render_ms
                    )
                })
            } else {
                None
            }
            .unwrap_or_else(|| "—".into());
            let native = s.native_timing.snapshot();
            let host_video = native.host.and_then(|host| host.video);
            let decode_queue = s
                .frame_cadence
                .lock()
                .unwrap()
                .decode_queue_ms
                .map(|v| v[0] as f32);
            let native_pipeline = if fresh {
                host_video.and_then(|host| {
                    wait.zip(decode_queue).map(|(wait, queue)| {
                        (host.capture_us as f32 + host.encode_us as f32 + host.send_wait_us as f32)
                            / 1000.0
                            + s.rtt_us.load(Relaxed) as f32 / 2000.0
                            + queue
                            + s.decode_ms()
                            + wait
                            + self.render_ms
                    })
                })
            } else {
                None
            };
            let pipeline_line = if host_video.is_some() {
                let value = native_pipeline
                    .map(|ms| format!("≈{ms:.1}ms"))
                    .unwrap_or_else(|| "—".into());
                format!("阶段均值合计 {value}（非端到端延迟）· 待绘制 {queue} · 输入到光子延迟未测")
            } else {
                format!("已知段合计 {partial} · 待绘制 {queue} · 总延迟待测（远端/呈现未计）")
            };
            let native_line = host_video
                .map(|host| {
                    let input = native
                        .input
                        .map(|v| {
                            format!(
                                "输入确认 {:.1}ms RTT · 主机排队 {:.2}ms · 注入 {:.2}ms",
                                v.confirmation_rtt_us as f32 / 1000.,
                                v.host_queue_us as f32 / 1000.,
                                v.host_inject_us as f32 / 1000.
                            )
                        })
                        .unwrap_or_else(|| "输入确认 —".into());
                    format!(
                        "\n主机采集 {:.2}ms · 编码 {:.2}ms · 发送等待 {:.2}ms | {input}",
                        host.capture_us as f32 / 1000.,
                        host.encode_us as f32 / 1000.,
                        host.send_wait_us as f32 / 1000.
                    )
                })
                .unwrap_or_default();
            let pipeline_help = if host_video.is_some() {
                "原生已知画面段估计：主机采集调用 + 编码调用 + 编码端发送等待 + 控制RTT/2 + 解码前排队 + Decode + 待绘制 + Render；网络单程假设往返对称，各阶段窗口均值近似相加，未按同一帧关联。还缺视频组帧、应用响应、VSync/桌面合成/扫描与显示器处理，不能当作鼠标到光子的总延迟。\n输入确认RTT：抽样输入发出至主机实际注入完成再返回的往返时间；主机排队/注入是该抽样事件的分段。它不代表应用已处理事件或画面已显示。主机阶段采样为最近2秒最多120次，超过5秒无更新显示未知。"
            } else {
                "已知段合计：控制 RTT + Decode + 待绘制 + Render，窗口均值近似相加，非同一输入/帧的全程测量，也非严格下界。尚缺输入等待、远端捕获/编码、视频组帧/解码前排队、VSync/桌面合成/扫描等，不能据此推算完整总延迟。"
            };
            // This sum is an approximation across different sample populations,
            // not an input-correlated end-to-end measurement or strict lower bound.
            // Render is whole UI-frame CPU preparation/submission wall time.
            // It excludes SwapBuffers, but GL calls may still block in the driver.
            let fec_line = if s.fec_enabled.load(Relaxed) || s.fec_packets.load(Relaxed)>0 {
                format!("\nFEC XOR · 校验 {} 包 · 恢复 {} 包 · 校验内容 {:.2} MiB · 无效/丢弃 {}",s.fec_packets.load(Relaxed),s.fec_recovered.load(Relaxed),s.fec_bytes.load(Relaxed) as f64/1048576.,s.fec_drops.load(Relaxed))
            } else {String::new()};
            let text = format!(
            "{w}×{h}@{rd:.0}   {codec} · {decoder}   目标 {} FPS{source}   FPS {:.1} Rx · {:.1} De · {rd:.1} Rd\n\
             控制 RTT {network}{rtt_tail}   Loss {:.2}%   Bandwidth {:.2} Mbps | Render CPU {:.2}ms · Decode {:.2}ms\n\
             {pipeline_line}{native_line}\n\
             丢包 {} · NACK {} · RTX {} · 乱序 {} · 迟到 {} · 坏帧 {} · 解码错误 {} · 丢队列 {} · 恢复请求 {}\n\
             恢复触发：接收损坏 {} · 解码积压 {} · 解码器错误 {} · 格式切换 {}{fec_line}",
            target_fps, s.src_fps(), s.dec_fps(), s.loss_ppm.load(Relaxed) as f32 / 10000.0,
            s.bitrate_kbps.load(Relaxed) as f32 / 1000.0, self.render_ms, s.decode_ms(),
            s.lost.load(Relaxed), s.nack_requests.load(Relaxed), s.rtx_packets.load(Relaxed),
            s.reordered.load(Relaxed), s.late_packets.load(Relaxed), s.damaged_frames.load(Relaxed),
            s.errors.load(Relaxed), s.queue_drops.load(Relaxed), s.keyframe_requests.load(Relaxed),
            s.recovery_receive_damage.load(Relaxed),s.recovery_decode_queue.load(Relaxed),s.recovery_decoder_error.load(Relaxed),s.recovery_format_change.load(Relaxed));
            let mut job = egui::text::LayoutJob::simple(
                text,
                FontId::proportional(12.0),
                Color32::from_rgb(174, 238, 219),
                wrap,
            );
            job.halign = egui::Align::Center;
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
            self.hud_cache.detail=format!("{}\n输入通道：{} · 本地发送 {} 条 · 发送失败 {} 条\nCtrl+Alt+S：显示 / 隐藏统计\n统计每 500ms 更新；视频仍逐帧呈现\n控制 RTT：CONTROL 应用消息往返均值 ± 标准差（最近 32 次），包含协议/排队/对端响应，不等于 ICMP ping；不能用 ping 替换实际应用等待\n{pipeline_help}\n待绘制：解码图像发布到 UI 取走的等待，最近 2 秒最多 120 次交付；被覆盖图像不计，超过 2 秒无图像显示未知\nLoss：最近 500ms 超时未修复的 RTP 包比例，重复 RTX 不计入分母\n其余包、帧、请求计数为本次会话累计值\nRd：每秒提交的新画面数，不是显示器扫描次数\nRender：整轮 UI 准备及渲染提交 CPU 时间，不含交换缓冲的 VSync 等待，但驱动同步仍可能阻塞提交\nDecode：解码调用耗时；读回路径包含显存读回，GPU 共享路径的完成等待计入后续 Render，均不代表端到端延迟",s.backend(),if self.input_ready(){"已打开"}else{"未打开"},s.input_messages_sent.load(Relaxed),s.input_send_errors.load(Relaxed));
            if let Some(us)=native.client_input_queue_us {self.hud_cache.detail.push_str(&format!("\n客户端输入队列：{:.2}ms",us as f32/1000.));}
        self.hud_cache.detail.push_str(&rtt_detail);
            self.hud_cache.detail.push_str("\nP95：约95%的样本不超过此值；±为标准差，少数尖峰可使标准差大于均值，并非负延迟范围");
        }
        let Some(galley) = self.hud_cache.galley.clone() else {
            return;
        };
        let bounds = Rect::from_min_size(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0 - 8.0,
                rect.top() + 8.0,
            ),
            galley.size() + egui::vec2(16.0, 10.0),
        );
        ui.painter()
            .rect_filled(bounds, 6.0, Color32::from_black_alpha(180));
        ui.painter().galley(
            egui::pos2(rect.center().x, bounds.top() + 5.0),
            galley,
            Color32::WHITE,
        );
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
