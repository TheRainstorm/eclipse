//! Raw opt-in DXGI submission evidence. None of these fields is a display time.
use serde::Serialize;

/// Versioned C ABI shared with hdr_present.cpp. HRESULTs and DXGI statistics
/// remain unmodified, including disjoint/error/occluded results.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct RawPresentObservation {
    pub version: u32,
    pub struct_size: u32,
    pub serial: u64,
    pub chain_id: u64,
    pub surface_epoch: u64,
    pub chain_address: u64,
    pub native_source_id: u64,
    pub native_source_ready: u64,
    pub qpc_frequency: u64,
    pub present_before_qpc: i64,
    pub present_after_qpc: i64,
    pub present_hresult: i32,
    pub sync_interval: u32,
    pub present_flags: u32,
    pub last_present_count_hresult: i32,
    pub last_present_count: u32,
    pub stats_present_count: u32,
    pub stats_present_refresh_count: u32,
    pub stats_sync_refresh_count: u32,
    pub stats_hresult: i32,
    pub reserved: u32,
    pub last_count_query_before_qpc: i64,
    pub last_count_query_after_qpc: i64,
    pub stats_query_before_qpc: i64,
    pub stats_query_after_qpc: i64,
    pub stats_sync_qpc_time: i64,
    pub stats_sync_gpu_time: i64,
    pub surface_epoch_after_query: u64,
}
impl RawPresentObservation {
    pub const VERSION: u32 = 1;
    pub fn valid_layout(&self) -> bool {
        self.version == Self::VERSION && self.struct_size as usize == std::mem::size_of::<Self>()
    }
    /// Identity only. A matching converted GPU image says nothing about when
    /// its present became visible, or whether a successful present was dropped.
    pub fn matches_native_image(&self, source_id: u64, ready: u64) -> bool {
        source_id != 0 && self.native_source_id == source_id && self.native_source_ready == ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ffi_layout_matches_the_explicit_cpp_contract() {
        assert_eq!(std::mem::size_of::<RawPresentObservation>(), 176);
        assert_eq!(std::mem::offset_of!(RawPresentObservation, chain_id), 16);
        assert_eq!(
            std::mem::offset_of!(RawPresentObservation, present_before_qpc),
            64
        );
        assert_eq!(
            std::mem::offset_of!(RawPresentObservation, last_count_query_before_qpc),
            120
        );
        assert_eq!(
            std::mem::offset_of!(RawPresentObservation, surface_epoch_after_query),
            168
        );
        let mut raw = RawPresentObservation::default();
        assert!(!raw.valid_layout());
        raw.version = 1;
        raw.struct_size = 176;
        assert!(raw.valid_layout());
    }
    #[test]
    fn source_fence_value_is_identity_and_does_not_certify_display() {
        let raw = RawPresentObservation {
            native_source_id: 9,
            native_source_ready: 4,
            present_hresult: -1,
            ..Default::default()
        };
        assert!(raw.matches_native_image(9, 4));
        assert!(!raw.matches_native_image(9, 5));
        assert!(!RawPresentObservation::default().matches_native_image(0, 0));
    }
}
