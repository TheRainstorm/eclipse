//! The SDK's separated HDR API preserves the user's independent WCG setting.
//! Packet layout from Microsoft's SDK header:
//! https://github.com/microsoft/win32metadata/blob/main/generation/WinSDK/RecompiledIdlHeaders/um/wingdi.h
use super::super::color_state::{self, Api, State, Version};
use super::*;
use windows_sys::Win32::System::{
    LibraryLoader::{GetModuleHandleW, GetProcAddress},
    SystemInformation::OSVERSIONINFOW,
};

#[repr(C)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

#[repr(C)]
struct HdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

const _: [(); 36] = [(); size_of::<AdvancedColorInfo2>()];
const _: [(); 24] = [(); size_of::<HdrState>()];
const GET_ADVANCED_COLOR_INFO_2: i32 = 15;
const SET_HDR_STATE: i32 = 16;

fn version() -> Result<Version> {
    type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
    let library: Vec<u16> = "ntdll.dll".encode_utf16().chain([0]).collect();
    let module = unsafe { GetModuleHandleW(library.as_ptr()) };
    if module.is_null() {
        return Err(error("GetModuleHandleW(ntdll)", unsafe { GetLastError() }));
    }
    let function = unsafe { GetProcAddress(module, c"RtlGetVersion".as_ptr().cast()) }
        .ok_or_else(|| error("GetProcAddress(RtlGetVersion)", unsafe { GetLastError() }))?;
    // ntdll remains loaded for the process lifetime; its OS API ABI is fixed.
    let function: RtlGetVersion = unsafe { std::mem::transmute(function) };
    let mut info: OSVERSIONINFOW = unsafe { zeroed() };
    info.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
    let status = unsafe { function(&mut info) };
    if status < 0 || info.dwMajorVersion < 6 || info.dwBuildNumber == 0 {
        return Err(Error::Platform(format!(
            "RtlGetVersion could not verify the color API platform: NTSTATUS {:#x}",
            status as u32,
        )));
    }
    Ok(Version {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
    })
}

pub(super) fn read(path: &DISPLAYCONFIG_PATH_INFO) -> Result<State> {
    let mut modern: AdvancedColorInfo2 = unsafe { zeroed() };
    modern.header = header::<AdvancedColorInfo2>(
        GET_ADVANCED_COLOR_INFO_2,
        path.targetInfo.adapterId,
        path.targetInfo.id,
    );
    let code = unsafe { DisplayConfigGetDeviceInfo(&mut modern.header) } as u32;
    if code == 0 {
        return State::modern(
            modern.value,
            modern.active_color_mode,
            modern.color_encoding as u32,
            modern.bits_per_color_channel,
        ).map_err(|mode| Error::Platform(format!(
            "DisplayConfigGetDeviceInfo(ADVANCED_COLOR_INFO_2) returned unknown active color mode {mode}"
        )));
    }
    if !matches!(code, 87 | 50 | 120) {
        return Err(error(
            "DisplayConfigGetDeviceInfo(ADVANCED_COLOR_INFO_2)",
            code,
        ));
    }
    let version = version().map_err(|failure| {
        Error::Platform(format!(
            "DisplayConfigGetDeviceInfo(ADVANCED_COLOR_INFO_2): Windows error {code}; {failure}"
        ))
    })?;
    if !color_state::legacy_query_allowed(code, Some(version)) {
        return Err(error(
            "DisplayConfigGetDeviceInfo(ADVANCED_COLOR_INFO_2)",
            code,
        ));
    }
    let mut legacy: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = unsafe { zeroed() };
    legacy.header = header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        path.targetInfo.adapterId,
        path.targetInfo.id,
    );
    get_info(&mut legacy).map_err(|failure| {
        Error::Platform(format!(
            "DisplayConfigGetDeviceInfo(ADVANCED_COLOR_INFO) after older API selection: {failure}"
        ))
    })?;
    Ok(State::legacy(
        unsafe { legacy.Anonymous.value },
        legacy.colorEncoding as u32,
        legacy.bitsPerColorChannel,
    ))
}

pub(super) fn set(path: &DISPLAYCONFIG_PATH_INFO, requested_hdr: bool) -> Result<()> {
    let state = read(path)?;
    state.configure(
        requested_hdr,
        || Error::Platform("this virtual target does not support HDR".into()),
        |api, requested_hdr| {
            // Select one setter from the successful query. Never compensate an error
            // from the HDR-only API by changing the combined legacy HDR/WCG switch.
            let (code, operation) = match api {
                Api::Hdr => {
                    let mut info: HdrState = unsafe { zeroed() };
                    info.header = header::<HdrState>(
                        SET_HDR_STATE,
                        path.targetInfo.adapterId,
                        path.targetInfo.id,
                    );
                    info.value = u32::from(requested_hdr);
                    (
                        unsafe { DisplayConfigSetDeviceInfo(&info.header) },
                        "DisplayConfigSetDeviceInfo(SET_HDR_STATE)",
                    )
                }
                Api::Legacy => {
                    let mut info: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE = unsafe { zeroed() };
                    info.header = header::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(
                        DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                        path.targetInfo.adapterId,
                        path.targetInfo.id,
                    );
                    info.Anonymous.value = u32::from(requested_hdr);
                    (
                        unsafe { DisplayConfigSetDeviceInfo(&info.header) },
                        "DisplayConfigSetDeviceInfo(SET_ADVANCED_COLOR_STATE)",
                    )
                }
            };
            if code == 0 {
                Ok(())
            } else {
                Err(error(operation, code as u32))
            }
        },
    )
}
