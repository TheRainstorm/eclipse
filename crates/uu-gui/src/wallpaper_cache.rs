//! A bounded archive of original wallpapers, separate from user favorites.
use anyhow::Context as _;
use fs2::FileExt as _;
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}, time::{Duration, Instant, SystemTime}};

const LIMIT:usize=100;

pub(super) fn directory()->Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|path|path.join("wallpapers/cache"))
}

fn owned(name:&str)->bool {
    let Some((stem,extension))=name.rsplit_once('.') else{return false;};
    let Some(hash)=stem.strip_prefix("cache-") else{return false;};
    hash.len()==64 && hash.bytes().all(|b|b.is_ascii_hexdigit())
        && matches!(extension,"png"|"jpg"|"webp"|"bmp"|"ico"|"gif"|"tiff"|"avif"|"image")
}

/// The caller already decoded and validated these original encoded bytes.
pub(super) fn save(root:&Path,bytes:&[u8],extension:&str)->anyhow::Result<()> {
    anyhow::ensure!(bytes.len()<=crate::wallpapers::MAX_WALLPAPER_BYTES,"wallpaper cache image too large");
    let name=format!("cache-{:x}.{extension}",Sha256::digest(bytes));
    anyhow::ensure!(owned(&name),"unsupported wallpaper cache filename");
    fs::create_dir_all(root).context("cannot create wallpaper archive")?;
    let lock=fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(root.join("cache.lock"))?;
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        match lock.try_lock_exclusive() {
            Ok(())=>break,
            Err(error) if error.kind()==std::io::ErrorKind::WouldBlock && Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(10)),
            Err(error)=>return Err(error).context("wallpaper archive is busy"),
        }
    }
    // Replacing a duplicate refreshes its age without adding a second entry.
    super::settings::save_bytes(&root.join(&name),bytes)?;
    let mut older=Vec::new();
    for item in fs::read_dir(root)? {
        let item=item?;
        let filename=item.file_name();let Some(filename)=filename.to_str() else{continue;};
        if filename==name || !owned(filename) {continue;}
        let metadata=item.metadata()?;
        if metadata.is_file() {older.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),filename.to_owned()));}
    }
    older.sort_unstable_by(|a,b|b.cmp(a));
    for (_,filename) in older.into_iter().skip(LIMIT-1) {fs::remove_file(root.join(filename))?;}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_images_are_deduplicated_and_only_the_latest_hundred_are_retained() {
        let root=std::env::temp_dir().join(format!("eclipse-wallpaper-archive-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let unrelated=root.join("my-photo.png");fs::write(&unrelated,b"user file").unwrap();
        for index in 0u16..105 {save(&root,&index.to_le_bytes(),"png").unwrap();}
        let images=||fs::read_dir(&root).unwrap().flatten().filter(|entry|owned(&entry.file_name().to_string_lossy())).count();
        assert_eq!(images(),100);
        let bytes=104u16.to_le_bytes();save(&root,&bytes,"png").unwrap();
        assert_eq!(images(),100);
        assert_eq!(fs::read(root.join(format!("cache-{:x}.png",Sha256::digest(bytes)))).unwrap(),bytes);
        assert_eq!(fs::read(unrelated).unwrap(),b"user file");
        fs::remove_dir_all(root).unwrap();
    }
}
