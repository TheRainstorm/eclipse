//! An executable-local, bounded wallpaper collection. IO and decode run off UI.
use super::ui_feedback::{Feedback,Scope,Kind};
use super::wallpapers;
use anyhow::Context as _;
use eframe::egui;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::Duration,
};

const CAPACITY: usize = 10;
const MAX_BYTES: usize = wallpapers::MAX_WALLPAPER_BYTES;
#[derive(Clone, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    images: Vec<String>,
}
impl Manifest {
    fn empty() -> Self {
        Self {
            version: 1,
            images: vec![],
        }
    }
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 1 && self.images.len() <= CAPACITY,
            "invalid favorites manifest version or count"
        );
        let mut ids = std::collections::HashSet::new();
        for id in &self.images {
            anyhow::ensure!(
                valid_id(id) && ids.insert(id),
                "invalid or duplicate favorite identity"
            );
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn regular(path: &Path, directory: bool) -> anyhow::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            meta.file_attributes() & 0x400 == 0,
            "favorite path is a reparse point"
        );
    }
    anyhow::ensure!(
        !meta.file_type().is_symlink()
            && if directory {
                meta.is_dir()
            } else {
                meta.is_file()
            },
        "favorite path is not a regular {}",
        if directory { "directory" } else { "file" }
    );
    Ok(())
}
fn regular_if_present(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            regular(path, false)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn open(path: &Path, write: bool, create: bool) -> anyhow::Result<fs::File> {
    regular_if_present(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(write).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "favorite handle is not a regular file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            meta.file_attributes() & 0x400 == 0,
            "favorite handle is a reparse point"
        );
    }
    Ok(file)
}
fn read(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open(path, false, false)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= limit, "favorite file exceeds size limit");
    Ok(bytes)
}
#[derive(Clone)]
struct Store {
    root: PathBuf,
}
impl Store {
    fn application() -> anyhow::Result<Self> {
        let exe = std::env::current_exe().context("cannot locate application folder")?;
        Ok(Self {
            root: exe
                .parent()
                .context("application folder unavailable")?
                .join("wallpapers/favorites"),
        })
    }
    fn prepare(&self, create: bool) -> anyhow::Result<bool> {
        // Check existing parents before create_dir_all, including Windows
        // junctions. Never use a redirected favorites folder as a write target.
        for path in self.root.ancestors() {
            match fs::symlink_metadata(path) {
                Ok(_) => regular(path, true)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !self.root.exists() {
            if !create {
                return Ok(false);
            }
            fs::create_dir_all(&self.root)?;
        }
        regular(&self.root, true)?;
        Ok(true)
    }
    fn lock(&self, write: bool) -> anyhow::Result<fs::File> {
        let file = open(&self.root.join("favorites.lock"), write, write)?;
        let started = std::time::Instant::now();
        loop {
            let result = if write {
                fs2::FileExt::try_lock_exclusive(&file)
            } else {
                fs2::FileExt::try_lock_shared(&file)
            };
            match result {
                Ok(()) => return Ok(file),
                Err(e)
                    if e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                        && started.elapsed() < Duration::from_secs(3) =>
                {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e).context("favorites collection is busy or inaccessible"),
            }
        }
    }
    fn manifest(&self) -> anyhow::Result<Manifest> {
        let path = self.root.join("favorites.json");
        let manifest = match read(&path, 64 * 1024) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("favorites manifest is invalid; original preserved")?,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Manifest::empty()
            }
            Err(e) => return Err(e),
        };
        manifest.validate()?;
        Ok(manifest)
    }
    fn load(&self) -> anyhow::Result<Vec<(String, wallpapers::Thumbnail)>> {
        if !self.prepare(false)? {
            return Ok(vec![]);
        }
        if !regular_if_present(&self.root.join("favorites.json"))? {
            return Ok(vec![]);
        }
        let lock = self.lock(false)?;
        let manifest = self.manifest()?;
        let images = self.read_images(&manifest)?;
        drop(lock);
        decode_images(images)
    }
    fn read_images(&self, manifest: &Manifest) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let mut images = Vec::new();
        for id in &manifest.images {
            let bytes = read(&self.root.join(format!("{id}.image")), MAX_BYTES)?;
            anyhow::ensure!(
                format!("{:x}", Sha256::digest(&bytes)) == *id,
                "favorite image hash mismatch"
            );
            images.push((id.clone(), bytes));
        }
        Ok(images)
    }
    fn save(
        &self,
        bytes: &[u8],
    ) -> anyhow::Result<(String, bool, Vec<(String, wallpapers::Thumbnail)>)> {
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_BYTES,
            "favorite image exceeds size limit"
        );
        // Validate before claiming an identity or changing the collection.
        wallpapers::decode_favorite(
            wallpapers::Request {
                device_id: "favorite".into(),
                url: String::new(),
            },
            bytes.to_vec(),
        )?;
        self.prepare(true)?;
        let lock = self.lock(true)?;
        let mut manifest = self.manifest()?;
        let id = format!("{:x}", Sha256::digest(bytes));
        if manifest.images.contains(&id) {
            let images = self.read_images(&manifest)?;
            drop(lock);
            return Ok((id, false, decode_images(images)?));
        }
        let path = self.root.join(format!("{id}.image"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let result = file.write_all(bytes).and_then(|_| file.sync_all());
                drop(file);
                if let Err(error) = result {
                    let _ = fs::remove_file(&path);
                    return Err(error.into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    read(&path, MAX_BYTES)? == bytes,
                    "existing favorite file does not match its identity"
                );
            }
            Err(e) => return Err(e.into()),
        }
        manifest.images.push(id.clone());
        let retired = if manifest.images.len() > CAPACITY {
            Some(manifest.images.remove(0))
        } else {
            None
        };
        let manifest_path = self.root.join("favorites.json");
        regular_if_present(&manifest_path)?;
        let temporary = self
            .root
            .join(format!(".favorites-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        // This path is owned only after create_new succeeds; a collision can
        // never cause cleanup to delete a pre-existing unrelated file.
        let written = file
            .write_all(&serde_json::to_vec_pretty(&manifest)?)
            .and_then(|_| file.sync_all());
        drop(file);
        let result = written.and_then(|_| fs::rename(&temporary, &manifest_path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        // Only the oldest file explicitly owned by the preimage manifest can
        // be retired. Ignore unrelated files and preserve externally changed data.
        if let Some(retired) = retired {
            let path = self.root.join(format!("{retired}.image"));
            if read(&path, MAX_BYTES)
                .is_ok_and(|data| format!("{:x}", Sha256::digest(data)) == retired)
            {
                fs::remove_file(path)?;
            }
        }
        let images = self.read_images(&manifest)?;
        drop(lock);
        Ok((id, true, decode_images(images)?))
    }
}
fn decode_images(
    images: Vec<(String, Vec<u8>)>,
) -> anyhow::Result<Vec<(String, wallpapers::Thumbnail)>> {
    images
        .into_iter()
        .map(|(id, bytes)| {
            let image = wallpapers::decode_favorite(
                wallpapers::Request {
                    device_id: "favorite".into(),
                    url: format!("favorite:{id}"),
                },
                bytes,
            )?;
            Ok((id, image))
        })
        .collect()
}

pub(super) struct Item {
    pub id: String,
    pub texture: egui::TextureHandle,
    pub encoded: Arc<Vec<u8>>,
}
struct Loaded {
    images: Vec<(String, wallpapers::Thumbnail)>,
    saved: Option<(String, bool)>,
}
pub(super) struct Favorites {
    store: Result<Store, String>,
    pub items: Vec<Item>,
    worker: Option<mpsc::Receiver<Result<Loaded, String>>>,
    pub loaded: bool,
    pub notice: Feedback,
    pending_save: std::collections::VecDeque<Arc<Vec<u8>>>,
}
impl Default for Favorites {
    fn default() -> Self {
        Self {
            store: Store::application().map_err(|e| format!("{e:#}")),
            items: vec![],
            worker: None,
            loaded: false,
            notice: Feedback::default(),
            pending_save: Default::default(),
        }
    }
}
impl Favorites {
    pub fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if !self.loaded && self.worker.is_none() {
            self.reload(ctx);
        }
    }
    pub fn reload(&mut self, ctx: &egui::Context) {
        if self.worker.is_none() {
            self.start(None, ctx);
        }
    }
    pub fn save(&mut self, bytes: Arc<Vec<u8>>, ctx: &egui::Context) {
        if self.worker.is_some() {
            if self.pending_save.len() < CAPACITY {
                self.pending_save.push_back(bytes);
            } else {
                self.notice.error(Scope::Wallpaper,uu_i18n::text("收藏队列已满，请稍后重试"));
            }
            return;
        }
        self.start(Some(bytes), ctx);
    }
    fn start(&mut self, bytes: Option<Arc<Vec<u8>>>, ctx: &egui::Context) {
        let store = match &self.store {
            Ok(store) => store.clone(),
            Err(error) => {
                self.notice.error(Scope::Wallpaper,uu_i18n::format!("无法读取壁纸收藏：{error}"));
                self.loaded = true;
                return;
            }
        };
        self.notice.report(Scope::Wallpaper,Kind::Progress,uu_i18n::text(if bytes.is_some() {
            "正在收藏当前壁纸…"
        } else {
            "正在读取收藏壁纸…"
        }));
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        let context = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("eclipse-favorites".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<Loaded> {
                    if let Some(bytes) = bytes {
                        let (id, added, images) = store.save(&bytes)?;
                        Ok(Loaded {
                            images,
                            saved: Some((id, added)),
                        })
                    } else {
                        Ok(Loaded {
                            images: store.load()?,
                            saved: None,
                        })
                    }
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                context.request_repaint();
            });
        if let Err(error) = spawned {
            self.worker = None;
            self.loaded = true;
            self.notice.error(Scope::Wallpaper,uu_i18n::format!("壁纸收藏操作失败：{error}"));
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.worker.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("wallpaper favorites worker stopped".into()))
            }
        }) {
            self.worker = None;
            self.loaded = true;
            match result {
                Ok(loaded) => {
                    self.items = loaded
                        .images
                        .into_iter()
                        .map(|(id, image)| Item {
                            id: id.clone(),
                            texture: ctx.load_texture(
                                format!("favorite-{id}"),
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width as usize, image.height as usize],
                                    &image.rgba,
                                ),
                                egui::TextureOptions::LINEAR,
                            ),
                            encoded: Arc::new(image.encoded),
                        })
                        .collect();
                    match loaded.saved {
                        Some((_, true)) => self.notice.success(Scope::Wallpaper,uu_i18n::text("已收藏当前壁纸")),
                        Some((_, false)) => self.notice.report(Scope::Wallpaper,Kind::Info,uu_i18n::text("这张壁纸已在收藏中")),
                        None => self.notice.clear(),
                    }
                }
                Err(error) => self.notice.error(Scope::Wallpaper,uu_i18n::format!("壁纸收藏操作失败：{error}")),
            }
            if let Some(bytes) = self.pending_save.pop_front() {
                self.start(Some(bytes), ctx);
            }
        }
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    #[cfg(test)]
    pub fn at(root: PathBuf) -> Self {
        Self {
            store: Ok(Store { root }),
            ..Self::default()
        }
    }
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.worker.is_some() || !self.pending_save.is_empty()
    }
    pub fn selected(&self, id: Option<&str>) -> Option<&Item> {
        id.and_then(|id| self.items.iter().find(|item| item.id == id))
            .or_else(|| self.items.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png(color: u8) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(16, 9, image::Rgb([color, 100, 180]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    fn new_store() -> Store {
        Store {
            root: std::env::temp_dir().join(format!("eclipse-favorites-{}", uuid::Uuid::new_v4())),
        }
    }
    #[test]
    fn favorites_keep_ten_in_order_deduplicate_and_never_delete_unrelated_files() {
        let store = new_store();
        store.prepare(true).unwrap();
        fs::write(store.root.join("unrelated.txt"), b"keep").unwrap();
        let mut ids = vec![];
        for color in 0..11 {
            ids.push(store.save(&png(color)).unwrap().0);
        }
        let images = store.load().unwrap();
        assert_eq!(
            images.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            ids[1..].iter().collect::<Vec<_>>()
        );
        assert!(!store.root.join(format!("{}.image", ids[0])).exists());
        let (_, added, images) = store.save(&png(5)).unwrap();
        assert!(!added);
        assert_eq!(images.len(), 10);
        assert_eq!(images[0].0, ids[1]);
        assert_eq!(fs::read(store.root.join("unrelated.txt")).unwrap(), b"keep");
        fs::remove_dir_all(store.root).unwrap();
    }
    #[test]
    fn concurrent_saves_do_not_lose_ordered_entries() {
        let store = new_store();
        let threads = (0..8)
            .map(|color| {
                let store = store.clone();
                std::thread::spawn(move || store.save(&png(color)).unwrap().0)
            })
            .collect::<Vec<_>>();
        let ids = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        let actual = store
            .load()
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(actual, ids);
        fs::remove_dir_all(store.root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn readonly_collection_can_be_viewed_and_symbolic_lock_cannot_redirect_writes() {
        use std::os::unix::fs::PermissionsExt;
        let store = new_store();
        store.save(&png(1)).unwrap();
        let lock = store.root.join("favorites.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(&store.root, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
        fs::set_permissions(&store.root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(&lock).unwrap();
        let destination = store.root.with_extension("must-not-create");
        std::os::unix::fs::symlink(&destination, &lock).unwrap();
        assert!(store.save(&png(2)).is_err());
        assert!(!destination.exists());
        fs::remove_dir_all(store.root).unwrap();
    }
    #[test]
    fn invalid_manifest_is_preserved_and_linked_images_are_rejected() {
        let store = new_store();
        store.prepare(true).unwrap();
        let path = store.root.join("favorites.json");
        fs::write(&path, b"broken").unwrap();
        assert!(store.save(&png(1)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::remove_dir_all(store.root).unwrap();
        #[cfg(unix)]
        {
            let store = new_store();
            let data = png(2);
            let (id, _, _) = store.save(&data).unwrap();
            let file = store.root.join(format!("{id}.image"));
            fs::remove_file(&file).unwrap();
            std::os::unix::fs::symlink("unrelated", &file).unwrap();
            assert!(store.load().is_err());
            fs::remove_dir_all(store.root).unwrap();
        }
    }
}
