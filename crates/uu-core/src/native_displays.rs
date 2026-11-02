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
                    screen(0, uu_i18n::text("当前授权屏幕").into(), 2, 2, true, true,None),
                    screen(1, uu_i18n::text("重新选择共享屏幕…").into(), 2, 2, false, false,None),
                ],
            };
            return Ok(CaptureSource::Primary);
        }
        self.prepare_monitors(crate::native::monitors()?)
    }
    fn prepare_monitors(&self, mut monitors: Vec<CaptureMonitor>) -> anyhow::Result<CaptureSource> {
        anyhow::ensure!(!monitors.is_empty(), "{}", uu_i18n::format!("当前没有可共享的显示器"));
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
                        m.refresh_hz,
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
    /// Refresh source IDs after a leased target appears. This does not request
    /// a media restart or change the current capture source.
    pub(crate) fn refresh(&self)->anyhow::Result<()> {
        self.refresh_monitors(crate::native::monitors()?)
    }
    pub(crate) fn refresh_source(&self,source:&str)->anyhow::Result<()> {
        // CCD can confirm activation before DXGI publishes the output. Wait
        // outside input/media threads; otherwise one early refresh would leave
        // a newly created display absent from this session's cached directory.
        let started=std::time::Instant::now();
        loop {
            if let Ok(monitors)=crate::native::monitors() {
                self.refresh_monitors(monitors)?;
                if self.references_catalog_source(source){return Ok(());}
            }
            anyhow::ensure!(started.elapsed()<std::time::Duration::from_secs(3),"{}",uu_i18n::text("虚拟显示器尚未出现在可采集目录中，请稍后重试"));
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    fn references_catalog_source(&self,source:&str)->bool {
        let state=self.state.lock().unwrap();
        state.catalog.screens.iter().any(|screen|state.names.get(screen.id as usize).is_some_and(|name|name==source))
    }
    fn refresh_monitors(&self,mut monitors:Vec<CaptureMonitor>)->anyhow::Result<()> {
        let mut state=self.state.lock().unwrap();
        let current=state.catalog.current_id;
        let name=state.names.get(current as usize).cloned().ok_or_else(||anyhow::anyhow!("current capture source is not prepared"))?;
        let previous=state.catalog.screens.iter().find(|screen|screen.id==current).cloned().ok_or_else(||anyhow::anyhow!("current capture catalog is unavailable"))?;
        monitors.sort_by(|a,b|a.name.cmp(&b.name));
        let active=monitors.iter().position(|monitor|monitor.name==name).ok_or_else(||anyhow::anyhow!("current capture source disappeared; reconnect before changing virtual displays"))?;
        if active>=MAX_DISPLAYS {monitors.swap(active,MAX_DISPLAYS-1);}
        monitors.truncate(MAX_DISPLAYS);
        for monitor in &monitors {if !state.names.contains(&monitor.name){state.names.push(monitor.name.clone());}}
        let screens=monitors.iter().map(|monitor| {
            let id=state.names.iter().position(|name|name==&monitor.name).unwrap() as i32;
            let mut value=screen(id,monitor.name.chars().filter(|c|!c.is_control()).take(96).collect(),monitor.width,monitor.height,monitor.primary,id==current,monitor.refresh_hz);
            if id==current {value.width=previous.width;value.height=previous.height;}
            value
        }).collect();
        // selected may already contain a pending client switch. A catalog
        // refresh must describe the still-active capture, never that future
        // selection or a fallback source without a new media epoch.
        state.catalog=RemoteScreens{current_id:current,screens};
        Ok(())
    }
    pub(crate) fn references_source(&self,source:&str)->bool {
        let state=self.state.lock().unwrap();
        state.selected.as_deref()==Some(source)||state.names.get(state.catalog.current_id as usize).is_some_and(|name|name==source)
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
    refresh_hz:Option<u32>,
) -> RemoteScreen {
    RemoteScreen {
        id,
        display_name: name,
        width,
        height,
        primary,
        refresh_hz: refresh_hz.filter(|hz|*hz>1).unwrap_or(0),
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
    pub fn request(&self, request: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.request.is_some() {
            return false;
        }
        state.request = Some(request);
        true
    }
    pub fn response(&self, response: Response) {
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
            refresh_hz:Some(165),
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
        assert_eq!(host.catalog().screens[0].refresh_hz,165);
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
    fn refresh_adds_targets_without_publishing_a_pending_or_missing_source_as_current() {
        let host=Host::default();
        host.prepare_monitors(vec![monitor("a",true),monitor("b",false)]).unwrap();
        host.dimensions(320,240);
        assert_eq!(host.select(1),Some(true));
        host.refresh_monitors(vec![monitor("a",true),monitor("b",false),monitor("c",false)]).unwrap();
        assert_eq!(host.catalog().current_id,0);
        assert_eq!(host.catalog().screens[0].width,320);
        assert!(host.references_source("a")&&host.references_source("b"));
        let before=host.catalog();
        assert!(host.refresh_monitors(vec![monitor("b",false),monitor("c",true)]).is_err());
        assert_eq!(host.catalog().current_id,before.current_id);
        assert_eq!(host.catalog().screens.len(),before.screens.len());
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
            screens: vec![screen(0, "a".into(), 640, 480, true, true,None)],
        };
        assert!(valid(&catalog));
        catalog.screens.push(catalog.screens[0].clone());
        assert!(!valid(&catalog));
    }
}
