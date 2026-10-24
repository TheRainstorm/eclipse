use super::{ds4,xbox,Kind,Snapshot,State,VirtualGamepads,SLOTS};
use crate::InputError;
use vigem_rust::{BusError,Client,ClientError,DualShock4,Xbox360,TargetHandle,Ds4ReportEx,X360Report,X360Button};
fn error(e:impl std::fmt::Display)->InputError{InputError::Unavailable(format!("ViGEmBus 虚拟手柄不可用：{e}；请确认已安装并启用兼容的 ViGEmBus 驱动"))}
// PnP can briefly reject reports even after the ready probe succeeded. Keep
// this owned device instead of repeatedly unplugging it during enumeration.
fn submit(mut update:impl FnMut()->Result<(),ClientError>)->Result<(),InputError>{
    let deadline=std::time::Instant::now()+std::time::Duration::from_millis(100);
    loop {match update(){
        Err(ClientError::Bus(BusError::TargetNotReady{..})) if std::time::Instant::now()<deadline=>std::thread::sleep(std::time::Duration::from_millis(5)),
        result=>return result.map_err(error),
    }}
}
enum Pad {Xbox(TargetHandle<Xbox360>),Ds4(TargetHandle<DualShock4>)}
impl Pad {
    fn kind(&self)->Kind{match self{Self::Xbox(_)=>Kind::Xbox360,Self::Ds4(_)=>Kind::DualShock4}}
    fn update(&self,state:&State)->Result<(),InputError>{match self {
        Self::Ds4(pad)=>submit(||pad.update_ex(&report(state))),
        Self::Xbox(pad)=>{
            let report=xbox::report(state);
            let report=X360Report{buttons:X360Button::from_bits_retain(report.buttons),left_trigger:report.triggers[0],right_trigger:report.triggers[1],thumb_lx:report.sticks[0],thumb_ly:report.sticks[1],thumb_rx:report.sticks[2],thumb_ry:report.sticks[3]};
            submit(||pad.update(&report))
        },
    }}
}
pub struct Backend {client:Client,pads:[Option<Pad>;SLOTS]}
impl Backend {
    pub fn new()->Result<Self,InputError>{Ok(Self{client:Client::connect().map_err(error)?,pads:std::array::from_fn(|_|None)})}
}
fn report(state:&State)->Ds4ReportEx{Ds4ReportEx::from_bytes(ds4::report(state,ds4::VIGEM_CALIBRATION)[1..].try_into().unwrap())}
impl VirtualGamepads for Backend {
    fn update(&mut self,state:&Snapshot)->Result<(),InputError>{
        for (slot,state) in state.iter().enumerate(){
            if let Some(state)=state {
                if !state.valid(){return Err(InputError::Backend("invalid controller state".into()));}
                if self.pads[slot].as_ref().is_some_and(|pad|pad.kind()!=state.kind) {
                    if let Some(previous)=self.pads[slot].take(){previous.update(&State::default())?;}
                }
                if self.pads[slot].is_none(){
                    let wait=std::time::Duration::from_secs(2);
                    self.pads[slot]=Some(match state.kind {
                        Kind::Xbox360=>Pad::Xbox(self.client.new_x360_target().plug().map_err(error)?.wait_for_ready_timeout(wait).map_err(error)?),
                        Kind::DualShock4=>Pad::Ds4(self.client.new_ds4_target().plug().map_err(error)?.wait_for_ready_timeout(wait).map_err(error)?),
                    });
                }
                self.pads[slot].as_ref().unwrap().update(state)?;
            }else{self.pads[slot].take();}
        }Ok(())
    }
    fn reset(&mut self)->Result<(),InputError>{
        let mut failed=None;
        for slot in &mut self.pads {if let Some(pad)=slot.take(){if let Err(e)=pad.update(&State::default()){failed=Some(e);}drop(pad);}}
        failed.map_or(Ok(()),Err)
    }
}
impl Drop for Backend {fn drop(&mut self){let _=self.reset();}}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn only_transient_enumeration_errors_retry_the_same_owned_target(){
        let mut calls=0;
        submit(||{calls+=1;if calls<3{Err(ClientError::Bus(BusError::TargetNotReady{serial_no:1}))}else{Ok(())}}).unwrap();
        assert_eq!(calls,3);
        calls=0;
        assert!(submit(||{calls+=1;Err(ClientError::TargetDoesNotExist{serial_no:1})}).is_err());
        assert_eq!(calls,1);
    }
    #[test]
    #[ignore="requires ViGEmBus; creates only an owned Xbox target with neutral buttons and a sub-deadzone stick sample"]
    fn owned_xbox_target_roundtrips_through_xinput_without_desktop_actions(){
        use windows_sys::Win32::UI::Input::XboxController::{XInputGetState,XINPUT_STATE};
        let disconnected:Vec<_>=(0..4).filter(|&index|{let mut state:XINPUT_STATE=unsafe{std::mem::zeroed()};(unsafe{XInputGetState(index,&mut state)})!=0}).collect();
        assert!(!disconnected.is_empty(),"the isolated test requires a free XInput slot");
        let mut backend=Backend::new().unwrap();
        let mut state=[None;SLOTS];state[0]=Some(State{kind:Kind::Xbox360,sticks:[256,0,0,0],..Default::default()});
        backend.update(&state).unwrap();
        assert!(matches!(&backend.pads[0],Some(Pad::Xbox(_))),"Xbox input must create an XInput device");
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(2);
        loop {
            let observed=disconnected.iter().find_map(|&index|{
                let mut observed:XINPUT_STATE=unsafe{std::mem::zeroed()};
                (unsafe{XInputGetState(index,&mut observed)}==0 && observed.Gamepad.sThumbLX==256).then_some(observed)
            });
            if let Some(observed)=observed {
                assert_eq!(observed.Gamepad.wButtons,0);assert_eq!(observed.Gamepad.bLeftTrigger,0);assert_eq!(observed.Gamepad.bRightTrigger,0);assert_eq!(observed.Gamepad.sThumbLY,0);break;
            }
            assert!(std::time::Instant::now()<deadline,"owned report did not reach the XInput slot");std::thread::sleep(std::time::Duration::from_millis(10));
        }
        backend.reset().unwrap();assert!(backend.pads.iter().all(Option::is_none));
    }
}
