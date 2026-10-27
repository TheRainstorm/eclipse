//! Protocol-independent access to the existing input ownership actor.
use super::*;

pub struct InputSession(Arc<InputWorker>);
impl InputSession {
    pub async fn for_capture(capture:&mut crate::native::EncodedCapture,
        authority:Arc<crate::input_ownership::Authority>,keyboard:bool,mouse:bool)->Result<Option<Self>> {
        if !keyboard && !mouse {return Ok(None);}
        let (rect,injector)=capture.take_input();
        let Some(rect)=rect else{return Ok(None);};
        let rect=DesktopRect::new(rect.x,rect.y,rect.width,rect.height)?;
        let worker=if let Some(injector)=injector {
            InputWorker::with_timing_factory(rect,move||Ok(injector),Arc::default(),authority).await?
        }else{InputWorker::start(rect,Arc::default(),authority).await?};
        worker.state.allowed.store(u8::from(keyboard)|(u8::from(mouse)<<1),Ordering::Release);
        Ok(Some(Self(worker)))
    }
    pub fn request(&self,enabled:bool) {self.0.request(enabled);}
    pub fn touch(&self) {self.0.touch();}
    pub fn available(&self)->bool {self.0.available()}
    pub fn granted(&self)->bool {self.0.state.enabled.load(Ordering::Acquire)&&self.0.available()}
    pub fn submit(&self,command:crate::native_input::InputCommand)->bool {self.0.push(command)}
    pub fn supports_text(&self)->bool {self.0.state.text_supported.load(Ordering::Acquire)}
    pub fn text(&self,value:&str)->bool {self.0.push_text(value)}
    pub fn counters(&self)->(u64,u64) {(self.0.state.applied.load(Ordering::Relaxed),self.0.state.errors.load(Ordering::Relaxed))}
    pub async fn stop(&self) {self.0.stop().await;}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn protocol_text_obeys_keyboard_permission_and_actor_lifecycle() {
        struct Injector(Arc<Mutex<Vec<String>>>);
        impl uu_input::InputInjector for Injector {
            fn inject(&mut self,_:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{Ok(())}
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{Ok(())}
            fn supports_text(&self)->bool {true}
            fn text(&mut self,value:&str)->std::result::Result<(),uu_input::InputError>{self.0.lock().unwrap().push(value.into());Ok(())}
        }
        let texts=Arc::new(Mutex::new(Vec::new()));let recorded=texts.clone();
        let worker=InputWorker::with_timing_factory(DesktopRect::new(0,0,32,32).unwrap(),move||Ok(Box::new(Injector(recorded))),Arc::default(),Arc::default()).await.unwrap();
        let session=InputSession(worker);
        assert!(!session.text("not authorized"));session.request(true);
        session.0.state.allowed.store(2,Ordering::Release);assert!(!session.text("mouse only"));
        session.0.state.allowed.store(3,Ordering::Release);
        assert!(session.text("生成🙂"));
        for _ in 0..100 {if !texts.lock().unwrap().is_empty(){break;}tokio::time::sleep(Duration::from_millis(2)).await;}
        assert_eq!(*texts.lock().unwrap(),vec!["生成🙂"]);
        session.request(false);assert!(!session.text("after release"));
        session.stop().await;assert!(!session.text("after stop"));
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn protocol_and_native_sessions_share_one_owner_and_release_on_stop() {
        let authority=Arc::new(crate::input_ownership::Authority::default());
        let rect=DesktopRect::new(0,0,320,180).unwrap();
        let events=Arc::new(Mutex::new(Vec::new()));
        struct Injector(Arc<Mutex<Vec<uu_input::InputEvent>>>);
        impl uu_input::InputInjector for Injector {
            fn inject(&mut self,event:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{self.0.lock().unwrap().push(event);Ok(())}
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{Ok(())}
        }
        let recorded=events.clone();
        let a=InputSession(InputWorker::with_timing_factory(rect,move||Ok(Box::new(Injector(recorded))),Arc::default(),authority.clone()).await.unwrap());
        let b=InputSession(InputWorker::with_timing_factory(rect,||Ok(Box::new(Injector(Arc::default()))),Arc::default(),authority).await.unwrap());
        a.request(true);b.request(true);assert!(a.granted());assert!(!b.granted());
        assert!(a.submit(InputCommand::Key {usage:4,pressed:true}));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(events.lock().unwrap().len(),1);
        a.stop().await;b.request(true);assert!(b.granted());b.stop().await;
    }
}
