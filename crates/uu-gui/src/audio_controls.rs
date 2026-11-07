//! All mute controls share the current stream's state without changing volume.
use std::sync::atomic::Ordering::Relaxed;
use std::{cell::{Cell,RefCell},sync::{Arc,Weak}};

#[derive(Clone,Copy)]
pub(super) struct Playback {pub muted:bool,pub volume:u32}
pub(super) struct Group {
    pub playback:Cell<Playback>,
    pub owner:Cell<Option<u64>>,
    pub stats:RefCell<Weak<uu_core::media::MediaStats>>,
}
impl Group {
    pub fn new(playback:Playback)->Self {Self{playback:Cell::new(playback),owner:Cell::new(None),stats:RefCell::new(Weak::new())}}
}

pub(super) fn volume_slider(ui: &mut eframe::egui::Ui, volume: &mut u32) -> bool {
    use eframe::egui;
    let before = *volume;
    let palette=*super::theme::palette(ui.visuals());
    let width=ui.available_width();
    egui::Frame::none().fill(palette.base).rounding(8.).inner_margin(10.).show(ui,|ui|{
        // Frame margins are outside the child UI's minimum width. Bound the
        // content explicitly before either the header or rail can expand it.
        let content_width=(width-20.).max(1.);
        ui.set_width(content_width);
        ui.spacing_mut().interact_size.y=22.;
        ui.spacing_mut().slider_rail_height=4.;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(uu_i18n::text("音量")).size(13.).color(palette.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                super::theme::number_input(ui,egui::DragValue::new(volume).range(0..=100).suffix(" %").speed(0.5));
            });
        });
        ui.add_space(4.);
        ui.spacing_mut().slider_width=content_width;
        ui.visuals_mut().selection.bg_fill=palette.accent;
        ui.visuals_mut().widgets.inactive.bg_fill=palette.control;
        ui.visuals_mut().widgets.inactive.bg_stroke=egui::Stroke::new(1.5,palette.accent);
        ui.visuals_mut().widgets.inactive.fg_stroke=egui::Stroke::new(1.5,palette.accent);
        ui.visuals_mut().widgets.hovered.bg_fill=palette.control;
        ui.visuals_mut().widgets.hovered.bg_stroke=egui::Stroke::new(2.,palette.accent);
        ui.visuals_mut().widgets.hovered.fg_stroke=egui::Stroke::new(2.,palette.accent);
        ui.visuals_mut().widgets.active.bg_fill=palette.pressed;
        ui.visuals_mut().widgets.active.weak_bg_fill=palette.pressed;
        ui.visuals_mut().widgets.active.bg_stroke=egui::Stroke::new(1.5,palette.accent);
        ui.visuals_mut().widgets.active.fg_stroke=egui::Stroke::new(1.5,palette.accent);
        crate::theme::slider(ui,egui::Slider::new(volume,0..=100).show_value(false).trailing_fill(true));
    });
    before != *volume
}

impl super::App {
    fn audio_group(&self)->Option<std::rc::Rc<Group>> {
        self.workspace_tabs.as_ref()?.audio_group(self.stream_target.as_ref()?)
    }
    pub(super) fn audio_stats(&self)->Option<Arc<uu_core::media::MediaStats>> {
        self.audio_group().and_then(|group|group.stats.borrow().upgrade())
            .or_else(||self.controller.as_ref().map(|h|h.stats.clone()))
    }
    pub(super) fn audio_playback(&self)->Playback {
        if let Some(group)=self.audio_group() {return group.playback.get();}
        self.audio_stats().map(|stats|Playback {muted:stats.audio.muted.load(Relaxed),volume:stats.audio.volume.load(Relaxed)})
            .unwrap_or(Playback{muted:self.preferences.muted,volume:self.preferences.volume})
    }
    pub(super) fn set_audio_playback(&mut self,muted:bool,volume:u32) {
        if let Some(group)=self.audio_group() {group.playback.set(Playback{muted,volume});}
        if let Some(stats)=self.audio_stats() {stats.audio.muted.store(muted,Relaxed);stats.audio.volume.store(volume,Relaxed);}
        self.preferences.muted=muted;self.preferences.volume=volume;
        self.settings_draft.muted=muted;self.settings_draft.volume=volume;
        self.audio_save_after=Some(std::time::Instant::now()+std::time::Duration::from_millis(250));
    }
    fn audio_muted(&self) -> bool {
        self.audio_playback().muted
    }

    pub(super) fn toggle_audio_mute(&mut self) {
        if !self.audio_stats().is_some_and(|stats|stats.capabilities.allows(uu_core::capabilities::Capability::Audio)){return;}
        let muted = !self.audio_muted();
        self.set_audio_playback(muted,self.audio_playback().volume);
    }

    pub(super) fn audio_button(&mut self, ui: &mut eframe::egui::Ui) {
        if !self.audio_stats().is_some_and(|stats|stats.capabilities.allows(uu_core::capabilities::Capability::Audio)){return;}
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
