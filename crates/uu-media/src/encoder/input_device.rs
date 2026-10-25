//! Optional capture device lease for encoder initialization. Pixel resources
//! remain owned by their frame; this only avoids initializing the wrong device.
#[derive(Clone)]
pub struct InputDevice {
    #[cfg(all(windows, feature = "mf-encode"))]
    pub(crate) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    #[cfg(all(windows, feature = "mf-encode"))]
    pub(super) vendor: u16,
    _private: (),
}

#[cfg(all(windows, feature = "mf-encode"))]
impl InputDevice {
    pub fn from_d3d11(
        device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<Self, crate::CodecError> {
        use windows::{
            core::Interface,
            Win32::Graphics::{
                Direct3D10::ID3D10Multithread, Direct3D11::D3D11_CREATE_DEVICE_SINGLETHREADED,
                Dxgi::IDXGIDevice,
            },
        };
        let prepare = || -> windows::core::Result<u16> {
            unsafe {
                if device.GetCreationFlags() & D3D11_CREATE_DEVICE_SINGLETHREADED.0 as u32 != 0 {
                    return Err(windows::core::Error::empty());
                }
                let context = device.GetImmediateContext()?;
                let threading: ID3D10Multithread = context.cast()?;
                let _ = threading.SetMultithreadProtected(true);
                if !threading.GetMultithreadProtected().as_bool() {
                    return Err(windows::core::Error::empty());
                }
                let dxgi: IDXGIDevice = device.cast()?;
                let adapter = dxgi.GetAdapter()?;
                Ok(adapter.GetDesc()?.VendorId as u16)
            }
        };
        let vendor = prepare().map_err(|error| super::error(format!("capture device: {error}")))?;
        Ok(Self {
            device,
            vendor,
            _private: (),
        })
    }
}

/// Canonical COM identity plus the complete adapter LUID. Vendor IDs cannot
/// establish that two resources belong to the same physical adapter.
#[cfg(all(windows, feature = "mf-encode"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DeviceIdentity {
    pub object: u64,
    pub adapter_low: u32,
    pub adapter_high: i32,
}
#[cfg(all(windows, feature = "mf-encode"))]
impl InputDevice {
    pub fn identity(&self) -> Result<DeviceIdentity, crate::CodecError> {
        device_identity(&self.device).map_err(super::error)
    }
    /// One protected encode device on the capture device's exact adapter.
    pub fn separate_on_same_adapter(&self) -> Result<Self, crate::CodecError> {
        use windows::{
            core::Interface,
            Win32::{
                Foundation::HMODULE,
                Graphics::{Direct3D::D3D_DRIVER_TYPE_UNKNOWN, Direct3D11::*, Dxgi::IDXGIDevice},
            },
        };
        let create = || -> windows::core::Result<_> {
            unsafe {
                let dxgi: IDXGIDevice = self.device.cast()?;
                let adapter = dxgi.GetAdapter()?;
                let mut device = None;
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
                )?;
                device.ok_or_else(windows::core::Error::empty)
            }
        };
        let next = Self::from_d3d11(create().map_err(|e| super::error(e.to_string()))?)?;
        let (a, b) = (self.identity()?, next.identity()?);
        if a.object == b.object
            || (a.adapter_low, a.adapter_high) != (b.adapter_low, b.adapter_high)
        {
            return Err(super::error(
                "separate encode device identity/LUID mismatch",
            ));
        }
        Ok(next)
    }
}
#[cfg(all(windows, feature = "mf-encode"))]
pub(crate) fn device_identity(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<DeviceIdentity, String> {
    use windows::{
        core::{IUnknown, Interface},
        Win32::Graphics::Dxgi::IDXGIDevice,
    };
    let id: IUnknown = device.cast().map_err(|e| e.to_string())?;
    let dxgi: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
    let desc = unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }.map_err(|e| e.to_string())?;
    Ok(DeviceIdentity {
        object: id.as_raw() as usize as u64,
        adapter_low: desc.AdapterLuid.LowPart,
        adapter_high: desc.AdapterLuid.HighPart,
    })
}

// Construction rejects SINGLETHREADED devices and enables immediate-context
// multithread protection before the lease can cross the encoder worker boundary.
#[cfg(all(windows, feature = "mf-encode"))]
unsafe impl Send for InputDevice {}
#[cfg(all(windows, feature = "mf-encode"))]
unsafe impl Sync for InputDevice {}

#[cfg(all(test, windows, feature = "mf-encode"))]
mod tests {
    use super::*;
    use windows::{
        core::Interface,
        Win32::{
            Foundation::HMODULE,
            Graphics::{
                Direct3D::D3D_DRIVER_TYPE_WARP, Direct3D10::ID3D10Multithread, Direct3D11::*,
            },
        },
    };

    fn device(flags: D3D11_CREATE_DEVICE_FLAG) -> ID3D11Device {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
        }
        device.unwrap()
    }

    #[test]
    fn single_threaded_device_cannot_cross_encoder_worker_boundary() {
        let device = device(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED);
        assert!(InputDevice::from_d3d11(device).is_err());
    }

    #[test]
    fn cloned_capture_lease_retains_a_protected_context_on_another_thread() {
        let device = device(D3D11_CREATE_DEVICE_BGRA_SUPPORT);
        let lease = InputDevice::from_d3d11(device.clone()).unwrap();
        let retained = lease.clone();
        drop(lease);
        drop(device);
        std::thread::spawn(move || {
            let context = unsafe { retained.device.GetImmediateContext().unwrap() };
            let threading: ID3D10Multithread = context.cast().unwrap();
            assert!(unsafe { threading.GetMultithreadProtected() }.as_bool());
        })
        .join()
        .unwrap();
    }
}
