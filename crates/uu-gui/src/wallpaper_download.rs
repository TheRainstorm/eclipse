//! Save the displayed original bytes; dialogs and file IO stay off the UI thread.
use super::wallpapers::Original;
use eframe::egui;
use std::{path::PathBuf, sync::{mpsc, Arc}};

#[derive(Default)]
pub(super) struct Download {
    worker: Option<mpsc::Receiver<Result<Option<PathBuf>, String>>>,
    pub notice: String,
}
impl Download {
    pub fn busy(&self) -> bool { self.worker.is_some() }
    pub fn start(&mut self, bytes:Arc<Vec<u8>>, original:Original, ctx:&egui::Context) {
        let parent=eframe::viewport_window(ctx);
        let mut dialog=rfd::AsyncFileDialog::new()
            .set_title(uu_i18n::text("下载壁纸"))
            .set_file_name(format!("Eclipse-wallpaper-{}x{}.{}", original.width,original.height,original.extension()))
            .add_filter(original.extension().to_uppercase(), &[original.extension()]);
        if let Some(window)=&parent {dialog=dialog.set_parent(window.as_ref());}
        self.start_with(bytes, ctx, move || {
            // Retain the parent for the complete native dialog lifetime. The
            // worker only sends a value back; a retired UI drops its receiver.
            let _parent=parent;
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            Ok(runtime.block_on(dialog.save_file()).map(|file|file.path().to_owned()))
        });
    }
    fn start_with(&mut self, bytes:Arc<Vec<u8>>, ctx:&egui::Context,
        choose:impl FnOnce()->anyhow::Result<Option<PathBuf>> + Send + 'static) {
        if self.busy() {return;}
        self.notice=uu_i18n::text("请选择壁纸保存位置…").into();
        let (tx,rx)=mpsc::channel();
        let context=ctx.clone();
        match std::thread::Builder::new().name("eclipse-wallpaper-save".into()).spawn(move || {
            let result=(||->anyhow::Result<Option<PathBuf>> {
                let Some(path)=choose()? else {return Ok(None);};
                super::settings::save_bytes(&path,&bytes)?;
                Ok(Some(path))
            })().map_err(|error|format!("{error:#}"));
            let _=tx.send(result);
            context.request_repaint();
        }) {
            Ok(_)=>self.worker=Some(rx),
            Err(error)=>self.notice=uu_i18n::format!("壁纸保存失败：{error}"),
        }
    }
    pub fn poll(&mut self,ctx:&egui::Context)->bool {
        let result=self.worker.as_ref().and_then(|rx|match rx.try_recv() {
            Ok(value)=>Some(value),
            Err(mpsc::TryRecvError::Empty)=>None,
            Err(mpsc::TryRecvError::Disconnected)=>Some(Err("wallpaper save worker stopped".into())),
        });
        if let Some(result)=result {
            self.worker=None;
            self.notice=match result {
                Ok(Some(path))=>uu_i18n::format!("壁纸已保存：{}",path.display()),
                Ok(None)=>String::new(),
                Err(error)=>uu_i18n::format!("壁纸保存失败：{error}"),
            };
            return true;
        }
        if self.busy() {ctx.request_repaint_after(std::time::Duration::from_millis(100));}
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn finish(download:&mut Download,ctx:&egui::Context) {
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
        while download.busy() {download.poll(ctx);assert!(std::time::Instant::now()<deadline);std::thread::sleep(std::time::Duration::from_millis(5));}
    }
    #[test]
    fn original_is_pinned_before_the_dialog_and_cancel_never_writes() {
        let root=std::env::temp_dir().join(format!("eclipse-wallpaper-save-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path=root.join("original.webp");
        let bytes=Arc::new(b"original encoded image, never UI preview RGBA".to_vec());
        let ctx=egui::Context::default();let mut download=Download::default();
        let chosen=path.clone();download.start_with(bytes.clone(),&ctx,move||Ok(Some(chosen)));
        finish(&mut download,&ctx);
        assert_eq!(std::fs::read(&path).unwrap(),*bytes);
        let previous=download.notice.clone();assert!(!previous.is_empty());
        download.start_with(Arc::new(b"replacement must not be saved".to_vec()),&ctx,||Ok(None));
        finish(&mut download,&ctx);
        assert!(download.notice.is_empty());assert_eq!(std::fs::read(&path).unwrap(),*bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}
