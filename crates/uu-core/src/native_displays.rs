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
use anyhow::{ensure,Result};
use serde::{Deserialize,Serialize};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

pub const ALL_MONITORS: &str = "*";
const MAX_DISPLAYS: usize = 16;
mod names;
const INITIAL_REQUEST:&str="x-eclipse-initial-display-client";
const INITIAL_REPLY:&str="x-eclipse-initial-display-host";
const INITIAL_SUPPORTED:&str="x-eclipse-initial-display-supported";

#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum InitialRequest {
    Existing{id:i32,source:String},
    Virtual{#[serde(with="initial_spec")] spec:crate::virtual_display::Spec},
    VirtualPersistent{#[serde(with="initial_spec")] spec:crate::virtual_display::Spec},
}
// Keep the extension strict without changing the existing Spec wire format.
mod initial_spec {
    use super::*;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {width:u32,height:u32,refresh_millihz:u32,hdr:bool}
    pub fn serialize<S:serde::Serializer>(spec:&crate::virtual_display::Spec,serializer:S)->std::result::Result<S::Ok,S::Error>{spec.serialize(serializer)}
    pub fn deserialize<'de,D:serde::Deserializer<'de>>(deserializer:D)->std::result::Result<crate::virtual_display::Spec,D::Error>{
        let value=Strict::deserialize(deserializer)?;
        Ok(crate::virtual_display::Spec{width:value.width,height:value.height,refresh_millihz:value.refresh_millihz,hdr:value.hdr})
    }
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct InitialEnvelope<T>{version:u8,value:T}
fn read_initial<T:serde::de::DeserializeOwned>(sdp:&RTCSessionDescription,key:&str)->Result<Option<T>> {
    let prefix=format!("a={key}");
    for line in sdp.sdp.lines().filter(|line|line.starts_with(&prefix)) {
        ensure!(line.strip_prefix(&prefix).is_some_and(|suffix|suffix.starts_with(':')),"malformed initial display attribute");
    }
    let value:Option<InitialEnvelope<T>>=crate::native_quality::get(sdp,key)?;
    value.map(|value|{ensure!(value.version==1,"unsupported initial display version");Ok(value.value)}).transpose()
}
fn add_initial<T:Serialize>(sdp:&mut RTCSessionDescription,key:&str,value:&T)->Result<()> {
    ensure!(crate::native_quality::get::<serde_json::Value>(sdp,key)?.is_none(),"duplicate initial display attribute");
    crate::native_quality::put(sdp,key,&InitialEnvelope{version:1,value})
}
fn valid_source(source:&str,empty:bool)->bool {(empty||!source.is_empty())&&source.len()<=384&&!source.chars().any(char::is_control)}
impl InitialRequest {
    pub fn virtual_display(spec:crate::virtual_display::Spec,persistent:bool)->Self {
        if persistent {Self::VirtualPersistent{spec}}else{Self::Virtual{spec}}
    }
    pub fn virtual_spec(&self)->Option<(crate::virtual_display::Spec,bool)> {
        match self {Self::Virtual{spec}=>Some((*spec,false)),Self::VirtualPersistent{spec}=>Some((*spec,true)),_=>None}
    }
    fn validate(&self)->Result<()> {
        match self {
            Self::Existing{id,source}=>ensure!(*id>=0&&valid_source(source,true),"invalid initial display target"),
            Self::Virtual{spec}|Self::VirtualPersistent{spec}=>spec.validate()?,
        }
        Ok(())
    }
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;add_initial(sdp,INITIAL_REQUEST,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {
        let value:Option<Self>=read_initial(sdp,INITIAL_REQUEST)?;
        if let Some(value)=&value {value.validate()?;}
        Ok(value)
    }
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialReply {
    pub request:InitialRequest,
    pub id:i32,
    pub source:String,
    #[serde(with="initial_display")]
    pub virtual_display:Option<crate::virtual_display::Display>,
}
mod initial_display {
    use super::*;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        id:crate::virtual_display::LeaseId,
        owner:crate::virtual_display::Owner,
        #[serde(with="initial_spec")]
        spec:crate::virtual_display::Spec,
        actual:Option<StrictSpec>,
        name:String,source:String,active:bool,cleanup_pending:bool,last_error:Option<String>,
        #[serde(default)]
        persistent:bool,
    }
    #[derive(Deserialize)]
    struct StrictSpec(#[serde(with="initial_spec")] crate::virtual_display::Spec);
    pub fn serialize<S:serde::Serializer>(value:&Option<crate::virtual_display::Display>,serializer:S)->std::result::Result<S::Ok,S::Error> {value.serialize(serializer)}
    pub fn deserialize<'de,D:serde::Deserializer<'de>>(deserializer:D)->std::result::Result<Option<crate::virtual_display::Display>,D::Error> {
        Ok(Option::<Strict>::deserialize(deserializer)?.map(|v|crate::virtual_display::Display{id:v.id,owner:v.owner,spec:v.spec,actual:v.actual.map(|s|s.0),name:v.name,source:v.source,active:v.active,persistent:v.persistent,cleanup_pending:v.cleanup_pending,last_error:v.last_error}))
    }
}
impl InitialReply {
    pub(crate) fn from_catalog(request:InitialRequest,catalog:&RemoteScreens,virtual_display:Option<crate::virtual_display::Display>)->Result<Self> {
        let current=catalog.screens.iter().find(|screen|screen.id==catalog.current_id)
            .ok_or_else(||anyhow::anyhow!("initial display catalog has no current source"))?;
        let reply=Self{request,id:current.id,source:current.display_name.clone(),virtual_display};
        reply.validate()?;
        Ok(reply)
    }
    fn validate(&self)->Result<()> {
        self.request.validate()?;
        ensure!(self.id>=0&&valid_source(&self.source,false),"invalid initial display reply source");
        match (&self.request,&self.virtual_display) {
            (InitialRequest::Existing{id,source},None)=>ensure!(if source.is_empty(){self.id==*id}else{self.source==*source},"initial display reply target mismatch"),
            (request,Some(display)) if request.virtual_spec().is_some()=>ensure!(Some((display.spec,display.persistent))==request.virtual_spec()&&display.source==self.source&&display.active
                &&matches!(display.owner,crate::virtual_display::Owner::Session(owner) if !owner.is_nil())&&!display.id.0.is_nil()
                &&!display.cleanup_pending&&display.actual.is_none_or(|actual|actual.validate().is_ok()),"initial virtual display reply mismatch"),
            _=>anyhow::bail!("initial display reply kind mismatch"),
        }
        Ok(())
    }
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;add_initial(sdp,INITIAL_REPLY,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {
        let value:Option<Self>=read_initial(sdp,INITIAL_REPLY)?;
        if let Some(value)=&value {value.validate()?;}
        Ok(value)
    }
}
pub fn advertise_initial(sdp:&mut RTCSessionDescription)->Result<()> {add_initial(sdp,INITIAL_SUPPORTED,&true)}
pub fn initial_offered(sdp:&RTCSessionDescription)->Result<bool> {
    let value:Option<bool>=read_initial(sdp,INITIAL_SUPPORTED)?;
    ensure!(value!=Some(false),"invalid initial display capability");
    Ok(value==Some(true))
}

#[derive(Default)]
struct Selection {
    names_metadata:names::Names,
    names: Vec<String>,
    selected: Option<String>,
    catalog: RemoteScreens,
}
#[derive(Default)]
pub(crate) struct Host {
    state: Mutex<Selection>,
    pub changed: Notify,
    switching: AtomicBool,
    refreshing: AtomicBool,
    refresh_after: Mutex<Option<std::time::Instant>>,
}
impl Host {
    pub fn add_names(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.state.lock().unwrap().names_metadata.add(sdp)}
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
            for screen in &mut state.catalog.screens {screen.kind=crate::screen_control::PORTAL_CHOICE_KIND;}
            return Ok(CaptureSource::Primary);
        }
        self.prepare_monitors(crate::native::monitors()?)
    }
    /// Resolve the first source before opening a capture. Source names are the
    /// stable cross-connection identity; numeric IDs are only a legacy fallback.
    pub fn prepare_initial(&self, request: &InitialRequest) -> Result<CaptureSource> {
        ensure!(!crate::native::uses_desktop_portal(), "portal source selection requires authorization");
        self.prepare_initial_monitors(request, crate::native::monitors()?)
    }
    fn prepare_initial_monitors(&self, request: &InitialRequest, mut monitors: Vec<CaptureMonitor>) -> Result<CaptureSource> {
        request.validate()?;
        let InitialRequest::Existing {id,source}=request else {anyhow::bail!("virtual target must be created before source selection")};
        monitors.sort_by(|a,b|a.name.cmp(&b.name));
        let name=if source.is_empty() {
            let state=self.state.lock().unwrap();
            if state.names.is_empty() {monitors.iter().take(MAX_DISPLAYS).nth(*id as usize).map(|m|m.name.clone())}
            else {state.names.get(*id as usize).filter(|name|monitors.iter().any(|m|&m.name==*name)).cloned()}
                .ok_or_else(||anyhow::anyhow!("initial display ID is unavailable"))?
        } else {
            let matches:Vec<_>=monitors.iter().filter(|m|display_name(&m.name)==*source).collect();
            ensure!(matches.len()==1,"initial display source is missing or ambiguous");
            matches[0].name.clone()
        };
        self.state.lock().unwrap().selected=Some(name);
        self.prepare_monitors(monitors)
    }
    /// CCD activation precedes DXGI publication. Wait without ever opening the
    /// default output, which may already be duplicated by another connection.
    pub fn prepare_initial_source(&self, source: &str) -> Result<CaptureSource> {
        ensure!(valid_source(source,false),"invalid initial virtual display source");
        ensure!(!crate::native::uses_desktop_portal(),"virtual targets require desktop capture");
        let started=std::time::Instant::now();
        loop {
            if let Ok(monitors)=crate::native::monitors() {
                let matches:Vec<_>=monitors.iter().filter(|m|m.name==source).collect();
                ensure!(matches.len()<=1,"initial virtual display source is ambiguous");
                if matches.len()==1 {
                    return self.prepare_initial_monitors(&InitialRequest::Existing{id:0,source:display_name(source)},monitors);
                }
            }
            ensure!(started.elapsed()<std::time::Duration::from_secs(3),"{}",uu_i18n::text("虚拟显示器尚未出现在可采集目录中，请稍后重试"));
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    fn prepare_monitors(&self, mut monitors: Vec<CaptureMonitor>) -> anyhow::Result<CaptureSource> {
        anyhow::ensure!(!monitors.is_empty(), "{}", uu_i18n::format!("当前没有可共享的显示器"));
        monitors.sort_by(|a, b| a.name.cmp(&b.name));
        let mut state = self.state.lock().unwrap();
        if let Some(index)=state.selected.as_ref().and_then(|name|monitors.iter().position(|m|&m.name==name)) {
            if index>=MAX_DISPLAYS {monitors.swap(index,MAX_DISPLAYS-1);}
        }
        monitors.truncate(MAX_DISPLAYS);
        let selected = state
            .selected
            .as_ref()
            .and_then(|name| monitors.iter().find(|m| &m.name == name))
            .or_else(|| monitors.iter().find(|m| m.primary))
            .unwrap_or(&monitors[0]);
        let selected_name = selected.name.clone();
        state.names_metadata=names::Names::from_monitors(&monitors);
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
                        display_name(&m.name),
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
    /// Inventory refresh never blocks control packets or the capture thread.
    /// Concurrent sessions share one short-lived OS enumeration result.
    pub(crate) fn poll_catalog(self:&std::sync::Arc<Self>) {
        if crate::native::uses_desktop_portal() {return;}
        let now=std::time::Instant::now();
        let mut after=self.refresh_after.lock().unwrap();
        if after.is_some_and(|at|now<at) || self.refreshing.swap(true,Ordering::AcqRel) {return;}
        *after=Some(now+std::time::Duration::from_secs(2));
        let host=self.clone();
        tokio::task::spawn_blocking(move|| {
            struct Done(std::sync::Arc<Host>);
            impl Drop for Done {fn drop(&mut self){self.0.refreshing.store(false,Ordering::Release);}}
            let _done=Done(host.clone());
            type Cached=Option<(std::time::Instant,Vec<CaptureMonitor>)>;
            static CACHE:std::sync::OnceLock<Mutex<Cached>>=std::sync::OnceLock::new();
            let mut cache=CACHE.get_or_init(Default::default).lock().unwrap();
            if cache.as_ref().is_none_or(|(at,_)|at.elapsed()>=std::time::Duration::from_secs(2)) {
                match crate::native::monitors() {Ok(monitors)=>*cache=Some((std::time::Instant::now(),monitors)),Err(_)=>return,}
            }
            let monitors=cache.as_ref().unwrap().1.clone();drop(cache);
            let _=host.refresh_monitors(monitors);
        });
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
        state.names_metadata=names::Names::from_monitors(&monitors);
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
fn display_name(name:&str)->String {name.chars().filter(|c|!c.is_control()).take(96).collect()}
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
    names_metadata:names::Names,
    request: Option<u64>,
    responses: VecDeque<Response>,
    switch: Option<u64>,
    catalog_epoch: u64,
    catalog_updated: Option<std::time::Instant>,
    initial: InitialState,
}
#[derive(Default)]
struct InitialState {
    request:Option<InitialRequest>,
    offer_written:bool,
    sent:bool,
    finished:bool,
    supported:bool,
    result:Option<std::result::Result<InitialReply,String>>,
}
#[derive(Default)]
pub struct Client {
    state: Mutex<Pending>,
    pub(crate) changed: Notify,
}
impl Client {
    pub(crate) fn read_names(&self,sdp:&RTCSessionDescription)->Result<()> {
        let mut state=self.state.lock().unwrap();
        state.names_metadata=names::Names::default();
        state.names_metadata=names::Names::read(sdp)?;
        Ok(())
    }
    pub fn display_label(&self,source:&str)->Option<String> {self.state.lock().unwrap().names_metadata.label(source)}
    pub fn request_initial(&self, request:InitialRequest)->Result<()> {
        request.validate()?;
        let mut state=self.state.lock().unwrap();
        let initial=&mut state.initial;
        ensure!(!initial.offer_written,"initial display offer already written");
        ensure!(initial.request.as_ref().is_none_or(|old|old==&request),"initial display request cannot change");
        initial.request=Some(request);
        Ok(())
    }
    pub fn initial_request(&self)->Option<InitialRequest> {self.state.lock().unwrap().initial.request.clone()}
    pub fn initial_supported(&self)->bool {self.state.lock().unwrap().initial.supported}
    pub fn take_initial_result(&self)->Option<std::result::Result<InitialReply,String>> {self.state.lock().unwrap().initial.result.take()}
    pub(crate) fn write_initial_offer(&self, sdp:&mut RTCSessionDescription)->Result<()> {
        let mut state=self.state.lock().unwrap();
        let initial=&mut state.initial;
        if initial.offer_written {return Ok(());}
        if let Some(request)=&initial.request {request.add(sdp)?;initial.sent=true;}
        initial.offer_written=true;
        Ok(())
    }
    pub(crate) fn read_initial_answer(&self, sdp:&RTCSessionDescription)->Result<()> {
        let supported=initial_offered(sdp)?;
        let reply=InitialReply::read(sdp)?;
        let mut state=self.state.lock().unwrap();
        let initial=&mut state.initial;
        initial.supported=supported;
        if initial.finished || !initial.sent {
            ensure!(reply.is_none(),"unsolicited initial display reply");
            return Ok(());
        }
        let request=initial.request.as_ref().expect("sent initial request");
        let result=if let Some(reply)=reply {
            ensure!(supported,"initial display reply without capability");
            ensure!(&reply.request==request,"initial display reply does not match request");
            Some(Ok(reply))
        } else if request.virtual_spec().is_some() {
            Some(Err("remote host does not support initial virtual displays; update its background service".into()))
        } else {
            // Old hosts still use the GUI's original exact-source selection
            // gate and runtime control-channel switch after their first frame.
            None
        };
        initial.finished=true;
        initial.result=result;
        if let Some(Err(error))=&initial.result {anyhow::bail!("{error}");}
        Ok(())
    }
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
        let mut state=self.state.lock().unwrap();
        state.catalog_epoch = epoch;
        state.catalog_updated=Some(std::time::Instant::now());
    }
    pub fn catalog_epoch(&self) -> u64 {
        self.state.lock().unwrap().catalog_epoch
    }
    pub fn catalog_updated(&self)->Option<std::time::Instant> {self.state.lock().unwrap().catalog_updated}
    pub fn responses(&self) -> Vec<Response> {
        self.state.lock().unwrap().responses.drain(..).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn description()->RTCSessionDescription {
        RTCSessionDescription::offer("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\na=recvonly\r\n".into()).unwrap()
    }
    fn virtual_reply()->InitialReply {
        use crate::virtual_display::*;
        let request=InitialRequest::Virtual{spec:Spec::default()};
        InitialReply{request,id:1,source:"display-b".into(),virtual_display:Some(Display{id:LeaseId(uuid::Uuid::new_v4()),owner:Owner::Session(uuid::Uuid::new_v4()),spec:Spec::default(),actual:Some(Spec::default()),name:"test virtual".into(),source:"display-b".into(),active:true,persistent:false,cleanup_pending:false,last_error:None})}
    }
    #[test]
    fn persistent_initial_intent_roundtrips_and_rejects_a_temporary_reply() {
        let mut reply=virtual_reply();
        reply.request=InitialRequest::virtual_display(crate::virtual_display::Spec::default(),true);
        let mut offer=description();reply.request.add(&mut offer).unwrap();
        assert_eq!(InitialRequest::read(&offer).unwrap(),Some(reply.request.clone()));
        assert!(reply.validate().is_err(),"a host cannot silently turn a persistent request into temporary");
        reply.virtual_display.as_mut().unwrap().persistent=true;
        let mut answer=description();reply.add(&mut answer).unwrap();
        assert_eq!(InitialReply::read(&answer).unwrap(),Some(reply));
    }
    #[test]
    fn initial_wire_is_independent_strict_versioned_and_round_trips_through_sdp() {
        let mut offer=description();
        assert!(!initial_offered(&offer).unwrap());
        assert_eq!(InitialRequest::read(&offer).unwrap(),None);
        let request=InitialRequest::Existing{id:3,source:"display-b".into()};
        request.add(&mut offer).unwrap();
        assert_eq!(InitialRequest::read(&offer).unwrap(),Some(request.clone()));
        assert!(crate::native_quality::Request::read(&offer).unwrap().is_none());
        assert!(offer.unmarshal().unwrap().attributes.iter().any(|v|v.key==INITIAL_REQUEST));
        assert!(request.add(&mut offer).is_err());
        let mut answer=description();
        advertise_initial(&mut answer).unwrap();
        let reply=virtual_reply();
        reply.add(&mut answer).unwrap();
        assert!(initial_offered(&answer).unwrap());
        assert_eq!(InitialReply::read(&answer).unwrap(),Some(reply));
        assert!(advertise_initial(&mut answer).is_err());
        for (key,value) in [
            (INITIAL_REQUEST,serde_json::json!({"version":2,"value":request})),
            (INITIAL_REQUEST,serde_json::json!({"version":1,"value":{"kind":"existing","id":-1,"source":"a"}})),
            (INITIAL_REQUEST,serde_json::json!({"version":1,"value":{"kind":"existing","id":0,"source":"a","extra":true}})),
            (INITIAL_REQUEST,serde_json::json!({"version":1,"value":{"kind":"virtual","spec":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false,"extra":true}}})),
            (INITIAL_REQUEST,serde_json::json!({"version":1,"value":{"kind":"virtual","spec":{"width":0,"height":1080,"refresh_millihz":60000,"hdr":false}}})),
            (INITIAL_REQUEST,serde_json::json!({"version":1,"value":{"kind":"existing","id":0,"source":"a\nb"}})),
            (INITIAL_SUPPORTED,serde_json::json!({"version":1,"value":false})),
        ] {
            let mut invalid=description();
            crate::native_quality::put(&mut invalid,key,&value).unwrap();
            assert!(if key==INITIAL_REQUEST {InitialRequest::read(&invalid).map(|_|())}else{initial_offered(&invalid).map(|_|())}.is_err());
        }
        for suffix in ["",":ff",":00",":{}"] {
            let mut invalid=description();invalid.sdp.push_str(&format!("a={INITIAL_REQUEST}{suffix}\r\n"));
            assert!(InitialRequest::read(&invalid).is_err());
        }
        let mut duplicate=offer.clone();duplicate.sdp.push_str(offer.sdp.lines().find(|v|v.starts_with(&format!("a={INITIAL_REQUEST}:"))).unwrap());duplicate.sdp.push_str("\r\n");
        assert!(InitialRequest::read(&duplicate).is_err());
        let mut large=description();large.sdp.push_str(&format!("a={INITIAL_REQUEST}:{}\r\n","0".repeat(8192)));
        assert!(InitialRequest::read(&large).is_err());
    }
    #[test]
    fn initial_client_consumes_exact_reply_once_without_replaying_after_media_epoch() {
        let client=Client::default();let reply=virtual_reply();
        client.request_initial(reply.request.clone()).unwrap();
        assert!(client.request_initial(InitialRequest::Existing{id:0,source:String::new()}).is_err());
        let mut offer=description();client.write_initial_offer(&mut offer).unwrap();
        assert_eq!(InitialRequest::read(&offer).unwrap(),Some(reply.request.clone()));
        let mut answer=description();advertise_initial(&mut answer).unwrap();reply.add(&mut answer).unwrap();
        client.read_initial_answer(&answer).unwrap();
        assert!(client.initial_supported());assert_eq!(client.take_initial_result().unwrap().unwrap(),reply);
        assert!(client.take_initial_result().is_none());
        client.catalog(7);
        let mut next=description();client.write_initial_offer(&mut next).unwrap();
        assert!(InitialRequest::read(&next).unwrap().is_none());
        let mut next_answer=description();advertise_initial(&mut next_answer).unwrap();client.read_initial_answer(&next_answer).unwrap();
        assert!(client.initial_supported());assert!(client.take_initial_result().is_none());
        assert!(client.request_initial(InitialRequest::Virtual{spec:crate::virtual_display::Spec::default()}).is_err());
    }
    #[test]
    fn initial_old_host_falls_back_only_for_existing_and_replies_must_match_request() {
        let ordinary=Client::default();ordinary.write_initial_offer(&mut description()).unwrap();
        assert!(ordinary.request_initial(virtual_reply().request).is_err());
        for supported in [false,true] {
            let client=Client::default();client.request_initial(InitialRequest::Existing{id:0,source:"display-b".into()}).unwrap();
            client.write_initial_offer(&mut description()).unwrap();let mut answer=description();
            if supported {advertise_initial(&mut answer).unwrap();}
            client.read_initial_answer(&answer).unwrap();assert!(client.take_initial_result().is_none());
            let mut next=description();client.write_initial_offer(&mut next).unwrap();assert!(InitialRequest::read(&next).unwrap().is_none());
            let virtual_client=Client::default();virtual_client.request_initial(virtual_reply().request).unwrap();virtual_client.write_initial_offer(&mut description()).unwrap();
            assert!(virtual_client.read_initial_answer(&answer).is_err());assert!(virtual_client.take_initial_result().unwrap().is_err());assert!(virtual_client.take_initial_result().is_none());
        }
        let client=Client::default();client.request_initial(InitialRequest::Existing{id:0,source:"display-a".into()}).unwrap();client.write_initial_offer(&mut description()).unwrap();
        let mut answer=description();advertise_initial(&mut answer).unwrap();
        InitialReply{request:InitialRequest::Existing{id:0,source:"display-b".into()},id:0,source:"display-b".into(),virtual_display:None}.add(&mut answer).unwrap();
        assert!(client.read_initial_answer(&answer).is_err());assert!(client.take_initial_result().is_none());
        let mut invalid=virtual_reply();invalid.virtual_display.as_mut().unwrap().spec.width+=1;assert!(invalid.add(&mut description()).is_err());
        let mut invalid=virtual_reply();invalid.source="wrong".into();assert!(invalid.add(&mut description()).is_err());
        let mut value=serde_json::to_value(virtual_reply()).unwrap();value["virtual_display"]["unknown"]=serde_json::Value::Bool(true);
        let mut invalid=description();crate::native_quality::put(&mut invalid,INITIAL_REPLY,&serde_json::json!({"version":1,"value":value})).unwrap();assert!(InitialReply::read(&invalid).is_err());
    }
    #[test]
    fn initial_source_resolves_name_before_id_without_primary_fallback_or_limit_loss() {
        let host=Host::default();let monitors=vec![monitor("a",true),monitor("b",false)];
        assert_eq!(host.prepare_initial_monitors(&InitialRequest::Existing{id:0,source:"b".into()},monitors.clone()).unwrap(),CaptureSource::Monitor("b".into()));
        let before=host.catalog();
        for request in [InitialRequest::Existing{id:0,source:"missing".into()},InitialRequest::Existing{id:999,source:String::new()}] {
            assert!(host.prepare_initial_monitors(&request,monitors.clone()).is_err());assert_eq!(host.catalog().current_id,before.current_id);
        }
        assert_eq!(host.prepare_initial_monitors(&InitialRequest::Existing{id:0,source:String::new()},monitors).unwrap(),CaptureSource::Monitor("a".into()));
        let fresh=Host::default();let monitors=(0..20).map(|n|monitor(&format!("display-{n:02}"),n==0)).collect();
        assert_eq!(fresh.prepare_initial_monitors(&InitialRequest::Existing{id:0,source:"display-19".into()},monitors).unwrap(),CaptureSource::Monitor("display-19".into()));
        assert_eq!(fresh.catalog().screens.len(),16);assert!(valid(&fresh.catalog()));
        let numeric=Host::default();let monitors:Vec<_>=(0..20).map(|n|monitor(&format!("display-{n:02}"),n==0)).collect();
        assert!(numeric.prepare_initial_monitors(&InitialRequest::Existing{id:19,source:String::new()},monitors.clone()).is_err());
        assert!(numeric.catalog().screens.is_empty());
        let request=InitialRequest::Existing{id:15,source:String::new()};
        assert_eq!(numeric.prepare_initial_monitors(&request,monitors).unwrap(),CaptureSource::Monitor("display-15".into()));
        assert_eq!(InitialReply::from_catalog(request,&numeric.catalog(),None).unwrap().id,15);
        let duplicate=Host::default();let prefix="x".repeat(96);
        assert!(duplicate.prepare_initial_monitors(&InitialRequest::Existing{id:0,source:prefix.clone()},vec![monitor(&(prefix.clone()+"a"),true),monitor(&(prefix+"b"),false)]).is_err());assert!(duplicate.catalog().screens.is_empty());
    }
    #[test]
    fn initial_reply_uses_advertised_identity_while_capture_keeps_the_full_monitor_name() {
        let host=Host::default();let raw=format!("{}\nprivate-suffix","x".repeat(96));
        let request=InitialRequest::Existing{id:0,source:"x".repeat(96)};
        assert_eq!(host.prepare_initial_monitors(&request,vec![monitor(&raw,true)]).unwrap(),CaptureSource::Monitor(raw));
        let reply=InitialReply::from_catalog(request,&host.catalog(),None).unwrap();
        let mut answer=description();reply.add(&mut answer).unwrap();
        assert_eq!(InitialReply::read(&answer).unwrap(),Some(reply));
        assert!(InitialReply::from_catalog(InitialRequest::Existing{id:0,source:String::new()},&RemoteScreens::default(),None).is_err());
    }
    fn monitor(name: &str, primary: bool) -> CaptureMonitor {
        CaptureMonitor {
            friendly_name:None,hdr_enabled:None,
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
