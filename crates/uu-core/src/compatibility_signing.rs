//! Shared PC-client request signing value, intentionally distributed for compatibility.
//!
//! This is not a user login token, a device invitation, or a media encryption key.
//! It is public protocol data in this distribution and can be read from source or
//! binaries. Local signing-key / UU_SIGNING_KEY_FILE configuration overrides it.
//! Public comparison: VallierDev/uu-remote-linux, commit 169edf2224775db9e2300fc825ffa08bfc2da428,
//! src/auth.rs. Only the compatibility value is recorded; no implementation is copied.

pub(super) const PUBLIC_PC_SIGNING_KEY: &[u8] = b"alWiSzXZTLu3WfFnw13uBru3";
