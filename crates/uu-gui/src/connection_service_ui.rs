//! Shared enrollment and online-device controls for foreground and installed hosts.
mod directory;
mod enrollment;
#[cfg(any(windows, test))]
mod pending;
pub(super) use directory::Directory;
pub(super) use enrollment::Join;
use std::sync::mpsc;

fn received<T>(job: &Option<mpsc::Receiver<T>>) -> Option<Result<T, String>> {
    match job.as_ref()?.try_recv() {
        Ok(value) => Some(Ok(value)),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(uu_i18n::text(
            "后台操作未返回结果，请刷新确认状态。",
        )
        .into())),
    }
}
