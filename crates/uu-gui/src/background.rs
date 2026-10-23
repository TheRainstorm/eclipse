//! Optional background. Cached artwork and refresh failures never gate devices.
use super::{settings::Settings,wallpapers};
use eframe::egui;
use std::{path::PathBuf,sync::mpsc,time::{Duration,Instant}};
#[derive(Default)]
pub(super) struct Background {
    pub texture:Option<egui::TextureHandle>,
    pub notice:String,
    worker:Option<wallpapers::Worker>,
    cache_rx:Option<mpsc::Receiver<Option<wallpapers::Thumbnail>>>,
    source:String,
    texture_source:String,
    received:bool,
    cache:Option<PathBuf>,
    initialized:bool,
    enabled:bool,
    retry_at:Option<Instant>,
}
impl Background {
    pub fn new(cache:Option<PathBuf>)->Self {Self{cache,..Default::default()}}
    pub fn sync(&mut self,s:&Settings,ctx:&egui::Context) {
        self.enabled=s.wallpaper_enabled;
        if !self.enabled {self.worker=None;self.cache_rx=None;self.texture=None;self.initialized=false;return;}
        if !self.initialized || self.source!=s.wallpaper_api {
            self.initialized=true;self.source=s.wallpaper_api.clone();self.notice.clear();
            let mut fresh=false;
            if let Some(path)=self.cache.clone() {
                fresh=std::fs::metadata(&path).and_then(|m|m.modified()).ok().and_then(|m|m.elapsed().ok()).is_some_and(|age|age<Duration::from_secs(86400));
                let source=self.source.clone();let (tx,rx)=mpsc::channel();self.cache_rx=Some(rx);
                std::thread::spawn(move|| {
                    let value=(||->anyhow::Result<_>{
                        let saved=super::settings::read_bounded(&path.with_extension("source"),4096)?;
                        anyhow::ensure!(saved==source.as_bytes(),"wallpaper source changed");
                        let bytes=super::settings::read_bounded(&path,4*1024*1024)?;
                        wallpapers::decode(wallpapers::Request{device_id:"background".into(),url:source},bytes)
                    })().ok();let _=tx.send(value);
                });
            }
            if !fresh {self.refresh(ctx);}
        }
        if let Some(value)=self.cache_rx.as_ref().and_then(|rx|rx.try_recv().ok()) {
            self.cache_rx=None;
            if let Some(image)=value {
                if self.texture.is_none()||self.texture_source!=self.source {self.set(image,ctx,false);}
            }else if self.worker.is_none(){self.refresh(ctx);}
        }
        while let Some(image)=self.worker.as_mut().and_then(|w|w.try_recv()) {self.received=true;self.set(image,ctx,true);}
        if self.worker.as_ref().is_some_and(|w|w.finished()) {
            self.worker=None;
            if !self.received {self.notice=if self.texture.is_some(){"壁纸更新失败，继续使用已有图片"}else{"图片暂时无法获取，正在使用内置背景；可在应用设置中刷新"}.into();}
        }
        if self.worker.is_some()||self.cache_rx.is_some(){ctx.request_repaint_after(Duration::from_millis(100));}
    }
    fn set(&mut self,image:wallpapers::Thumbnail,ctx:&egui::Context,cache:bool) {
        if image.url!=self.source{return;}
        self.texture_source=image.url.clone();
        self.texture=Some(ctx.load_texture("workspace-background",egui::ColorImage::from_rgba_unmultiplied([image.width as usize,image.height as usize],&image.rgba),egui::TextureOptions::LINEAR));
        self.notice.clear();
        if cache {if let Some(path)=self.cache.clone() {
            std::thread::spawn(move|| {
                let result=(||->anyhow::Result<()>{
                    let rgba=image::RgbaImage::from_raw(image.width,image.height,image.rgba).ok_or_else(||anyhow::anyhow!("invalid background pixels"))?;
                    let rgb=image::DynamicImage::ImageRgba8(rgba).to_rgb8();let mut bytes=Vec::new();
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes,85).encode_image(&rgb)?;
                    super::settings::save_bytes(&path,&bytes)?;
                    super::settings::save_bytes(&path.with_extension("source"),image.url.as_bytes())?;Ok(())
                })();if let Err(e)=result{eprintln!("wallpaper cache: {e}");}
            });
        }}
    }
    pub fn refresh(&mut self,ctx:&egui::Context) {
        if self.retry_at.is_some_and(|t|t.elapsed()<Duration::from_secs(1)){return;}
        self.retry_at=Some(Instant::now());
        self.worker=None;self.received=false;
        match wallpapers::Worker::start(vec![wallpapers::Request{device_id:"background".into(),url:self.source.clone()}],ctx.clone()) {
            Ok(worker)=>{self.worker=Some(worker);self.notice="正在更新壁纸…".into();},Err(e)=>self.notice=format!("壁纸加载失败：{e}")
        }
    }
    pub fn paint(&self,ui:&egui::Ui,rect:egui::Rect) {
        if !self.enabled { return; }
        let Some(texture)=&self.texture else {
            // Always provide a visible background while the optional image
            // service is loading or unreachable, without blocking the page.
            let mut mesh=egui::Mesh::default();
            for (position,color) in [
                (rect.left_top(),egui::Color32::from_rgb(215,234,255)),
                (rect.right_top(),egui::Color32::from_rgb(237,245,255)),
                (rect.right_bottom(),egui::Color32::from_rgb(194,220,250)),
                (rect.left_bottom(),egui::Color32::from_rgb(235,242,255)),
            ] { mesh.colored_vertex(position,color); }
            mesh.add_triangle(0,1,2);mesh.add_triangle(0,2,3);
            ui.painter().add(egui::Shape::mesh(mesh));
            return;
        };
        let source=texture.size_vec2();let ratio=(rect.width()/source.x).max(rect.height()/source.y);
        let fraction=rect.size()/(source*ratio);let uv=egui::Rect::from_center_size(egui::pos2(0.5,0.5),fraction);
        ui.painter().image(texture.id(),rect,uv,egui::Color32::WHITE);
        let canvas=super::theme::CANVAS;
        ui.painter().rect_filled(rect,0.,egui::Color32::from_rgba_unmultiplied(canvas.r(),canvas.g(),canvas.b(),135));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_cached_background_is_loaded_without_requesting_the_image_service() {
        let root=std::env::temp_dir().join(format!("eclipse-background-{}",uuid::Uuid::new_v4()));
        let path=root.join("wallpaper.jpg");
        let pixels=image::RgbImage::from_pixel(64,36,image::Rgb([40,100,180]));
        let mut bytes=Vec::new();image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&pixels).unwrap();
        let settings=Settings{wallpaper_api:"https://wallpaper.example.invalid/image".into(),..Default::default()};
        super::super::settings::save_bytes(&path,&bytes).unwrap();super::super::settings::save_bytes(&path.with_extension("source"),settings.wallpaper_api.as_bytes()).unwrap();
        let ctx=egui::Context::default();let mut background=Background::new(Some(path));
        let deadline=Instant::now()+Duration::from_secs(3);
        while background.texture.is_none(){background.sync(&settings,&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert!(background.worker.is_none());assert_eq!(background.texture.as_ref().unwrap().size(),[64,36]);
        let settings=Settings{wallpaper_api:"https://second.example.invalid/image".into(),..settings};
        super::super::settings::save_bytes(&background.cache.as_ref().unwrap().with_extension("source"),settings.wallpaper_api.as_bytes()).unwrap();
        let deadline=Instant::now()+Duration::from_secs(3);
        while background.texture_source!=settings.wallpaper_api {background.sync(&settings,&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert!(background.worker.is_none());
        let settings=Settings{wallpaper_enabled:false,..settings};background.sync(&settings,&ctx);assert!(background.texture.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
