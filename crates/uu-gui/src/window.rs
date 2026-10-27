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

// Native size requests can update the API's cached size before the compositor
// applies them. Keep ownership through a short stable observation interval,
// including a late configure that restores a smaller size. Never block input
// or keep fighting an uncooperative window manager indefinitely.
struct SizeTransition {
    target: egui::Vec2,
    started: std::time::Instant,
    stable_since: Option<std::time::Instant>,
}
#[derive(Debug,PartialEq,Eq)]
enum SizeProgress { Retry, Waiting, Complete }
impl SizeTransition {
    fn new(target:egui::Vec2,now:std::time::Instant)->Self {Self{target,started:now,stable_since:None}}
    fn observe(&mut self,actual:egui::Vec2,now:std::time::Instant)->SizeProgress {
        if now.duration_since(self.started)>=std::time::Duration::from_secs(2) {return SizeProgress::Complete;}
        let delta=(actual-self.target).abs();
        if delta.x>1.0 || delta.y>1.0 {
            self.stable_since=None;return SizeProgress::Retry;
        }
        let stable=*self.stable_since.get_or_insert(now);
        if now.duration_since(stable)>=std::time::Duration::from_millis(500) {SizeProgress::Complete}else{SizeProgress::Waiting}
    }
}

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
    settings_page: bool,
    canvas_px: Option<egui::Vec2>,
    home_points: Option<egui::Vec2>,
    resizing: bool,
    previous: Option<(f32, egui::Vec2, egui::Rect)>,
    transition: Option<SizeTransition>,
}

impl WindowSizing {
    pub fn set_settings_page(&mut self, open: bool) {
        self.settings_page = open;
    }
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
        self.update_at(ctx,streaming,video_px,resize_started,native,std::time::Instant::now());
    }
    fn update_at(&mut self,ctx:&egui::Context,streaming:bool,video_px:Option<egui::Vec2>,
        resize_started:bool,native:Option<Geometry>,now:std::time::Instant) {
        let viewport = ctx.input(|i| i.viewport().clone());
        if viewport.minimized == Some(true) {
            return;
        }
        let video_layout = streaming && !self.settings_page;
        if video_layout != self.was_streaming {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(if video_layout {
                egui::vec2(320.0, 200.0)
            } else {
                egui::vec2(560.0, 360.0)
            }));
            self.was_streaming = video_layout;
        }
        if !streaming {
            self.stream_pending = false;
        }
        if viewport.fullscreen == Some(true) || viewport.maximized == Some(true) {
            self.transition=None;
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
            let minimum = if video_layout {
                egui::vec2(320.0, 200.0)
            } else {
                egui::vec2(560.0, 360.0)
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(minimum.min(capacity)));
        }
        if self.settings_page {
            self.transition=None;
            // A settings page uses the application minimum, even in a live
            // stream process. Keep pending video sizing until the user returns.
            let size=inner.size().max(egui::vec2(560.0,360.0).min(capacity)).min(capacity);
            if size!=inner.size() {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                let position=egui::pos2(
                    outer.left().clamp(available.left(),(available.right()-size.x-chrome.x).max(available.left())),
                    outer.top().clamp(available.top(),(available.bottom()-size.y-chrome.y).max(available.top())));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
            }
            self.previous=Some((ppp,size,work));
            self.initialized=true;
            return;
        }
        if resize_started {
            self.transition=None;
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
        let mut desired = if initial_stream {
            self.stream_pending = false;
            self.canvas_px = video_px;
            Some(stream_size(video_px.unwrap(), ppp, capacity))
        } else if !self.initialized {
            self.home_points = Some(egui::vec2(1100.0, 760.0));
            Some(self.home_points.unwrap().min(capacity))
        } else if let Some((old_ppp, _, old_work)) = self.previous {
            if (old_ppp - ppp).abs() > 0.01 || old_work != work
                || (!self.resizing && self.transition.is_none() && (inner.width() > capacity.x + 1.0 || inner.height() > capacity.y + 1.0)) {
                // A monitor move must constrain position even if the size fits
                // (Win+Shift+Arrow can place an otherwise small window outside).
                // A delayed compositor configure may restore the old size
                // after our resize request. Converge using observed geometry,
                // while preserving manual sizes that fit the current work area.
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
        if let Some(size)=desired {
            self.transition=Some(SizeTransition::new(size,now));
        } else if let Some(transition)=self.transition.as_mut() {
            match transition.observe(inner.size(),now) {
                SizeProgress::Retry=>desired=Some(transition.target),
                SizeProgress::Complete=>self.transition=None,
                SizeProgress::Waiting=>{},
            }
        }
        if self.transition.is_some() {ctx.request_repaint_after(std::time::Duration::from_millis(50));}
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
#[derive(Clone, Copy, PartialEq)]
pub struct Geometry {
    pub inner: egui::Rect,
    pub outer: egui::Rect,
    pub work: egui::Rect,
    pub ppp: f32,
}

/// Observe geometry independently of the user's requested window mode.
/// This never changes focus, fullscreen, styles or the display configuration.
#[derive(Clone, Copy, PartialEq)]
pub struct Snapshot {
    native: Option<Geometry>,
    logical: egui::Rect,
    ppp: f32,
    focused: bool,
    fullscreen: Option<bool>,
    maximized: Option<bool>,
}
impl Snapshot {
    pub fn new(ctx:&egui::Context,native:Option<Geometry>)->Self {
        let ppp=ctx.pixels_per_point();
        ctx.input(|i|Self{native,logical:i.screen_rect(),ppp,
            focused:i.focused,fullscreen:i.viewport().fullscreen,maximized:i.viewport().maximized})
    }
    pub fn report(&self,ctx:&egui::Context)->serde_json::Value {
        let rect=|r:egui::Rect|[r.left(),r.top(),r.right(),r.bottom()];
        #[cfg(windows)]
        let style=eframe::viewport_window(ctx).and_then(|w|{
            use raw_window_handle::{HasWindowHandle,RawWindowHandle};
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW,GWL_STYLE};
            let RawWindowHandle::Win32(handle)=w.window_handle().ok()?.as_raw() else{return None;};
            Some(unsafe{GetWindowLongPtrW(handle.hwnd.get() as _,GWL_STYLE)} as u32)
        });
        #[cfg(not(windows))]
        let style:Option<u32>={let _=ctx;None};
        serde_json::json!({"event":"window_geometry","focused":self.focused,
            "fullscreen":self.fullscreen,"maximized":self.maximized,"pixels_per_point":self.ppp,
            "logical_screen_rect":rect(self.logical),"native_style":style,
            "native_geometry":self.native.map(|g|serde_json::json!({
                "inner":rect(g.inner),"outer":rect(g.outer),"work":rect(g.work),"pixels_per_point":g.ppp,
            }))})
    }
}
impl Geometry {
    fn from_context(ctx: &egui::Context) -> Option<Self> {
        let ppp = ctx.pixels_per_point();
        // Wayland deliberately does not expose global window positions.
        // Its local viewport still has a valid size; sizing must not depend
        // on an outer/inner position query succeeding.
        let local = ctx.screen_rect();
        ctx.input(|i| {
            let inner = i.viewport().inner_rect.unwrap_or(local);
            Some(Self {
                inner,
                outer: i.viewport().outer_rect.unwrap_or(inner),
                work: egui::Rect::from_min_size(egui::Pos2::ZERO, i.viewport().monitor_size?),
                ppp,
            })
        })
    }
}

pub fn native_geometry(_ctx: &egui::Context) -> Option<Geometry> {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let native=eframe::viewport_window(_ctx)?;
        let RawWindowHandle::Win32(handle) = native.window_handle().ok()?.as_raw() else {
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
    fn settings_use_application_minimum_and_preserve_pending_stream_fit() {
        let ctx=egui::Context::default();
        let mut sizing=WindowSizing::default();
        sizing.begin_stream();
        sizing.set_settings_page(true);
        let run=|sizing:&mut WindowSizing,size,video|{
            let inner=egui::Rect::from_min_size(egui::pos2(40.0,40.0),size);
            let geometry=Geometry {inner,outer:inner,ppp:1.0,
                work:egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1920.0,1080.0))};
            ctx.run(Default::default(),|ctx|sizing.update(ctx,true,video,false,Some(geometry)))
                .viewport_output[&egui::ViewportId::ROOT].commands.clone()
        };
        let commands=run(&mut sizing,egui::vec2(320.0,200.0),None);
        assert!(commands.contains(&egui::ViewportCommand::MinInnerSize(egui::vec2(560.0,360.0))));
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(560.0,360.0))));
        assert!(sizing.stream_pending);
        sizing.set_settings_page(false);
        let commands=run(&mut sizing,egui::vec2(560.0,360.0),Some(egui::vec2(1280.0,720.0)));
        assert!(commands.contains(&egui::ViewportCommand::MinInnerSize(egui::vec2(320.0,200.0))));
        assert!(commands.iter().any(|command|matches!(command,egui::ViewportCommand::InnerSize(size) if size.x>1200.0)));
        assert!(!sizing.stream_pending);
    }
    #[test]
    fn wayland_sizing_does_not_require_global_window_position() {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 760.0)));
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().monitor_size = Some(egui::vec2(1600.0, 1000.0));
        let mut sizing = WindowSizing::default();
        sizing.begin_stream();
        let output = ctx.run(raw, |ctx| {
            sizing.update(ctx, true, Some(egui::vec2(1920.0, 1080.0)), false, None);
        });
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.iter().any(|command| matches!(command, egui::ViewportCommand::InnerSize(size) if size.x > 1200.0 && size.x < 1280.0 && size.y < 800.0)));
    }

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
        let mut run = |work: egui::Rect, ppp: f32, manual:bool| {
            let inner = egui::Rect::from_min_size(work.min,egui::vec2(750.0,450.0));
            ctx.run(egui::RawInput::default(), |ctx| sizing.update(ctx,true,
                Some(egui::vec2(1920.0,1080.0)),manual,Some(Geometry {inner,outer:inner,work,ppp})))
                .viewport_output[&egui::ViewportId::ROOT].commands.clone()
        };
        let work = egui::Rect::from_min_size(egui::pos2(-1920.0,0.0),egui::vec2(1920.0,1040.0));
        let commands = run(work,1.0,false);
        let size = stream_size(egui::vec2(1920.0,1080.0),1.0,work.shrink(8.0).size());
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(size)));
        assert!(commands.contains(&egui::ViewportCommand::OuterPosition(work.center()-size/2.0)));
        assert!(!run(work,1.0,true).iter().any(|c|matches!(c,egui::ViewportCommand::InnerSize(_))));
        let work = egui::Rect::from_min_size(egui::pos2(0.0,-1080.0),egui::vec2(1280.0,720.0));
        let commands = run(work,1.5,false);
        let size = stream_size(egui::vec2(1920.0,1080.0),1.5,work.shrink(8.0).size());
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(size)));
        assert!(commands.contains(&egui::ViewportCommand::OuterPosition(work.center()-size/2.0)));
    }
    #[test]
    fn late_large_and_small_configures_converge_before_releasing_size_ownership() {
        use std::time::{Duration,Instant};
        for streaming in [false,true] {
            let ctx=egui::Context::default();let mut sizing=WindowSizing::default();
            if streaming {sizing.begin_stream();}
            let started=Instant::now();
            let mut run=|work:egui::Rect,ppp:f32,size:egui::Vec2,ms:u64,manual:bool| {
                let inner=egui::Rect::from_min_size(work.min,size);
                ctx.run(egui::RawInput::default(),|ctx|sizing.update_at(ctx,streaming,
                    streaming.then_some(egui::vec2(1280.,720.)),manual,Some(Geometry{inner,outer:inner,work,ppp}),started+Duration::from_millis(ms)))
                    .viewport_output[&egui::ViewportId::ROOT].commands.clone()
            };
            let requested=|commands:Vec<egui::ViewportCommand>|commands.into_iter().find_map(|c|if let egui::ViewportCommand::InnerSize(s)=c{Some(s)}else{None});
            let low=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1600.,1000.));
            let large=requested(run(low,1.,egui::vec2(1100.,760.),0,false)).unwrap();
            assert!(requested(run(low,1.,large,100,false)).is_none());
            assert!(requested(run(low,1.,large,700,false)).is_none());
            let high=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(853.,533.));
            let small=requested(run(high,1.5,large,1000,false)).unwrap();
            assert!(requested(run(high,1.5,small,1100,false)).is_none());
            assert_eq!(requested(run(high,1.5,large,1200,false)),Some(small));
            assert!(requested(run(high,1.5,small,1300,false)).is_none());
            assert!(requested(run(high,1.5,small,1900,false)).is_none());
            let restored=requested(run(low,1.,small,2000,false)).unwrap();
            assert_eq!(restored,large);
            assert!(requested(run(low,1.,large,2100,false)).is_none());
            // A smaller stale size fits the new monitor but still belongs to
            // the previous configure; accepting it would cancel enlargement.
            assert_eq!(requested(run(low,1.,small,2200,false)),Some(large));
            assert!(requested(run(low,1.,large,2300,false)).is_none());
            assert!(requested(run(low,1.,large,2900,false)).is_none());
            let manual=low.shrink(8.).size()*0.95;
            assert!(requested(run(low,1.,manual,3000,false)).is_none());
            run(high,1.5,large,3100,false);
            assert!(requested(run(high,1.5,high.shrink(8.).size()*0.95,3200,true)).is_none(),"explicit resizing takes ownership immediately");
        }
    }
    #[test]
    fn an_uncooperative_compositor_cannot_hold_size_ownership_forever() {
        use std::time::{Duration,Instant};
        let now=Instant::now();let mut state=SizeTransition::new(egui::vec2(670.,413.),now);
        assert_eq!(state.observe(egui::vec2(1267.,749.),now),SizeProgress::Retry);
        assert_eq!(state.observe(egui::vec2(1267.,749.),now+Duration::from_secs(2)),SizeProgress::Complete);
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
