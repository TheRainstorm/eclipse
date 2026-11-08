//! Mute exactly the render endpoint opened for loopback; COM stays on its worker.
use super::mute::{Endpoint, Lease};
use anyhow::{ensure, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use windows::{
    core::{implement, GUID},
    Win32::{
        Media::Audio::{
            eConsole, eRender,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDeviceEnumerator, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_MULTITHREADED,
        },
    },
};
const CONTEXT: GUID = GUID::from_u128(0x74787291_83ef_4fe8_b60c_70c6bbd26f73);
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
#[implement(IAudioEndpointVolumeCallback)]
struct Observer(Arc<AtomicBool>);
impl IAudioEndpointVolumeCallback_Impl for Observer_Impl {
    fn OnNotify(
        &self,
        notification: *mut AUDIO_VOLUME_NOTIFICATION_DATA,
    ) -> windows::core::Result<()> {
        if let Some(value) = unsafe { notification.as_ref() } {
            if value.guidEventContext != CONTEXT {
                self.0.store(true, Ordering::Release);
            }
        }
        Ok(())
    }
}
pub(super) struct Output {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
    key: String,
    overridden: Arc<AtomicBool>,
    _apartment: Apartment,
}
fn default_output() -> Result<(String, IAudioEndpointVolume)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let raw = device.GetId()?;
        let id = raw.to_string();
        CoTaskMemFree(Some(raw.0.cast()));
        Ok((id?, device.Activate(CLSCTX_ALL, None)?))
    }
}
impl Output {
    pub fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let apartment = Apartment;
        let (key, volume) = default_output()?;
        let overridden = Arc::new(AtomicBool::new(false));
        let callback: IAudioEndpointVolumeCallback = Observer(overridden.clone()).into();
        unsafe {
            volume.RegisterControlChangeNotify(&callback)?;
        }
        Ok(Self {
            volume,
            callback,
            key: format!("wasapi:{key}"),
            overridden,
            _apartment: apartment,
        })
    }
    pub fn lease(mut self) -> Result<Guard> {
        let (current, _) = default_output()?;
        ensure!(
            self.key == format!("wasapi:{current}"),
            "default output changed while opening loopback; retrying audio capture"
        );
        let observed = self.overridden.clone();
        let lease = Lease::acquire(&mut self, observed)?;
        Ok(Guard {
            endpoint: self,
            lease: Some(lease),
        })
    }
}
impl Endpoint for Output {
    fn key(&self) -> &str {
        &self.key
    }
    fn muted(&mut self) -> Result<bool> {
        Ok(unsafe { self.volume.GetMute()? }.as_bool())
    }
    fn set_muted(&mut self, value: bool) -> Result<()> {
        unsafe {
            self.volume.SetMute(value, &CONTEXT)?;
        }
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = unsafe { self.volume.UnregisterControlChangeNotify(&self.callback) };
    }
}
pub(super) struct Guard {
    endpoint: Output,
    lease: Option<Lease>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            if let Err(error) = lease.release(&mut self.endpoint) {
                tracing::warn!(%error,"host audio mute restoration failed");
            }
        }
    }
}
