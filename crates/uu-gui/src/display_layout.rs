//! Local tab order and stable local numbers, independent of capture/OS layout.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uu_core::screen_control::RemoteScreen;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct DisplayLayout {
    numbers: Vec<i32>,
    order: Vec<i32>,
    last_selected: Option<i32>,
}

pub(super) struct DraggedDisplay(pub i32);

impl DisplayLayout {
    pub fn path(config: &uu_core::Config, device: &str) -> Option<PathBuf> {
        if device.is_empty() || device.len() > 96 {
            return None;
        }
        let key: String = device.bytes().map(|b| format!("{b:02x}")).collect();
        Some(config.at(format!("display-tabs/{key}.json")))
    }
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(data.len() <= 65536, "显示器排序文件过大");
        let mut layout: Self = serde_json::from_slice(&data)?;
        for ids in [&mut layout.numbers, &mut layout.order] {
            let mut seen = std::collections::HashSet::new();
            ids.retain(|id| *id >= 0 && seen.len() < 256 && seen.insert(*id));
        }
        if layout.last_selected.is_some_and(|id| id < 0) {layout.last_selected=None;}
        Ok(layout)
    }
    pub fn sync(&mut self, screens: &[RemoteScreen]) -> bool {
        let mut changed = false;
        for screen in screens {
            if !self.numbers.contains(&screen.id) {
                self.numbers.push(screen.id);
                changed = true;
            }
            if !self.order.contains(&screen.id) {
                self.order.push(screen.id);
                changed = true;
            }
        }
        changed
    }
    pub fn last_selected(&self) -> Option<i32> {self.last_selected}
    pub fn remember_selected(&mut self, id: i32) -> bool {
        if id < 0 || self.last_selected == Some(id) {return false;}
        self.last_selected = Some(id);
        true
    }
    pub fn number(&self, id: i32) -> Option<usize> {
        self.numbers.iter().position(|v| *v == id).map(|i| i + 1)
    }
    pub fn label(&self, screen: &RemoteScreen) -> String {
        let number = self
            .number(screen.id)
            .map(|n| format!(" {n}"))
            .unwrap_or_default();
        if screen.display_name.is_empty() {
            format!("显示屏{number}")
        } else {
            format!("显示屏{number}（{}）", screen.display_name)
        }
    }
    pub fn sort(&self, screens: &mut [RemoteScreen]) {
        screens.sort_by_key(|s| {
            self.order
                .iter()
                .position(|id| *id == s.id)
                .unwrap_or(usize::MAX)
        });
    }
    pub fn move_tab(&mut self, source: i32, target: i32, after: bool) -> bool {
        if source == target || !self.order.contains(&source) || !self.order.contains(&target) {
            return false;
        }
        let old = self.order.clone();
        self.order.retain(|id| *id != source);
        let index = self.order.iter().position(|id| *id == target).unwrap() + usize::from(after);
        self.order.insert(index, source);
        self.order != old
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        super::settings::save_json(self, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen(id: i32) -> RemoteScreen {
        RemoteScreen {
            id,
            refresh_hz: 60,
            kind: 0,
            width: 1920,
            height: 1080,
            dpi_scale: None,
            video_track_index: -1,
            display_name: format!("屏幕{id}"),
            primary: id == 7,
        }
    }
    #[test]
    fn drag_changes_order_without_changing_numbers_sources_or_primary() {
        let mut layout = DisplayLayout::default();
        let mut screens = vec![screen(7), screen(42), screen(11)];
        layout.sync(&screens);
        assert!(layout.move_tab(42, 7, false));
        layout.sort(&mut screens);
        assert_eq!(
            screens.iter().map(|s| s.id).collect::<Vec<_>>(),
            [42, 7, 11]
        );
        assert_eq!(layout.number(42), Some(2));
        assert!(screens[1].primary);
        assert!(!layout.move_tab(42, 42, true));
        assert!(layout.move_tab(42, 11, true));
        assert!(!layout.move_tab(999, 7, false));
        // Inventory order changes and temporary removal don't renumber a monitor.
        layout.sync(&[screen(11), screen(7)]);
        layout.sync(&[screen(42), screen(11), screen(7), screen(99)]);
        assert_eq!(layout.number(42), Some(2));
        assert_eq!(layout.number(99), Some(4));
    }
    #[test]
    fn saved_layout_restores_order_independently_of_global_preferences() {
        let path =
            std::env::temp_dir().join(format!("uu-display-layout-{}.json", std::process::id()));
        let mut layout = DisplayLayout::default();
        layout.sync(&[screen(7), screen(42)]);
        layout.move_tab(42, 7, false);
        assert!(layout.remember_selected(42));
        assert!(!layout.remember_selected(42));
        layout.save(&path).unwrap();
        let restored = DisplayLayout::load(&path).unwrap();
        assert_eq!(restored.order, [42, 7]);
        assert_eq!(restored.number(42), Some(2));
        assert_eq!(restored.last_selected(), Some(42));
        std::fs::remove_file(path).unwrap();
    }
}
