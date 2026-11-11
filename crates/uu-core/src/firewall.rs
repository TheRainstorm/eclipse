//! Windows firewall changes have one owner and one structured execution path.
//! Existing rules retain their scope; inspection never claims remote reachability.
/// Bound the OS operation separately from the IPC transport. Both management
/// hops allow an additional margin for delivering the completed result.
pub const OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
