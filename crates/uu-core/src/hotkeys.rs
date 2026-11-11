//! Shared client-command bindings for native routing and GUI key events.
//! These are foreground shortcuts, not process-global hotkey registrations.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Stats,
    Menu,
    Mute,
    Fullscreen,
    Exit,
    Capture,
    MouseMode,
    CursorMode,
    ToggleNested,
    ToggleLocal,
    PreviousDisplay,
    NextDisplay,
    PreviousComputer,
    NextComputer,
    Vsync,
    BossKey,
}
impl Action {
    pub const ALL: [Self; 16] = [
        Self::Stats,
        Self::Menu,
        Self::Mute,
        Self::Fullscreen,
        Self::Exit,
        Self::Capture,
        Self::MouseMode,
        Self::CursorMode,
        Self::ToggleNested,
        Self::ToggleLocal,
        Self::PreviousDisplay,
        Self::NextDisplay,
        Self::PreviousComputer,
        Self::NextComputer,
        Self::Vsync,
        Self::BossKey,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Stats => "HUD",
            Self::Vsync => uu_i18n::text("垂直同步"),
            Self::BossKey => uu_i18n::text("老板键：隐藏 / 恢复所有窗口"),
            Self::Menu => uu_i18n::text("悬浮菜单"),
            Self::Mute => uu_i18n::text("静音"),
            Self::Fullscreen => uu_i18n::text("全屏"),
            Self::Exit => uu_i18n::text("结束串流"),
            Self::Capture => uu_i18n::text("释放 / 恢复控制"),
            Self::MouseMode => uu_i18n::text("桌面 / 游戏鼠标"),
            Self::CursorMode => uu_i18n::text("光标显示"),
            Self::ToggleNested => uu_i18n::text("嵌套快捷键透传"),
            Self::ToggleLocal => uu_i18n::text("返回本机 / 恢复串流"),
            Self::PreviousDisplay => uu_i18n::text("上一个远端屏幕（全屏）"),
            Self::NextDisplay => uu_i18n::text("下一个远端屏幕（全屏）"),
            Self::PreviousComputer => uu_i18n::text("上一台串流电脑（全屏）"),
            Self::NextComputer => uu_i18n::text("下一台串流电脑（全屏）"),
        }
    }
    fn defaults(self) -> &'static [Chord] {
        const DEFAULTS: [&[Chord]; 16] = [
            &[Chord::ctrl_alt(0x53)],
            &[Chord::ctrl_alt(0x48)],
            &[Chord::ctrl_alt(0x4d)],
            &[Chord::ctrl_alt(0x46), Chord::ctrl_alt(0x58)],
            &[Chord::ctrl_alt(0x51)],
            &[Chord::ctrl_alt(0x5a)],
            &[Chord::ctrl_alt(0x50)],
            &[Chord::ctrl_alt(0x43)],
            &[Chord::ctrl_alt(0x7b)],
            &[Chord::ctrl_alt(0x44)],
            &[Chord::ctrl_alt(0x25)],
            &[Chord::ctrl_alt(0x27)],
            &[Chord::ctrl_alt(0x26)],
            &[Chord::ctrl_alt(0x28)],
            &[Chord::ctrl_alt(0x56)],
            &[Chord::ctrl_alt(0x42)],
        ];
        DEFAULTS[self as usize]
    }
    pub fn fullscreen_only(self) -> bool {
        matches!(self, Self::PreviousDisplay | Self::NextDisplay | Self::PreviousComputer | Self::NextComputer)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chord {
    pub key: u32,
    pub modifiers: Modifiers,
}
impl Chord {
    pub const fn ctrl_alt(key: u32) -> Self {
        Self {
            key,
            modifiers: Modifiers {
                ctrl: true,
                alt: true,
                shift: false,
                meta: false,
            },
        }
    }
    pub const fn ctrl_alt_shift(key:u32)->Self {
        let mut chord=Self::ctrl_alt(key);chord.modifiers.shift=true;chord
    }
    pub fn valid(self) -> bool {
        matches!(self.key,0x08|0x09|0x0d|0x1b|0x20..=0x28|0x2d..=0x2e|0x30..=0x39|0x41..=0x5a|0x70..=0x87|0xba..=0xc0|0xdb..=0xde)
    }
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (yes, name) in [
            (self.modifiers.ctrl, "Ctrl"),
            (self.modifiers.alt, "Alt"),
            (self.modifiers.shift, "Shift"),
            (self.modifiers.meta, "Win/Super"),
        ] {
            if yes {
                parts.push(name.to_owned());
            }
        }
        parts.push(match self.key {
            0x30..=0x39 | 0x41..=0x5a => char::from_u32(self.key).unwrap().to_string(),
            0x70..=0x87 => format!("F{}", self.key - 0x6f),
            8 => "Backspace".into(),
            9 => "Tab".into(),
            13 => "Enter".into(),
            27 => "Esc".into(),
            32 => "Space".into(),
            33 => "PageUp".into(),
            34 => "PageDown".into(),
            35 => "End".into(),
            36 => "Home".into(),
            37 => "Left".into(),
            38 => "Up".into(),
            39 => "Right".into(),
            40 => "Down".into(),
            45 => "Insert".into(),
            46 => "Delete".into(),
            0xba => ";".into(),
            0xbb => "=".into(),
            0xbc => ",".into(),
            0xbd => "-".into(),
            0xbe => ".".into(),
            0xbf => "/".into(),
            0xc0 => "`".into(),
            0xdb => "[".into(),
            0xdc => "\\".into(),
            0xdd => "]".into(),
            0xde => "'".into(),
            _ => format!("VK{:02X}", self.key),
        });
        parts.join("+")
    }
    pub fn warning(self) -> Option<&'static str> {
        let m = self.modifiers;
        if m.meta
            || (m.alt && self.key == 0x09)
            || (m.alt && self.key == 0x73)
            || (m.ctrl && self.key == 0x1b)
        {
            Some(uu_i18n::text("可能与系统快捷键冲突，是否可截获取决于平台与全屏捕获设置"))
        } else if m == Modifiers::default() {
            Some(uu_i18n::text("无修饰键；此键将在串流前台被客户端占用"))
        } else {
            None
        }
    }
}
/// Missing actions inherit defaults; an explicit empty list disables an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Bindings(BTreeMap<Action, Vec<Chord>>);
impl<'de> Deserialize<'de> for Bindings {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> {
        let mut bindings=BTreeMap::<Action,Vec<Chord>>::deserialize(deserializer)?;
        // Defaults may evolve. Explicit choices keep ownership of their keys;
        // inherit only the still-free defaults, preserving explicit conflicts
        // for the normal validation UI instead of discarding user settings.
        let explicit:std::collections::BTreeSet<_>=bindings.values().flatten().copied().collect();
        for action in Action::ALL {
            if !bindings.contains_key(&action) && action.defaults().iter().any(|chord|explicit.contains(chord)) {
                bindings.insert(action,action.defaults().iter().filter(|chord|!explicit.contains(chord)).copied().collect());
            }
        }
        Ok(Self(bindings))
    }
}
#[derive(Clone, Debug)]
pub struct Issue {
    pub actions: Vec<Action>,
    pub message: String,
}
impl Bindings {
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }
    pub fn chords(&self, action: Action) -> &[Chord] {
        self.0
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or_else(|| action.defaults())
    }
    pub fn set(&mut self, action: Action, chords: Vec<Chord>) {
        self.0.insert(action, chords);
    }
    pub fn reset(&mut self, action: Action) {
        self.0.remove(&action);
    }
    pub fn label(&self, action: Action) -> String {
        let cs = self.chords(action);
        if cs.is_empty() {
            "Unbound".into()
        } else {
            cs.iter().map(|c| c.label()).collect::<Vec<_>>().join(" / ")
        }
    }
    pub fn find(
        &self,
        key: u32,
        modifiers: Modifiers,
        nested: bool,
        streaming: bool,
        altgr: bool,
    ) -> Option<Action> {
        self.find_in_view(key, modifiers, nested, streaming, false, altgr)
    }
    pub fn find_in_view(
        &self, key: u32, modifiers: Modifiers, nested: bool, streaming: bool,
        fullscreen: bool, altgr: bool,
    ) -> Option<Action> {
        if altgr {
            return None;
        }
        let chord = Chord { key, modifiers };
        Action::ALL.into_iter().find(|a| {
            // Returning to the local desktop is an escape command and remains
            // local even while an explicit nested-passthrough mode is active.
            (!nested || matches!(*a, Action::ToggleNested | Action::ToggleLocal | Action::BossKey))
                && (streaming || matches!(*a, Action::Fullscreen | Action::BossKey))
                && (fullscreen || !a.fullscreen_only())
                && self.chords(*a).contains(&chord)
        })
    }
    pub fn issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut owners = BTreeMap::new();
        if self.chords(Action::ToggleNested).is_empty() {
            issues.push(Issue {
                actions: vec![Action::ToggleNested],
                message: uu_i18n::text("嵌套透传必须保留一个切回本机的快捷键").into(),
            });
        }
        for a in Action::ALL {
            if self.chords(a).len() > 3 {
                issues.push(Issue {
                    actions: vec![a],
                    message: uu_i18n::text("每项最多三个快捷键").into(),
                });
            }
            for c in self.chords(a) {
                if !c.valid() {
                    issues.push(Issue {
                        actions: vec![a],
                        message: uu_i18n::text("不是可用的普通按键").into(),
                    });
                }
                if cfg!(windows) && c.key == 0x2e && c.modifiers.ctrl && c.modifiers.alt {
                    issues.push(Issue {
                        actions: vec![a],
                        message: uu_i18n::text("Ctrl+Alt+Delete 由 Windows 安全桌面保留").into(),
                    });
                }
                if let Some(previous) = owners.insert(*c, a) {
                    issues.push(Issue {
                        actions: vec![previous, a],
                        message: uu_i18n::format!(
                            "{} 同时用于 {} 与 {}",
                            c.label(),
                            previous.label(),
                            a.label()
                        ),
                    });
                }
            }
        }
        issues
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(issue) = self.issues().first() {
            anyhow::bail!("{}", uu_i18n::format!("快捷键冲突：{}", issue.message));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boss_key_is_local_in_launcher_stream_and_nested_mode_and_preserves_existing_bindings() {
        let bindings=Bindings::default();let modifiers=Chord::ctrl_alt(0).modifiers;
        for streaming in [false,true] {for nested in [false,true] {
            assert_eq!(bindings.find(0x42,modifiers,nested,streaming,false),Some(Action::BossKey));
            assert_eq!(bindings.find(0x42,modifiers,nested,streaming,true),None);
        }}
        let legacy:Bindings=serde_json::from_str(r#"{"stats":[{"key":66,"modifiers":{"ctrl":true,"alt":true}}]}"#).unwrap();
        assert!(legacy.chords(Action::BossKey).is_empty());assert_eq!(legacy.find(0x42,modifiers,false,true,false),Some(Action::Stats));
    }
    #[test]
    fn evolving_navigation_defaults_keep_explicit_ownership_and_free_alternatives() {
        let loaded:Bindings=serde_json::from_str(r#"{"next_display":[{"key":40,"modifiers":{"ctrl":true,"alt":true}}],"previous_display":[]}"#).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded.chords(Action::NextDisplay), &[Chord::ctrl_alt(0x28)]);
        assert!(loaded.chords(Action::NextComputer).is_empty());
        assert!(loaded.chords(Action::PreviousDisplay).is_empty());
        assert_eq!(loaded.chords(Action::PreviousComputer), &[Chord::ctrl_alt(0x26)]);
        let restored:Bindings=serde_json::from_slice(&serde_json::to_vec(&loaded).unwrap()).unwrap();
        assert_eq!(loaded,restored);
        let partial:Bindings=serde_json::from_str(r#"{"stats":[{"key":70,"modifiers":{"ctrl":true,"alt":true}}]}"#).unwrap();
        partial.validate().unwrap();
        assert_eq!(partial.chords(Action::Fullscreen), &[Chord::ctrl_alt(0x58)]);
    }
    #[test]
    fn adding_vsync_does_not_invalidate_an_existing_custom_binding() {
        let mut old=Bindings::default();old.set(Action::Stats,vec![Chord::ctrl_alt(0x56)]);
        let loaded:Bindings=serde_json::from_slice(&serde_json::to_vec(&old).unwrap()).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded.chords(Action::Stats),&[Chord::ctrl_alt(0x56)]);
        assert!(loaded.chords(Action::Vsync).is_empty());
        assert_eq!(loaded.find(0x56,Chord::ctrl_alt(0).modifiers,false,true,false),Some(Action::Stats));
        let reopened:Bindings=serde_json::from_slice(&serde_json::to_vec(&loaded).unwrap()).unwrap();
        assert_eq!(loaded,reopened);
        old.set(Action::Vsync,vec![Chord::ctrl_alt(0x56)]);
        let explicit:Bindings=serde_json::from_slice(&serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(explicit.validate().is_err(),"explicit conflicts must not silently lose either binding");
    }
    #[test]
    fn vsync_inherits_default_and_can_be_rebound_disabled_or_passed_through() {
        let mut bindings:Bindings=serde_json::from_str(r#"{"stats":[]}"#).unwrap();
        assert_eq!(bindings.label(Action::Vsync),"Ctrl+Alt+V");
        let modifiers=Chord::ctrl_alt(0).modifiers;
        assert_eq!(bindings.find(0x56,modifiers,false,true,false),Some(Action::Vsync));
        for (nested,streaming,altgr) in [(true,true,false),(false,false,false),(false,true,true)] {
            assert_eq!(bindings.find(0x56,modifiers,nested,streaming,altgr),None);
        }
        bindings.set(Action::Vsync,vec![Chord::ctrl_alt(0x42)]);
        let mut restored:Bindings=serde_json::from_slice(&serde_json::to_vec(&bindings).unwrap()).unwrap();
        assert_eq!(restored.find(0x56,modifiers,false,true,false),None);
        assert_eq!(restored.find(0x42,modifiers,false,true,false),Some(Action::Vsync));
        restored.set(Action::Vsync,vec![]);restored.validate().unwrap();
        assert_eq!(restored.find(0x42,modifiers,false,true,false),None);
        restored.reset(Action::Vsync);assert_eq!(restored.label(Action::Vsync),"Ctrl+Alt+V");
    }
    #[test]
    fn local_desktop_toggle_is_global_escape_and_can_be_customized() {
        let mut bindings:Bindings=serde_json::from_str("{}").unwrap();
        let modifiers=Chord::ctrl_alt(0).modifiers;
        assert_eq!(bindings.find(0x44,modifiers,false,true,false),Some(Action::ToggleLocal));
        assert_eq!(bindings.find(0x44,modifiers,true,true,false),Some(Action::ToggleLocal));
        assert_eq!(bindings.find(0x44,modifiers,false,false,false),None);
        bindings.set(Action::ToggleLocal,vec![Chord::ctrl_alt_shift(0x70)]);
        assert_eq!(bindings.find(0x44,modifiers,false,true,false),None);
        assert_eq!(bindings.find(0x70,Chord::ctrl_alt_shift(0).modifiers,true,true,false),Some(Action::ToggleLocal));
    }
    #[test]
    fn navigation_is_configurable_and_only_claimed_in_fullscreen() {
        let mut bindings:Bindings=serde_json::from_str("{}").unwrap();
        for (key,action) in [(0x25,Action::PreviousDisplay),(0x27,Action::NextDisplay),
            (0x26,Action::PreviousComputer),(0x28,Action::NextComputer)] {
            let modifiers=Chord::ctrl_alt(key).modifiers;
            assert_eq!(bindings.find_in_view(key,modifiers,false,true,true,false),Some(action));
            assert_eq!(bindings.find_in_view(key,modifiers,false,true,false,false),None);
            assert_eq!(bindings.find_in_view(key,modifiers,false,false,true,false),None);
            assert_eq!(bindings.find_in_view(key,modifiers,true,true,true,false),None);
            assert_eq!(bindings.find_in_view(key,modifiers,false,true,true,true),None);
            assert_eq!(bindings.find_in_view(key,Chord::ctrl_alt_shift(key).modifiers,false,true,true,false),None);
        }
        bindings.set(Action::NextComputer,vec![Chord::ctrl_alt_shift(0x70)]);
        assert_eq!(bindings.label(Action::NextComputer),"Ctrl+Alt+Shift+F1");
        assert_eq!(bindings.find_in_view(0x28,Chord::ctrl_alt(0).modifiers,false,true,true,false),None);
        bindings.set(Action::PreviousDisplay,vec![]);
        let restored:Bindings=serde_json::from_slice(&serde_json::to_vec(&bindings).unwrap()).unwrap();
        assert!(restored.chords(Action::PreviousDisplay).is_empty());
        restored.validate().unwrap();
    }
    #[test]
    fn all_defaults_use_ctrl_alt_and_saved_overrides_remain_exact() {
        let b:Bindings=serde_json::from_str("{}").unwrap();
        for (action,keys) in Action::ALL.into_iter().zip([&[0x53][..],&[0x48],&[0x4d],&[0x46,0x58],&[0x51],&[0x5a],&[0x50],&[0x43],&[0x7b],&[0x44],&[0x25],&[0x27],&[0x26],&[0x28],&[0x56],&[0x42]]) {
            assert_eq!(b.chords(action),keys.iter().map(|key|Chord::ctrl_alt(*key)).collect::<Vec<_>>());
            for key in keys {
                assert_eq!(b.find_in_view(*key,Chord::ctrl_alt(0).modifiers,false,true,true,false),Some(action));
                assert_eq!(b.find_in_view(*key,Chord::ctrl_alt_shift(0).modifiers,false,true,true,false),None);
            }
        }
        let mut custom=b;custom.set(Action::Stats,vec![Chord::ctrl_alt_shift(0x54)]);custom.set(Action::Menu,vec![]);
        let restored:Bindings=serde_json::from_slice(&serde_json::to_vec(&custom).unwrap()).unwrap();
        assert_eq!(restored.label(Action::Stats),"Ctrl+Alt+Shift+T");assert!(restored.chords(Action::Menu).is_empty());
        assert_eq!(restored.label(Action::Mute),"Ctrl+Alt+M");
        assert_eq!(restored.find(0x54,Chord::ctrl_alt_shift(0).modifiers,false,true,false),Some(Action::Stats));
    }
    #[test]
    fn missing_defaults_disabled_actions_and_conflicts_roundtrip() {
        let mut b: Bindings = serde_json::from_str("{}").unwrap();
        b.validate().unwrap();
        assert_eq!(b.label(Action::Fullscreen), "Ctrl+Alt+F / Ctrl+Alt+X");
        b.set(Action::Stats, vec![]);
        assert_eq!(
            b.find(0x53, Chord::ctrl_alt(0).modifiers, false, true, false),
            None
        );
        b.set(Action::Menu, vec![Chord::ctrl_alt(0x46)]);
        assert!(!b.issues().is_empty());
        b.reset(Action::Menu);
        b.validate().unwrap();
        let decoded: Bindings = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(decoded, b);
        b.set(Action::ToggleNested, vec![]);
        assert!(b.validate().is_err());
    }
    #[test]
    fn rebinding_changes_both_modifier_matching_and_nested_escape_by_action() {
        let mut b = Bindings::default();
        let chord = Chord {
            key: 0x70,
            modifiers: Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        };
        b.set(Action::ToggleNested, vec![chord]);
        b.validate().unwrap();
        assert_eq!(
            b.find(chord.key, chord.modifiers, true, true, false),
            Some(Action::ToggleNested)
        );
        assert_eq!(
            b.find(0x7b, Chord::ctrl_alt(0).modifiers, true, true, false),
            None
        );
        assert_eq!(
            b.find(0x53, Chord::ctrl_alt(0).modifiers, true, true, false),
            None
        );
        assert_eq!(
            b.find(
                chord.key,
                Modifiers {
                    alt: true,
                    ..chord.modifiers
                },
                true,
                true,
                false
            ),
            None
        );
        assert_eq!(b.find(chord.key, chord.modifiers, true, true, true), None);
    }
}
