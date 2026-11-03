//! Windows firewall changes have one owner and one structured execution path.
//! Existing rules retain their scope; inspection never claims remote reachability.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
