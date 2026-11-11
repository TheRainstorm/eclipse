use super::*;
use sha2::{Digest, Sha256};
use std::fs;

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
fn settle(favorites: &mut Favorites, ctx: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while favorites.busy() {
        favorites.poll(ctx);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn unlimited_favorites_use_standard_extensions_and_stable_descending_pages() {
    let store = new_store();
    let mut ids = vec![];
    for color in 0..40 {
        ids.push(store.save(&png(color)).unwrap().0);
    }
    let before = store.scan().unwrap();
    assert_eq!(before.len(), 40);
    assert_eq!(
        page_entries(&before, 0)
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ids[..20]
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    for color in 40..43 {
        ids.push(store.save(&png(color)).unwrap().0);
    }
    let after = store.scan().unwrap();
    assert_eq!(after.len(), 43);
    assert_eq!(page_entries(&before, 0), page_entries(&after, 0));
    assert_eq!(page_entries(&before, 1), page_entries(&after, 1));
    assert_eq!(page_entries(&after, 2).len(), 3);
    assert_eq!(after[0].id, ids[42]);
    assert!(after.iter().all(|e| e.file.ends_with(".png")));
    let (_, added, again) = store.save(&png(5)).unwrap();
    assert!(!added);
    assert_eq!(after, again);
    fs::remove_dir_all(store.root).unwrap();
}
#[test]
fn manual_images_use_first_scan_time_and_delete_all_known_duplicates() {
    let store = new_store();
    store.prepare(true).unwrap();
    let file = store.root.join("My photo.PNG");
    fs::write(&file, png(5)).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(10)),
        )
        .unwrap();
    fs::write(store.root.join("broken.jpg"), b"not an image").unwrap();
    fs::write(store.root.join("unrelated.txt"), b"keep").unwrap();
    let now = uu_core::time::now_ms();
    let first = store.scan().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].file, "My photo.PNG");
    assert!(first[0].added_at >= now);
    assert_eq!(store.scan().unwrap(), first);
    fs::write(store.root.join("duplicate.png"), png(5)).unwrap();
    let duplicate = store.scan().unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(duplicate[0].added_at, first[0].added_at);
    assert!(store.delete(&first[0].id).unwrap().is_empty());
    assert!(!file.exists());
    assert!(!store.root.join("duplicate.png").exists());
    assert!(store.scan().unwrap().is_empty());
    assert_eq!(fs::read(store.root.join("unrelated.txt")).unwrap(), b"keep");
    fs::remove_dir_all(store.root).unwrap();
}
#[test]
fn legacy_collection_migrates_without_losing_bytes_order_or_unrelated_files() {
    let store = new_store();
    store.prepare(true).unwrap();
    let mut ids = vec![];
    for color in 0..3 {
        let bytes = png(color);
        let id = format!("{:x}", Sha256::digest(&bytes));
        fs::write(store.root.join(format!("{id}.image")), &bytes).unwrap();
        ids.push(id);
    }
    fs::write(
        store.root.join("favorites.json"),
        serde_json::to_vec(&serde_json::json!({"version":1,"images":ids})).unwrap(),
    )
    .unwrap();
    let catalog = store.scan().unwrap();
    assert_eq!(catalog[0].id, ids[2]);
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(
            fs::read(store.root.join(format!("{id}.png"))).unwrap(),
            png(i as u8)
        );
        assert!(!store.root.join(format!("{id}.image")).exists());
    }
    assert_eq!(store.scan().unwrap(), catalog);
    fs::remove_dir_all(store.root).unwrap();
}
#[test]
fn concurrent_saves_preserve_every_entry() {
    let store = new_store();
    let threads: Vec<_> = (0..8)
        .map(|color| {
            let store = store.clone();
            std::thread::spawn(move || store.save(&png(color)).unwrap().0)
        })
        .collect();
    let ids: std::collections::HashSet<_> =
        threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        store
            .scan()
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect::<std::collections::HashSet<_>>(),
        ids
    );
    fs::remove_dir_all(store.root).unwrap();
}
#[test]
fn gallery_decodes_only_the_page_reuses_textures_and_keeps_offpage_selection() {
    let store = new_store();
    store.prepare(true).unwrap();
    for color in 0..25 {
        fs::write(store.root.join(format!("{color:02}.png")), png(color)).unwrap();
    }
    let ctx = egui::Context::default();
    let mut favorites = Favorites::at(store.root.clone());
    favorites.ensure_loaded(&ctx);
    settle(&mut favorites, &ctx);
    assert_eq!(
        (favorites.count(), favorites.page(), favorites.items.len()),
        (25, 1, 5)
    );
    let selected = favorites.items[0].id.clone();
    let textures: Vec<_> = favorites
        .items
        .iter()
        .map(|i| i.texture.as_ref().unwrap().id())
        .collect();
    favorites.reload(&ctx);
    settle(&mut favorites, &ctx);
    assert_eq!(
        textures,
        favorites
            .items
            .iter()
            .map(|i| i.texture.as_ref().unwrap().id())
            .collect::<Vec<_>>()
    );
    favorites.set_page(0, &ctx);
    settle(&mut favorites, &ctx);
    assert_eq!(favorites.items.len(), 20);
    let selection = favorites.selected(Some(&selected)).unwrap();
    assert_eq!(selection.id, selected);
    assert!(selection.load(4096).is_ok());
    fs::write(store.root.join("new.png"), png(27)).unwrap();
    favorites.reload(&ctx);
    settle(&mut favorites, &ctx);
    assert_eq!(favorites.page(), 0);
    assert_eq!(favorites.items.len(), 20);
    fs::remove_dir_all(store.root).unwrap();
}
#[test]
fn invalid_manifest_is_preserved() {
    let store = new_store();
    store.prepare(true).unwrap();
    let path = store.root.join("favorites.json");
    fs::write(&path, b"broken").unwrap();
    assert!(store.save(&png(1)).is_err());
    assert_eq!(fs::read(path).unwrap(), b"broken");
    fs::remove_dir_all(store.root).unwrap();
}
#[cfg(unix)]
#[test]
fn readonly_catalog_remains_viewable_and_links_do_not_redirect_writes() {
    use std::os::unix::fs::PermissionsExt;
    let store = new_store();
    store.save(&png(1)).unwrap();
    let lock = store.root.join("favorites.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(&store.root, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(store.scan().unwrap().len(), 1);
    fs::set_permissions(&store.root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(&lock).unwrap();
    let destination = store.root.with_extension("must-not-create");
    std::os::unix::fs::symlink(&destination, &lock).unwrap();
    assert!(store.save(&png(2)).is_err());
    assert!(!destination.exists());
    fs::remove_dir_all(store.root).unwrap();
}
