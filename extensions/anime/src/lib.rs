//! Independently enabled anime wallpaper provider.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use uu_plugin_sdk::{
    anyhow::{ensure, Result},
    wallpaper, Context, Plugin,
};
struct Source {
    enabled: AtomicBool,
}
impl wallpaper::Provider for Source {
    fn sources(&self) -> Vec<wallpaper::Source> {
        vec![wallpaper::Source {
            id: "anime".into(),
            name: "二次元壁纸（Pipw）".into(),
            revision: "1".into(),
        }]
    }
    fn resolve<'a>(&'a self, _: wallpaper::Client, source: &'a str) -> wallpaper::Resolution<'a> {
        Box::pin(async move {
            ensure!(
                self.enabled.load(Ordering::Acquire),
                "anime wallpaper plugin is disabled"
            );
            ensure!(source == "anime", "unknown anime wallpaper source");
            Ok("https://img-api.pipw.top".into())
        })
    }
}
struct Extension(Arc<Source>);
impl Plugin for Extension {
    fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.0.enabled.store(enabled, Ordering::Release);
        Ok(())
    }
    fn wallpaper_provider(&self) -> Option<Arc<dyn wallpaper::Provider>> {
        Some(self.0.clone())
    }
}
fn create(context: Context) -> Result<Box<dyn Plugin>> {
    ensure!(
        context.id == "wallpaper-pipw",
        "anime plugin manifest ID must be wallpaper-pipw"
    );
    Ok(Box::new(Extension(Arc::new(Source {
        enabled: AtomicBool::new(false),
    }))))
}
#[no_mangle]
pub extern "C" fn eclipse_plugin_entry_v1() -> *const uu_plugin_sdk::Entry {
    static ENTRY: uu_plugin_sdk::Entry = uu_plugin_sdk::entry(create);
    &ENTRY
}
