//! All mute controls share the current stream's state without changing volume.
use std::sync::atomic::Ordering::Relaxed;

pub(super) fn volume_slider(ui: &mut eframe::egui::Ui, volume: &mut u32) -> bool {
    use eframe::egui;
    let before = *volume;
    egui::Frame::none().fill(super::theme::CANVAS).rounding(8.).inner_margin(10.).show(ui,|ui|{
        ui.spacing_mut().interact_size.y=22.;
        ui.spacing_mut().slider_rail_height=4.;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(uu_i18n::text("音量")).size(13.).color(super::theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                ui.add(egui::DragValue::new(volume).range(0..=100).suffix(" %").speed(0.5));
            });
        });
        ui.add_space(4.);
        ui.spacing_mut().slider_width=ui.available_width().max(1.);
        ui.visuals_mut().selection.bg_fill=super::theme::ACCENT;
        ui.visuals_mut().widgets.inactive.bg_fill=egui::Color32::WHITE;
        ui.visuals_mut().widgets.inactive.bg_stroke=egui::Stroke::new(1.5,super::theme::ACCENT);
        ui.visuals_mut().widgets.inactive.fg_stroke=egui::Stroke::new(1.5,super::theme::ACCENT);
        ui.visuals_mut().widgets.hovered.bg_fill=egui::Color32::WHITE;
        ui.visuals_mut().widgets.hovered.bg_stroke=egui::Stroke::new(2.,super::theme::ACCENT);
        ui.visuals_mut().widgets.hovered.fg_stroke=egui::Stroke::new(2.,super::theme::ACCENT);
        ui.visuals_mut().widgets.active=ui.visuals().widgets.hovered;
        ui.add(egui::Slider::new(volume,0..=100).show_value(false).trailing_fill(true));
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
            uu_i18n::text("启用声音")
        } else {
            uu_i18n::text("静音")
        };
        let tip=format!("{tip} · {}",self.preferences.hotkeys.label(uu_core::hotkeys::Action::Mute));
        if super::ui::icon_button_selected(ui, icon, &tip, muted).clicked() {
            self.toggle_audio_mute();
        }
    }
}
