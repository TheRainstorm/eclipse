//! Passive desktop feedback; independent of capture, input and history IO.
pub const DURATION: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::show;
