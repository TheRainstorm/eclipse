//! Optional capture device lease for encoder initialization. Pixel resources
//! remain owned by their frame; this only avoids initializing the wrong device.
#[derive(Clone)]
pub struct InputDevice {
    #[cfg(all(windows, feature = "mf-encode"))]
    pub(super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
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
                threading.SetMultithreadProtected(true);
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
