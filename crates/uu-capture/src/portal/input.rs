use ashpd::desktop::{
    remote_desktop::{Axis, KeyState, RemoteDesktop},
    Session,
};
use std::{sync::mpsc, time::Duration};
use uu_input::{HeldInput, InputError, InputEvent, InputInjector, MouseButton};

pub(super) enum Event {
    Absolute(f64, f64),
    Relative(f64, f64),
    Button(i32, bool),
    Key(i32, bool),
    Scroll(i32, i32),
}
pub(super) struct Command {
    pub event: Event,
    pub reply: mpsc::SyncSender<Result<(), String>>,
}
pub(super) async fn dispatch(
    remote: &RemoteDesktop<'_>,
    session: &Session<'_, RemoteDesktop<'_>>,
    node: u32,
    event: Event,
) -> ashpd::Result<()> {
    let state = |pressed| {
        if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        }
    };
    match event {
        Event::Absolute(x, y) => {
            remote
                .notify_pointer_motion_absolute(session, node, x, y)
                .await
        }
        Event::Relative(x, y) => remote.notify_pointer_motion(session, x, y).await,
        Event::Button(button, pressed) => {
            remote
                .notify_pointer_button(session, button, state(pressed))
                .await
        }
        Event::Key(key, pressed) => {
            remote
                .notify_keyboard_keycode(session, key, state(pressed))
                .await
        }
        Event::Scroll(x, y) => {
            if x != 0 {
                remote
                    .notify_pointer_axis_discrete(session, Axis::Horizontal, x)
                    .await?;
            }
            if y != 0 {
                remote
                    .notify_pointer_axis_discrete(session, Axis::Vertical, y)
                    .await?;
            }
            Ok(())
        }
    }
}
pub(super) struct Injector {
    tx: tokio::sync::mpsc::Sender<Command>,
    pixels: (u32, u32),
    logical: (u32, u32),
    held: HeldInput,
    wheel: (i32, i32),
}
impl Injector {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<Command>,
        pixels: (u32, u32),
        logical: (u32, u32),
    ) -> Self {
        Self {
            tx,
            pixels,
            logical,
            held: HeldInput::default(),
            wheel: (0, 0),
        }
    }
    fn send(&self, event: Event) -> Result<(), InputError> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command { event, reply })
            .map_err(|_| InputError::Unavailable("系统共享会话已关闭或输入队列繁忙".into()))?;
        rx.recv_timeout(Duration::from_millis(300))
            .map_err(|_| InputError::Backend("系统输入处理超时".into()))?
            .map_err(InputError::Backend)
    }
    fn emit(&mut self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::MouseMoveAbsolute { x, y } => self.send(Event::Absolute(
                (x as f64).clamp(0., self.pixels.0.saturating_sub(1) as f64)
                    * self.logical.0 as f64
                    / self.pixels.0.max(1) as f64,
                (y as f64).clamp(0., self.pixels.1.saturating_sub(1) as f64)
                    * self.logical.1 as f64
                    / self.pixels.1.max(1) as f64,
            )),
            InputEvent::MouseMoveRelative { dx, dy } => {
                self.send(Event::Relative(dx as f64, dy as f64))
            }
            InputEvent::MouseButton { button, pressed } => self.send(Event::Button(
                match button {
                    MouseButton::Left => 0x110,
                    MouseButton::Right => 0x111,
                    MouseButton::Middle => 0x112,
                    MouseButton::Back => 0x113,
                    MouseButton::Forward => 0x114,
                },
                pressed,
            )),
            InputEvent::Key { key, pressed } => {
                self.send(Event::Key(key.mapping()?.evdev as i32, pressed))
            }
            InputEvent::MouseScroll { dx, dy } => {
                let x = self.wheel.0.saturating_add(dx);
                let y = self.wheel.1.saturating_sub(dy);
                self.send(Event::Scroll(x / 120, y / 120))?;
                self.wheel = (x % 120, y % 120);
                Ok(())
            }
        }
    }
}
impl InputInjector for Injector {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        if self.held.should_emit(event) {
            self.emit(event)?;
            self.held.applied(event);
        }
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), InputError> {
        let mut failure = None;
        for event in self.held.releases() {
            if let Err(e) = self.inject(event) {
                failure.get_or_insert(e);
            }
        }
        self.wheel = (0, 0);
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
