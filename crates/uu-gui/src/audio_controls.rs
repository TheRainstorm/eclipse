//! All mute controls share the current stream's state without changing volume.
use std::sync::atomic::Ordering::Relaxed;

impl super::App {
    fn audio_muted(&self) -> bool {
        self.controller
            .as_ref()
            .map(|controller| controller.stats.audio.muted.load(Relaxed))
            .unwrap_or(self.preferences.muted)
    }

    pub(super) fn toggle_audio_mute(&mut self) {
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
