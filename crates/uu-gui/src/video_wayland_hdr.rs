//! A normal stream viewport owns its Wayland color description and render targets.
//! The default framebuffer remains the SDR path while output discovery is pending.
use eframe::{
    egui,
    glow_presentation::{self, ColorTarget, Presenter},
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Weak},
};
use winit::window::Window;

type Ptr = *mut c_void;
unsafe extern "C" {
    fn eclipse_pq_open(display: Ptr, surface: Ptr) -> Ptr;
    fn eclipse_pq_output_status(child: Ptr) -> i32;
    fn eclipse_pq_has_presented(child: Ptr) -> i32;
    fn eclipse_pq_admit_gpu(child: Ptr, vsync: i32, width: i32, height: i32) -> i32;
    fn eclipse_pq_submit_gpu(
        child: Ptr,
        composer: Ptr,
        white: f32,
        width: i32,
        height: i32,
        logical_width: i32,
        logical_height: i32,
    ) -> i32;
    fn eclipse_pq_hide(child: Ptr);
    fn eclipse_pq_close(child: Ptr);
    fn eclipse_compose_new() -> Ptr;
    fn eclipse_compose_begin(composer: Ptr, width: i32, height: i32) -> i32;
    fn eclipse_compose_finish(composer: Ptr);
    fn eclipse_compose_free(composer: Ptr);
    fn eclipse_compose_swap(interval: i32) -> i32;
}

const WHITE: f32 = 203.0;
const SDR: ColorTarget = ColorTarget {
    hdr: false,
    sdr_white_nits: WHITE,
};

struct Output {
    window: Arc<Window>,
    child: NonNull<c_void>,
    composer: Option<NonNull<c_void>>,
    eligible: bool,
    drawing_hdr: bool,
    deferred: bool,
    size: [u32; 2],
    parent_interval: Option<bool>,
}
// eframe's registry is shared, but only the owning UI thread with this
// viewport's EGL context current may draw or retire its GL resources.
unsafe impl Send for Output {}

#[derive(Clone)]
struct Registration {
    window: Weak<Window>,
    installed: bool,
}

pub(super) fn update(ctx: &egui::Context, active: bool, query: bool, vsync: bool) -> Option<bool> {
    let viewport = ctx.viewport_id();
    let registration = egui::Id::new(("wayland-hdr-output", viewport));
    let registered = ctx.data(|data| data.get_temp::<Registration>(registration));
    if !active {
        if registered.as_ref().is_some_and(|r| r.installed) {
            glow_presentation::set_active(ctx, viewport, false, vsync);
        }
        // A failed attempt can be revisited on the next connection.
        if registered.is_some() && glow_presentation::available_color_for(ctx, viewport).is_none() {
            ctx.data_mut(|data| data.remove::<Registration>(registration));
        }
        return Some(false);
    }
    let window = eframe::viewport_window_for(ctx, viewport)?;
    let identity = Arc::downgrade(&window);
    if !registered
        .as_ref()
        .is_some_and(|r| r.window.ptr_eq(&identity))
    {
        let output = Output::new(window);
        ctx.data_mut(|data| {
            data.insert_temp(
                registration,
                Registration {
                    window: identity,
                    installed: output.is_some(),
                },
            )
        });
        let Some(output) = output else {
            return Some(false);
        };
        glow_presentation::install(ctx, Box::new(output), vsync, SDR);
    }
    glow_presentation::set_active(ctx, viewport, true, vsync);
    if query {
        glow_presentation::verified_hdr_output(ctx, viewport)
    } else {
        None
    }
}

impl Output {
    fn new(window: Arc<Window>) -> Option<Self> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
            return None;
        };
        let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        let child = NonNull::new(unsafe {
            eclipse_pq_open(display.display.as_ptr(), handle.surface.as_ptr())
        })?;
        Some(Self {
            window,
            child,
            composer: None,
            eligible: false,
            drawing_hdr: false,
            deferred: false,
            size: [0; 2],
            parent_interval: None,
        })
    }
}
impl Presenter for Output {
    fn verified_hdr_output(&mut self) -> Option<bool> {
        let child = self.child.as_ptr();
        match unsafe { eclipse_pq_output_status(child) } {
            1 if unsafe { eclipse_pq_has_presented(child) } != 0 => Some(true),
            0 | 1 => None,
            _ => Some(false),
        }
    }
    fn prepare_frame(&mut self, size: [u32; 2], vsync: bool) -> Result<bool, String> {
        self.deferred = false;
        let result = unsafe {
            eclipse_pq_admit_gpu(
                self.child.as_ptr(),
                i32::from(vsync),
                size[0] as i32,
                size[1] as i32,
            )
        };
        self.eligible = result == 1 && !size.contains(&0);
        match result {
            0 | 1 => Ok(true),
            2 => Ok(false),
            _ => Err("Wayland HDR output unavailable".into()),
        }
    }
    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String> {
        self.drawing_hdr = false;
        self.size = size;
        if !self.eligible {
            return Ok(SDR);
        }
        let composer = match self.composer {
            Some(existing) => existing,
            None => {
                let new = NonNull::new(unsafe { eclipse_compose_new() })
                    .ok_or("HDR scene initialization failed")?;
                self.composer = Some(new);
                new
            }
        };
        if unsafe { eclipse_compose_begin(composer.as_ptr(), size[0] as i32, size[1] as i32) } == 0
        {
            return Err("HDR framebuffer unavailable".into());
        }
        self.drawing_hdr = true;
        Ok(ColorTarget {
            hdr: true,
            sdr_white_nits: WHITE,
        })
    }
    fn present(&mut self, vsync: bool) -> Result<(), String> {
        let mut covered = false;
        if self.drawing_hdr {
            let composer = self.composer.expect("begin acquired a composer").as_ptr();
            let logical = winit::dpi::PhysicalSize::new(self.size[0], self.size[1])
                .to_logical::<u32>(self.window.scale_factor());
            let result = unsafe {
                eclipse_pq_submit_gpu(
                    self.child.as_ptr(),
                    composer,
                    WHITE,
                    self.size[0] as i32,
                    self.size[1] as i32,
                    logical.width as i32,
                    logical.height as i32,
                )
            };
            self.finish();
            match result {
                1 => covered = true,
                0 => self.deferred = true,
                _ => return Err("Wayland HDR buffer submission failed".into()),
            }
        }
        // Waiting on an occluded parent's frame callback can stall its entire
        // event loop. The visible child applies the requested VSync itself.
        let parent_vsync = vsync && !covered && !self.deferred;
        let interval = if self.parent_interval == Some(parent_vsync) {
            -1
        } else {
            i32::from(parent_vsync)
        };
        if unsafe { eclipse_compose_swap(interval) } == 0 {
            return Err("Wayland viewport swap failed".into());
        }
        self.parent_interval = Some(parent_vsync);
        Ok(())
    }
    fn present_deferred(&self) -> bool {
        self.deferred
    }
    fn finish(&mut self) {
        if self.drawing_hdr {
            if let Some(composer) = self.composer {
                unsafe {
                    eclipse_compose_finish(composer.as_ptr());
                }
            }
            self.drawing_hdr = false;
        }
    }
    fn deactivate(&mut self) -> Result<(), String> {
        unsafe {
            eclipse_pq_hide(self.child.as_ptr());
        }
        self.parent_interval = None;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.finish();
        unsafe {
            eclipse_pq_close(self.child.as_ptr());
            if let Some(composer) = self.composer {
                eclipse_compose_free(composer.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI8, Ordering};

    struct SessionOutput(Arc<AtomicI8>);
    impl Presenter for SessionOutput {
        fn verified_hdr_output(&mut self) -> Option<bool> {
            match self.0.load(Ordering::Acquire) {
                1 => Some(true),
                0 => None,
                _ => Some(false),
            }
        }
        fn begin(&mut self, _: [u32; 2]) -> Result<ColorTarget, String> {
            Ok(SDR)
        }
        fn present(&mut self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn finish(&mut self) {}
        fn deactivate(&mut self) -> Result<(), String> {
            self.0.store(0, Ordering::Release);
            Ok(())
        }
    }

    #[test]
    fn hdr_evidence_follows_the_active_viewport_and_expires_on_hide() {
        let ctx = egui::Context::default();
        let other = egui::Context::default();
        let viewport = ctx.viewport_id();
        let status = Arc::new(AtomicI8::new(0));
        glow_presentation::install(&ctx, Box::new(SessionOutput(status.clone())), true, SDR);
        glow_presentation::set_active(&ctx, viewport, true, true);
        assert_eq!(glow_presentation::verified_hdr_output(&ctx, viewport), None);
        status.store(1, Ordering::Release);
        assert_eq!(
            glow_presentation::verified_hdr_output(&ctx, viewport),
            Some(true)
        );
        assert_eq!(
            glow_presentation::verified_hdr_output(&other, other.viewport_id()),
            Some(false)
        );
        glow_presentation::set_active(&ctx, viewport, false, true);
        assert_eq!(
            glow_presentation::verified_hdr_output(&ctx, viewport),
            Some(false)
        );
        glow_presentation::set_active(&ctx, viewport, true, true);
        assert_eq!(glow_presentation::verified_hdr_output(&ctx, viewport), None);
        status.store(-1, Ordering::Release);
        assert_eq!(
            glow_presentation::verified_hdr_output(&ctx, viewport),
            Some(false)
        );
        glow_presentation::remove(&ctx, viewport);
        assert_eq!(
            glow_presentation::verified_hdr_output(&ctx, viewport),
            Some(false)
        );
    }
}
