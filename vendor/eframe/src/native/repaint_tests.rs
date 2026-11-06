//! Exercise the real event-loop scheduler with owned Win32 windows and no GPU.
#![allow(unsafe_code)] // Read station identity and post only to PID-verified fixture HWNDs.
use super::*;
use std::sync::{Arc,atomic::{AtomicBool,AtomicU64,Ordering}};
use std::time::Duration;
use winit::platform::{run_on_demand::EventLoopExtRunOnDemand,windows::EventLoopBuilderExtWindows};
use winit::raw_window_handle::{HasWindowHandle,RawWindowHandle};
use winapi::um::winuser::{GetProcessWindowStation,GetUserObjectInformationW,GetWindowThreadProcessId,PostMessageW,ShowWindow,SW_SHOWNOACTIVATE,UOI_NAME,WM_MOUSEMOVE,WM_CLOSE};

struct Shared {busy:AtomicBool,paints:[AtomicU64;2],moves:AtomicU64}
struct Windows {
    ctx:egui::Context,windows:Vec<Arc<winit::window::Window>>,shared:Arc<Shared>,
    proxy:winit::event_loop::EventLoopProxy<UserEvent>,worker:Option<std::thread::JoinHandle<()>>,
}
fn viewport(index:usize)->egui::ViewportId {egui::ViewportId::from_hash_of(("owned-busy-window",index))}
impl WinitApp for Windows {
    fn egui_ctx(&self)->Option<&egui::Context>{Some(&self.ctx)}
    fn window(&self,id:WindowId)->Option<Arc<winit::window::Window>>{self.windows.iter().find(|window|window.id()==id).cloned()}
    fn window_id_from_viewport_id(&self,id:egui::ViewportId)->Option<WindowId>{self.windows.iter().enumerate().find(|(index,_)|viewport(*index)==id).map(|(_,window)|window.id())}
    fn save_and_destroy(&mut self){self.windows.clear();}
    fn run_ui_and_paint(&mut self,_:&ActiveEventLoop,id:WindowId)->Result<EventResult>{
        if self.shared.busy.load(Ordering::Acquire) {
            let index=self.windows.iter().position(|window|window.id()==id).unwrap();
            self.shared.paints[index].fetch_add(1,Ordering::Relaxed);
        }
        Ok(EventResult::Wait)
    }
    fn suspended(&mut self,_:&ActiveEventLoop)->Result<EventResult>{Ok(EventResult::Wait)}
    fn resumed(&mut self,event_loop:&ActiveEventLoop)->Result<EventResult>{
        for index in 0..2 {
            self.windows.push(Arc::new(event_loop.create_window(winit::window::Window::default_attributes()
                .with_title(format!("Owned repaint scheduler {index}"))).unwrap()));
        }
        let RawWindowHandle::Win32(handle)=self.windows[0].window_handle().unwrap().as_raw() else{panic!("not Win32");};
        for window in &self.windows {
            let RawWindowHandle::Win32(handle)=window.window_handle().unwrap().as_raw() else{panic!("not Win32");};
            // A real renderer shows its surface after its first paint. This
            // GPU-free fixture does that step explicitly on the owned station.
            unsafe{ShowWindow(handle.hwnd.get() as _,SW_SHOWNOACTIVATE);}
            println!("owned window visibility {:?}",window.is_visible());
        }
        let hwnd=handle.hwnd.get();let shared=self.shared.clone();let proxy=self.proxy.clone();
        self.worker=Some(std::thread::spawn(move|| {
            shared.busy.store(true,Ordering::Release);
            let started=Instant::now();let mut tick=0isize;
            while started.elapsed()<Duration::from_secs(2) {
                for index in 0..2 {proxy.send_event(UserEvent::RequestRepaint{viewport_id:viewport(index),when:Instant::now(),cumulative_pass_nr:0}).unwrap();}
                unsafe {
                    let mut owner=0;assert_ne!(GetWindowThreadProcessId(hwnd as _,&mut owner),0);
                    assert_eq!(owner,std::process::id(),"messages must stay in the fixture process");
                    for _ in 0..4 {assert_ne!(PostMessageW(hwnd as _,WM_MOUSEMOVE,0,(60<<16)|(40+tick%200)),0);}
                }
                tick+=1;std::thread::sleep(Duration::from_millis(4));
            }
            shared.busy.store(false,Ordering::Release);
            unsafe {assert_ne!(PostMessageW(hwnd as _,WM_CLOSE,0,0),0);}
        }));
        Ok(EventResult::Wait)
    }
    fn device_event(&mut self,_:&ActiveEventLoop,_:winit::event::DeviceId,_:winit::event::DeviceEvent)->Result<EventResult>{Ok(EventResult::Wait)}
    fn window_event(&mut self,_:&ActiveEventLoop,id:WindowId,event:winit::event::WindowEvent)->Result<EventResult>{
        match event {
            winit::event::WindowEvent::CursorMoved{..}=>{
                self.shared.moves.fetch_add(1,Ordering::Relaxed);
                // Model finite UI work while keeping the input queue nonempty.
                if self.shared.busy.load(Ordering::Acquire){std::thread::sleep(Duration::from_millis(1));}
                Ok(EventResult::RepaintNext(id))
            },
            winit::event::WindowEvent::CloseRequested=>Ok(EventResult::Exit),
            _=>Ok(EventResult::Wait),
        }
    }
    #[cfg(feature="accesskit")]
    fn on_accesskit_event(&mut self,_:accesskit_winit::Event)->Result<EventResult>{Ok(EventResult::Wait)}
}

#[test]
#[ignore="run alone on a noninteractive Windows station; uses owned windows and posted messages only"]
fn due_windows_repaint_while_another_window_has_continuous_input() {
    unsafe {
        let mut name=[0u16;256];let mut needed=0;
        assert_ne!(GetUserObjectInformationW(GetProcessWindowStation().cast(),UOI_NAME as i32,name.as_mut_ptr().cast(),std::mem::size_of_val(&name) as u32,&mut needed),0);
        let end=name.iter().position(|value|*value==0).unwrap();
        assert_ne!(String::from_utf16(&name[..end]).unwrap(),"WinSta0","refusing the interactive station");
    }
    let shared=Arc::new(Shared{busy:AtomicBool::new(false),paints:std::array::from_fn(|_|AtomicU64::new(0)),moves:AtomicU64::new(0)});
    let mut event_loop=EventLoop::with_user_event().with_any_thread(true).build().unwrap();
    let app=Windows{ctx:egui::Context::default(),windows:Vec::new(),shared:shared.clone(),proxy:event_loop.create_proxy(),worker:None};
    let mut wrapper=WinitAppWrapper::new(app,true);
    event_loop.run_app_on_demand(&mut wrapper).unwrap();
    wrapper.winit_app.worker.take().unwrap().join().unwrap();
    let paints:[u64;2]=std::array::from_fn(|index|shared.paints[index].load(Ordering::Acquire));
    let moves=shared.moves.load(Ordering::Acquire);
    println!("owned input events={moves}, paints during input={paints:?}; no GPU, desktop capture or global input");
    assert!(moves>50,"fixture did not create sustained input pressure");
    assert!(paints.iter().all(|count|*count>=10),"a due owned window stopped repainting");
}
