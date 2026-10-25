//! Bounded, same-adapter capture snapshots. Arc leases gate CPU reuse;
//! keyed mutexes independently order GPU access across protected devices.
use crate::encoder::{DeviceIdentity, InputDevice};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;
use windows::{
    core::{Interface, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Graphics::{Direct3D11::*, Dxgi::*},
    },
};
static NEXT: AtomicU64 = AtomicU64::new(1);
const WAIT_MS: u32 = 50;
#[derive(Debug, Default, serde::Serialize)]
struct Counts {
    handles_created: u64,
    handles_closed: u64,
    views_opened: u64,
    views_closed: u64,
    slots_created: u64,
    slots_closed: u64,
    acquires: u64,
    releases: u64,
    held: u64,
    timeouts: u64,
    abandoned: u64,
    acquire_errors: u64,
    release_errors: u64,
    wait_ns: u64,
    max_wait_ns: u64,
    writer_attempts: u64,
    writer_wait_ns: u64,
    writer_max_wait_ns: u64,
    reader_attempts: u64,
    reader_wait_ns: u64,
    reader_max_wait_ns: u64,
}
#[derive(Debug)]
pub(crate) struct Stats {
    generation: u64,
    pub capture: DeviceIdentity,
    pub encode: DeviceIdentity,
    counts: Mutex<Counts>,
    poisoned: AtomicBool,
    retired: AtomicBool,
}
impl Stats {
    pub fn new(capture: &InputDevice, encode: &InputDevice) -> Result<Arc<Self>, String> {
        let (a, b) = (
            capture.identity().map_err(|e| e.to_string())?,
            encode.identity().map_err(|e| e.to_string())?,
        );
        if a.object == b.object
            || (a.adapter_low, a.adapter_high) != (b.adapter_low, b.adapter_high)
        {
            return Err(
                "shared capture requires different devices on the same adapter LUID".into(),
            );
        }
        Ok(Arc::new(Self {
            generation: NEXT.fetch_add(1, Ordering::Relaxed),
            capture: a,
            encode: b,
            counts: Mutex::default(),
            poisoned: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        }))
    }
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
    fn valid(&self) -> Result<(), String> {
        if self.poisoned.load(Ordering::Acquire) {
            Err("shared capture generation poisoned".into())
        } else {
            Ok(())
        }
    }
    fn proof(&self) -> serde_json::Value {
        serde_json::json!({"generation":self.generation,"capture":self.capture,"encode":self.encode,"key":0,"wait_ms":WAIT_MS,"actual_separate":true})
    }
}
impl Drop for Stats {
    fn drop(&mut self) {
        let c = self.counts.get_mut().unwrap_or_else(|e| e.into_inner());
        log(
            serde_json::json!({"event":"shared_capture_closed","generation":self.generation,"capture":self.capture,"encode":self.encode,
            "actual_separate":true,"retired":self.retired.load(Ordering::Acquire),"poisoned":self.poisoned.load(Ordering::Acquire),"counts":c}),
        );
    }
}
#[derive(Debug)]
struct Handle {
    value: HANDLE,
    stats: Arc<Stats>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        let ok = unsafe { CloseHandle(self.value) }.is_ok();
        if ok {
            self.stats
                .counts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .handles_closed += 1;
        } else {
            self.stats.poisoned.store(true, Ordering::Release);
            eprintln!("shared capture CloseHandle failed");
        }
    }
}
/// Serialize before writing so the C encoder close record cannot split JSON
/// into individual formatting fragments. This is never called per frame.
pub(crate) fn log(value: serde_json::Value) {
    use std::io::Write;
    let mut line = value.to_string();
    line.push('\n');
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}
#[derive(Debug)]
struct View {
    texture: Option<ID3D11Texture2D>,
    stats: Arc<Stats>,
}
impl Drop for View {
    fn drop(&mut self) {
        self.texture.take();
        self.stats
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .views_closed += 1;
    }
}
#[derive(Debug)]
pub(crate) struct Slot {
    // Explicitly cleared before the final Stats Arc can emit its close proof.
    capture: Option<ID3D11Texture2D>,
    encode: Option<View>,
    writer: Option<IDXGIKeyedMutex>,
    reader: Option<IDXGIKeyedMutex>,
    handle: Option<Handle>,
    stats: Arc<Stats>,
}
impl Slot {
    pub fn new(
        capture: &InputDevice,
        encode: &InputDevice,
        desc: &D3D11_TEXTURE2D_DESC,
        stats: Arc<Stats>,
    ) -> Result<Arc<Self>, String> {
        stats.valid()?;
        let mut desc = *desc;
        desc.MiscFlags =
            (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX).0 as u32;
        let mut texture = None;
        unsafe {
            capture
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture))
        }
        .map_err(|e| e.to_string())?;
        let texture = texture.ok_or("shared capture allocation absent")?;
        let resource: IDXGIResource1 = texture.cast().map_err(|e| e.to_string())?;
        let raw = unsafe {
            resource.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                PCWSTR::null(),
            )
        }
        .map_err(|e| e.to_string())?;
        stats
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handles_created += 1;
        let handle = Handle {
            value: raw,
            stats: stats.clone(),
        };
        let device: ID3D11Device1 = encode.device.cast().map_err(|e| e.to_string())?;
        let opened: ID3D11Texture2D =
            unsafe { device.OpenSharedResource1(handle.value) }.map_err(|e| e.to_string())?;
        stats
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .views_opened += 1;
        let opened = View {
            texture: Some(opened),
            stats: stats.clone(),
        };
        let writer = texture.cast().map_err(|e| e.to_string())?;
        let reader = opened
            .texture
            .as_ref()
            .unwrap()
            .cast()
            .map_err(|e| e.to_string())?;
        {
            let mut c = stats.counts.lock().unwrap_or_else(|e| e.into_inner());
            c.slots_created += 1;
        }
        Ok(Arc::new(Self {
            capture: Some(texture),
            encode: Some(opened),
            writer: Some(writer),
            reader: Some(reader),
            handle: Some(handle),
            stats,
        }))
    }
    pub fn view(&self) -> ID3D11Texture2D {
        self.encode
            .as_ref()
            .unwrap()
            .texture
            .as_ref()
            .unwrap()
            .clone()
    }
    pub fn proof(&self) -> serde_json::Value {
        self.stats.proof()
    }
    pub fn write(
        &self,
        context: &ID3D11DeviceContext,
        source: &ID3D11Texture2D,
    ) -> Result<(), String> {
        if self.stats.retired.load(Ordering::Acquire) {
            return Err("shared capture generation retired".into());
        }
        let guard = Guard::acquire(self.writer.as_ref().unwrap(), &self.stats, true)?;
        unsafe {
            context.CopyResource(self.capture.as_ref().unwrap(), source);
            context.Flush();
        }
        guard.finish()
    }
    pub fn read(&self) -> Result<Guard<'_>, String> {
        Guard::acquire(self.reader.as_ref().unwrap(), &self.stats, false)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.writer.take();
        self.reader.take();
        self.encode.take();
        self.capture.take();
        self.handle.take();
        let mut c = self.stats.counts.lock().unwrap_or_else(|e| e.into_inner());
        c.slots_closed += 1;
    }
}
/// Only S_OK grants ownership. The generated Result binding uses .ok(), which
/// treats positive WAIT_TIMEOUT/WAIT_ABANDONED as success, so call raw vtables.
#[derive(Debug, PartialEq, Eq)]
enum Acquire {
    Owned,
    Timeout,
    Abandoned,
    Error(i32),
}
fn classify(hr: i32) -> Acquire {
    match hr {
        0 => Acquire::Owned,
        258 => Acquire::Timeout,
        128 => Acquire::Abandoned,
        n => Acquire::Error(n),
    }
}
pub(crate) struct Guard<'a> {
    mutex: &'a IDXGIKeyedMutex,
    stats: &'a Stats,
    held: bool,
}
impl<'a> Guard<'a> {
    fn acquire(mutex: &'a IDXGIKeyedMutex, stats: &'a Stats, writer: bool) -> Result<Self, String> {
        stats.valid()?;
        let started = Instant::now();
        let hr = unsafe { (mutex.vtable().AcquireSync)(mutex.as_raw(), 0, WAIT_MS) }.0;
        let ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let mut c = stats.counts.lock().unwrap_or_else(|e| e.into_inner());
        c.wait_ns = c.wait_ns.saturating_add(ns);
        c.max_wait_ns = c.max_wait_ns.max(ns);
        if writer {
            c.writer_attempts += 1;
            c.writer_wait_ns = c.writer_wait_ns.saturating_add(ns);
            c.writer_max_wait_ns = c.writer_max_wait_ns.max(ns);
        } else {
            c.reader_attempts += 1;
            c.reader_wait_ns = c.reader_wait_ns.saturating_add(ns);
            c.reader_max_wait_ns = c.reader_max_wait_ns.max(ns);
        }
        match classify(hr) {
            Acquire::Owned => {
                c.acquires += 1;
                c.held += 1;
                Ok(Self {
                    mutex,
                    stats,
                    held: true,
                })
            }
            status => {
                match status {
                    Acquire::Timeout => c.timeouts += 1,
                    Acquire::Abandoned => c.abandoned += 1,
                    _ => c.acquire_errors += 1,
                };
                stats.poisoned.store(true, Ordering::Release);
                Err(format!(
                    "shared capture AcquireSync failed: {status:?} HRESULT={hr:#x}"
                ))
            }
        }
    }
    fn release(&mut self) -> Result<(), String> {
        if !self.held {
            return Ok(());
        }
        // Never retry an uncertain release or manufacture a successful close.
        self.held = false;
        let hr = unsafe { (self.mutex.vtable().ReleaseSync)(self.mutex.as_raw(), 0) }.0;
        let mut c = self.stats.counts.lock().unwrap_or_else(|e| e.into_inner());
        if hr == 0 {
            c.releases += 1;
            c.held -= 1;
            Ok(())
        } else {
            c.release_errors += 1;
            self.stats.poisoned.store(true, Ordering::Release);
            Err(format!("shared capture ReleaseSync failed HRESULT={hr:#x}"))
        }
    }
    pub fn finish(mut self) -> Result<(), String> {
        self.release()
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.release() {
            eprintln!("{e}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_s_ok_grants_ownership() {
        assert_eq!(classify(0), Acquire::Owned);
        assert_eq!(classify(258), Acquire::Timeout);
        assert_eq!(classify(128), Acquire::Abandoned);
        assert_eq!(classify(1), Acquire::Error(1));
        assert_eq!(classify(-1), Acquire::Error(-1));
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    use crate::{encoder::CapturePool, Timestamp};
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{Direct3D::D3D_DRIVER_TYPE_UNKNOWN, Dxgi::Common::*},
    };
    fn devices() -> (InputDevice, InputDevice) {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
        let adapter = (0..16)
            .find_map(|i| {
                let a = unsafe { factory.EnumAdapters1(i) }.ok()?;
                let d = unsafe { a.GetDesc1() }.ok()?;
                (d.VendorId == 0x1002).then_some(a)
            })
            .expect("AMD adapter required for this ignored hardware test");
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
        }
        let capture = InputDevice::from_d3d11(device.unwrap()).unwrap();
        let encode = capture.separate_on_same_adapter().unwrap();
        let (a, b) = (capture.identity().unwrap(), encode.identity().unwrap());
        assert_ne!(a.object, b.object);
        assert_eq!(
            (a.adapter_low, a.adapter_high),
            (b.adapter_low, b.adapter_high)
        );
        (capture, encode)
    }
    fn texture(device: &InputDevice) -> (ID3D11Texture2D, ID3D11RenderTargetView) {
        let mut t = None;
        let mut v = None;
        unsafe {
            device
                .device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: 4,
                        Height: 4,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut t),
                )
                .unwrap();
            device
                .device
                .CreateRenderTargetView(t.as_ref().unwrap(), None, Some(&mut v))
                .unwrap();
        }
        (t.unwrap(), v.unwrap())
    }
    #[test]
    #[ignore = "requires AMD D3D11 hardware; run explicitly in the isolated Windows lab"]
    fn separate_device_lifecycle_preserves_live_pixels_and_reclaims_unconsumed_slots() {
        let (capture, encode) = devices();
        assert!(Stats::new(&capture, &capture).is_err());
        let context = unsafe { capture.device.GetImmediateContext().unwrap() };
        let (source, view) = texture(&capture);
        let mut pool = CapturePool::separate(capture.clone(), encode.clone(), 4, 4).unwrap();
        let stats = pool.shared_stats();
        let mut held = Vec::new();
        for (i, color) in [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]]
            .into_iter()
            .enumerate()
        {
            unsafe {
                context.ClearRenderTargetView(&view, &color);
            }
            held.push(pool.capture(&source, Timestamp(i as u64)).unwrap().unwrap());
        }
        assert!(pool.capture(&source, Timestamp(3)).unwrap().is_none());
        for _ in 0..2 {
            for (f, rgb) in held.iter().zip([[255, 0, 0], [0, 255, 0], [0, 0, 255]]) {
                assert_eq!(
                    crate::encoder::capture_rgb_samples(f, &[(1, 1)], 203.).unwrap(),
                    vec![rgb]
                );
            }
        }
        // The middle pending snapshot is dropped without any consumer lock.
        held.remove(1);
        unsafe {
            context.ClearRenderTargetView(&view, &[1., 1., 0., 1.]);
        }
        let pending = pool.capture(&source, Timestamp(4)).unwrap().unwrap();
        drop(pending);
        // Replacing an unconsumed publication and reading again cannot strand a key.
        for i in 5..21 {
            let f = pool.capture(&source, Timestamp(i)).unwrap().unwrap();
            drop(f);
        }
        assert_eq!(
            crate::encoder::capture_rgb_samples(&held[0], &[(1, 1)], 203.).unwrap(),
            vec![[255, 0, 0]]
        );
        assert_eq!(
            crate::encoder::capture_rgb_samples(&held[1], &[(1, 1)], 203.).unwrap(),
            vec![[0, 0, 255]]
        );
        drop(pool);
        assert!(stats.retired.load(Ordering::Acquire));
        assert_eq!(stats.counts.lock().unwrap().handles_closed, 1);
        // A retired pool still permits existing immutable consumers to finish.
        assert_eq!(
            crate::encoder::capture_rgb_samples(&held[0], &[(1, 1)], 203.).unwrap(),
            vec![[255, 0, 0]]
        );
        drop(held);
        let c = stats.counts.lock().unwrap();
        assert_eq!(c.handles_created, 3);
        assert_eq!(c.handles_closed, 3);
        assert_eq!(c.views_opened, c.views_closed);
        assert_eq!(c.slots_created, c.slots_closed);
        assert_eq!(c.acquires, c.releases);
        assert_eq!(c.held, 0);
        assert_eq!(
            c.timeouts + c.abandoned + c.acquire_errors + c.release_errors,
            0
        );
        assert!(!stats.poisoned.load(Ordering::Acquire));
    }
    #[test]
    #[ignore = "requires AMD D3D11 hardware; run explicitly in the isolated Windows lab"]
    fn separate_device_timeout_never_releases_an_unacquired_mutex_and_poison_is_terminal() {
        let (capture, encode) = devices();
        let stats = Stats::new(&capture, &encode).unwrap();
        let (source, _) = texture(&capture);
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            source.GetDesc(&mut desc);
        }
        let slot = Slot::new(&capture, &encode, &desc, stats.clone()).unwrap();
        let frame = Arc::new(
            crate::d3d11_frame::Frame::shared_capture(slot.clone(), encode.device.clone()).unwrap(),
        );
        let writer = Guard::acquire(slot.writer.as_ref().unwrap(), &stats, true).unwrap();
        let reader = frame.clone();
        let timeout_started = Instant::now();
        let failed = std::thread::spawn(move || reader.capture_read().is_err())
            .join()
            .unwrap();
        assert!(failed);
        assert!(timeout_started.elapsed() < std::time::Duration::from_secs(2));
        {
            let c = stats.counts.lock().unwrap();
            assert_eq!(c.acquires, 1);
            assert_eq!(c.timeouts, 1);
            assert_eq!(c.releases, 0);
            assert_eq!(c.held, 1);
        }
        writer.finish().unwrap();
        assert!(frame.capture_read().is_err());
        drop(frame);
        drop(slot);
        stats.retire();
        let c = stats.counts.lock().unwrap();
        assert_eq!(c.releases, 1);
        assert_eq!(c.held, 0);
        assert_eq!(c.handles_created, c.handles_closed);
        assert_eq!(c.views_opened, c.views_closed);
    }
}
