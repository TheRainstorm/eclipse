//! Optional bundled rounded Latin font; preserves host CJK/IME font fallbacks.
use egui::{FontData, FontDefinitions, FontFamily};
pub const FAMILY: &str = "animal-island.rounded";
fn installed() -> egui::Id {
    egui::Id::new("animal-island.fonts.installed.v1")
}
pub fn family() -> FontFamily {
    FontFamily::Name(FAMILY.into())
}
/// Register a named family without changing the host's proportional family.
pub fn register(definitions: &mut FontDefinitions) {
    let mut fallback = definitions
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    definitions.font_data.insert(
        FAMILY.into(),
        FontData::from_static(include_bytes!("../assets/IslandRounded-SemiBold.ttf")),
    );
    fallback.retain(|name| name != FAMILY);
    fallback.insert(0, FAMILY.into());
    definitions.families.insert(family(), fallback);
}
/// The application can supply its complete font set once, before any frame.
pub fn install(ctx: &egui::Context, mut definitions: FontDefinitions) {
    register(&mut definitions);
    ctx.set_fonts(definitions);
    ctx.data_mut(|d| d.insert_temp(installed(), true));
}
/// Standalone/demo contexts can use the bundled font with egui defaults.
/// Host contexts should call `install` with their existing CJK fallbacks first.
pub fn ensure(ctx: &egui::Context) {
    if !ctx.data(|d| d.get_temp::<bool>(installed()).unwrap_or(false)) {
        install(ctx, FontDefinitions::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_preserves_host_families_and_is_idempotent() {
        let mut definitions = FontDefinitions::default();
        definitions
            .families
            .get_mut(&FontFamily::Proportional)
            .unwrap()
            .insert(0, "host-cjk".into());
        let host = definitions.families[&FontFamily::Proportional].clone();
        register(&mut definitions);
        register(&mut definitions);
        assert_eq!(definitions.families[&FontFamily::Proportional], host);
        let names = &definitions.families[&family()];
        assert_eq!(names[0], FAMILY);
        assert_eq!(&names[1..], host.as_slice());
        assert_eq!(
            names.iter().filter(|name| name.as_str() == FAMILY).count(),
            1
        );
    }
}
