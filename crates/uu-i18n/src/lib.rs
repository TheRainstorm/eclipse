//! Application language is independent of connection profiles and user content.
extern crate self as uu_i18n;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
pub use uu_i18n_macros::format;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    Chinese,
    English,
}
impl Language {
    pub const ALL: [Self; 2] = [Self::Chinese, Self::English];
    pub fn label(self) -> &'static str {
        match self {
            Self::Chinese => "简体中文",
            Self::English => "English",
        }
    }
}
static LANGUAGE: AtomicU8 = AtomicU8::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);
thread_local! { static OVERRIDE: std::cell::Cell<Option<Language>> = const { std::cell::Cell::new(None) }; }
pub fn language() -> Language {
    OVERRIDE.with(|v| v.get()).unwrap_or_else(|| {
        if LANGUAGE.load(Ordering::Relaxed) == 0 {
            Language::Chinese
        } else {
            Language::English
        }
    })
}
pub fn set_language(value: Language) {
    if LANGUAGE.swap(u8::from(value == Language::English), Ordering::Relaxed)
        != u8::from(value == Language::English)
    {
        EPOCH.fetch_add(1, Ordering::Relaxed);
    }
}
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::Relaxed)
}
pub fn is_english() -> bool {
    language() == Language::English
}
/// Temporary language on this thread, primarily for previews and isolated tests.
/// Restores the previous language even if the callback unwinds.
pub fn with_language<T>(value: Language, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Language>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|v| v.replace(Some(value))));
    f()
}
/// Translate a known static UI label. Never pass names or remote/user text here.
pub fn text(source: &str) -> &str {
    if is_english() {
        catalogue::english(source).or_else(||extensions().read().unwrap().get(source).copied()).unwrap_or(source)
    } else {
        source
    }
}
fn extensions()->&'static std::sync::RwLock<std::collections::BTreeMap<&'static str,&'static str>> {
    static VALUES:std::sync::OnceLock<std::sync::RwLock<std::collections::BTreeMap<&'static str,&'static str>>>=std::sync::OnceLock::new();
    VALUES.get_or_init(Default::default)
}
/// Register immutable extension-owned vocabulary for this process lifetime.
/// The borrowed strings live in a library retained by the plugin loader.
pub fn register_catalogue(values:&'static [(&'static str,&'static str)])->Result<(),&'static str> {
    if values.len()>4096{return Err("extension catalogue exceeds limit");}
    let mut target=extensions().write().unwrap();
    for (source,english) in values {
        if source.len()>16384||english.len()>16384||target.get(source).copied().or_else(||catalogue::english(source)).is_some_and(|old|old!=*english){return Err("conflicting extension translation");}
    }
    target.extend(values.iter().copied());Ok(())
}
/// Match stored status text independently of the current UI language.
pub fn matches(value: &str, source: &str) -> bool {
    value == source || catalogue::english(source).or_else(||extensions().read().unwrap().get(source).copied()).is_some_and(|v| v == value)
}
pub fn starts_with(value: &str, source: &str) -> bool {
    value.starts_with(source) || catalogue::english(source).or_else(||extensions().read().unwrap().get(source).copied()).is_some_and(|v| value.starts_with(v))
}
mod catalogue;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_and_user_content_are_preserved() {
        with_language(Language::Chinese, || {
            assert_eq!(text("设置"), "设置");
            with_language(Language::English, || {
                assert_eq!(text("设置"), "Settings");
                assert_eq!(text("设备由用户取名"), "设备由用户取名");
            });
            assert_eq!(language(), Language::Chinese);
        });
    }
    #[test]
    fn templates_keep_named_capture_and_arguments() {
        let fps = 165;
        assert_eq!(
            with_language(Language::English, || crate::format!("{fps} 帧")),
            "165 FPS"
        );
        assert_eq!(
            with_language(Language::Chinese, || crate::format!("{fps} 帧")),
            "165 帧"
        );
        assert_eq!(
            with_language(Language::English, || crate::format!("{:.1} 秒前", 2.25)),
            "2.2 seconds ago"
        );
    }
    #[test]
    fn language_wire_values_are_stable() {
        assert_eq!(
            serde_json::to_string(&Language::English).unwrap(),
            "\"english\""
        );
        assert_eq!(
            serde_json::from_str::<Language>("\"chinese\"").unwrap(),
            Language::Chinese
        );
    }
    #[test]
    fn status_comparisons_survive_a_language_switch() {
        assert!(matches("已连接", "已连接"));
        assert!(matches(
            "Connected",
            "已连接"
        ));
        assert!(starts_with("Sent: 60 FPS", "已发送"));
        assert!(!matches("user device name", "已请求目标画质/码率"));
    }
}
