//! Optional display selection inside an already authenticated native session.
//! The host advertises only its explicit sharing scope; client IDs are never
//! interpreted as output paths. A selection renews media, not device trust.
use crate::{
    native::{CaptureMonitor, CaptureSource},
    screen_control::{RemoteScreen, RemoteScreens},
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tokio::sync::Notify;

pub const ALL_MONITORS: &str = "*";
const MAX_DISPLAYS: usize = 16;

#[derive(Default)]
struct Selection {
    names: Vec<String>,
    selected: Option<String>,
    catalog: RemoteScreens,
}
#[derive(Default)]
pub(crate) struct Host {
    state: Mutex<Selection>,
    pub changed: Notify,
    switching: AtomicBool,
}
impl Host {
    pub fn for_source(source: &CaptureSource) -> Option<std::sync::Arc<Self>> {
        let CaptureSource::Desktop { initial_monitor } = source else { return None; };
        let host=Self::default();
        host.state.lock().unwrap().selected=initial_monitor.clone();
        Some(std::sync::Arc::new(host))
    }
    pub fn prepare(&self) -> anyhow::Result<CaptureSource> {
        self.switching.store(false, Ordering::Release);
        if crate::native::uses_desktop_portal() {
            let mut state = self.state.lock().unwrap();
            state.catalog = RemoteScreens {
                current_id: 0,
                screens: vec![
                    screen(0, "当前授权屏幕".into(), 2, 2, true, true),
                    screen(1, "重新选择共享屏幕…".into(), 2, 2, false, false),
                ],
            };
            return Ok(CaptureSource::Primary);
        }
        self.prepare_monitors(crate::native::monitors()?)
    }
    fn prepare_monitors(&self, mut monitors: Vec<CaptureMonitor>) -> anyhow::Result<CaptureSource> {
        anyhow::ensure!(!monitors.is_empty(), "当前没有可共享的显示器");
        monitors.sort_by(|a, b| a.name.cmp(&b.name));
        monitors.truncate(MAX_DISPLAYS);
        let mut state = self.state.lock().unwrap();
        let selected = state
            .selected
            .as_ref()
            .and_then(|name| monitors.iter().find(|m| &m.name == name))
            .or_else(|| monitors.iter().find(|m| m.primary))
            .unwrap_or(&monitors[0]);
        let selected_name = selected.name.clone();
        // Stable IDs for outputs which disappear and return during this session.
        for monitor in &monitors {
            if !state.names.contains(&monitor.name) {
                state.names.push(monitor.name.clone());
            }
        }
        let id = |name: &str| state.names.iter().position(|n| n == name).unwrap() as i32;
        state.catalog = RemoteScreens {
            current_id: id(&selected_name),
            screens: monitors
                .iter()
                .map(|m| {
                    screen(
                        id(&m.name),
                        m.name
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(96)
                            .collect(),
                        m.width,
                        m.height,
                        m.primary,
                        m.name == selected_name,
                    )
                })
                .collect(),
        };
        state.selected = Some(selected_name.clone());
        Ok(CaptureSource::Monitor(selected_name))
    }
    pub fn dimensions(&self, width: u32, height: u32) {
        let mut state = self.state.lock().unwrap();
        let current = state.catalog.current_id;
        if let Some(screen) = state.catalog.screens.iter_mut().find(|s| s.id == current) {
            screen.width = width;
            screen.height = height;
        }
    }
    pub fn catalog(&self) -> RemoteScreens {
        self.state.lock().unwrap().catalog.clone()
    }
    pub fn select(&self, id: i32) -> Option<bool> {
        if self.switching.load(Ordering::Acquire) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        if !state.catalog.screens.iter().any(|s| s.id == id) {
            return None;
        }
        if id == state.catalog.current_id {
            return Some(false);
        }
        if !crate::native::uses_desktop_portal() {
            state.selected = state.names.get(id as usize).cloned();
        }
        Some(true)
    }
    pub fn commit(&self) {
        self.switching.store(true, Ordering::Release);
        self.changed.notify_one();
    }
    pub fn switching(&self) -> bool {
        self.switching.load(Ordering::Acquire)
    }
}
fn screen(
    id: i32,
    name: String,
    width: u32,
    height: u32,
    primary: bool,
    selected: bool,
) -> RemoteScreen {
    RemoteScreen {
        id,
        display_name: name,
        width,
        height,
        primary,
        refresh_hz: 0,
        kind: 0,
        dpi_scale: None,
        video_track_index: if selected { 0 } else { -1 },
    }
}
pub(crate) fn valid(catalog: &RemoteScreens) -> bool {
    !catalog.screens.is_empty()
        && catalog.screens.len() <= MAX_DISPLAYS
        && catalog.screens.iter().enumerate().all(|(i, s)| {
            s.id >= 0
                && (2..=16384).contains(&s.width)
                && (2..=16384).contains(&s.height)
                && s.display_name.len() <= 384
                && !s.display_name.chars().any(char::is_control)
                && s.video_track_index == if s.id == catalog.current_id { 0 } else { -1 }
                && !catalog.screens[..i].iter().any(|old| old.id == s.id)
        })
        && catalog.screens.iter().any(|s| s.id == catalog.current_id)
}
#[derive(Clone, Debug)]
pub struct Response {
    pub request: u64,
    pub accepted: bool,
    pub restart: bool,
}
#[derive(Default)]
struct Pending {
    request: Option<u64>,
    responses: VecDeque<Response>,
    switch: Option<u64>,
    catalog_epoch: u64,
}
#[derive(Default)]
pub struct Client {
    state: Mutex<Pending>,
    pub(crate) changed: Notify,
}
impl Client {
    pub(crate) fn request(&self, request: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.request.is_some() {
            return false;
        }
        state.request = Some(request);
        true
    }
    pub(crate) fn response(&self, response: Response) {
        let mut state = self.state.lock().unwrap();
        if state.request != Some(response.request) {
            return;
        }
        state.request = None;
        if response.accepted && response.restart {
            state.switch = Some(response.request);
            self.changed.notify_one();
        }
        if state.responses.len() == 16 {
            state.responses.pop_front();
        }
        state.responses.push_back(response);
    }
    pub(crate) fn take_switch(&self) -> Option<u64> {
        std::mem::take(&mut self.state.lock().unwrap().switch)
    }
    pub(crate) fn catalog(&self, epoch: u64) {
        self.state.lock().unwrap().catalog_epoch = epoch;
    }
    pub fn catalog_epoch(&self) -> u64 {
        self.state.lock().unwrap().catalog_epoch
    }
    pub fn responses(&self) -> Vec<Response> {
        self.state.lock().unwrap().responses.drain(..).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn monitor(name: &str, primary: bool) -> CaptureMonitor {
        CaptureMonitor {
            name: name.into(),
            primary,
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        }
    }
    #[test]
    fn scope_rejects_unknown_ids_and_keeps_selection_by_name_across_reordering_and_hotplug() {
        let host = Host::default();
        assert_eq!(
            host.prepare_monitors(vec![monitor("b", false), monitor("a", true)])
                .unwrap(),
            CaptureSource::Monitor("a".into())
        );
        assert!(valid(&host.catalog()));
        assert_eq!(host.select(999), None);
        assert_eq!(host.select(1), Some(true));
        assert_eq!(
            host.prepare_monitors(vec![monitor("b", false), monitor("a", true)])
                .unwrap(),
            CaptureSource::Monitor("b".into())
        );
        assert_eq!(host.catalog().current_id, 1);
        assert_eq!(
            host.prepare_monitors(vec![monitor("a", true)]).unwrap(),
            CaptureSource::Monitor("a".into())
        );
        assert_eq!(host.catalog().current_id, 0);
    }
    #[test]
    fn only_matched_acceptance_can_renew_media_and_catalog_is_bounded() {
        let client = Client::default();
        assert!(client.request(7));
        assert!(!client.request(8));
        client.response(Response {
            request: 8,
            accepted: true,
            restart: true,
        });
        assert_eq!(client.take_switch(), None);
        client.response(Response {
            request: 7,
            accepted: true,
            restart: true,
        });
        assert_eq!(client.take_switch(), Some(7));
        assert_eq!(client.take_switch(), None);
        assert_eq!(client.responses().len(), 1);
        let mut catalog = RemoteScreens {
            current_id: 0,
            screens: vec![screen(0, "a".into(), 640, 480, true, true)],
        };
        assert!(valid(&catalog));
        catalog.screens.push(catalog.screens[0].clone());
        assert!(!valid(&catalog));
    }
}
