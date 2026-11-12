//! Optional background. Cached artwork and refresh failures never gate devices.
use super::{settings::{Settings,WallpaperSource},wallpapers,wallpaper_favorites};
use eframe::egui;
use super::ui_feedback::{Feedback,Scope,Kind};
use std::{path::PathBuf,sync::{mpsc,Arc},time::{Duration,Instant}};
mod refresh;

const CACHE_MAGIC:&[u8]=b"Eclipse-wallpaper-original-1\n";
fn write_cache(path:&std::path::Path,source:&str,bytes:&[u8])->anyhow::Result<()> {
    anyhow::ensure!(source.len()<=4096&&bytes.len()<=wallpapers::MAX_WALLPAPER_BYTES,"wallpaper cache too large");
    let mut saved=Vec::with_capacity(CACHE_MAGIC.len()+4+source.len()+bytes.len());
    saved.extend_from_slice(CACHE_MAGIC);saved.extend_from_slice(&(source.len() as u32).to_le_bytes());saved.extend_from_slice(source.as_bytes());saved.extend_from_slice(bytes);
    super::settings::save_bytes(path,&saved)
}
fn read_cache(path:&std::path::Path,source:&str,side:usize)->anyhow::Result<(wallpapers::Thumbnail,Duration)> {
    let saved=super::settings::read_bounded(path,wallpapers::MAX_WALLPAPER_BYTES+8192)?;
    let original=saved.starts_with(CACHE_MAGIC);
    let bytes=if original {
        let offset=CACHE_MAGIC.len();
        let length=u32::from_le_bytes(saved.get(offset..offset+4).ok_or_else(||anyhow::anyhow!("truncated wallpaper cache"))?.try_into()?) as usize;
        anyhow::ensure!(length<=4096&&saved.get(offset+4..offset+4+length)==Some(source.as_bytes()),"wallpaper source changed");
        saved[offset+4+length..].to_vec()
    } else {
        anyhow::ensure!(super::settings::read_bounded(&path.with_extension("source"),4096)?==source.as_bytes(),"wallpaper source changed");
        saved
    };
    let mut image=wallpapers::decode_wallpaper_with_limit(wallpapers::Request{device_id:"background".into(),url:source.into()},bytes,side)?;
    if !original {image.encoded.clear();}
    let age=if original {std::fs::metadata(path).and_then(|m|m.modified()).ok().and_then(|m|m.elapsed().ok()).unwrap_or(Duration::MAX)}else{Duration::MAX};
    Ok((image,age))
}

#[derive(Default)]
pub(super) struct Background {
    pub texture:Option<egui::TextureHandle>,
    pub notice:Feedback,
    worker:Option<wallpapers::Worker>,
    cache_rx:Option<mpsc::Receiver<Option<(wallpapers::Thumbnail,Duration)>>>,
    source:String,
    texture_source:String,
    received:bool,
    failure:Option<String>,
    cache:Option<PathBuf>,
    initialized:bool,
    enabled:bool,
    retry_at:Option<Instant>,
    favorites:wallpaper_favorites::Favorites,
    favorites_mode:bool,
    favorite_preference:Option<String>,
    favorite_current:Option<String>,
    favorite_open_pending:bool,
    schedule:refresh::Schedule,
    favorite_notice:Feedback,
    current_encoded:Option<Arc<Vec<u8>>>,
    original:Option<wallpapers::Original>,
    painted_original:std::cell::RefCell<Option<wallpapers::Original>>,
    download:super::wallpaper_download::Download,
    painted_encoded:std::cell::RefCell<Option<Arc<Vec<u8>>>>,
}
impl Background {
    pub fn new(cache:Option<PathBuf>)->Self {Self{cache,..Default::default()}}
    pub fn sync(&mut self,s:&Settings,ctx:&egui::Context) {
        let mode_changed=self.favorites_mode!=(s.wallpaper_source==WallpaperSource::Favorites);
        self.enabled=s.wallpaper_enabled;self.favorites_mode=s.wallpaper_source==WallpaperSource::Favorites;
        self.schedule.configure(s.wallpaper_refresh,s.wallpaper_refresh_hours,Instant::now());
        if !self.enabled {self.worker=None;self.cache_rx=None;self.texture=None;self.current_encoded=None;self.painted_encoded.get_mut().take();self.painted_original.get_mut().take();self.original=None;self.initialized=false;return;}
        if mode_changed {self.initialized=false;self.cache_rx=None;self.schedule.reset();self.failure=None;}
        if self.favorites_mode {
            self.worker=None;self.retry_at=None;
            if !self.initialized {
                self.favorite_open_pending=true;self.favorite_current=s.wallpaper_favorite.clone();self.favorite_preference=s.wallpaper_favorite.clone();
            }else if self.favorite_preference!=s.wallpaper_favorite {
                self.favorite_current=s.wallpaper_favorite.clone();self.favorite_preference=s.wallpaper_favorite.clone();
            }
            self.favorites.ensure_loaded(ctx);self.favorites.poll(ctx);
            self.initialized=true;
            if self.favorites.loaded && self.favorite_open_pending {
                self.favorite_open_pending=false;
                if self.schedule.opened(Some(Duration::ZERO),Instant::now()) {self.favorite_current=self.favorites.next(self.favorite_current.as_deref());}
            }
            if let Some(item)=self.favorites.selected(self.favorite_current.as_deref()) {
                self.favorite_current=Some(item.id.clone());
                let source=format!("favorite:{}",item.id);
                if self.source!=source || (self.texture.is_none()&&self.cache_rx.is_none()&&self.failure.is_none()) {
                    self.source=source.clone();
                    self.failure=None;
                    let side=ctx.input(|i|i.max_texture_side);let (tx,rx)=mpsc::channel();self.cache_rx=Some(rx);
                    let context=ctx.clone();
                    std::thread::spawn(move|| {
                        let image=item.load(side).ok();
                        let _=tx.send(image.map(|image|(image,Duration::ZERO)));
                        context.request_repaint();
                    });
                }
            }else if self.favorites.loaded {
                self.texture=None;self.current_encoded=None;self.original=None;self.cache_rx=None;
            }
            self.poll(ctx);return;
        }
        if !self.initialized || self.source!=s.wallpaper_url() {
            self.initialized=true;self.source=s.wallpaper_url().into();self.notice.clear();
            self.schedule.reset();
            // A new source owns a new request; neither throttling nor a pending
            // result from the previous service can suppress its first refresh.
            self.worker=None;self.cache_rx=None;self.retry_at=None;self.received=false;self.failure=None;
            if let Some(path)=self.cache.clone() {
                let source=self.source.clone();let side=ctx.input(|i|i.max_texture_side);let (tx,rx)=mpsc::channel();self.cache_rx=Some(rx);
                std::thread::spawn(move|| {let _=tx.send(read_cache(&path,&source,side).ok());});
            }else {self.refresh(ctx);}
        }
        self.poll(ctx);
    }
    pub fn poll(&mut self,ctx:&egui::Context) {
        self.favorites.poll(ctx);
        if self.favorite_notice!=self.favorites.notice {
            self.favorite_notice=self.favorites.notice.clone();
            if !self.favorite_notice.is_empty(){self.notice=self.favorite_notice.clone();}
        }
        if let Some(value)=self.cache_rx.as_ref().and_then(|rx|rx.try_recv().ok()) {
            self.cache_rx=None;
            if let Some((image,age))=value {
                if self.texture.is_none()||self.texture_source!=self.source {self.set(image,ctx,false);}
                if self.favorites_mode {self.schedule.finished(true,Instant::now());}
                else if self.schedule.opened(Some(age),Instant::now()) && self.worker.is_none() {self.refresh(ctx);}
            }else if self.favorites_mode {
                self.failure=Some("favorite image could not be loaded".into());
                self.notice.error(Scope::Wallpaper,uu_i18n::text("收藏图片无法加载，请扫描目录或选择其它图片。"));
            }else if self.worker.is_none(){self.refresh(ctx);}
        }
        while let Some(outcome)=self.worker.as_mut().and_then(|w|w.try_recv()) {
            match outcome {
                Ok(image)=>{self.received=true;self.failure=None;self.set(image,ctx,true);},
                Err(failure) if failure.device_id=="background" && failure.url==self.source => {
                    self.failure=Some(failure.error.clone());
                    self.notice.error(Scope::Wallpaper,uu_i18n::format!("壁纸加载失败：{e}",e=failure.error));
                },
                Err(_)=>{},
            }
        }
        if self.worker.as_ref().is_some_and(|w|w.finished()) {
            self.worker=None;
            self.schedule.finished(self.received,Instant::now());
            if !self.received && self.failure.is_none() {self.notice.error(Scope::Wallpaper,if self.texture.is_some(){uu_i18n::text("壁纸更新失败，继续使用已有图片")}else{uu_i18n::text("图片暂时无法获取，正在使用内置背景；可在应用设置中刷新")});}
        }
        let previous=self.download.notice.clone();
        if self.download.poll(ctx) && (!self.download.notice.is_empty() || self.notice==previous) {self.notice=self.download.notice.clone();}
        self.notice.expire(Instant::now());self.notice.tick(ctx);
        if self.enabled && self.initialized && self.worker.is_none() && self.cache_rx.is_none() && self.schedule.take_due(Instant::now()){self.refresh(ctx);}
        if self.enabled {self.schedule.wake(ctx);}
        if self.worker.is_some()||self.cache_rx.is_some(){ctx.request_repaint_after(Duration::from_millis(100));}
    }
    fn set(&mut self,mut image:wallpapers::Thumbnail,ctx:&egui::Context,cache:bool) {
        if image.url!=self.source{return;}
        self.original=Some(image.original);
        self.current_encoded=if image.encoded.is_empty(){None}else{Some(Arc::new(std::mem::take(&mut image.encoded)))};
        self.texture_source=image.url.clone();
        self.texture=Some(ctx.load_texture("workspace-background",egui::ColorImage::from_rgba_unmultiplied([image.width as usize,image.height as usize],&image.rgba),egui::TextureOptions::LINEAR));
        // Loading a fallback after a network failure must not erase its reason.
        if cache || self.failure.is_none() {self.notice.clear();}
        if cache {if let (Some(path),Some(bytes))=(self.cache.clone(),self.current_encoded.clone()) {
            std::thread::spawn(move|| {if let Err(error)=write_cache(&path,&image.url,&bytes) {eprintln!("wallpaper cache: {error}");}});
        }}
    }
    fn displayed(&self)->Option<(Arc<Vec<u8>>,wallpapers::Original)> {
        if let (Some(bytes),Some(original))=(self.painted_encoded.borrow().clone(),*self.painted_original.borrow()) {return Some((bytes,original));}
        Some((self.current_encoded.clone()?,self.original?))
    }
    pub fn original_size(&self)->Option<(u32,u32)> {if !self.has_wallpaper(){return None;}self.displayed().map(|(_,image)|(image.width,image.height))}
    pub fn can_download(&self)->bool {self.has_wallpaper()&&!self.download.busy()}
    pub fn download(&mut self,ctx:&egui::Context) {
        if let Some((bytes,original))=self.displayed() {self.download.start(bytes,original,ctx);self.notice=self.download.notice.clone();}
    }
    pub fn has_wallpaper(&self)->bool {self.texture.is_some()&&self.current_encoded.is_some()}
    pub fn favorite(&mut self,ctx:&egui::Context) {
        let displayed=self.painted_encoded.borrow().clone().or_else(||self.current_encoded.clone());
        if let Some(bytes)=displayed {self.favorites.save(bytes,ctx);self.notice=self.favorites.notice.clone();}
        else {self.notice.report(Scope::Wallpaper,Kind::Info,uu_i18n::text("当前没有可收藏的壁纸"));}
    }
    pub fn favorites_editor(&mut self,ui:&mut egui::Ui,s:&mut Settings)->bool {
        if s.wallpaper_source!=WallpaperSource::Favorites {return false;}
        ui.add_space(10.0);
        let changed=self.favorites.editor(ui,&mut s.wallpaper_favorite);
        if changed {self.sync(s,ui.ctx());}
        changed
    }
    pub fn refresh(&mut self,ctx:&egui::Context) {
        self.schedule.started();
        if self.favorites_mode {
            self.failure=None;self.favorite_current=self.favorites.next(self.favorite_current.as_deref());self.favorites.reload(ctx);
            self.schedule.finished(true,Instant::now());ctx.request_repaint();return;
        }
        if self.retry_at.is_some_and(|t|t.elapsed()<Duration::from_secs(1)){return;}
        self.retry_at=Some(Instant::now());
        // A stale cache is still useful when the new network request fails.
        // Source changes retire cache receivers in sync; set() prevents a late
        // cached image from replacing a successfully downloaded texture.
        self.worker=None;self.received=false;self.failure=None;
        match wallpapers::Worker::start_wallpaper(wallpapers::Request{device_id:"background".into(),url:self.source.clone()},ctx.clone()) {
            Ok(worker)=>{self.worker=Some(worker);self.notice.report(Scope::Wallpaper,Kind::Progress,uu_i18n::text("正在更新壁纸…"));},Err(e)=>self.notice.error(Scope::Wallpaper,uu_i18n::format!("壁纸加载失败：{e}"))
        }
    }
    pub fn paint_background(&self, ctx: &egui::Context, rect: egui::Rect) {
        self.paint_with(
            &ctx.layer_painter(egui::LayerId::background()),
            rect,
        );
    }

    fn paint_with(&self, painter: &egui::Painter, rect: egui::Rect) {
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
            painter.add(egui::Shape::mesh(mesh));
            return;
        };
        // A network completion can arrive between painting and the title-bar
        // click. Save the bytes of the image used by this paint pass.
        *self.painted_encoded.borrow_mut()=self.current_encoded.clone();
        *self.painted_original.borrow_mut()=self.original;
        let source=texture.size_vec2();let ratio=(rect.width()/source.x).max(rect.height()/source.y);
        let fraction=rect.size()/(source*ratio);let uv=egui::Rect::from_center_size(egui::pos2(0.5,0.5),fraction);
        painter.image(texture.id(),rect,uv,egui::Color32::WHITE);
        painter.rect_filled(rect,0.,egui::Color32::from_black_alpha(70));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_keeps_an_old_cache_and_every_open_fetches_once_from_a_fresh_cache() {
        use std::io::{Read,Write};
        let root=std::env::temp_dir().join(format!("eclipse-refresh-policy-{}",uuid::Uuid::new_v4()));
        let path=root.join("wallpaper.cache");
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source=format!("http://{}/image",listener.local_addr().unwrap());
        let mut bytes=std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(32,18,image::Rgb([20,100,180])).write_to(&mut bytes,image::ImageFormat::Png).unwrap();
        let bytes=bytes.into_inner();write_cache(&path,&source,&bytes).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()-Duration::from_secs(90000))).unwrap();
        let ctx=egui::Context::default();let settings=Settings{wallpaper_source:WallpaperSource::CustomApi,wallpaper_api:source.clone(),wallpaper_refresh:crate::settings::WallpaperRefresh::Never,..Default::default()};
        let mut background=Background::new(Some(path.clone()));background.sync(&settings,&ctx);
        let deadline=Instant::now()+Duration::from_secs(5);
        while background.cache_rx.is_some(){background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(2));}
        assert!(background.texture.is_some());assert!(background.worker.is_none());
        drop(background);write_cache(&path,&source,&bytes).unwrap();
        let server=std::thread::spawn(move|| {
            listener.set_nonblocking(true).unwrap();let deadline=Instant::now()+Duration::from_secs(5);
            let mut stream=loop {match listener.accept(){Ok((stream,_))=>break stream,Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(2));},Err(e)=>panic!("{e}")}};
            stream.set_nonblocking(false).unwrap();stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();let mut request=[0;4096];stream.read(&mut request).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();stream.write_all(&bytes).unwrap();
        });
        let settings=Settings{wallpaper_refresh:crate::settings::WallpaperRefresh::OnOpen,..settings};
        let mut background=Background::new(Some(path));background.sync(&settings,&ctx);
        // The initial reader already owns the cache path. This scheduling
        // fixture does not need a detached writer racing its directory cleanup.
        background.cache=None;
        while !background.received || background.worker.is_some() || background.cache_rx.is_some(){background.sync(&settings,&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(2));}
        for _ in 0..4 {background.sync(&settings,&ctx);assert!(background.worker.is_none());}
        server.join().unwrap();std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn completed_feedback_keeps_its_deadline_when_mirrored_and_does_not_reappear() {
        let ctx=egui::Context::default();let mut background=Background::new(None);
        background.favorites.notice.success(Scope::Wallpaper,"generated saved image");
        background.poll(&ctx);
        assert_eq!(background.notice.text(),"generated saved image");
        assert!(background.notice.expire(Instant::now()+Duration::from_secs(3)));
        background.poll(&ctx);assert!(background.notice.is_empty());
        // The same text from a later operation owns a fresh deadline.
        background.favorites.notice.success(Scope::Wallpaper,"generated saved image");
        background.poll(&ctx);assert_eq!(background.notice.text(),"generated saved image");
        background.favorites.notice.error(Scope::Wallpaper,"generated save failure");
        background.poll(&ctx);
        assert!(!background.notice.expire(Instant::now()+Duration::from_secs(30)));
        let (tx,rx)=mpsc::channel();background.download.test_worker(rx);
        tx.send(Ok(Some(PathBuf::from("generated-original.webp")))).unwrap();
        background.poll(&ctx);assert!(background.notice.text().contains("generated-original.webp"));
        assert!(background.notice.expire(Instant::now()+Duration::from_secs(3)));
        background.poll(&ctx);assert!(background.notice.is_empty());
    }
    #[test]
    fn original_cache_is_atomic_source_bound_and_legacy_preview_is_not_exported() {
        let root=std::env::temp_dir().join(format!("eclipse-original-wallpaper-{}",uuid::Uuid::new_v4()));
        let path=root.join("wallpaper.jpg");let source="https://example.invalid/current";
        let mut encoded=std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(1280,720,image::Rgb([20,40,80])).write_to(&mut encoded,image::ImageFormat::Png).unwrap();
        let encoded=encoded.into_inner();
        write_cache(&path,source,&encoded).unwrap();
        let (image,fresh)=read_cache(&path,source,4096).unwrap();
        assert!(fresh<Duration::from_secs(5));assert_eq!(image.encoded,encoded);assert_eq!(image.original.width,1280);
        assert!(read_cache(&path,"https://example.invalid/other",4096).is_err());
        super::super::settings::save_bytes(&path,&encoded).unwrap();
        super::super::settings::save_bytes(&path.with_extension("source"),source.as_bytes()).unwrap();
        let (legacy,fresh)=read_cache(&path,source,4096).unwrap();
        assert_eq!(fresh,Duration::MAX);assert!(legacy.encoded.is_empty());assert_eq!(legacy.width,1280);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn favorite_wallpaper_saves_the_painted_bytes_and_local_mode_never_starts_http() {
        let root=std::env::temp_dir().join(format!("eclipse-displayed-favorite-{}",uuid::Uuid::new_v4()));
        let pixels=image::RgbImage::from_pixel(64,36,image::Rgb([40,100,180]));
        let mut output=std::io::Cursor::new(Vec::new());pixels.write_to(&mut output,image::ImageFormat::Png).unwrap();let bytes=output.into_inner();
        let source="https://no-request.example.invalid/image".to_string();
        let ctx=egui::Context::default();let mut background=Background::new(None);
        background.favorites=wallpaper_favorites::Favorites::at(root.clone());background.source=source.clone();background.enabled=true;background.initialized=true;
        background.set(wallpapers::decode(wallpapers::Request{device_id:"background".into(),url:source},bytes.clone()).unwrap(),&ctx,false);
        assert!(background.has_wallpaper());
        let _=ctx.run(Default::default(),|ctx|background.paint_background(ctx,egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,360.0))));
        // Simulate a refresh completing after the current image was painted.
        let mut second=std::io::Cursor::new(Vec::new());image::RgbImage::from_pixel(64,36,image::Rgb([180,30,20])).write_to(&mut second,image::ImageFormat::Png).unwrap();
        background.set(wallpapers::decode(wallpapers::Request{device_id:"background".into(),url:background.source.clone()},second.into_inner()).unwrap(),&ctx,false);
        background.favorite(&ctx);
        let deadline=Instant::now()+Duration::from_secs(5);
        while background.favorites.busy(){background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert_eq!(background.favorites.items.len(),1);
        let id=background.favorites.items[0].id.clone();
        assert_eq!(std::fs::read(root.join(format!("{id}.png"))).unwrap(),bytes);
        let settings=Settings{wallpaper_source:WallpaperSource::Favorites,wallpaper_favorite:Some(id.clone()),wallpaper_api:"http://127.0.0.1:9/must-not-request".into(),..Default::default()};
        background.sync(&settings,&ctx);
        while background.cache_rx.is_some() {background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert!(background.worker.is_none());assert!(background.cache_rx.is_none());
        assert_eq!(background.texture_source,format!("favorite:{id}"));assert!(background.has_wallpaper());
        background.favorite(&ctx);
        while background.favorites.busy(){background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert_eq!(background.favorites.items.len(),1);
        drop(background);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_reports_actual_failure_and_retains_the_existing_wallpaper() {
        use std::io::{Read,Write};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source=format!("http://{}/image",listener.local_addr().unwrap());
        let server=std::thread::spawn(move|| {
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request=[0;4096];let _=stream.read(&mut request).unwrap();
            write!(stream,"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let ctx=egui::Context::default();
        let mut background=Background::new(None);background.source=source.clone();background.enabled=true;background.initialized=true;
        background.set(wallpapers::Thumbnail{device_id:"background".into(),url:source,width:1,height:1,rgba:vec![10,20,30,255],encoded:vec![],original:wallpapers::Original{width:1,height:1,format:image::ImageFormat::Png}},&ctx,false);
        let texture=background.texture.as_ref().unwrap().id();
        background.refresh(&ctx);
        let deadline=Instant::now()+Duration::from_secs(5);
        while background.worker.is_some(){background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert_eq!(background.texture.as_ref().unwrap().id(),texture);
        assert!(background.notice.text().contains("403"));assert!(background.failure.is_some());
        server.join().unwrap();
    }

    #[test]
    fn stale_cache_survives_refresh_failure_and_keeps_the_actual_reason() {
        use std::io::{Read,Write};
        use std::time::SystemTime;
        let root=std::env::temp_dir().join(format!("eclipse-stale-background-{}",uuid::Uuid::new_v4()));
        let path=root.join("wallpaper.jpg");
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source=format!("http://{}/image",listener.local_addr().unwrap());
        let pixels=image::RgbImage::from_pixel(64,36,image::Rgb([40,100,180]));
        let mut bytes=Vec::new();image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&pixels).unwrap();
        super::super::settings::save_bytes(&path,&bytes).unwrap();
        super::super::settings::save_bytes(&path.with_extension("source"),source.as_bytes()).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now()-Duration::from_secs(90000))
        ).unwrap();
        let server=std::thread::spawn(move|| {
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request=[0;4096];let _=stream.read(&mut request).unwrap();
            write!(stream,"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let settings=Settings{wallpaper_source:super::super::settings::WallpaperSource::CustomApi,
            wallpaper_api:source.clone(),..Default::default()};
        let ctx=egui::Context::default();let mut background=Background::new(Some(path));
        background.sync(&settings,&ctx);
        let deadline=Instant::now()+Duration::from_secs(5);
        while background.texture.is_none()||background.worker.is_some()||background.cache_rx.is_some(){
            background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(background.texture.as_ref().unwrap().size(),[64,36]);
        assert_eq!(background.texture_source,source);
        assert!(background.notice.text().contains("403"));
        // Explicitly cover the opposite completion order as well: a late cache
        // publish cannot clear a network error already shown to the user.
        background.set(wallpapers::decode(wallpapers::Request{device_id:"background".into(),url:source},bytes).unwrap(),&ctx,false);
        assert!(background.notice.text().contains("403"));
        server.join().unwrap();std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changing_source_bypasses_its_previous_refresh_throttle() {
        use std::io::{Read, Write};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let settings=Settings{wallpaper_source:super::super::settings::WallpaperSource::CustomApi,
            wallpaper_api:format!("http://{}/image",listener.local_addr().unwrap()),..Default::default()};
        let server=std::thread::spawn(move|| {
            let deadline=Instant::now()+Duration::from_secs(5);
            let mut stream=loop {match listener.accept(){
                Ok((stream,_))=>break stream,
                Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));},
                Err(e)=>panic!("{e}"),
            }};
            // Winsock accepts inherit the listener's nonblocking mode. The
            // request reader needs blocking I/O bounded by SO_RCVTIMEO.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request=[0;4096];stream.read(&mut request).unwrap();
            let pixels=image::RgbImage::from_pixel(64,36,image::Rgb([40,100,180]));
            let mut bytes=Vec::new();image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&pixels).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();stream.write_all(&bytes).unwrap();
        });
        let ctx=egui::Context::default();
        let mut background=Background::new(None);
        background.initialized=true;background.enabled=true;
        background.source="https://old.example.invalid/image".into();
        background.retry_at=Some(Instant::now());
        // A source change must start the new request even inside the old
        // one-second throttle. Poll alone also works while settings is open.
        background.sync(&settings,&ctx);
        assert!(background.worker.is_some());
        let deadline=Instant::now()+Duration::from_secs(5);
        while background.texture.is_none(){background.poll(&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert_eq!(background.texture_source,settings.wallpaper_api);
        assert!(background.notice.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn fresh_cached_background_is_loaded_without_requesting_the_image_service() {
        let root=std::env::temp_dir().join(format!("eclipse-background-{}",uuid::Uuid::new_v4()));
        let path=root.join("wallpaper.jpg");
        let pixels=image::RgbImage::from_pixel(64,36,image::Rgb([40,100,180]));
        let mut bytes=Vec::new();image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&pixels).unwrap();
        let settings=Settings{wallpaper_source:super::super::settings::WallpaperSource::CustomApi,wallpaper_api:"https://wallpaper.example.invalid/image".into(),..Default::default()};
        write_cache(&path,&settings.wallpaper_api,&bytes).unwrap();
        let ctx=egui::Context::default();let mut background=Background::new(Some(path));
        let deadline=Instant::now()+Duration::from_secs(3);
        while background.texture.is_none(){background.sync(&settings,&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert!(background.worker.is_none());assert_eq!(background.texture.as_ref().unwrap().size(),[64,36]);
        let settings=Settings{wallpaper_source:super::super::settings::WallpaperSource::CustomApi,wallpaper_api:"https://second.example.invalid/image".into(),..settings};
        write_cache(background.cache.as_ref().unwrap(),&settings.wallpaper_api,&bytes).unwrap();
        let deadline=Instant::now()+Duration::from_secs(3);
        while background.texture_source!=settings.wallpaper_api {background.sync(&settings,&ctx);assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        assert!(background.worker.is_none());
        let settings=Settings{wallpaper_enabled:false,..settings};background.sync(&settings,&ctx);assert!(background.texture.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
