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
}
impl Action {
    pub const ALL: [Self; 9] = [
        Self::Stats,
        Self::Menu,
        Self::Mute,
        Self::Fullscreen,
        Self::Exit,
        Self::Capture,
        Self::MouseMode,
        Self::CursorMode,
        Self::ToggleNested,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Stats => "HUD",
            Self::Menu => "悬浮菜单",
            Self::Mute => "静音",
            Self::Fullscreen => "全屏",
            Self::Exit => "结束串流",
            Self::Capture => "释放 / 恢复控制",
            Self::MouseMode => "桌面 / 游戏鼠标",
            Self::CursorMode => "光标显示",
            Self::ToggleNested => "嵌套快捷键透传",
        }
    }
    fn defaults(self) -> &'static [Chord] {
        const DEFAULTS: [&[Chord]; 9] = [
            &[Chord::ctrl_alt(0x53)],
            &[Chord::ctrl_alt(0x48)],
            &[Chord::ctrl_alt(0x4d)],
            &[Chord::ctrl_alt(0x46), Chord::ctrl_alt(0x58)],
            &[Chord::ctrl_alt(0x51)],
            &[Chord::ctrl_alt(0x5a)],
            &[Chord::ctrl_alt(0x50)],
            &[Chord::ctrl_alt(0x43)],
            &[Chord::ctrl_alt(0x7b)],
        ];
        DEFAULTS[self as usize]
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
            Some("可能与系统快捷键冲突，是否可截获取决于平台与全屏捕获设置")
        } else if m == Modifiers::default() {
            Some("无修饰键；此键将在串流前台被客户端占用")
        } else {
            None
        }
    }
}
/// Missing actions inherit defaults; an explicit empty list disables an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bindings(BTreeMap<Action, Vec<Chord>>);
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
        if altgr {
            return None;
        }
        let chord = Chord { key, modifiers };
        Action::ALL.into_iter().find(|a| {
            (!nested || *a == Action::ToggleNested)
                && (streaming || *a == Action::Fullscreen)
                && self.chords(*a).contains(&chord)
        })
    }
    pub fn issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut owners = BTreeMap::new();
        if self.chords(Action::ToggleNested).is_empty() {
            issues.push(Issue {
                actions: vec![Action::ToggleNested],
                message: "嵌套透传必须保留一个切回本机的快捷键".into(),
            });
        }
        for a in Action::ALL {
            if self.chords(a).len() > 3 {
                issues.push(Issue {
                    actions: vec![a],
                    message: "每项最多三个快捷键".into(),
                });
            }
            for c in self.chords(a) {
                if !c.valid() {
                    issues.push(Issue {
                        actions: vec![a],
                        message: "不是可用的普通按键".into(),
                    });
                }
                if cfg!(windows) && c.key == 0x2e && c.modifiers.ctrl && c.modifiers.alt {
                    issues.push(Issue {
                        actions: vec![a],
                        message: "Ctrl+Alt+Delete 由 Windows 安全桌面保留".into(),
                    });
                }
                if let Some(previous) = owners.insert(*c, a) {
                    issues.push(Issue {
                        actions: vec![previous, a],
                        message: format!(
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
            anyhow::bail!("快捷键冲突：{}", issue.message);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
