//! HDR intent, active HDR and wide color are separate platform state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Api {
    Legacy,
    Hdr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Version {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

pub(super) fn legacy_query_allowed(code: u32, version: Option<Version>) -> bool {
    // 87 alone is not proof of an old API: it also reports invalid packets.
    // Version confirmation confines this fallback to the older interface.
    matches!(code, 87 | 50 | 120)
        && version.is_some_and(|v| {
            v.major >= 6 && v.build != 0 && (v.major, v.minor, v.build) < (10, 0, 26100)
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct State {
    pub api: Api,
    pub hdr_supported: bool,
    pub hdr_user_enabled: bool,
    pub hdr_active: bool,
    raw: u32,
    active_mode: i32,
    encoding: u32,
    bits: u32,
}

impl State {
    pub fn modern(raw: u32, active_mode: i32, encoding: u32, bits: u32) -> Result<Self, i32> {
        if !matches!(active_mode, 0..=2) {
            return Err(active_mode);
        }
        Ok(Self {
            api: Api::Hdr,
            hdr_supported: raw & (1 << 4) != 0,
            hdr_user_enabled: raw & (1 << 5) != 0,
            hdr_active: active_mode == 2,
            raw,
            active_mode,
            encoding,
            bits,
        })
    }

    pub fn legacy(raw: u32, encoding: u32, bits: u32) -> Self {
        // The old interface has only the combined Advanced Color switch.
        // This compatibility path is confined to a verified older platform.
        Self {
            api: Api::Legacy,
            hdr_supported: raw & 1 != 0,
            hdr_user_enabled: raw & 2 != 0,
            hdr_active: raw & 2 != 0,
            raw,
            active_mode: -1,
            encoding,
            bits,
        }
    }

    fn update(self, requested_hdr: bool) -> Result<Option<Api>, ()> {
        if requested_hdr && !self.hdr_supported {
            return Err(());
        }
        // A user setting may be enabled but inactive because of OS policy.
        // Keep intent separate; the caller still verifies actual active HDR.
        if requested_hdr == self.hdr_user_enabled {
            Ok(None)
        } else {
            Ok(Some(self.api))
        }
    }

    pub fn configure<E>(
        self,
        requested_hdr: bool,
        unsupported: impl FnOnce() -> E,
        submit: impl FnOnce(Api, bool) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.update(requested_hdr).map_err(|()| unsupported())? {
            None => Ok(()),
            Some(api) => submit(api, requested_hdr),
        }
    }

    pub fn signature(self) -> [i64; 5] {
        [
            i64::from(self.api == Api::Hdr),
            i64::from(self.raw),
            i64::from(self.active_mode),
            i64::from(self.encoding),
            i64::from(self.bits),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcg_is_not_hdr_and_an_sdr_request_keeps_wcg() {
        let state = State::modern(1 | 2 | 16 | 64 | 128, 1, 0, 10).unwrap();
        assert!(!state.hdr_active);
        assert!(!state.hdr_user_enabled);
        assert_eq!(state.update(false), Ok(None));
        assert_eq!(state.update(true), Ok(Some(Api::Hdr)));
        assert_eq!(
            state.configure(
                false,
                || "unsupported",
                |_, _| { panic!("an SDR request must not turn WCG off") }
            ),
            Ok(())
        );
    }

    #[test]
    fn hdr_user_intent_does_not_claim_active_hdr() {
        let state = State::modern(1 | 16 | 32, 0, 0, 8).unwrap();
        assert!(state.hdr_user_enabled);
        assert!(!state.hdr_active);
        assert_eq!(state.update(true), Ok(None));
        assert_eq!(state.update(false), Ok(Some(Api::Hdr)));
    }

    #[test]
    fn active_hdr_does_not_replace_the_user_setting() {
        let state = State::modern(1 | 2 | 16, 2, 0, 10).unwrap();
        assert!(state.hdr_active);
        assert!(!state.hdr_user_enabled);
        assert_eq!(state.update(false), Ok(None));
        assert_eq!(state.update(true), Ok(Some(Api::Hdr)));
    }

    #[test]
    fn unsupported_hdr_can_stay_sdr_without_a_setter() {
        let state = State::modern(0, 0, 0, 8).unwrap();
        assert_eq!(state.update(false), Ok(None));
        assert_eq!(state.update(true), Err(()));
    }

    #[test]
    fn unknown_active_mode_is_an_error_not_sdr() {
        assert_eq!(State::modern(0, 3, 0, 0), Err(3));
        assert_eq!(State::modern(0, -1, 0, 0), Err(-1));
    }

    #[test]
    fn original_color_guard_keeps_wcg_and_other_packet_fields() {
        let sdr = State::modern(1 | 16 | 64, 0, 0, 8).unwrap();
        for changed in [
            State::modern(1 | 2 | 16 | 64 | 128, 1, 0, 8).unwrap(),
            State::modern(1 | 16 | 64, 0, 1, 8).unwrap(),
            State::modern(1 | 16 | 64, 0, 0, 10).unwrap(),
        ] {
            assert_eq!(sdr.hdr_active, changed.hdr_active);
            assert_ne!(sdr.signature(), changed.signature());
        }
    }

    #[test]
    fn old_interface_preserves_its_existing_meaning_and_setter() {
        let state = State::legacy(3, 0, 10);
        assert!(state.hdr_supported && state.hdr_active && state.hdr_user_enabled);
        assert_eq!(state.update(false), Ok(Some(Api::Legacy)));
        assert_eq!(state.update(true), Ok(None));
    }

    #[test]
    fn an_hdr_only_setter_error_does_not_retry_the_combined_legacy_setter() {
        let state = State::modern(1 | 2 | 16 | 32 | 64 | 128, 2, 0, 10).unwrap();
        let mut calls = Vec::new();
        let result = state.configure(
            false,
            || "unsupported",
            |api, enabled| {
                calls.push((api, enabled));
                Err("HDR-only API failed")
            },
        );
        assert_eq!(result, Err("HDR-only API failed"));
        assert_eq!(calls, [(Api::Hdr, false)]);
    }

    #[test]
    fn only_explicit_rejection_on_a_verified_old_os_can_query_legacy() {
        let old = Some(Version {
            major: 10,
            minor: 0,
            build: 22631,
        });
        for code in [87, 50, 120] {
            assert!(legacy_query_allowed(code, old));
            assert!(!legacy_query_allowed(code, None));
            for version in [
                Version {
                    major: 10,
                    minor: 0,
                    build: 26100,
                },
                Version {
                    major: 11,
                    minor: 0,
                    build: 1000,
                },
                Version {
                    major: 0,
                    minor: 0,
                    build: 0,
                },
            ] {
                assert!(!legacy_query_allowed(code, Some(version)));
            }
        }
        for code in [0, 5, 31, 122] {
            assert!(!legacy_query_allowed(code, old));
        }
    }
}
