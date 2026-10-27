pub use uu_core::config::{Config,local_device_name};
/// Service-facing label is derived from this computer, not shared-host preferences.
pub fn uu_device_name() -> String {
    uu_name_for_host(&local_device_name())
}
fn uu_name_for_host(host: &str) -> String {
    const SUFFIX: &str = "-Eclipse";
    let prefix: String = host.chars().take(64 - SUFFIX.chars().count()).collect();
    format!("{prefix}{SUFFIX}")
}

/// Optional routing hint. Never ship a developer's public IP as a default.
pub fn geo_exit_ip() -> String {
    std::env::var("UU_GEO_IP").unwrap_or_else(|_| "0.0.0.0".into())
}

#[cfg(test)]
mod device_label_tests {
    #[test]
    fn uu_suffix_is_explicit_and_fits_the_service_character_limit() {
        assert_eq!(super::uu_name_for_host("Workstation"), "Workstation-Eclipse");
        let label = super::uu_name_for_host(&"名".repeat(64));
        assert_eq!(label.chars().count(), 64);
        assert!(label.ends_with("-Eclipse"));
        assert!(crate::api::validate_device_name(&label).is_ok());
    }
}

use uu_core::video_options::CaptureResolution;
pub fn resolution_from_env() -> CaptureResolution {
        let value = std::env::var("UU_RESOLUTION").unwrap_or_default();
        match value
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        {
            Some((w, h))
                if (320..=3840).contains(&w)
                    && (240..=2160).contains(&h)
                    && w % 2 == 0
                    && h % 2 == 0 =>
            {
                CaptureResolution::Exact(w, h)
            }
            _ => CaptureResolution::Native,
        }
    }
