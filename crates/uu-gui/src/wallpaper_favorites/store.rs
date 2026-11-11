//! Executable-local image catalog. Discovery time belongs to content, not mtime.
use super::{files::*, wallpapers};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

pub(super) const PAGE_SIZE: usize = 20;
const MAX_BYTES: usize = wallpapers::MAX_WALLPAPER_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    pub id: String,
    pub file: String,
    pub added_at: u64,
    size: u64,
    modified_ns: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    images: Vec<Entry>,
}
#[derive(Deserialize)]
struct Legacy {
    version: u32,
    images: Vec<String>,
}

#[derive(Clone)]
pub(super) struct Store {
    pub root: PathBuf,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn timestamp(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_nanos().min(u64::MAX as u128) as u64)
}
fn filename(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}
fn extension(name: &str) -> Option<String> {
    let ext = std::path::Path::new(name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "ico" | "image"
    )
    .then_some(ext)
}
fn original(bytes: &[u8]) -> anyhow::Result<wallpapers::Original> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format().context("unknown image encoding")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let (width, height) = reader.into_dimensions()?;
    anyhow::ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192,
        "invalid image dimensions"
    );
    Ok(wallpapers::Original {
        width,
        height,
        format,
    })
}

impl Store {
    pub fn application() -> anyhow::Result<Self> {
        let exe = std::env::current_exe().context("cannot locate application folder")?;
        Ok(Self {
            root: exe
                .parent()
                .context("application folder unavailable")?
                .join("wallpapers/favorites"),
        })
    }
    pub fn prepare(&self, create: bool) -> anyhow::Result<bool> {
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
        let started = Instant::now();
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
        if !regular_if_present(&path)? {
            return Ok(Manifest {
                version: 2,
                images: vec![],
            });
        }
        // Metadata grows with the collection; there is no photo-count ceiling.
        let value: serde_json::Value = serde_json::from_reader(open(&path, false, false)?)
            .context("favorites manifest is invalid; original preserved")?;
        let manifest = if value.get("version").and_then(|v| v.as_u64()) == Some(1) {
            let old: Legacy = serde_json::from_value(value)?;
            anyhow::ensure!(old.version == 1, "invalid legacy favorites version");
            let mut seen = HashSet::new();
            let mut previous = 0;
            let mut images = vec![];
            for id in old.images {
                anyhow::ensure!(
                    valid_id(&id) && seen.insert(id.clone()),
                    "invalid legacy favorite identity"
                );
                let file = format!("{id}.image");
                let meta = fs::symlink_metadata(self.root.join(&file)).ok();
                let time = meta
                    .as_ref()
                    .map_or(0, |m| timestamp(m) / 1_000_000)
                    .max(previous + 1);
                previous = time;
                images.push(Entry {
                    id,
                    file,
                    added_at: time,
                    size: 0,
                    modified_ns: 0,
                });
            }
            Manifest { version: 1, images }
        } else {
            serde_json::from_value(value)?
        };
        anyhow::ensure!(
            matches!(manifest.version, 1 | 2),
            "invalid favorites manifest version"
        );
        let mut names = HashSet::new();
        for item in &manifest.images {
            anyhow::ensure!(
                valid_id(&item.id)
                    && filename(&item.file)
                    && extension(&item.file).is_some()
                    && names.insert(item.file.clone()),
                "invalid or duplicate favorite file"
            );
        }
        Ok(manifest)
    }
    fn publish(&self, manifest: &Manifest) -> anyhow::Result<()> {
        regular_if_present(&self.root.join("favorites.json"))?;
        super::super::settings::save_json(manifest, &self.root.join("favorites.json"))
    }
    fn write_image(&self, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let path = self.root.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let result = file.write_all(bytes).and_then(|_| file.sync_all());
                drop(file);
                if result.is_err() {
                    let _ = fs::remove_file(path);
                }
                result?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => anyhow::ensure!(
                read(&path, MAX_BYTES)? == bytes,
                "existing favorite file changed"
            ),
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
    fn scan_locked(&self) -> anyhow::Result<Manifest> {
        let old = self.manifest()?;
        let by_file: HashMap<_, _> = old.images.iter().map(|e| (e.file.as_str(), e)).collect();
        let mut times: HashMap<String, u64> = HashMap::new();
        for item in &old.images {
            times
                .entry(item.id.clone())
                .and_modify(|t| *t = (*t).min(item.added_at))
                .or_insert(item.added_at);
        }
        let mut next = uu_core::time::now_ms().max(
            old.images
                .iter()
                .map(|e| e.added_at)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        let mut paths = fs::read_dir(&self.root)?.collect::<Result<Vec<_>, _>>()?;
        paths.sort_by_key(|e| e.file_name());
        let mut images = vec![];
        let mut retired = vec![];
        let mut names = HashSet::new();
        for path in paths {
            let Some(name) = path.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(ext) = extension(&name) else {
                continue;
            };
            if regular(&path.path(), false).is_err() {
                continue;
            }
            let meta = fs::metadata(path.path())?;
            if meta.len() == 0 || meta.len() > MAX_BYTES as u64 {
                continue;
            }
            let stamp = timestamp(&meta);
            if let Some(item) = by_file
                .get(name.as_str())
                .filter(|e| ext != "image" && e.size == meta.len() && e.modified_ns == stamp)
            {
                if names.insert(name) {
                    images.push((*item).clone());
                }
                continue;
            }
            let Ok(bytes) = read(&path.path(), MAX_BYTES) else {
                continue;
            };
            let Ok(info) = original(&bytes) else { continue };
            let id = hash(&bytes);
            // Only content-addressed legacy .image files belong to migration.
            if ext == "image" && name != format!("{id}.image") {
                continue;
            }
            let added_at = *times.entry(id.clone()).or_insert_with(|| {
                let time = next;
                next = next.saturating_add(1);
                time
            });
            let file = if ext == "image" {
                let file = format!("{id}.{}", info.extension());
                self.write_image(&file, &bytes)?;
                retired.push((name, id.clone()));
                file
            } else {
                name
            };
            let meta = fs::metadata(self.root.join(&file))?;
            if names.insert(file.clone()) {
                images.push(Entry {
                    id,
                    file,
                    added_at,
                    size: meta.len(),
                    modified_ns: timestamp(&meta),
                });
            }
        }
        let manifest = Manifest { version: 2, images };
        if old.version != 2 || old.images != manifest.images {
            self.publish(&manifest)?;
        }
        // A failed manifest publication leaves both the legacy original and its
        // standard-format copy intact. Only retire verified legacy bytes after it.
        for (name, id) in retired {
            let path = self.root.join(name);
            if read(&path, MAX_BYTES).is_ok_and(|bytes| hash(&bytes) == id) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(manifest)
    }
    fn catalog(manifest: Manifest) -> Vec<Entry> {
        let mut images = manifest.images;
        images.sort_by(|a, b| {
            b.added_at
                .cmp(&a.added_at)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.file.cmp(&b.file))
        });
        let mut seen = HashSet::new();
        images.retain(|e| seen.insert(e.id.clone()));
        images
    }
    pub fn scan(&self) -> anyhow::Result<Vec<Entry>> {
        if !self.prepare(false)? {
            return Ok(vec![]);
        }
        match self.lock(true) {
            Ok(_lock) => Ok(Self::catalog(self.scan_locked()?)),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied) =>
            {
                let _lock = self.lock(false)?;
                Ok(Self::catalog(self.manifest()?))
            }
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, bytes: &[u8]) -> anyhow::Result<(String, bool, Vec<Entry>)> {
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_BYTES,
            "favorite image exceeds size limit"
        );
        let image = wallpapers::decode_favorite(
            wallpapers::Request {
                device_id: "favorite".into(),
                url: String::new(),
            },
            bytes.to_vec(),
        )?;
        self.prepare(true)?;
        let _lock = self.lock(true)?;
        let mut manifest = self.scan_locked()?;
        let id = hash(bytes);
        if manifest.images.iter().any(|e| e.id == id) {
            return Ok((id, false, Self::catalog(manifest)));
        }
        let file = format!("{id}.{}", image.original.extension());
        self.write_image(&file, bytes)?;
        let meta = fs::metadata(self.root.join(&file))?;
        let added_at = uu_core::time::now_ms().max(
            manifest
                .images
                .iter()
                .map(|e| e.added_at)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        manifest.images.push(Entry {
            id: id.clone(),
            file,
            added_at,
            size: meta.len(),
            modified_ns: timestamp(&meta),
        });
        self.publish(&manifest)?;
        Ok((id, true, Self::catalog(manifest)))
    }
    pub fn delete(&self, id: &str) -> anyhow::Result<Vec<Entry>> {
        anyhow::ensure!(valid_id(id), "invalid favorite identity");
        self.prepare(false)?;
        let _lock = self.lock(true)?;
        let mut manifest = self.scan_locked()?;
        let files: Vec<_> = manifest.images.iter().filter(|e| e.id == id).collect();
        for item in &files {
            anyhow::ensure!(
                hash(&read(&self.root.join(&item.file), MAX_BYTES)?) == id,
                "favorite changed; refresh before deleting"
            );
        }
        for item in files {
            fs::remove_file(self.root.join(&item.file))?;
        }
        manifest.images.retain(|e| e.id != id);
        self.publish(&manifest)?;
        Ok(Self::catalog(manifest))
    }
    pub fn bytes(&self, item: &Entry) -> anyhow::Result<Vec<u8>> {
        let bytes = read(&self.root.join(&item.file), MAX_BYTES)?;
        anyhow::ensure!(
            hash(&bytes) == item.id,
            "favorite image changed; refresh the collection"
        );
        Ok(bytes)
    }
}
