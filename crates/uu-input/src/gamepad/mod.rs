//! Full controller state in SDL's portable coordinates and SI sensor units.
use crate::InputError;
use serde::{Deserialize, Serialize};
mod ds4;
#[cfg(target_os="linux")] mod linux;
#[cfg(windows)] mod windows;
pub const SLOTS: usize = 4;
pub const BUTTON_MASK: u32 = 0x7fff | (1 << 20);
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub buttons: u32,
    pub sticks: [i16; 4],
    pub triggers: [u16; 2],
    /// Relative monotonic sample time; never interpreted as a wall clock.
    pub timestamp_us: u64,
    /// Radians per second, matching SDL_SENSOR_GYRO.
    pub gyro: Option<[f32; 3]>,
    /// Metres per second squared, matching SDL_SENSOR_ACCEL.
    pub acceleration: Option<[f32; 3]>,
}
impl Default for State {
    fn default()->Self{Self{buttons:0,sticks:[0;4],triggers:[0;2],timestamp_us:0,gyro:None,acceleration:None}}
}
impl State {
    pub fn valid(&self)->bool {
        self.buttons & !BUTTON_MASK == 0
            && self.gyro.is_none_or(|v|v.iter().all(|x|x.is_finite()&&x.abs()<=100.))
            && self.acceleration.is_none_or(|v|v.iter().all(|x|x.is_finite()&&x.abs()<=200.))
    }
}
pub type Snapshot = [Option<State>; SLOTS];
pub trait VirtualGamepads: Send {
    fn update(&mut self, state: &Snapshot)->Result<(),InputError>;
    /// Neutralize and unplug only controllers owned by this backend.
    fn reset(&mut self)->Result<(),InputError>;
    fn poll(&mut self)->Result<(),InputError>{Ok(())}
}
pub fn default_backend()->Result<Box<dyn VirtualGamepads>,InputError> {
    #[cfg(target_os="linux")] {return Ok(Box::new(linux::Backend::default()));}
    #[cfg(windows)] {return Ok(Box::new(windows::Backend::new()?));}
    #[allow(unreachable_code)] Err(InputError::Unavailable("virtual gamepads are unavailable on this platform".into()))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn untrusted_sensor_values_and_unmapped_buttons_are_rejected(){
        assert!(State::default().valid());
        for v in [f32::NAN,f32::INFINITY,101.] {assert!(!State{gyro:Some([v,0.,0.]),..State::default()}.valid());}
        assert!(!State{buttons:1<<31,..State::default()}.valid());
        assert!(State{buttons:BUTTON_MASK,gyro:Some([1.,-1.,0.]),..State::default()}.valid());
    }
}
