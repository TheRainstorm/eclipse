//! Shared enrollment and online-device controls for foreground and installed hosts.
mod directory;
mod enrollment;
mod servers;
mod labels;
pub(crate) mod removal;
pub(crate) mod device_picker;
pub(crate) mod endpoint;
#[cfg(any(windows, test))]
mod pending;
pub(crate) use directory::Directory;
pub(crate) use enrollment::Join;
pub(crate) use servers::{Server, ServerAction, Servers};
pub(crate) use labels::{address_key, display_name, route_name, load_names, Labels, Names};
use std::sync::mpsc;

pub(crate) fn received<T>(job: &Option<mpsc::Receiver<T>>) -> Option<Result<T, String>> {
    match job.as_ref()?.try_recv() {
        Ok(value) => Some(Ok(value)),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(uu_i18n::text(
            "后台操作未返回结果，请刷新确认状态。",
        )
        .into())),
    }
}
