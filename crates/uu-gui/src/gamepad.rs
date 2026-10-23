//! SDL owns the physical controller and sensors on one dedicated thread.
//! The GUI supplies its ordinary input-focus decision; this is not a second
//! input policy and it never starts capture or requests control itself.
use std::{
    sync::{atomic::Ordering, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use uu_input::gamepad::{Snapshot, State, SLOTS};
#[derive(Clone)]
pub(super) struct Target {
    pub input: uu_core::native_input::NativeInputSender,
    pub stats: Arc<uu_core::media::MediaStats>,
}
#[derive(Default)]
struct Desired {
    target: Option<Target>,
    generation: u64,
    stop: bool,
}
#[derive(Default)]
pub(super) struct Reader {
    shared: Arc<(Mutex<Desired>, Condvar)>,
    task: Option<std::thread::JoinHandle<()>>,
    key: Option<usize>,
    owner:Option<u64>,
}
impl Reader {
    pub fn configure(&mut self,owner:u64,target: Option<Target>) {
        // A background window may release only its own lease. There is one
        // physical SDL reader for the entire streaming workspace.
        if target.is_none() && self.owner!=Some(owner){return;}
        self.owner=target.as_ref().map(|_|owner);
        let key = target.as_ref().map(|t| Arc::as_ptr(&t.stats) as usize);
        if key == self.key {
            return;
        }
        self.key = key;
        {
            let mut state = self.shared.0.lock().unwrap();
            if let Some(previous) = state.target.take() {
                neutral(&previous);
            }
            state.target = target;
            state.generation = state.generation.wrapping_add(1);
        }
        self.shared.1.notify_one();
        if self.task.is_none() && key.is_some() {
            let shared = self.shared.clone();
            match std::thread::Builder::new()
                .name("local-gamepad".into())
                .spawn(move || run(shared))
            {
                Ok(task) => self.task = Some(task),
                Err(error) => {
                    if let Some(target) = &self.shared.0.lock().unwrap().target {
                        *target.stats.gamepad.local_status.lock().unwrap() =
                            format!("无法启动手柄线程：{error}");
                    }
                }
            }
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if self.task.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = self.task.take().unwrap().join();
        }
    }
}
fn neutral(target: &Target) {
    target.input.gamepads([None; SLOTS]);
    target.stats.gamepad.active.store(false, Ordering::Release);
}
fn run(shared: Arc<(Mutex<Desired>, Condvar)>) {
    let mut target: Option<Target> = None;
    let mut generation = u64::MAX;
    let mut sampler: Option<Sampler> = None;
    let mut retry = Instant::now();
    let mut published = Instant::now() - Duration::from_secs(2);
    let mut has_pad = false;
    loop {
        let desired = shared.0.lock().unwrap();
        if desired.stop {
            break;
        }
        if desired.generation != generation {
            if let Some(target) = target.take() {
                neutral(&target);
            }
            sampler = None;
            generation = desired.generation;
            target = desired.target.clone();
            retry = Instant::now();
            published = Instant::now() - Duration::from_secs(2);
            has_pad = false;
        }
        if target.is_none() {
            drop(
                shared
                    .1
                    .wait_timeout(desired, Duration::from_secs(1))
                    .unwrap(),
            );
            continue;
        }
        drop(desired);
        let current = target.as_ref().unwrap();
        if sampler.is_none() && Instant::now() >= retry {
            match Sampler::new() {
                Ok(created) => sampler = Some(created),
                Err(error) => {
                    *current.stats.gamepad.local_status.lock().unwrap() = error;
                    retry = Instant::now() + Duration::from_secs(3);
                }
            }
        }
        if let Some(reader) = &mut sampler {
            let (state, notice) = reader.sample();
            // Recheck after SDL work: focus can change while a driver opens.
            let desired = shared.0.lock().unwrap();
            if desired.generation == generation && !desired.stop {
                current.stats.gamepad.active.store(true, Ordering::Release);
                current
                    .stats
                    .gamepad
                    .local_count
                    .store(state.iter().flatten().count(), Ordering::Release);
                current.stats.gamepad.motion.store(
                    state.iter().flatten().any(|s| s.gyro.is_some()),
                    Ordering::Release,
                );
                *current.stats.gamepad.local_status.lock().unwrap() = notice;
                has_pad = state.iter().any(Option::is_some);
                if has_pad || published.elapsed() >= Duration::from_secs(1) {
                    current.input.gamepads(state);
                    published = Instant::now();
                }
            }
        }
        let desired = shared.0.lock().unwrap();
        drop(
            shared
                .1
                .wait_timeout(
                    desired,
                    if has_pad {
                        Duration::from_millis(8)
                    } else {
                        Duration::from_millis(100)
                    },
                )
                .unwrap(),
        );
    }
    if let Some(target) = target {
        neutral(&target);
    }
}
struct Sampler {
    // Field drop order keeps SDL alive until all controller handles are closed.
    pads: [Option<sdl2::controller::GameController>; SLOTS],
    events: sdl2::EventPump,
    controllers: sdl2::GameControllerSubsystem,
    _sdl: sdl2::Sdl,
    start: Instant,
    last_scan: Instant,
    notice: String,
    #[cfg(test)]
    only: Option<u32>,
}
impl Sampler {
    fn new() -> Result<Self, String> {
        sdl2::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        let sdl = sdl2::init().map_err(|e| format!("SDL 手柄初始化失败：{e}"))?;
        let controllers = sdl
            .game_controller()
            .map_err(|e| format!("无法打开本地手柄子系统：{e}"))?;
        let events = sdl
            .event_pump()
            .map_err(|e| format!("无法读取本地手柄：{e}"))?;
        Ok(Self {
            pads: std::array::from_fn(|_| None),
            events,
            controllers,
            _sdl: sdl,
            start: Instant::now(),
            last_scan: Instant::now() - Duration::from_secs(2),
            notice: String::new(),
            #[cfg(test)]
            only: None,
        })
    }
    fn sample(&mut self) -> (Snapshot, String) {
        let mut hotplug = false;
        for event in self.events.poll_iter() {
            hotplug |= matches!(
                event,
                sdl2::event::Event::ControllerDeviceAdded { .. }
                    | sdl2::event::Event::ControllerDeviceRemoved { .. }
            );
        }
        for slot in &mut self.pads {
            if slot.as_ref().is_some_and(|p| !p.attached()) {
                slot.take();
            }
        }
        if hotplug || self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.last_scan = Instant::now();
            self.notice.clear();
            for index in 0..self.controllers.num_joysticks().unwrap_or(0).min(32) {
                if self.pads.iter().all(Option::is_some) {
                    break;
                }
                if !self.controllers.is_game_controller(index) {
                    continue;
                }
                let id = unsafe { sdl2::sys::SDL_JoystickGetDeviceInstanceID(index as i32) };
                #[cfg(test)]
                if self.only.is_some_and(|only| only != id as u32) {
                    continue;
                }
                if id < 0
                    || self
                        .pads
                        .iter()
                        .flatten()
                        .any(|p| p.instance_id() == id as u32)
                {
                    continue;
                }
                match self.controllers.open(index) {
                    Ok(pad) => {
                        for sensor in [
                            sdl2::sensor::SensorType::Gyroscope,
                            sdl2::sensor::SensorType::Accelerometer,
                        ] {
                            if pad.has_sensor(sensor) {
                                if let Err(e) = pad.sensor_set_enabled(sensor, true) {
                                    self.notice = format!("手柄可用，但运动传感器未能启用：{e}");
                                }
                            }
                        }
                        *self.pads.iter_mut().find(|slot| slot.is_none()).unwrap() = Some(pad);
                    }
                    Err(e) => self.notice = format!("本地手柄不可用：{e}"),
                }
            }
        }
        let timestamp_us = self.start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let state = std::array::from_fn(|slot| {
            self.pads[slot]
                .as_ref()
                .map(|pad| sample(pad, timestamp_us))
        });
        let notice = if !self.notice.is_empty() {
            self.notice.clone()
        } else if state.iter().all(Option::is_none) {
            "未检测到本地手柄".into()
        } else {
            String::new()
        };
        (state, notice)
    }
}
fn sample(pad: &sdl2::controller::GameController, timestamp_us: u64) -> State {
    use sdl2::controller::{Axis, Button};
    let mut buttons = 0;
    for button in [
        Button::A,
        Button::B,
        Button::X,
        Button::Y,
        Button::Back,
        Button::Guide,
        Button::Start,
        Button::LeftStick,
        Button::RightStick,
        Button::LeftShoulder,
        Button::RightShoulder,
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
        Button::Touchpad,
    ] {
        if pad.button(button) {
            buttons |= 1u32 << (button as u32);
        }
    }
    let sensor = |kind| {
        let mut data = [0.; 3];
        (pad.has_sensor(kind)
            && pad.sensor_enabled(kind)
            && pad.sensor_get_data(kind, &mut data).is_ok())
        .then_some(data)
    };
    State {
        buttons,
        sticks: [Axis::LeftX, Axis::LeftY, Axis::RightX, Axis::RightY].map(|axis| pad.axis(axis)),
        triggers: [Axis::TriggerLeft, Axis::TriggerRight]
            .map(|axis| (i32::from(pad.axis(axis)).max(0) * 65535 / 32767) as u16),
        timestamp_us,
        gyro: sensor(sdl2::sensor::SensorType::Gyroscope),
        acceleration: sensor(sdl2::sensor::SensorType::Accelerometer),
    }
}

impl super::App {
    pub(super) fn gamepad_status(&self) -> Option<String> {
        if !self.is_native() {
            return None;
        }
        let stats = &self.controller.as_ref()?.stats.gamepad;
        let mut text = format!(
            "最近检测到 {} 个本地手柄 · {} · {}",
            stats.local_count.load(Ordering::Acquire),
            if stats.motion.load(Ordering::Acquire) {
                "陀螺仪可用"
            } else {
                "无陀螺仪数据"
            },
            if stats.active.load(Ordering::Acquire) {
                "正在转发"
            } else {
                "已暂停转发"
            }
        );
        let local = stats.local_status.lock().unwrap();
        if !local.is_empty() {
            text.push_str(&format!("\n{local}"));
        }
        let remote = stats.status.lock().unwrap();
        if !remote.is_empty() {
            text.push_str(&format!("\n被控端：{remote}"));
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_session_cannot_release_the_current_gamepad_recipient() {
        let mut reader=Reader::default();reader.owner=Some(11);reader.key=Some(42);
        reader.configure(22,None);
        assert_eq!(reader.owner,Some(11));assert_eq!(reader.key,Some(42));
        reader.configure(11,None);
        assert_eq!(reader.owner,None);assert_eq!(reader.key,None);
        assert!(reader.task.is_none(),"releasing focus must not open physical devices");
    }
    #[test]
    fn owned_sdl_virtual_gamepad_preserves_axes_buttons_slots_and_hot_unplug() {
        std::thread::spawn(|| {
            let mut sampler = Sampler::new().unwrap();
            let index = unsafe {
                sdl2::sys::SDL_JoystickAttachVirtual(
                    sdl2::sys::SDL_JoystickType::SDL_JOYSTICK_TYPE_GAMECONTROLLER,
                    6,
                    21,
                    1,
                )
            };
            assert!(index >= 0, "{}", sdl2::get_error());
            struct OwnedPad {
                index: i32,
                pad: *mut sdl2::sys::SDL_Joystick,
            }
            impl Drop for OwnedPad {
                fn drop(&mut self) {
                    unsafe {
                        sdl2::sys::SDL_JoystickClose(self.pad);
                        if self.index >= 0 {
                            sdl2::sys::SDL_JoystickDetachVirtual(self.index);
                        }
                    }
                }
            }
            let pad = OwnedPad {
                index,
                pad: unsafe { sdl2::sys::SDL_JoystickOpen(index) },
            };
            assert!(!pad.pad.is_null());
            sampler.only =
                Some(unsafe { sdl2::sys::SDL_JoystickGetDeviceInstanceID(index) } as u32);
            sampler.sample(); // Opening a controller establishes SDL trigger zero points.
            unsafe {
                assert_eq!(sdl2::sys::SDL_JoystickSetVirtualAxis(pad.pad, 0, 16000), 0);
                assert_eq!(sdl2::sys::SDL_JoystickSetVirtualAxis(pad.pad, 1, -10000), 0);
                assert_eq!(sdl2::sys::SDL_JoystickSetVirtualAxis(pad.pad, 4, 32767), 0);
                assert_eq!(sdl2::sys::SDL_JoystickSetVirtualButton(pad.pad, 0, 1), 0);
                sdl2::sys::SDL_JoystickUpdate();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            let states = loop {
                let (state, _) = sampler.sample();
                if state[0].is_some_and(|s| s.buttons & 1 != 0) {
                    break state;
                }
                assert!(
                    Instant::now() < deadline,
                    "SDL did not update owned virtual pad"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            let state = states[0].unwrap();
            assert_eq!(state.sticks[0], 16000);
            assert_eq!(state.sticks[1], -10000);
            assert_eq!(state.triggers[0], 65535);
            assert_eq!(state.gyro, None);
            assert!(states[1..].iter().all(Option::is_none));
            let mut pad = pad;
            unsafe {
                assert_eq!(sdl2::sys::SDL_JoystickDetachVirtual(pad.index), 0);
            }
            pad.index = -1;
            assert!(
                sampler.sample().0.iter().all(Option::is_none),
                "unplugged held buttons must not remain in the snapshot"
            );
        })
        .join()
        .unwrap();
    }
}
