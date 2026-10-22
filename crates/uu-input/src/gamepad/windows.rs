use super::{ds4,Snapshot,State,VirtualGamepads,SLOTS};
use crate::InputError;
use vigem_rust::{Client,DualShock4,TargetHandle,Ds4ReportEx};
fn error(e:impl std::fmt::Display)->InputError{InputError::Unavailable(format!("ViGEmBus 虚拟手柄不可用：{e}；请确认已安装并启用兼容的 ViGEmBus 驱动"))}
pub struct Backend {client:Client,pads:[Option<TargetHandle<DualShock4>>;SLOTS]}
impl Backend {
    pub fn new()->Result<Self,InputError>{Ok(Self{client:Client::connect().map_err(error)?,pads:std::array::from_fn(|_|None)})}
}
fn report(state:&State)->Ds4ReportEx{Ds4ReportEx::from_bytes(ds4::report(state,ds4::VIGEM_CALIBRATION)[1..].try_into().unwrap())}
impl VirtualGamepads for Backend {
    fn update(&mut self,state:&Snapshot)->Result<(),InputError>{
        for (slot,state) in state.iter().enumerate(){
            if let Some(state)=state {
                if !state.valid(){return Err(InputError::Backend("invalid controller state".into()));}
                if self.pads[slot].is_none(){
                    self.pads[slot]=Some(self.client.new_ds4_target().plug().map_err(error)?.wait_for_ready_timeout(std::time::Duration::from_secs(2)).map_err(error)?);
                }
                self.pads[slot].as_ref().unwrap().update_ex(&report(state)).map_err(error)?;
            }else{self.pads[slot].take();}
        }Ok(())
    }
    fn reset(&mut self)->Result<(),InputError>{
        let mut failed=None;
        for slot in &mut self.pads {if let Some(pad)=slot.take(){if let Err(e)=pad.update_ex(&report(&State::default())){failed=Some(error(e));}drop(pad);}}
        failed.map_or(Ok(()),Err)
    }
}
impl Drop for Backend {fn drop(&mut self){let _=self.reset();}}
