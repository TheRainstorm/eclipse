//! Typed native input, independent of the interoperability CONTROL JSON.
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};
pub use uu_input::{KeyCode, MouseButton};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputCommand {
    Absolute { x: u16, y: u16 },
    Relative { dx: i16, dy: i16 },
    Button { button: MouseButton, pressed: bool },
    Wheel { dx: i16, dy: i16 },
    Key { usage: u16, pressed: bool },
    ReleaseAll {},
}
impl InputCommand {
    pub(crate) fn validate(self) -> bool {
        match self {
            Self::Key { usage, .. } => KeyCode(usage).mapping().is_ok(),
            _ => true,
        }
    }
    pub(crate) fn event(self, rect: uu_input::DesktopRect) -> Option<uu_input::InputEvent> {
        use uu_input::InputEvent as I;
        Some(match self {
            Self::Absolute { x, y } => {
                let (x, y) = rect.point(x, y);
                I::MouseMoveAbsolute { x, y }
            }
            Self::Relative { dx, dy } => I::MouseMoveRelative {
                dx: i32::from(dx),
                dy: i32::from(dy),
            },
            Self::Button { button, pressed } => I::MouseButton { button, pressed },
            Self::Wheel { dx, dy } => I::MouseScroll {
                dx: i32::from(dx),
                dy: i32::from(dy),
            },
            Self::Key { usage, pressed } => I::Key {
                key: KeyCode(usage),
                pressed,
            },
            Self::ReleaseAll {} => return None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("native input queue is unavailable")]
pub struct InputQueueError;

#[derive(Clone)]
pub struct NativeInputSender {
    tx: mpsc::Sender<Envelope>,
    displays:mpsc::Sender<(u64,i32)>,
    reset: watch::Sender<u64>,
    mode: watch::Sender<bool>,
    pub(crate) requested: Arc<AtomicBool>,
    pub(crate) epoch: Arc<AtomicU64>,
}
pub struct NativeInputReceiver {
    pub(crate) rx: mpsc::Receiver<Envelope>,
    displays:mpsc::Receiver<(u64,i32)>,
    pub(crate) reset: watch::Receiver<u64>,
    pub(crate) mode: watch::Receiver<bool>,
    pub(crate) requested: Arc<AtomicBool>,
    pub(crate) epoch: Arc<AtomicU64>,
}
pub(crate) struct Envelope {
    pub epoch: u64,
    pub command: InputCommand,
    pub created:std::time::Instant,
}
pub(crate) enum NextInput {
    Display(u64,i32),
    Mode(bool),
    Reset,
    Command(Envelope),
    Closed,
}
impl NativeInputReceiver {
    pub(crate) async fn next(&mut self) -> NextInput {
        tokio::select! { biased;
            mode=self.mode.changed()=>if mode.is_ok(){NextInput::Mode(*self.mode.borrow_and_update())}else{NextInput::Closed},
            reset=self.reset.changed()=>if reset.is_ok(){self.reset.borrow_and_update();NextInput::Reset}else{NextInput::Closed},
            display=self.displays.recv()=>display.map_or(NextInput::Closed,|(request,id)|NextInput::Display(request,id)),
            item=self.rx.recv()=>item.map_or(NextInput::Closed,NextInput::Command),
        }
    }
}
pub fn channel(requested: bool) -> (NativeInputSender, NativeInputReceiver) {
    let (tx, rx) = mpsc::channel(256);
    let (displays,displays_rx)=mpsc::channel(8);
    let (reset, reset_rx) = watch::channel(0);
    let (mode, mode_rx) = watch::channel(requested);
    let requested = Arc::new(AtomicBool::new(requested));
    let epoch = Arc::new(AtomicU64::new(0));
    (
        NativeInputSender {
            tx,
            displays,
            reset,
            mode,
            requested: requested.clone(),
            epoch: epoch.clone(),
        },
        NativeInputReceiver {
            rx,
            displays:displays_rx,
            reset: reset_rx,
            mode: mode_rx,
            requested,
            epoch,
        },
    )
}
impl NativeInputSender {
    pub fn select_display(&self,request:u64,id:i32)->Result<(),InputQueueError>{
        self.release_all();
        self.displays.try_send((request,id)).map_err(|_|InputQueueError)
    }
    pub fn send(&self, command: InputCommand) -> Result<(), InputQueueError> {
        let envelope = Envelope {
            epoch: self.epoch.load(Ordering::Acquire),
            command,
            created:std::time::Instant::now(),
        };
        self.tx.try_send(envelope).map_err(|_| {
            self.release_all();
            InputQueueError
        })
    }
    /// Out-of-band generation reset: a full motion queue must never drop a
    /// key/button release or replay old input after focus changed.
    pub fn release_all(&self) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.reset.send_replace(epoch);
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.requested.store(enabled, Ordering::Release);
        self.release_all();
        self.mode.send_replace(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflowing_input_invalidates_pending_actions_and_keeps_release_out_of_band() {
        let (sender, mut receiver) = channel(true);
        for _ in 0..256 {
            sender
                .send(InputCommand::Relative { dx: 1, dy: 0 })
                .unwrap();
        }
        assert!(sender.send(InputCommand::ReleaseAll {}).is_err());
        assert_eq!(*receiver.reset.borrow_and_update(), 1);
        assert!(receiver.rx.try_recv().unwrap().epoch < receiver.epoch.load(Ordering::Acquire));
        sender.set_enabled(false);
        assert!(!receiver.requested.load(Ordering::Acquire));
        assert_eq!(*receiver.mode.borrow_and_update(), false);
    }
    #[test]
    fn native_input_is_strict_and_maps_only_into_the_selected_monitor() {
        let rect = uu_input::DesktopRect::new(-1920, 0, 1920, 1080).unwrap();
        assert_eq!(
            InputCommand::Absolute { x: 65535, y: 65535 }.event(rect),
            Some(uu_input::InputEvent::MouseMoveAbsolute { x: -1, y: 1079 })
        );
        assert!(!InputCommand::Key {
            usage: 0xffff,
            pressed: true
        }
        .validate());
        assert!(
            serde_json::from_str::<InputCommand>(r#"{"type":"absolute","x":65536,"y":0}"#).is_err()
        );
        assert!(serde_json::from_str::<InputCommand>(
            r#"{"type":"release_all","command":"arbitrary"}"#
        )
        .is_err());
    }
}
