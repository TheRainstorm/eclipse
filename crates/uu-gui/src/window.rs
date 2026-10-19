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

/// Fit the complete window inside 80% of the destination work area.
/// Preserve video aspect ratio, including when enlarging a small source.
fn stream_size(video_px: egui::Vec2, pixels_per_point: f32, available: egui::Vec2) -> egui::Vec2 {
    let available = available * 0.8;
    let video = video_px / pixels_per_point;
    let scale = (available.x / video.x)
        .min((available.y - CONNECTION_BAR_HEIGHT).max(1.0) / video.y);
    video * scale + egui::vec2(0.0, CONNECTION_BAR_HEIGHT)
}

#[derive(Default)]
pub struct WindowSizing {
    initialized: bool,
    stream_pending: bool,
    was_streaming: bool,
    canvas_px: Option<egui::Vec2>,
    home_points: Option<egui::Vec2>,
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
        native: Option<Geometry>,
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
        let Some(geometry) = native.or_else(|| Geometry::from_context(ctx)) else {
            return;
        };
        let Geometry {
            inner,
            outer,
            work,
            ppp,
        } = geometry;
        let chrome = (outer.size() - inner.size()).max(egui::Vec2::ZERO);
        let available = work.shrink(8.0);
        let capacity = available.size() - chrome;
        // eframe initially stores the primary monitor's minimum as physical
        // pixels. Refresh it after startup/DPI changes before requesting size.
        if !self.initialized
            || self.previous.is_some_and(|(old_ppp, _, old_work)| {
                (old_ppp - ppp).abs() > 0.01 || old_work != work
            })
        {
            let minimum = if streaming {
                egui::vec2(320.0, 200.0)
            } else {
                egui::vec2(560.0, 360.0)
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(minimum.min(capacity)));
        }
        if resize_started {
            self.resizing = true;
        }
        if self.resizing {
            if streaming {
                self.canvas_px =
                    Some((inner.size() - egui::vec2(0.0, CONNECTION_BAR_HEIGHT)) * ppp);
            } else {
                self.home_points = Some(inner.size());
            }
            if !ctx.input(|i| i.pointer.primary_down()) {
                self.resizing = false;
            }
        }
        let initial_stream = self.stream_pending && video_px.is_some();
        let desired = if initial_stream {
            self.stream_pending = false;
            self.canvas_px = video_px;
            Some(stream_size(video_px.unwrap(), ppp, capacity))
        } else if !self.initialized {
            self.home_points = Some(egui::vec2(1100.0, 760.0));
            Some(self.home_points.unwrap().min(capacity))
        } else if let Some((old_ppp, _, old_work)) = self.previous {
            if (old_ppp - ppp).abs() > 0.01 || old_work != work {
                // A monitor move must constrain position even if the size fits
                // (Win+Shift+Arrow can place an otherwise small window outside).
                Some(if streaming {
                    stream_size(video_px.or(self.canvas_px).unwrap_or(inner.size() * ppp), ppp, capacity)
                } else {
                    self.home_points.unwrap_or(inner.size()).min(capacity)
                })
            } else {
                None
            }
        } else {
            None
        };
        if let Some(size) = desired {
            let position = if initial_stream || !self.initialized || streaming {
                available.center() - (size + chrome) / 2.0
            } else {
                // Keep the user's placement while fitting the destination monitor.
                egui::pos2(
                    outer.left().clamp(
                        available.left(),
                        (available.right() - size.x - chrome.x).max(available.left()),
                    ),
                    outer.top().clamp(
                        available.top(),
                        (available.bottom() - size.y - chrome.y).max(available.top()),
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

/// Consistent geometry in current native DPI units. Never reconstruct physical
/// coordinates by multiplying an egui rect from a previous DPI event/pass.
#[derive(Clone, Copy)]
pub struct Geometry {
    pub inner: egui::Rect,
    pub outer: egui::Rect,
    pub work: egui::Rect,
    pub ppp: f32,
}
impl Geometry {
    fn from_context(ctx: &egui::Context) -> Option<Self> {
        let ppp = ctx.pixels_per_point();
        ctx.input(|i| {
            Some(Self {
                inner: i.viewport().inner_rect?,
                outer: i.viewport().outer_rect.or(i.viewport().inner_rect)?,
                work: egui::Rect::from_min_size(egui::Pos2::ZERO, i.viewport().monitor_size?),
                ppp,
            })
        })
    }
}

pub fn native_geometry(_frame: &eframe::Frame, _ctx: &egui::Context) -> Option<Geometry> {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(handle) = _frame.window_handle().ok()?.as_raw() else {
            return None;
        };
        #[repr(C)]
        #[derive(Default)]
        struct NativeRect {
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Point {
            x: i32,
            y: i32,
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
            fn MonitorFromWindow(window: isize, flags: u32) -> isize;
            fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
            fn GetWindowRect(window: isize, rect: *mut NativeRect) -> i32;
            fn GetClientRect(window: isize, rect: *mut NativeRect) -> i32;
            fn ClientToScreen(window: isize, point: *mut Point) -> i32;
            fn GetDpiForWindow(window: isize) -> u32;
        }
        let window = handle.hwnd.get();
        let mut outer = NativeRect::default();
        let mut client = NativeRect::default();
        let mut origin = Point::default();
        let mut info = MonitorInfo {
            size: std::mem::size_of::<MonitorInfo>() as u32,
            monitor: NativeRect::default(),
            work: NativeRect::default(),
            flags: 0,
        };
        // All queries use this exact HWND and the same native DPI coordinate space.
        let dpi = unsafe {
            if GetWindowRect(window, &mut outer) == 0
                || GetClientRect(window, &mut client) == 0
                || ClientToScreen(window, &mut origin) == 0
                || GetMonitorInfoW(MonitorFromWindow(window, 2), &mut info) == 0
            {
                return None;
            }
            GetDpiForWindow(window)
        };
        if dpi == 0 {
            return None;
        }
        let ppp = dpi as f32 / 96.0 * _ctx.zoom_factor();
        let to_rect = |r: NativeRect| {
            egui::Rect::from_min_max(
                egui::pos2(r.left as f32, r.top as f32),
                egui::pos2(r.right as f32, r.bottom as f32),
            ) / ppp
        };
        Some(Geometry {
            inner: egui::Rect::from_min_size(
                egui::pos2(origin.x as f32, origin.y as f32) / ppp,
                egui::vec2(client.right as f32, client.bottom as f32) / ppp,
            ),
            outer: to_rect(outer),
            work: to_rect(info.work),
            ppp,
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_fills_eighty_percent_with_aspect_ratio_at_every_dpi() {
        for ppp in [1.0, 1.25, 1.5, 2.0, 2.5] {
            for video in [egui::vec2(640.0,480.0),egui::vec2(3840.0,2160.0)] {
                let available = egui::vec2(1500.0,900.0);
                let size = stream_size(video,ppp,available);
                assert!(size.x <= 1200.01 && size.y <= 720.01);
                assert!((size.x - 1200.0).abs() < 0.01 || (size.y - 720.0).abs() < 0.01);
                assert!((size.x/(size.y-CONNECTION_BAR_HEIGHT)-video.x/video.y).abs() < 0.001);
            }
        }
    }
    #[test]
    fn stream_recenters_on_new_monitor_but_preserves_manual_size_on_same_monitor() {
        let ctx = egui::Context::default();
        let mut sizing = WindowSizing::default();
        sizing.begin_stream();
        let mut run = |work: egui::Rect, ppp: f32| {
            let inner = egui::Rect::from_min_size(work.min,egui::vec2(750.0,450.0));
            ctx.run(egui::RawInput::default(), |ctx| sizing.update(ctx,true,
                Some(egui::vec2(1920.0,1080.0)),false,Some(Geometry {inner,outer:inner,work,ppp})))
                .viewport_output[&egui::ViewportId::ROOT].commands.clone()
        };
        let work = egui::Rect::from_min_size(egui::pos2(-1920.0,0.0),egui::vec2(1920.0,1040.0));
        let commands = run(work,1.0);
        let size = stream_size(egui::vec2(1920.0,1080.0),1.0,work.shrink(8.0).size());
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(size)));
        assert!(commands.contains(&egui::ViewportCommand::OuterPosition(work.center()-size/2.0)));
        assert!(!run(work,1.0).iter().any(|c|matches!(c,egui::ViewportCommand::InnerSize(_))));
        let work = egui::Rect::from_min_size(egui::pos2(0.0,-1080.0),egui::vec2(1280.0,720.0));
        let commands = run(work,1.5);
        let size = stream_size(egui::vec2(1920.0,1080.0),1.5,work.shrink(8.0).size());
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(size)));
        assert!(commands.contains(&egui::ViewportCommand::OuterPosition(work.center()-size/2.0)));
    }
    #[test]
    fn launcher_uses_native_dpi_and_fits_position_on_negative_origin_monitor() {
        let ctx = egui::Context::default();
        let mut sizing = WindowSizing::default();
        let mut run = |geometry: Geometry| {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                sizing.update(ctx, false, None, false, Some(geometry));
            });
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .clone()
        };
        let first = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 760.0));
        run(Geometry {
            inner: first,
            outer: first,
            ppp: 2.0,
            work: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1536.0, 912.0)),
        });
        // Native monitor/DPI have changed, while egui still has the old frame's
        // coordinates. Window size and placement must use the native snapshot.
        let displaced =
            egui::Rect::from_min_size(egui::pos2(2000.0, -1060.0), egui::vec2(2200.0, 1520.0));
        let commands = run(Geometry {
            inner: displaced,
            outer: displaced,
            ppp: 1.0,
            work: egui::Rect::from_min_size(egui::pos2(580.0, -1080.0), egui::vec2(1920.0, 1032.0)),
        });
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(1100.0, 760.0))));
        assert!(
            commands.contains(&egui::ViewportCommand::MinInnerSize(egui::vec2(
                560.0, 360.0
            )))
        );
        assert!(
            commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
                1392.0, -1060.0
            )))
        );
        let small =
            egui::Rect::from_min_size(egui::pos2(-300.0, -200.0), egui::vec2(1100.0, 760.0));
        let commands = run(Geometry {
            inner: small,
            outer: small,
            ppp: 1.0,
            work: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1032.0)),
        });
        assert!(commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(8.0, 8.0))));
    }
}
