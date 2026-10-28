//! Font atlases share egui's TextureId(0), but their pixels belong to a DPI scale.
//! The painter owns every returned texture, including evictions and drains.
use egui::{epaint::FontImage, TextureOptions};

const CAPACITY: usize = 4;

struct Atlas<T> {
    scale: u32,
    texture: T,
    image: Option<FontImage>,
    options: TextureOptions,
}

pub(super) struct FontAtlases<T> {
    // Least recently used first. Four scales bound retained CPU/GPU images.
    entries: Vec<Atlas<T>>,
    active: Option<u32>,
}

impl<T: Copy> FontAtlases<T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            active: None,
        }
    }

    pub fn select(&mut self, pixels_per_point: f32) {
        let scale = pixels_per_point.to_bits();
        self.active = None;
        if let Some(index) = self.entries.iter().position(|e| e.scale == scale) {
            let atlas = self.entries.remove(index);
            self.entries.push(atlas);
            self.active = Some(scale);
        }
    }

    pub fn texture(&self) -> Option<T> {
        self.active
            .and_then(|scale| self.entries.iter().find(|e| e.scale == scale))
            .map(|e| e.texture)
    }

    pub fn unchanged(&self, image: &FontImage, options: TextureOptions) -> bool {
        self.active
            .and_then(|scale| self.entries.iter().find(|e| e.scale == scale))
            .is_some_and(|e| e.options == options && e.image.as_ref() == Some(image))
    }

    /// Prove that a pending delta cannot change a retained UI image. Inspect
    /// the requested scale, which can differ from the painter's active scale
    /// after another viewport painted. Unknown/partial updates stay dirty.
    pub fn delta_is_unchanged(&self, pixels_per_point: f32, delta: &egui::TexturesDelta) -> bool {
        delta.free.is_empty()
            && delta.set.iter().all(|(id, update)| {
                if *id != egui::TextureId::default() || update.pos.is_some() {
                    return false;
                }
                let egui::ImageData::Font(image) = &update.image else {
                    return false;
                };
                self.entries
                    .iter()
                    .find(|e| e.scale == pixels_per_point.to_bits())
                    .is_some_and(|e| e.options == update.options && e.image.as_ref() == Some(image))
            })
    }

    /// Record only after the upload. Partial glyph updates also update the CPU
    /// shadow, so the next upstream full atlas can be recognized as unchanged.
    pub fn uploaded(
        &mut self,
        pixels_per_point: f32,
        texture: T,
        image: &FontImage,
        pos: Option<[usize; 2]>,
        options: TextureOptions,
    ) -> Option<T> {
        let scale = pixels_per_point.to_bits();
        if let Some(entry) = self.entries.iter_mut().find(|e| e.scale == scale) {
            entry.options = options;
            if let Some([x, y]) = pos {
                if let Some(full) = entry.image.as_mut().filter(|full| {
                    x.checked_add(image.width())
                        .is_some_and(|end| end <= full.width())
                        && y.checked_add(image.height())
                            .is_some_and(|end| end <= full.height())
                }) {
                    for row in 0..image.height() {
                        let from = row * image.width();
                        let to = (y + row) * full.width() + x;
                        full.pixels[to..to + image.width()]
                            .copy_from_slice(&image.pixels[from..from + image.width()]);
                    }
                } else {
                    // Preserve the renderer's handling of invalid GL updates,
                    // but never suppress a later full upload using stale data.
                    entry.image = None;
                }
            } else {
                entry.image = Some(image.clone());
            }
            self.active = Some(scale);
            return None;
        }

        debug_assert!(pos.is_none(), "a new font atlas needs a full image");
        let evicted = if self.entries.len() == CAPACITY {
            Some(self.entries.remove(0).texture)
        } else {
            None
        };
        self.entries.push(Atlas {
            scale,
            texture,
            image: Some(image.clone()),
            options,
        });
        self.active = Some(scale);
        evicted
    }

    pub fn textures(&self) -> impl Iterator<Item = T> + '_ {
        self.entries.iter().map(|e| e.texture)
    }

    pub fn drain(&mut self) -> Vec<T> {
        self.active = None;
        self.entries.drain(..).map(|e| e.texture).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const LINEAR: TextureOptions = TextureOptions::LINEAR;

    #[test]
    fn independent_scales_reuse_their_own_atlases() {
        let mut cache = FontAtlases::new();
        let low = FontImage::new([4, 4]);
        let mut high = FontImage::new([8, 8]);
        high.pixels[0] = 1.;
        assert_eq!(cache.uploaded(1., 10, &low, None, LINEAR), None);
        assert_eq!(cache.uploaded(1.5, 20, &high, None, LINEAR), None);
        for _ in 0..10 {
            cache.select(1.);
            assert_eq!(cache.texture(), Some(10));
            assert!(cache.unchanged(&low, LINEAR));
            cache.select(1.5);
            assert_eq!(cache.texture(), Some(20));
            assert!(cache.unchanged(&high, LINEAR));
        }
        cache.select(2.);
        assert_eq!(cache.texture(), None);
    }

    #[test]
    fn glyph_patch_font_replacement_and_options_invalidate_only_affected_scale() {
        let mut cache = FontAtlases::new();
        let mut full = FontImage::new([4, 3]);
        cache.uploaded(1., 10, &full, None, LINEAR);
        cache.uploaded(1.5, 20, &full, None, LINEAR);
        let patch = FontImage {
            size: [2, 1],
            pixels: vec![1., 0.5],
        };
        cache.select(1.);
        cache.uploaded(1., 10, &patch, Some([1, 2]), LINEAR);
        full.pixels[9..11].copy_from_slice(&patch.pixels);
        assert!(cache.unchanged(&full, LINEAR));
        assert!(!cache.unchanged(&full, TextureOptions::NEAREST));
        cache.select(1.5);
        assert!(!cache.unchanged(&full, LINEAR));
        assert!(cache.unchanged(&FontImage::new([4, 3]), LINEAR));
        let replacement = FontImage::new([2, 2]);
        assert!(!cache.unchanged(&replacement, LINEAR));
        cache.uploaded(1.5, 20, &replacement, None, LINEAR);
        assert!(cache.unchanged(&replacement, LINEAR));
    }

    #[test]
    fn eviction_and_free_return_each_owned_texture_once() {
        let mut cache = FontAtlases::new();
        let image = FontImage::new([2, 2]);
        for i in 1..=CAPACITY {
            assert_eq!(cache.uploaded(i as f32, i, &image, None, LINEAR), None);
        }
        cache.select(1.);
        assert_eq!(cache.uploaded(5., 5, &image, None, LINEAR), Some(2));
        assert_eq!(cache.drain(), vec![3, 4, 1, 5]);
        assert_eq!(cache.texture(), None);
        assert!(cache.drain().is_empty());
    }

    #[test]
    fn invalid_patch_cannot_hide_a_later_full_upload() {
        let mut cache = FontAtlases::new();
        let image = FontImage::new([2, 2]);
        cache.uploaded(1., 10, &image, None, LINEAR);
        cache.uploaded(1., 10, &image, Some([2, 2]), LINEAR);
        assert!(!cache.unchanged(&image, LINEAR));
    }

    #[test]
    fn repeated_full_font_delta_uses_requested_scale_without_changing_active_atlas() {
        let mut cache = FontAtlases::new();
        let low = FontImage::new([4, 3]);
        let mut high = FontImage::new([4, 3]);
        high.pixels[0] = 1.;
        cache.uploaded(1., 10, &low, None, LINEAR);
        cache.uploaded(1.5, 20, &high, None, LINEAR);
        let mut delta = egui::TexturesDelta::default();
        delta.set.push((
            egui::TextureId::default(),
            egui::epaint::ImageDelta::full(low, LINEAR),
        ));
        assert!(cache.delta_is_unchanged(1., &delta));
        assert!(!cache.delta_is_unchanged(1.5, &delta));
        assert!(!cache.delta_is_unchanged(2., &delta));
        assert_eq!(cache.texture(), Some(20));
        delta.set[0].1.options = TextureOptions::NEAREST;
        assert!(!cache.delta_is_unchanged(1., &delta));
    }

    #[test]
    fn ui_reuse_requires_every_update_to_be_proven_unchanged() {
        let mut cache = FontAtlases::new();
        let image = FontImage::new([4, 3]);
        cache.uploaded(1., 10, &image, None, LINEAR);
        let mut delta = egui::TexturesDelta::default();
        assert!(cache.delta_is_unchanged(1., &delta));
        let full = egui::epaint::ImageDelta::full(image.clone(), LINEAR);
        delta.set.push((egui::TextureId::default(), full.clone()));
        assert!(cache.delta_is_unchanged(1., &delta));
        delta.set.push((egui::TextureId::Managed(1), full));
        assert!(!cache.delta_is_unchanged(1., &delta));
        delta.set.pop();
        delta.free.push(egui::TextureId::default());
        assert!(!cache.delta_is_unchanged(1., &delta));
        delta.free.clear();
        delta.set[0].1.pos = Some([0, 0]);
        assert!(!cache.delta_is_unchanged(1., &delta));
        delta.set[0].1 = egui::epaint::ImageDelta::full(
            egui::ColorImage::new([4, 3], egui::Color32::WHITE),
            LINEAR,
        );
        assert!(!cache.delta_is_unchanged(1., &delta));
        delta.set[0].1 = egui::epaint::ImageDelta::full(image, LINEAR);
        cache.drain();
        assert!(!cache.delta_is_unchanged(1., &delta));
    }
}
