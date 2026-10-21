//! Windows wait timeouts otherwise quantize short Tokio timers to ~15.6 ms.
//! Own one balanced request per active video receiver, including native peers.
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod, TIMERR_NOERROR};

const PERIOD_MS: u32 = 1;

#[must_use = "keep the timer request alive for the receiving session"]
pub(crate) struct VideoTimerResolution {
    result: u32,
}

impl VideoTimerResolution {
    pub(crate) fn request() -> Self {
        // Windows reference-counts equal requests. Each track/session owns its
        // own guard so closing one cannot release another's request.
        Self {
            result: unsafe { timeBeginPeriod(PERIOD_MS) },
        }
    }

    pub(crate) fn result(&self) -> u32 {
        self.result
    }
}

impl Drop for VideoTimerResolution {
    fn drop(&mut self) {
        if self.result == TIMERR_NOERROR {
            // Runs on ordinary exit, errors and async task cancellation. This
            // affects wait resolution, not Instant/QPC or requested video FPS.
            unsafe { timeEndPeriod(PERIOD_MS) };
        }
    }
}
