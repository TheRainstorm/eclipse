//! Optional wallpaper sources; the application owns image decoding and caches.
mod config;
mod history;
mod wallhaven;

use anyhow::{ensure, Context as _, Result};
use config::Config;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use uu_plugin_sdk::engine::plugins::PluginPreferences;
use uu_plugin_sdk::{egui, wallpaper, Action, Context, Page, PageState, Placement, Plugin};

const ANIME_URL: &str = "https://img-api.pipw.top";
struct Sources {
    enabled: AtomicBool,
    config: Mutex<Config>,
    history: Mutex<wallhaven::State>,
    storage: uu_plugin_sdk::engine::Config,
}
impl wallpaper::Provider for Sources {
    fn sources(&self) -> Vec<wallpaper::Source> {
        vec![
            wallpaper::Source {
                id: "wallhaven".into(),
                name: "Wallhaven".into(),
                revision: self.config.lock().unwrap().revision(),
            },
            wallpaper::Source {
                id: "anime".into(),
                name: uu_plugin_sdk::i18n::text("二次元").into(),
                revision: "1".into(),
            },
        ]
    }
    fn resolve<'a>(
        &'a self,
        client: wallpaper::Client,
        source: &'a str,
    ) -> wallpaper::Resolution<'a> {
        Box::pin(async move {
            ensure!(
                self.enabled.load(Ordering::Acquire),
                "wallpaper plugin is disabled"
            );
            match source {
                "anime" => Ok(ANIME_URL.into()),
                "wallhaven" => {
                    let config = self.config.lock().unwrap().clone();
                    wallhaven::resolve(client, config, &self.storage, &self.history).await
                }
                _ => anyhow::bail!("unknown wallpaper source"),
            }
        })
    }
}
struct Extension {
    context: Context,
    sources: Arc<Sources>,
    previous: Config,
}
fn options(preferences: &PluginPreferences, previous: &Config) -> Result<Config> {
    let config = preferences
        .options
        .get("wallhaven")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_else(|| previous.clone());
    Config::validate(&config)?;
    Ok(config)
}
fn create(context: Context) -> Result<Box<dyn Plugin>> {
    ensure!(
        context.id == "wallpapers",
        "wallpaper plugin manifest ID must be wallpapers"
    );
    let previous = Config::previous(&context.application);
    let sources = Arc::new(Sources {
        enabled: AtomicBool::new(false),
        config: Mutex::new(Config::default()),
        history: Mutex::new(wallhaven::State::new(
            history::History::read(&context.data.at("wallhaven-history.json")).unwrap_or_default(),
        )),
        storage: context.data.clone(),
    });
    Ok(Box::new(Extension {
        context,
        sources,
        previous,
    }))
}
#[no_mangle]
pub extern "C" fn eclipse_plugin_entry_v1() -> *const uu_plugin_sdk::Entry {
    static ENTRY: uu_plugin_sdk::Entry = uu_plugin_sdk::entry(create);
    &ENTRY
}
impl Plugin for Extension {
    fn configure(&self, preferences: &PluginPreferences) -> Result<()> {
        *self.sources.config.lock().unwrap() = options(preferences, &self.previous)?;
        Ok(())
    }
    fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.sources.enabled.store(enabled, Ordering::Release);
        Ok(())
    }
    fn wallpaper_provider(&self) -> Option<Arc<dyn wallpaper::Provider>> {
        Some(self.sources.clone())
    }
    fn pages(&self) -> Vec<Page> {
        vec![Page {
            id: "wallpapers".into(),
            title: "Wallhaven".into(),
            placement: Placement::Settings,
            icon: None,
        }]
    }
    fn gui(&self) -> Result<Box<dyn PageState>> {
        let preferences = self.context.preferences()?;
        Ok(Box::new(Editor {
            context: self.context.clone(),
            draft: options(&preferences, &self.previous)?,
            expected: preferences,
            notice: None,
        }))
    }
}

struct Editor {
    context: Context,
    draft: Config,
    expected: PluginPreferences,
    notice: Option<(std::time::Instant, String)>,
}
impl PageState for Editor {
    fn show(&mut self, ui: &mut egui::Ui, _: &str) -> Vec<Action> {
        use uu_plugin_sdk::{i18n::text, theme};
        let mut actions = Vec::new();
        ui.strong("Wallhaven");
        ui.horizontal(|ui| {
            ui.label(text("页面"));
            theme::Select::from_id_salt("wallhaven-page")
                .selected_text(text(self.draft.page.label()))
                .show_ui(ui, |ui| {
                    for page in config::Page::ALL {
                        theme::selectable_value(ui, &mut self.draft.page, page, text(page.label()));
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
            for (index, label) in ["综合", "动漫", "人物"].into_iter().enumerate() {
                theme::switch(ui, &mut self.draft.categories[index], text(label));
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (index, label) in ["SFW", "Sketchy"].into_iter().enumerate() {
                theme::switch(ui, &mut self.draft.purity[index], text(label));
            }
        });
        let valid = self.draft.validate();
        if let Err(error) = &valid {
            ui.colored_label(theme::palette(ui.visuals()).warning, error.to_string());
        }
        if ui
            .add_enabled(valid.is_ok(), theme::primary_button(text("保存")))
            .clicked()
        {
            let result = (|| -> Result<()> {
                let mut value = self.expected.clone();
                value
                    .options
                    .insert("wallhaven".into(), serde_json::to_value(&self.draft)?);
                self.context
                    .save_preferences(&self.expected, value.clone())
                    .context("cannot save wallpaper settings")?;
                self.expected = value;
                Ok(())
            })();
            self.notice = Some((
                std::time::Instant::now(),
                match result {
                    Ok(()) => {
                        actions.push(Action::PreferencesChanged);
                        text("已保存").into()
                    }
                    Err(e) => e.to_string(),
                },
            ));
        }
        if let Some((at, message)) = &self.notice {
            if at.elapsed() < std::time::Duration::from_secs(3) {
                ui.label(message);
                ui.ctx().request_repaint_after(
                    std::time::Duration::from_secs(3).saturating_sub(at.elapsed()),
                );
            }
        }
        actions
    }
}
