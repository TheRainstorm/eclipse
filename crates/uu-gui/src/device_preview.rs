//! A bounded per-window worker writes the final remote image without video-path I/O.
use eframe::egui;
use std::{sync::{mpsc,Arc,Mutex},thread::JoinHandle};
use uu_core::device_book::Store;
struct Job {store:Store,device:String,at:u64,image:Option<egui::ColorImage>}
pub(super) struct Writer {
    tx:Option<mpsc::SyncSender<Job>>,
    worker:Option<JoinHandle<()>>,
    pub error:Arc<Mutex<Option<String>>>,
}
impl Default for Writer {
    fn default()->Self {
        let (tx,rx)=mpsc::sync_channel::<Job>(2);let error=Arc::new(Mutex::new(None));let state=error.clone();
        let worker=std::thread::Builder::new().name("eclipse-device-preview".into()).spawn(move|| {
            while let Ok(job)=rx.recv() {
                let result=(||->anyhow::Result<()>{
                    if let Some(image)=job.image {
                        anyhow::ensure!(image.size[0]<=480&&image.size[1]<=270,"preview dimensions exceed limit");
                        let rgb:Vec<_>=image.pixels.iter().flat_map(|p|[p.r(),p.g(),p.b()]).collect();
                        let mut jpeg=Vec::new();
                        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg,82).encode(&rgb,image.size[0] as u32,image.size[1] as u32,image::ExtendedColorType::Rgb8)?;
                        job.store.record_preview(&job.device,job.at,&jpeg)
                    }else{job.store.record_connected(&job.device,job.at)}
                })();
                if let Err(e)=result{*state.lock().unwrap()=Some(uu_i18n::format!("设备记录保存失败：{e:#}"));}
            }
        }).ok();
        Self{tx:Some(tx),worker,error}
    }
}
impl Writer {
    pub fn submit(&self,store:Store,device:String,image:Option<egui::ColorImage>) {
        let at=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(0);
        let result=self.tx.as_ref().is_some_and(|tx|tx.try_send(Job{store,device,at,image}).is_ok());
        if !result{*self.error.lock().unwrap()=Some(uu_i18n::text("设备记录保存队列繁忙，可在下一次连接更新预览").into());}
    }
}
impl Drop for Writer {
    fn drop(&mut self){self.tx.take();if let Some(worker)=self.worker.take(){let _=worker.join();}}
}
