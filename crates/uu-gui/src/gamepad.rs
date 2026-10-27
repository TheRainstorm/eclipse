//! SDL owns the physical controller and sensors on one dedicated thread.
//! The GUI supplies its ordinary input-focus decision; this is not a second
//! input policy and it never starts capture or requests control itself.
use std::{
    sync::{atomic::Ordering, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use uu_input::gamepad::{Kind,Snapshot, State, SLOTS};
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
                            uu_i18n::format!("无法启动手柄线程：{error}");
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
    // Windows Raw Input arrives through SDL's message-only window. Without
    // the video event pump SDL updates XInput triggers but misses WM_INPUT
    // buttons/sticks. No visible window or focus change is needed.
    #[cfg(windows)]
    _video:sdl2::VideoSubsystem,
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
        let sdl = sdl2::init().map_err(|e| uu_i18n::format!("SDL 手柄初始化失败：{e}"))?;
        #[cfg(windows)]
        let video=sdl.video().map_err(|e|uu_i18n::format!("无法启动 Windows 手柄消息泵：{e}"))?;
        let controllers = sdl
            .game_controller()
            .map_err(|e| uu_i18n::format!("无法打开本地手柄子系统：{e}"))?;
        let events = sdl
            .event_pump()
            .map_err(|e| uu_i18n::format!("无法读取本地手柄：{e}"))?;
        Ok(Self {
            pads: std::array::from_fn(|_| None),
            events,
            controllers,
            #[cfg(windows)]
            _video:video,
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
                                    self.notice = uu_i18n::format!("手柄可用，但运动传感器未能启用：{e}");
                                }
                            }
                        }
                        *self.pads.iter_mut().find(|slot| slot.is_none()).unwrap() = Some(pad);
                    }
                    Err(e) => self.notice = uu_i18n::format!("本地手柄不可用：{e}"),
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
            uu_i18n::text("未检测到本地手柄").into()
        } else {
            let xbox=state.iter().flatten().filter(|pad|pad.kind==Kind::Xbox360).count();
            let ds4=state.iter().flatten().count()-xbox;
            uu_i18n::format!("模拟类型：Xbox 360 × {xbox}，DS4 × {ds4}")
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
        kind: {
            // SDL owns this handle; borrow it only on the sampler's SDL thread.
            let controller=unsafe{sdl2::sys::SDL_GameControllerFromInstanceID(pad.instance_id() as i32)};
            let kind=if controller.is_null(){sdl2::sys::SDL_GameControllerType::SDL_CONTROLLER_TYPE_UNKNOWN}else{unsafe{sdl2::sys::SDL_GameControllerGetType(controller)}};
            output_kind(kind,pad.has_sensor(sdl2::sensor::SensorType::Gyroscope))
        },
        buttons,
        sticks: [Axis::LeftX, Axis::LeftY, Axis::RightX, Axis::RightY].map(|axis| pad.axis(axis)),
        triggers: [Axis::TriggerLeft, Axis::TriggerRight]
            .map(|axis| (i32::from(pad.axis(axis)).max(0) * 65535 / 32767) as u16),
        timestamp_us,
        gyro: sensor(sdl2::sensor::SensorType::Gyroscope),
        acceleration: sensor(sdl2::sensor::SensorType::Accelerometer),
    }
}
fn output_kind(kind:sdl2::sys::SDL_GameControllerType,motion:bool)->Kind {
    use sdl2::sys::SDL_GameControllerType::*;
    match kind {
        SDL_CONTROLLER_TYPE_XBOX360|SDL_CONTROLLER_TYPE_XBOXONE=>Kind::Xbox360,
        SDL_CONTROLLER_TYPE_PS3|SDL_CONTROLLER_TYPE_PS4|SDL_CONTROLLER_TYPE_PS5
        |SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_PRO|SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_LEFT
        |SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_RIGHT|SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_JOYCON_PAIR=>Kind::DualShock4,
        _=>if motion{Kind::DualShock4}else{Kind::Xbox360},
    }
}

impl super::App {
    pub(super) fn gamepad_status(&self) -> Option<String> {
        if !self.is_native() {
            return None;
        }
        let stats = &self.controller.as_ref()?.stats.gamepad;
        let mut text = uu_i18n::format!(
            "最近检测到 {} 个本地手柄 · {} · {}",
            stats.local_count.load(Ordering::Acquire),
            if stats.motion.load(Ordering::Acquire) {
                uu_i18n::text("陀螺仪可用")
            } else {
                uu_i18n::text("无陀螺仪数据")
            },
            if stats.active.load(Ordering::Acquire) {
                uu_i18n::text("正在转发")
            } else {
                uu_i18n::text("已暂停转发")
            }
        );
        let local = stats.local_status.lock().unwrap();
        if !local.is_empty() {
            text.push_str(&format!("\n{local}"));
        }
        let remote = stats.status.lock().unwrap();
        if !remote.is_empty() {
            text.push_str(&uu_i18n::format!("\n被控端：{remote}"));
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Rust SDL owns one initialization thread per process, just like Reader's
    // single workspace owner. These native fixtures must not compete for it.
    static SDL_OWNER:Mutex<()>=Mutex::new(());
    #[cfg(windows)]
    #[test]
    fn sampler_dispatches_owned_windows_messages_without_a_visible_window(){
        std::thread::spawn(||{
            let _owner=SDL_OWNER.lock().unwrap();
            use windows_sys::Win32::{Foundation::*,UI::WindowsAndMessaging::*,System::LibraryLoader::GetModuleHandleW};
            const PROBE:u32=WM_APP+71;
            unsafe extern "system" fn procedure(window:HWND,message:u32,wparam:WPARAM,lparam:LPARAM)->LRESULT {
                if message==PROBE {
                    let seen=unsafe{GetWindowLongPtrW(window,GWLP_USERDATA)} as *const std::sync::atomic::AtomicBool;
                    if !seen.is_null(){unsafe{&*seen}.store(true,Ordering::Release);}return 0;
                }
                unsafe{DefWindowProcW(window,message,wparam,lparam)}
            }
            let mut sampler=Sampler::new().unwrap();
            let name:Vec<u16>=format!("OwnedGamepadPump-{}",uuid::Uuid::new_v4()).encode_utf16().chain(Some(0)).collect();
            let module=unsafe{GetModuleHandleW(std::ptr::null())};
            let mut class:WNDCLASSW=unsafe{std::mem::zeroed()};class.lpfnWndProc=Some(procedure);class.lpszClassName=name.as_ptr();class.hInstance=module;
            assert_ne!(unsafe{RegisterClassW(&class)},0);
            let window=unsafe{CreateWindowExW(0,name.as_ptr(),name.as_ptr(),0,0,0,0,0,HWND_MESSAGE,std::ptr::null_mut(),module,std::ptr::null())};
            assert!(!window.is_null());
            struct OwnedWindow{window:HWND,module:HINSTANCE,name:Vec<u16>}
            impl Drop for OwnedWindow{fn drop(&mut self){unsafe{DestroyWindow(self.window);UnregisterClassW(self.name.as_ptr(),self.module);}}}
            let window=OwnedWindow{window,module,name};
            let seen=std::sync::atomic::AtomicBool::new(false);
            unsafe{SetWindowLongPtrW(window.window,GWLP_USERDATA,&seen as *const _ as isize);}
            assert_ne!(unsafe{PostMessageW(window.window,PROBE,0,0)},0);
            let deadline=Instant::now()+Duration::from_secs(2);
            while !seen.load(Ordering::Acquire){sampler.sample();assert!(Instant::now()<deadline,"SDL did not dispatch its thread's Windows messages");std::thread::sleep(Duration::from_millis(5));}
        }).join().unwrap();
    }
    #[test]
    #[ignore="reads attached physical controllers for 25 seconds; run only with the device owner's authorization"]
    fn authorized_physical_controller_sampling(){
        std::thread::spawn(||{
            let _owner=SDL_OWNER.lock().unwrap();
            use std::io::Write;
            let mut sampler=Sampler::new().unwrap();let start=Instant::now();let mut last=None;let mut observed=false;let mut changes=0;
            while start.elapsed()<Duration::from_secs(25){
                let (mut state,notice)=sampler.sample();
                for pad in state.iter_mut().flatten(){pad.timestamp_us=0;}
                if Some(state)!=last {
                    observed|=state.iter().any(Option::is_some);changes+=1;
                    let names:Vec<_>=sampler.pads.iter().flatten().map(|pad|pad.name()).collect();
                    println!("{}",serde_json::json!({"event":"physical_gamepad_sample","elapsed_ms":start.elapsed().as_millis(),"names":names,"notice":notice,"state":state}));
                    std::io::stdout().flush().unwrap();last=Some(state);
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            println!("{}",serde_json::json!({"event":"physical_gamepad_complete","observed":observed,"changes":changes}));
            assert!(observed,"no physical controller was accessible in this session");
        }).join().unwrap();
    }
    #[test]
    fn controller_identity_selects_xbox_or_motion_capable_playstation(){
        use sdl2::sys::SDL_GameControllerType::*;
        assert_eq!(output_kind(SDL_CONTROLLER_TYPE_XBOXONE,true),Kind::Xbox360);
        assert_eq!(output_kind(SDL_CONTROLLER_TYPE_UNKNOWN,false),Kind::Xbox360);
        assert_eq!(output_kind(SDL_CONTROLLER_TYPE_UNKNOWN,true),Kind::DualShock4);
        assert_eq!(output_kind(SDL_CONTROLLER_TYPE_NINTENDO_SWITCH_PRO,false),Kind::DualShock4);
        assert_eq!(output_kind(SDL_CONTROLLER_TYPE_PS5,true),Kind::DualShock4);
    }
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
            let _owner=SDL_OWNER.lock().unwrap();
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
