//! Approximate client usage. One workspace samples all its screens together;
//! a disk worker merges small deltas without putting file IO on the UI thread.
use eframe::egui;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::{Path,PathBuf}, sync::mpsc, thread::JoinHandle, time::{Duration,Instant}};

#[derive(Clone,Default,Serialize,Deserialize)]
struct Device {name:String,millis:u64,connections:u64}
type Devices=BTreeMap<String,Device>;
#[derive(Default,Serialize,Deserialize)]
struct Data {devices:Devices}
fn merge(into:&mut Devices,delta:Devices) {
    for (id,value) in delta {
        let entry=into.entry(id).or_default();entry.name=value.name;
        entry.millis=entry.millis.saturating_add(value.millis);
        entry.connections=entry.connections.saturating_add(value.connections);
    }
}
fn read(path:&Path)->anyhow::Result<Data> {
    match crate::settings::read_bounded(path,1024*1024) {
        Ok(bytes)=>Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(Data::default()),
        Err(error)=>Err(error.into()),
    }
}
fn persist(path:&Path,delta:&Devices)->anyhow::Result<()> {
    std::fs::create_dir_all(path.parent().ok_or_else(||anyhow::anyhow!("statistics directory is missing"))?)?;
    let lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock"))?;
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {match lock.try_lock_exclusive(){
        Ok(())=>break,
        Err(error) if error.kind()==std::io::ErrorKind::WouldBlock && Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(10)),
        Err(error)=>return Err(error.into()),
    }}
    let mut data=read(path)?;merge(&mut data.devices,delta.clone());
    crate::settings::save_json(&data,path)
}

pub(super) struct Tracker {
    active:BTreeMap<String,String>,pending:Devices,last:Instant,flushed:Instant,
    send:Option<mpsc::SyncSender<Devices>>,worker:Option<JoinHandle<()>>,
}
impl Tracker {
    pub fn new(path:Option<PathBuf>)->Self {
        let (send,worker)=if let Some(path)=path {
            let (send,receive)=mpsc::sync_channel::<Devices>(8);
            let worker=std::thread::spawn(move|| {
                let mut pending=Devices::new();
                while let Ok(delta)=receive.recv() {
                    merge(&mut pending,delta);
                    match persist(&path,&pending) {
                        Ok(())=>pending.clear(),Err(error)=>eprintln!("stream statistics: {error:#}"),
                    }
                }
                if !pending.is_empty(){if let Err(error)=persist(&path,&pending){eprintln!("stream statistics final save: {error:#}");}}
            });
            (Some(send),Some(worker))
        }else{(None,None)};
        Self{active:Default::default(),pending:Default::default(),last:Instant::now(),flushed:Instant::now(),send,worker}
    }
    pub fn tick(&mut self,devices:impl IntoIterator<Item=(String,String)>) {
        self.sample(Instant::now(),devices.into_iter().collect());
        if !self.pending.is_empty() && self.flushed.elapsed()>=Duration::from_secs(5) {
            if let Some(send)=&self.send {
                match send.try_send(std::mem::take(&mut self.pending)) {
                    Ok(())=>self.flushed=Instant::now(),
                    Err(mpsc::TrySendError::Full(delta)|mpsc::TrySendError::Disconnected(delta))=>merge(&mut self.pending,delta),
                }
            }
        }
    }
    fn sample(&mut self,now:Instant,next:BTreeMap<String,String>) {
        let elapsed=now.saturating_duration_since(self.last);
        // A suspended or stalled event loop is not evidence of streaming.
        let millis=if elapsed>Duration::from_secs(30){0}else{elapsed.as_millis() as u64};
        for (id,name) in &self.active {
            let entry=self.pending.entry(id.clone()).or_default();entry.name=name.clone();
            entry.millis=entry.millis.saturating_add(millis);
        }
        for (id,name) in &next {if !self.active.contains_key(id) {
            let entry=self.pending.entry(id.clone()).or_default();entry.name=name.clone();entry.connections+=1;
        }}
        self.last=now;self.active=next;
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        self.sample(Instant::now(),Default::default());
        if let Some(send)=self.send.take(){if !self.pending.is_empty(){let _=send.send(std::mem::take(&mut self.pending));}}
        if let Some(worker)=self.worker.take(){let _=worker.join();}
    }
}

pub(super) fn device(target:&crate::sessions::StreamTarget)->Option<(String,String)> {
    use crate::sessions::ConnectionTarget;
    let id=match target.connection.base() {
        ConnectionTarget::NativeProfile{id,..}=>id.clone(),
        ConnectionTarget::NativeDevice(id)=>format!("native:{id}"),
        ConnectionTarget::Plugin{plugin,identity}=>format!("plugin:{plugin}:{identity}"),
        _=>return None,
    };
    Some((id,target.alias.clone()))
}
pub(super) fn duration(millis:u64)->String {
    let seconds=millis/1000;
    if seconds>=3600 {uu_i18n::format!("{} 小时 {} 分钟",seconds/3600,seconds/60%60)}
    else{uu_i18n::format!("{} 分钟 {} 秒",seconds/60,seconds%60)}
}

#[derive(Default)]
pub(super) struct View {
    pub open:bool,data:Data,error:String,read:Option<mpsc::Receiver<Result<Data,String>>>,next:Option<Instant>,
}
impl View {
    pub fn device_time(&self,id:&str)->String {duration(self.data.devices.get(id).map_or(0,|entry|entry.millis))}
    pub fn poll(&mut self,path:Option<PathBuf>,ctx:&egui::Context) {
        if let Some(result)=self.read.as_ref().and_then(|read|read.try_recv().ok()) {
            self.read=None;self.next=Some(Instant::now()+Duration::from_secs(5));
            match result {Ok(data)=>{self.data=data;self.error.clear();},Err(error)=>self.error=error}
        }
        if self.read.is_none()&&self.next.is_none_or(|at|Instant::now()>=at) {
            if let Some(path)=path {
                let (send,receive)=mpsc::channel();let context=ctx.clone();
                std::thread::spawn(move||{let _=send.send(read(&path).map_err(|error|format!("{error:#}")));context.request_repaint();});
                self.read=Some(receive);
            }
        }
        ctx.request_repaint_after(Duration::from_secs(5));
    }
    pub fn show(&mut self,ctx:&egui::Context,config:Option<&uu_core::Config>,book:&uu_core::device_book::Book) {
        if !self.open{return;}
        self.poll(config.map(|config|config.at("stream-statistics.json")),ctx);
        let mut open=true;
        egui::Window::new(uu_i18n::text("串流统计")).id("stream-statistics".into()).open(&mut open)
            .collapsible(false).default_width(660.).max_width((ctx.screen_rect().width()-40.).max(280.))
            .show(ctx,|ui| {
                let total=self.data.devices.values().map(|entry|entry.millis).fold(0u64,u64::saturating_add);
                let connections=self.data.devices.values().map(|entry|entry.connections).sum::<u64>();
                ui.columns(3,|columns|{
                    columns[0].weak(uu_i18n::text("累计串流时长"));columns[0].heading(duration(total));
                    columns[1].weak(uu_i18n::text("连接次数"));columns[1].heading(connections.to_string());
                    columns[2].weak(uu_i18n::text("使用过的设备"));columns[2].heading(self.data.devices.len().to_string());
                });
                ui.add_space(12.);
                ui.horizontal_wrapped(|ui|{
                    for (reached,label) in [(connections>0,"首次串流"),(total>=3_600_000,"累计一小时"),(total>=36_000_000,"累计十小时"),(total>=360_000_000,"累计百小时")] {
                        ui.colored_label(if reached{crate::theme::palette(ui.visuals()).success}else{crate::theme::palette(ui.visuals()).muted},format!("{} {}",if reached{"✓"}else{"○"},uu_i18n::text(label)));
                    }
                });
                ui.separator();ui.strong(uu_i18n::text("各设备串流时长"));
                let mut devices:Vec<_>=self.data.devices.iter().collect();devices.sort_by(|a,b|b.1.millis.cmp(&a.1.millis));
                egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-320.).max(120.)).show(ui,|ui| {
                    for (id,entry) in devices {
                        let name=book.device(id).map(|profile|profile.name.as_str()).unwrap_or(&entry.name);
                        ui.horizontal(|ui|{ui.label(name);ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.label(duration(entry.millis));});});
                    }
                });
                if !self.error.is_empty(){ui.colored_label(crate::theme::palette(ui.visuals()).danger,&self.error);}
                ui.separator();ui.weak(uu_i18n::text("从此版本开始累计；同一设备多屏只计一次，多台设备的时长相加。"));
            });
        self.open=open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screens_share_one_device_clock_and_separate_devices_accumulate_independently() {
        let mut tracker=Tracker::new(None);let start=tracker.last;
        let same=vec![("a".into(),"A".into()),("a".into(),"A screen 2".into())];
        tracker.sample(start,same.into_iter().collect());
        tracker.sample(start+Duration::from_secs(10),BTreeMap::from([("a".into(),"A".into()),("b".into(),"B".into())]));
        tracker.sample(start+Duration::from_secs(20),BTreeMap::new());
        assert_eq!(tracker.pending["a"].millis,20_000);assert_eq!(tracker.pending["b"].millis,10_000);
        assert_eq!(tracker.pending["a"].connections,1);assert_eq!(tracker.pending["b"].connections,1);
        tracker.active.clear();
    }
    #[test]
    fn disk_updates_merge_and_survive_reopening() {
        let root=std::env::temp_dir().join(format!("eclipse-usage-{}",uuid::Uuid::new_v4()));let path=root.join("stats.json");
        let delta=BTreeMap::from([("a".into(),Device{name:"A".into(),millis:10_000,connections:1})]);
        persist(&path,&delta).unwrap();persist(&path,&delta).unwrap();
        let data=read(&path).unwrap();assert_eq!(data.devices["a"].millis,20_000);assert_eq!(data.devices["a"].connections,2);
        std::fs::remove_dir_all(root).unwrap();
    }
}
