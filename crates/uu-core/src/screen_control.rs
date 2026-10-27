//! Provider-neutral remote displays and presentation preferences.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScreens {
    pub screens: Vec<RemoteScreen>,
    pub current_id: i32,
}
impl RemoteScreens {
    /// Observe the host's existing capture before considering any source switch.
    pub fn initial_screen(&self) -> Option<&RemoteScreen> {
        self.selected().filter(|s| s.assigned_track().is_some())
            .or_else(|| self.for_track(0))
            .or_else(|| self.screens.iter().find(|s| s.assigned_track().is_some()))
            .or_else(|| self.selected())
            .or_else(|| self.screens.iter().find(|s| s.primary))
            .or_else(|| self.screens.first())
    }
    pub fn selected(&self) -> Option<&RemoteScreen> {
        self.screens.iter().find(|s| s.id == self.current_id)
    }
    pub fn for_track(&self, track: u32) -> Option<&RemoteScreen> {
        self.selected()
            .filter(|s| s.assigned_track() == Some(track))
            .or_else(|| self.screens.iter().find(|s| s.assigned_track() == Some(track)))
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScreen {
    pub id: i32,
    pub refresh_hz: u32,
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub dpi_scale: Option<u32>,
    pub video_track_index: i32,
    pub display_name: String,
    pub primary: bool,
}
impl RemoteScreen {
    /// -1 is an advertised source with no allocated video track yet.
    pub fn assigned_track(&self) -> Option<u32> {
        u32::try_from(self.video_track_index).ok()
    }
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            0 => uu_i18n::text("物理屏"),
            1 => uu_i18n::text("虚拟屏"),
            2 => uu_i18n::text("超级屏"),
            _ => uu_i18n::text("未知屏幕"),
        }
    }
}
impl RemoteScreens {
    /// Apply the separately confirmed legacy capture source without assigning
    /// the same track to two monitors from an older inventory.
    pub fn with_desktop_capture(mut self, id: Option<i32>) -> Self {
        if let Some(id) = id.filter(|id| self.screens.iter().any(|s| s.id == *id)) {
            for screen in &mut self.screens {
                if screen.id == id { screen.video_track_index = 0; }
                else if screen.assigned_track() == Some(0) { screen.video_track_index = -1; }
            }
            self.current_id = id;
        }
        self
    }
}

/// Preserve is deliberately the default, including for existing settings files.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="snake_case")]
pub enum DisplayMode {
    #[default]
    Preserve,
    ExclusiveVirtual,
}
impl DisplayMode {
    pub fn label(self)->&'static str {
        match self {Self::Preserve=>uu_i18n::text("保持远端显示器现状"),Self::ExclusiveVirtual=>uu_i18n::text("UU虚拟屏独占（关闭其它显示器）")}
    }
}
