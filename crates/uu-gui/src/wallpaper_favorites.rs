//! Background catalog discovery and a bounded page of gallery textures.
use super::{
    ui_feedback::{Feedback, Kind, Scope},
    wallpapers,
};
use eframe::egui;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
mod files;
mod store;
use store::{Entry, Store, PAGE_SIZE};
const SCAN_INTERVAL: Duration = Duration::from_secs(10);

pub(super) struct Item {
    pub id: String,
    pub filename: String,
    pub texture: Option<egui::TextureHandle>,
    pub error: Option<String>,
}
pub(super) struct Selection {
    pub id: String,
    store: Store,
    item: Entry,
}
impl Selection {
    pub fn load(self, side: usize) -> anyhow::Result<wallpapers::Thumbnail> {
        wallpapers::decode_wallpaper_with_limit(
            wallpapers::Request {
                device_id: "background".into(),
                url: format!("favorite:{}", self.id),
            },
            self.store.bytes(&self.item)?,
            side,
        )
    }
}
enum Job {
    Load(Option<usize>),
    Save(Arc<Vec<u8>>),
    Delete(String),
}
enum Notice {
    Saved(bool),
    Deleted,
}
struct Loaded {
    catalog: Vec<Entry>,
    page: usize,
    follow_latest: bool,
    images: Option<Vec<(Entry, Result<wallpapers::Thumbnail, String>)>>,
    notice: Option<Notice>,
}
pub(super) struct Favorites {
    store: Result<Store, String>,
    catalog: Vec<Entry>,
    pub items: Vec<Item>,
    worker: Option<mpsc::Receiver<Result<Loaded, String>>>,
    pub loaded: bool,
    pub notice: Feedback,
    pending: VecDeque<Job>,
    page: usize,
    follow_latest: bool,
    scanned_at: Option<Instant>,
}
impl Default for Favorites {
    fn default() -> Self {
        Self {
            store: Store::application().map_err(|e| format!("{e:#}")),
            catalog: vec![],
            items: vec![],
            worker: None,
            loaded: false,
            notice: Default::default(),
            pending: Default::default(),
            page: 0,
            follow_latest: true,
            scanned_at: None,
        }
    }
}
impl Favorites {
    pub fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if self.worker.is_none()
            && (!self.loaded
                || self
                    .scanned_at
                    .is_some_and(|t| t.elapsed() >= SCAN_INTERVAL))
        {
            self.reload(ctx);
        }
        if self.worker.is_none() {
            if let Some(time) = self.scanned_at {
                ctx.request_repaint_after(SCAN_INTERVAL.saturating_sub(time.elapsed()));
            }
        }
    }
    pub fn reload(&mut self, ctx: &egui::Context) {
        self.enqueue(Job::Load((!self.follow_latest).then_some(self.page)), ctx);
    }
    pub fn save(&mut self, bytes: Arc<Vec<u8>>, ctx: &egui::Context) {
        self.enqueue(Job::Save(bytes), ctx);
    }
    pub fn delete(&mut self, id: String, ctx: &egui::Context) {
        self.enqueue(Job::Delete(id), ctx);
    }
    pub fn page(&self) -> usize {
        self.page
    }
    pub fn pages(&self) -> usize {
        self.catalog.len().div_ceil(PAGE_SIZE).max(1)
    }
    pub fn count(&self) -> usize {
        self.catalog.len()
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some() || !self.pending.is_empty()
    }
    pub fn set_page(&mut self, page: usize, ctx: &egui::Context) {
        self.enqueue(Job::Load(Some(page)), ctx);
    }
    pub fn selected(&self, id: Option<&str>) -> Option<Selection> {
        let item = id
            .and_then(|id| self.catalog.iter().find(|e| e.id == id))
            .or_else(|| self.catalog.first())?;
        Some(Selection {
            id: item.id.clone(),
            store: self.store.as_ref().ok()?.clone(),
            item: item.clone(),
        })
    }
    pub fn next(&self, current: Option<&str>) -> Option<String> {
        if self.catalog.is_empty() {
            return None;
        }
        let index = current
            .and_then(|id| self.catalog.iter().position(|e| e.id == id))
            .map_or(0, |i| (i + 1) % self.catalog.len());
        Some(self.catalog[index].id.clone())
    }
    pub fn editor(&mut self, ui: &mut egui::Ui, selected: &mut Option<String>) -> bool {
        self.ensure_loaded(ui.ctx());
        self.poll(ui.ctx());
        ui.weak(uu_i18n::text("按收藏时间从新到旧显示，每页20张。也可直接将图片放入收藏目录，首次扫描时间作为收藏时间。"));
        ui.horizontal_wrapped(|ui| {
            ui.label(uu_i18n::format!(
                "共 {} 张 · 第 {} / {} 页",
                self.count(),
                self.page() + 1,
                self.pages()
            ));
            if ui
                .add_enabled(
                    !self.busy(),
                    crate::theme::secondary_button(uu_i18n::text("扫描收藏目录")),
                )
                .clicked()
            {
                self.reload(ui.ctx());
            }
            if let Ok(store) = &self.store {
                if ui
                    .add(crate::theme::secondary_button(uu_i18n::text(
                        "复制收藏目录",
                    )))
                    .on_hover_text(store.root.display().to_string())
                    .clicked()
                {
                    ui.ctx().copy_text(store.root.display().to_string());
                }
            }
        });
        let mut target = None;
        let last = self.pages() - 1;
        ui.add_enabled_ui(!self.busy(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.page < last,
                        crate::theme::secondary_button(uu_i18n::text("较新一页")),
                    )
                    .clicked()
                {
                    target = Some(self.page + 1);
                }
                let mut pages = std::collections::BTreeSet::from([0, last]);
                pages.extend(self.page.saturating_sub(2)..=self.page.saturating_add(2).min(last));
                let mut previous = None;
                for page in pages.into_iter().rev() {
                    if previous.is_some_and(|p| p > page + 1) {
                        ui.label("…");
                    }
                    if ui
                        .selectable_label(page == self.page, (page + 1).to_string())
                        .clicked()
                    {
                        target = Some(page);
                    }
                    previous = Some(page);
                }
                if ui
                    .add_enabled(
                        self.page > 0,
                        crate::theme::secondary_button(uu_i18n::text("较旧一页")),
                    )
                    .clicked()
                {
                    target = Some(self.page - 1);
                }
            })
        });
        if let Some(page) = target {
            self.set_page(page, ui.ctx());
        }
        if self.loaded && self.catalog.is_empty() {
            ui.label(uu_i18n::text(
                "还没有收藏壁纸；可从其它来源收藏，或将照片放入收藏目录。",
            ));
        }
        let current = self.selected(selected.as_deref()).map(|s| s.id);
        let mut changed = false;
        let mut delete = None;
        ui.horizontal_wrapped(|ui| {
            for item in &self.items {
                let response = if let Some(texture) = &item.texture {
                    ui.add(
                        egui::ImageButton::new(
                            egui::Image::from_texture(texture).max_size(egui::vec2(128., 72.)),
                        )
                        .selected(current.as_ref() == Some(&item.id)),
                    )
                } else {
                    ui.add_sized(
                        [128., 72.],
                        egui::Button::new(uu_i18n::text("图片无法解码")),
                    )
                };
                let response =
                    response.on_hover_text(item.error.as_deref().unwrap_or(&item.filename));
                if response.clicked() && item.texture.is_some() {
                    *selected = Some(item.id.clone());
                    changed = true;
                }
                response.context_menu(|ui| {
                    if crate::theme::menu_action(ui, !self.busy(), uu_i18n::text("删除收藏图片"))
                        .clicked()
                    {
                        delete = Some(item.id.clone());
                        ui.close_menu();
                    }
                });
            }
        });
        if let Some(id) = delete {
            self.delete(id, ui.ctx());
        }
        self.notice.show(ui, Scope::Wallpaper);
        changed
    }
    fn enqueue(&mut self, job: Job, ctx: &egui::Context) {
        if self.worker.is_none() {
            self.start(job, ctx);
            return;
        }
        if matches!(&job, Job::Load(_)) {
            self.pending.retain(|j| !matches!(j, Job::Load(_)));
        }
        if self.pending.len() < 8 {
            self.pending.push_back(job);
        } else {
            self.notice
                .error(Scope::Wallpaper, uu_i18n::text("收藏队列已满，请稍后重试"));
        }
    }
    fn start(&mut self, job: Job, ctx: &egui::Context) {
        let store = match &self.store {
            Ok(store) => store.clone(),
            Err(error) => {
                self.notice.error(
                    Scope::Wallpaper,
                    uu_i18n::format!("无法读取壁纸收藏：{error}"),
                );
                self.loaded = true;
                self.scanned_at = Some(Instant::now());
                return;
            }
        };
        match &job {
            Job::Save(_) => self.notice.report(
                Scope::Wallpaper,
                Kind::Progress,
                uu_i18n::text("正在收藏当前壁纸…"),
            ),
            Job::Delete(_) => self.notice.report(
                Scope::Wallpaper,
                Kind::Progress,
                uu_i18n::text("正在删除收藏壁纸…"),
            ),
            Job::Load(_) if !self.loaded => self.notice.report(
                Scope::Wallpaper,
                Kind::Progress,
                uu_i18n::text("正在读取收藏壁纸…"),
            ),
            _ => {}
        }
        let previous = page_entries(&self.catalog, self.page).to_vec();
        let previous_page = self.page;
        let follow_latest = self.follow_latest;
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        let context = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("eclipse-favorites".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<Loaded> {
                    let (catalog, page, notice) = match job {
                        Job::Load(page) => (store.scan()?, page, None),
                        Job::Save(bytes) => {
                            let (_, added, catalog) = store.save(&bytes)?;
                            (catalog, None, Some(Notice::Saved(added)))
                        }
                        Job::Delete(id) => (
                            store.delete(&id)?,
                            (!follow_latest).then_some(previous_page),
                            Some(Notice::Deleted),
                        ),
                    };
                    let last = catalog.len().div_ceil(PAGE_SIZE).saturating_sub(1);
                    let page = page.unwrap_or(last).min(last);
                    let entries = page_entries(&catalog, page).to_vec();
                    let images = if entries == previous && page == previous_page {
                        None
                    } else {
                        Some(
                            entries
                                .into_iter()
                                .map(|item| {
                                    let image = (|| -> anyhow::Result<_> {
                                        let mut image = wallpapers::decode_favorite(
                                            wallpapers::Request {
                                                device_id: "favorite".into(),
                                                url: format!("favorite:{}", item.id),
                                            },
                                            store.bytes(&item)?,
                                        )?;
                                        image.encoded = Vec::new();
                                        Ok(image)
                                    })()
                                    .map_err(|e| format!("{e:#}"));
                                    (item, image)
                                })
                                .collect(),
                        )
                    };
                    Ok(Loaded {
                        catalog,
                        page,
                        follow_latest: page == last,
                        images,
                        notice,
                    })
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                context.request_repaint();
            });
        if let Err(error) = spawned {
            self.worker = None;
            self.loaded = true;
            self.scanned_at = Some(Instant::now());
            self.notice.error(
                Scope::Wallpaper,
                uu_i18n::format!("壁纸收藏操作失败：{error}"),
            );
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        let result = self.worker.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("wallpaper favorites worker stopped".into()))
            }
        });
        if let Some(result) = result {
            self.worker = None;
            self.loaded = true;
            self.scanned_at = Some(Instant::now());
            match result {
                Ok(loaded) => {
                    self.catalog = loaded.catalog;
                    self.page = loaded.page;
                    self.follow_latest = loaded.follow_latest;
                    if let Some(images) = loaded.images {
                        self.items = images
                            .into_iter()
                            .map(|(entry, image)| {
                                let (texture, error) = match image {
                                    Ok(image) => (
                                        Some(ctx.load_texture(
                                            format!("favorite-{}", entry.id),
                                            egui::ColorImage::from_rgba_unmultiplied(
                                                [image.width as usize, image.height as usize],
                                                &image.rgba,
                                            ),
                                            egui::TextureOptions::LINEAR,
                                        )),
                                        None,
                                    ),
                                    Err(error) => (None, Some(error)),
                                };
                                Item {
                                    id: entry.id,
                                    filename: entry.file,
                                    texture,
                                    error,
                                }
                            })
                            .collect();
                    }
                    match loaded.notice {
                        Some(Notice::Saved(true)) => self
                            .notice
                            .success(Scope::Wallpaper, uu_i18n::text("已收藏当前壁纸")),
                        Some(Notice::Saved(false)) => self.notice.report(
                            Scope::Wallpaper,
                            Kind::Info,
                            uu_i18n::text("这张壁纸已在收藏中"),
                        ),
                        Some(Notice::Deleted) => self
                            .notice
                            .success(Scope::Wallpaper, uu_i18n::text("已删除收藏壁纸")),
                        None if self
                            .notice
                            .current(&Scope::Wallpaper)
                            .is_some_and(|m| m.kind == Kind::Progress) =>
                        {
                            self.notice.clear()
                        }
                        None => {}
                    }
                }
                Err(error) => self.notice.error(
                    Scope::Wallpaper,
                    uu_i18n::format!("壁纸收藏操作失败：{error}"),
                ),
            }
            if let Some(job) = self.pending.pop_front() {
                self.start(job, ctx);
            }
        }
        self.notice.expire(Instant::now());
        self.notice.tick(ctx);
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    #[cfg(test)]
    pub fn at(root: PathBuf) -> Self {
        Self {
            store: Ok(Store { root }),
            ..Default::default()
        }
    }
}

// Number from oldest to newest, present each page newest first. Appending a
// favorite cannot move older photos into another numbered page.
fn page_entries(catalog: &[Entry], page: usize) -> &[Entry] {
    let end = catalog.len().saturating_sub(page.saturating_mul(PAGE_SIZE));
    &catalog[end.saturating_sub(PAGE_SIZE)..end]
}

#[cfg(test)]
mod tests;
