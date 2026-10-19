//! Native resize gestures for our undecorated window.
use eframe::egui::{self, viewport::ResizeDirection as D, CursorIcon as C};

pub fn resize_border(ctx: &egui::Context, fullscreen: bool) -> bool {
    if fullscreen || ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return false;
    }
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
        return false;
    };
    let r = ctx.screen_rect();
    if !r.contains(p) {
        return false;
    }
    let edge = 6.0;
    let corner = 16.0;
    let left = p.x < r.left() + edge;
    let right = p.x > r.right() - edge;
    let top = p.y < r.top() + edge;
    let bottom = p.y > r.bottom() - edge;
    let direction = if (left && p.y < r.top() + corner) || (top && p.x < r.left() + corner) {
        Some((D::NorthWest, C::ResizeNwSe))
    } else if (right && p.y < r.top() + corner) || (top && p.x > r.right() - corner) {
        Some((D::NorthEast, C::ResizeNeSw))
    } else if (left && p.y > r.bottom() - corner) || (bottom && p.x < r.left() + corner) {
        Some((D::SouthWest, C::ResizeNeSw))
    } else if (right && p.y > r.bottom() - corner) || (bottom && p.x > r.right() - corner) {
        Some((D::SouthEast, C::ResizeNwSe))
    } else if left {
        Some((D::West, C::ResizeHorizontal))
    } else if right {
        Some((D::East, C::ResizeHorizontal))
    } else if top {
        Some((D::North, C::ResizeVertical))
    } else if bottom {
        Some((D::South, C::ResizeVertical))
    } else {
        None
    };
    if let Some((direction, cursor)) = direction {
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
        return true;
    }
    false
}

pub const CONNECTION_BAR_HEIGHT: f32 = 36.0;

/// Size the video in physical pixels; egui/native viewport commands use points.
/// Preserve aspect ratio when the current work area cannot hold a 1:1 image.
fn stream_size(video_px: egui::Vec2, pixels_per_point: f32, available: egui::Vec2) -> egui::Vec2 {
    let video = video_px / pixels_per_point;
    let scale = (available.x / video.x)
        .min((available.y - CONNECTION_BAR_HEIGHT).max(1.0) / video.y)
        .min(1.0);
    video * scale + egui::vec2(0.0, CONNECTION_BAR_HEIGHT)
}

#[derive(Default)]
pub struct WindowSizing {
    initialized: bool,
    stream_pending: bool,
    was_streaming: bool,
    canvas_px: Option<egui::Vec2>,
    resizing: bool,
    previous: Option<(f32, egui::Vec2, egui::Rect)>,
}

impl WindowSizing {
    pub fn begin_stream(&mut self) {
        self.stream_pending = true;
    }

    pub fn update(
        &mut self,
        ctx: &egui::Context,
        streaming: bool,
        video_px: Option<egui::Vec2>,
        resize_started: bool,
    ) {
        let viewport = ctx.input(|i| i.viewport().clone());
        if viewport.minimized == Some(true) {
            return;
        }
        if streaming != self.was_streaming {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(if streaming {
                egui::vec2(320.0, 200.0)
            } else {
                egui::vec2(560.0, 360.0)
            }));
            self.was_streaming = streaming;
        }
        if !streaming {
            self.stream_pending = false;
        }
        if viewport.fullscreen == Some(true) || viewport.maximized == Some(true) {
            return;
        }
        let Some(inner) = viewport.inner_rect else {
            return;
        };
        let Some(work) = work_area(ctx) else { return };
        let available = work.shrink(8.0);
        let ppp = ctx.pixels_per_point();
        if streaming && resize_started {
            self.resizing = true;
        }
        if self.resizing {
            self.canvas_px = Some((inner.size() - egui::vec2(0.0, CONNECTION_BAR_HEIGHT)) * ppp);
            if !ctx.input(|i| i.pointer.primary_down()) {
                self.resizing = false;
            }
        }
        let initial_stream = self.stream_pending && video_px.is_some();
        let desired = if initial_stream {
            self.stream_pending = false;
            self.canvas_px = video_px;
            Some(stream_size(video_px.unwrap(), ppp, available.size()))
        } else if !self.initialized {
            Some(egui::vec2(1100.0, 760.0).min(available.size()))
        } else if let Some((old_ppp, _, old_work)) = self.previous {
            if streaming && ((old_ppp - ppp).abs() > 0.01 || old_work != work) {
                // Keep the preferred physical canvas even when a smaller
                // monitor temporarily forces it to fit; restore it on return.
                self.canvas_px
                    .map(|canvas| stream_size(canvas, ppp, available.size()))
            } else if old_work != work
                && (inner.width() > available.width() || inner.height() > available.height())
            {
                Some(inner.size().min(available.size()))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(size) = desired {
            let position = if initial_stream || !self.initialized {
                available.center() - size / 2.0
            } else {
                // Keep the user's placement while fitting the destination monitor.
                egui::pos2(
                    inner.left().clamp(
                        available.left(),
                        (available.right() - size.x).max(available.left()),
                    ),
                    inner.top().clamp(
                        available.top(),
                        (available.bottom() - size.y).max(available.top()),
                    ),
                )
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
            self.previous = Some((ppp, size, work));
        } else {
            self.previous = Some((ppp, inner.size(), work));
        }
        self.initialized = true;
    }
}

/// Actual work area, excluding the Windows taskbar, in viewport point units.
fn work_area(ctx: &egui::Context) -> Option<egui::Rect> {
    #[cfg(target_os = "windows")]
    {
        #[repr(C)]
        #[derive(Default)]
        struct NativeRect {
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
        }
        #[repr(C)]
        struct MonitorInfo {
            size: u32,
            monitor: NativeRect,
            work: NativeRect,
            flags: u32,
        }
        #[link(name = "user32")]
        extern "system" {
            fn MonitorFromRect(rect: *const NativeRect, flags: u32) -> isize;
            fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
        }
        let outer = ctx.input(|i| i.viewport().outer_rect)? * ctx.pixels_per_point();
        let rect = NativeRect {
            left: outer.left() as i32,
            top: outer.top() as i32,
            right: outer.right() as i32,
            bottom: outer.bottom() as i32,
        };
        let mut info = MonitorInfo {
            size: std::mem::size_of::<MonitorInfo>() as u32,
            monitor: NativeRect::default(),
            work: NativeRect::default(),
            flags: 0,
        };
        // Only copies monitor metadata into a correctly sized C-layout structure.
        if unsafe { GetMonitorInfoW(MonitorFromRect(&rect, 2), &mut info) } != 0 {
            return Some(
                egui::Rect::from_min_max(
                    egui::pos2(info.work.left as f32, info.work.top as f32),
                    egui::pos2(info.work.right as f32, info.work.bottom as f32),
                ) / ctx.pixels_per_point(),
            );
        }
    }
    ctx.input(|i| {
        i.viewport()
            .monitor_size
            .map(|size| egui::Rect::from_min_size(egui::Pos2::ZERO, size))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_stream_is_one_physical_pixel_per_video_pixel_at_each_dpi() {
        for ppp in [1.0, 1.25, 1.5, 2.0, 2.5] {
            let size = stream_size(egui::vec2(1920.0, 1080.0), ppp, egui::vec2(3000.0, 2000.0));
            assert_eq!(size.x * ppp, 1920.0);
            assert_eq!((size.y - CONNECTION_BAR_HEIGHT) * ppp, 1080.0);
        }
    }
    #[test]
    fn oversized_stream_fits_monitor_without_aspect_distortion() {
        let available = egui::vec2(1500.0, 900.0);
        let size = stream_size(egui::vec2(3840.0, 2160.0), 2.0, available);
        assert!(size.x <= available.x && size.y <= available.y);
        assert!((size.x / (size.y - CONNECTION_BAR_HEIGHT) - 16.0 / 9.0).abs() < 0.001);
        let low_res = stream_size(egui::vec2(640.0, 480.0), 2.0, available);
        assert_eq!(low_res, egui::vec2(320.0, 276.0)); // Never enlarge small streams.
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn viewport_commands_center_first_frame_preserve_pixels_across_dpi_and_allow_manual_resize() {
        let ctx = egui::Context::default();
        let mut sizing = WindowSizing::default();
        sizing.begin_stream();
        let mut run = |ppp: f32, size: egui::Vec2, monitor: egui::Vec2, resize_started: bool| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            };
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.native_pixels_per_point = Some(ppp);
            viewport.monitor_size = Some(monitor);
            viewport.inner_rect = input.screen_rect;
            let output = ctx.run(input, |ctx| {
                sizing.update(ctx, true, Some(egui::vec2(1920.0, 1080.0)), resize_started)
            });
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .clone()
        };
        let commands = run(
            2.0,
            egui::vec2(1100.0, 760.0),
            egui::vec2(1536.0, 936.0),
            false,
        );
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(960.0, 576.0))));
        assert!(
            commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
                288.0, 180.0
            )))
        );
        let commands = run(
            1.0,
            egui::vec2(960.0, 576.0),
            egui::vec2(3072.0, 1872.0),
            false,
        );
        assert!(
            commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(
                1920.0, 1116.0
            )))
        );
        let commands = run(
            1.0,
            egui::vec2(1500.0, 900.0),
            egui::vec2(3072.0, 1872.0),
            false,
        );
        assert!(!commands
            .iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::InnerSize(_))));
        // A small destination fits the stream temporarily; returning restores
        // the preferred physical canvas instead of accumulating shrinkage.
        let commands = run(
            1.0,
            egui::vec2(1500.0, 900.0),
            egui::vec2(1920.0, 1032.0),
            false,
        );
        assert!(commands
            .iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::InnerSize(size) if size.y <= 1016.0)));
        let commands = run(
            2.0,
            egui::vec2(900.0, 520.0),
            egui::vec2(1536.0, 936.0),
            false,
        );
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(960.0, 576.0))));
        run(
            2.0,
            egui::vec2(750.0, 450.0),
            egui::vec2(1536.0, 936.0),
            true,
        );
        let commands = run(
            1.0,
            egui::vec2(750.0, 450.0),
            egui::vec2(3072.0, 1872.0),
            false,
        );
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(1500.0, 864.0))));
    }
}
