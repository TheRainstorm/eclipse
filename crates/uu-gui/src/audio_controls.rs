//! All mute controls share the current stream's state without changing volume.
use std::sync::atomic::Ordering::Relaxed;

pub(super) fn volume_slider(ui: &mut eframe::egui::Ui, volume: &mut u32) -> bool {
    use eframe::egui;
    let before = *volume;
    ui.label("音量");
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 72.0).clamp(80.0, 240.0);
        ui.add(egui::Slider::new(volume, 0..=100).show_value(false));
        let id = ui.make_persistent_id("volume-percent");
        let mut text = if ui.memory(|m| m.has_focus(id)) {
            ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| volume.to_string())
        } else { volume.to_string() };
        let edit = ui.add(egui::TextEdit::singleline(&mut text).id(id).desired_width(36.0).char_limit(3));
        if edit.changed() {
            if let Ok(value) = text.parse::<u32>() { *volume = value.min(100); }
        }
        if edit.lost_focus() { text = volume.to_string(); }
        ui.data_mut(|d| d.insert_temp(id, text));
        ui.label("%");
    });
    before != *volume
}

impl super::App {
    fn audio_muted(&self) -> bool {
        self.controller
            .as_ref()
            .map(|controller| controller.stats.audio.muted.load(Relaxed))
            .unwrap_or(self.preferences.muted)
    }

    pub(super) fn toggle_audio_mute(&mut self) {
        if !self.supports_remote(uu_core::capabilities::Capability::Audio){return;}
        let muted = !self.audio_muted();
        self.preferences.muted = muted;
        self.settings_draft.muted = muted;
        if let Some(controller) = &self.controller {
            controller.stats.audio.muted.store(muted, Relaxed);
        }
        self.audio_save_after =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
    }

    pub(super) fn audio_button(&mut self, ui: &mut eframe::egui::Ui) {
        if !self.supports_remote(uu_core::capabilities::Capability::Audio){return;}
        let muted = self.audio_muted();
        let icon = if muted {
            super::ui::Icon::Muted
        } else {
            super::ui::Icon::Volume
        };
        let tip = if muted {
            "启用声音 · Ctrl+Alt+M"
        } else {
            "静音 · Ctrl+Alt+M"
        };
        if super::ui::icon_button_selected(ui, icon, tip, muted).clicked() {
            self.toggle_audio_mute();
        }
    }
}
