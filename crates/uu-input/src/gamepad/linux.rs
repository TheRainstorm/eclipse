use super::{ds4, Snapshot, State, VirtualGamepads, SLOTS};
use crate::InputError;
use std::{ffi::CString,fs::{File,OpenOptions},os::{fd::AsRawFd,unix::fs::OpenOptionsExt}};
const DESCRIPTOR:&[u8]=include_bytes!("../../../../vendor/ds4-data/descriptor.bin");
const PAIRING:&[u8]=include_bytes!("../../../../vendor/ds4-data/pairing.bin");
const FIRMWARE:&[u8]=include_bytes!("../../../../vendor/ds4-data/firmware.bin");
extern "C" {
    fn eclipse_pad_create(fd:i32,unique:*const std::ffi::c_char,descriptor:*const u8,size:u32)->i32;
    fn eclipse_pad_input(fd:i32,bytes:*const u8,size:u32)->i32;
    fn eclipse_pad_destroy(fd:i32)->i32;
    fn eclipse_pad_poll(fd:i32,id:*mut u32,number:*mut u32)->i32;
    fn eclipse_pad_reply(fd:i32,id:u32,number:u32,bytes:*const u8,size:u32)->i32;
    fn eclipse_pad_set_reply(fd:i32,id:u32)->i32;
}
fn result(code:i32)->Result<(),InputError>{if code<0 {Err(InputError::Backend(format!("virtual DS4: {}",std::io::Error::from_raw_os_error(-code))))}else{Ok(())}}
struct Device {file:File,mac:[u8;6]}
impl Device {
    fn new()->Result<Self,InputError>{
        let file=OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK|libc::O_CLOEXEC).open("/dev/uhid")
            .map_err(|e|InputError::Unavailable(format!("无法打开 /dev/uhid：{e}；请为运行共享的用户配置 UHID 访问权限")))?;
        let mut mac=[0;6];mac.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..6]);mac[0]=(mac[0]|2)&!1;
        let unique=CString::new(mac.iter().map(|v|format!("{v:02x}")).collect::<Vec<_>>().join(":" )).unwrap();
        result(unsafe{eclipse_pad_create(file.as_raw_fd(),unique.as_ptr(),DESCRIPTOR.as_ptr(),DESCRIPTOR.len() as u32)})?;
        Ok(Self{file,mac})
    }
    fn update(&mut self,state:&State)->Result<(),InputError>{
        let report=ds4::report(state,ds4::USB_CALIBRATION);
        result(unsafe{eclipse_pad_input(self.file.as_raw_fd(),report.as_ptr(),report.len() as u32)})
    }
    fn poll(&mut self)->Result<(),InputError>{
        for _ in 0..32 {
            let (mut id,mut number)=(0,0);let fd=self.file.as_raw_fd();
            match unsafe{eclipse_pad_poll(fd,&mut id,&mut number)} {
                0=>break,
                1=>{
                    let data:Vec<u8>=match number {
                        2=>ds4::USB_CALIBRATION.to_vec(),0xa3=>FIRMWARE.to_vec(),
                        0x12=>{let mut bytes=PAIRING.to_vec();for (index,byte) in self.mac.iter().rev().enumerate(){bytes[index+1]=*byte;}bytes},_=>Vec::new(),
                    };
                    result(unsafe{eclipse_pad_reply(fd,id,number,data.as_ptr(),data.len() as u32)})?;
                },
                2=>result(unsafe{eclipse_pad_set_reply(fd,id)})?,
                3=>{},code=>result(code)?,
            }
        }Ok(())
    }
}
impl Drop for Device {fn drop(&mut self){let _=self.update(&State::default());unsafe{eclipse_pad_destroy(self.file.as_raw_fd());}}}
#[derive(Default)] pub struct Backend {pads:[Option<Device>;SLOTS]}
impl VirtualGamepads for Backend {
    fn update(&mut self,state:&Snapshot)->Result<(),InputError>{
        for (slot,state) in state.iter().enumerate(){
            if let Some(state)=state {
                if !state.valid(){return Err(InputError::Backend("invalid controller state".into()));}
                if self.pads[slot].is_none(){self.pads[slot]=Some(Device::new()?);}
                self.pads[slot].as_mut().unwrap().update(state)?;
            }else{self.pads[slot].take();}
        }self.poll()
    }
    fn reset(&mut self)->Result<(),InputError>{
        let mut error=None;
        for slot in &mut self.pads {if let Some(mut pad)=slot.take(){if let Err(e)=pad.update(&State::default()){error=Some(e);}drop(pad);}}
        error.map_or(Ok(()),Err)
    }
    fn poll(&mut self)->Result<(),InputError>{for pad in self.pads.iter_mut().flatten(){pad.poll()?;}Ok(())}
}
