//! Installed extensions supply their own pages and options. This host only
//! owns discovery, explicit opt-in and generic session launch actions.
use eframe::egui;
use std::{collections::BTreeMap,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering},mpsc}};
use uu_core::{Config,plugins::{Preferences,PluginPreferences}};
use uu_plugin_sdk::{Action,Page,PageState,Placement,runtime::Runtime};

pub(super) struct Manager {
    config:Option<Config>,
    runtime:Option<Arc<Mutex<Runtime>>>,
    preferences:Preferences,
    pages:BTreeMap<String,Box<dyn PageState>>,
    error:String,
    notice:String,
    watch:Option<Watch>,
}
struct Watch {
    stop:Arc<AtomicBool>,epoch:Arc<AtomicU64>,thread:std::thread::JoinHandle<()>,
    values:mpsc::Receiver<(u64,Result<Preferences,String>)>,
}
impl Drop for Watch {fn drop(&mut self){self.stop.store(true,Ordering::Release);self.thread.thread().unpark();}}
impl Manager {
    pub fn new(config:Option<&Config>,ctx:&egui::Context)->Self {
        let value=config.ok_or_else(||"configuration directory unavailable".to_owned()).and_then(|c|Preferences::load(c).map_err(|e|e.to_string()));
        let (preferences,error)=match value {Ok(value)=>(value,String::new()),Err(error)=>(Preferences::default(),error)};
        let runtime=config.map(uu_plugin_sdk::runtime::shared);
        let watch=config.map(|config| {
            let config=config.clone();let ctx=ctx.clone();let stop=Arc::new(AtomicBool::new(false));let stopped=stop.clone();
            let epoch=Arc::new(AtomicU64::new(0));let generation=epoch.clone();let(tx,values)=mpsc::sync_channel(1);
            let thread=std::thread::spawn(move|| {
                let mut previous=None;
                while !stopped.load(Ordering::Acquire) {
                    let at=generation.load(Ordering::Acquire);
                    let value=Preferences::load(&config).map_err(|e|e.to_string());
                    if previous.as_ref()!=Some(&(at,value.clone()))&&tx.try_send((at,value.clone())).is_ok(){previous=Some((at,value));ctx.request_repaint();}
                    if !stopped.load(Ordering::Acquire){std::thread::park_timeout(std::time::Duration::from_millis(750));}
                }
            });
            Watch{stop,epoch,thread,values}
        });
        Self{config:config.cloned(),runtime,preferences,pages:BTreeMap::new(),error,notice:String::new(),watch}
    }
    pub fn poll(&mut self) {
        if let Some(watch)=&self.watch {
            while let Ok((epoch,value))=watch.values.try_recv() {
                if epoch!=watch.epoch.load(Ordering::Acquire){continue;}
                match value {
                    Ok(value)=>{self.preferences=value;self.error.clear();},
                    Err(error)=>self.error=error,
                }
                if let Some(runtime)=&self.runtime {let off=Preferences::default();runtime.lock().unwrap().apply(if self.error.is_empty(){&self.preferences}else{&off});}
            }
        }
        if let Some(runtime)=&self.runtime {let runtime=runtime.lock().unwrap();self.pages.retain(|id,_|runtime.get(id).is_ok());}
    }
    pub fn enabled(&self,id:&str)->bool {self.runtime.as_ref().is_some_and(|r|r.lock().unwrap().get(id).is_ok())}
    pub fn client_support(&self,id:&str)->uu_plugin_sdk::ClientSupport {
        self.runtime.as_ref().and_then(|r|r.lock().unwrap().get(id).ok()).map(|p|p.plugin.client_support()).unwrap_or_default()
    }
    pub fn client_settings(&self,id:&str,defaults:uu_core::client_settings::ClientSettings)->anyhow::Result<uu_core::client_settings::ClientSettings> {
        let loaded=self.runtime.as_ref().ok_or_else(||anyhow::anyhow!("plugin runtime unavailable"))?.lock().unwrap().get(id)?;
        loaded.plugin.client_settings(defaults)
    }
    pub fn save_client(&self,id:&str,settings:uu_core::client_settings::ClientSettings)->anyhow::Result<()> {
        let loaded=self.runtime.as_ref().ok_or_else(||anyhow::anyhow!("plugin runtime unavailable"))?.lock().unwrap().get(id)?;
        loaded.plugin.save_client(settings)
    }
    pub fn connect(&self,id:&str,target:zeroize::Zeroizing<String>,client:uu_core::client_settings::ClientSettings,stats:Arc<uu_core::media::MediaStats>)->anyhow::Result<uu_core::controller::ControllerHandle> {
        let loaded=self.runtime.as_ref().ok_or_else(||anyhow::anyhow!("plugin runtime unavailable"))?.lock().unwrap().get(id)?;
        loaded.plugin.connect(target,client,stats)
    }
    pub fn pages(&self,placement:Placement)->Vec<(String,Page)> {
        let active=self.runtime.as_ref().map(|r|r.lock().unwrap().active()).unwrap_or_default();
        active.into_iter().flat_map(|(id,p)|p.plugin.pages().into_iter().filter(move|p|p.placement==placement).map(move|p|(id.clone(),p))).collect()
    }
    pub fn show_page(&mut self,ui:&mut egui::Ui,id:&str,page:&str)->Vec<Action> {
        if !self.pages.contains_key(id) {
            let result=self.runtime.as_ref().ok_or_else(||anyhow::anyhow!("plugin runtime unavailable"))
                .and_then(|runtime|runtime.lock().unwrap().get(id)).and_then(|loaded|loaded.plugin.gui());
            match result {Ok(state)=>{self.pages.insert(id.into(),state);},Err(error)=>{ui.colored_label(super::theme::DANGER,error.to_string());return Vec::new();}}
        }
        self.pages.get_mut(id).map(|state|state.show(ui,page)).unwrap_or_default()
    }
    pub fn settings(&mut self,ui:&mut egui::Ui)->Vec<(String,Action)> {
        let mut actions=Vec::new();
        ui.label(uu_i18n::text("插件默认关闭，手动启用后提供对应功能。"));
        ui.label(uu_i18n::text("将插件包解压到软件目录，再点击重新扫描。"));
        if !self.error.is_empty(){ui.colored_label(super::theme::DANGER,&self.error);}
        if !self.notice.is_empty(){ui.label(&self.notice);}
        let Some(runtime)=self.runtime.clone() else{return actions;};
        if ui.button(uu_i18n::text("重新扫描插件")).clicked(){let mut r=runtime.lock().unwrap();r.rescan();r.apply(&self.preferences);}
        let (directory,records,errors)={let r=runtime.lock().unwrap();(r.directory.clone(),r.records.iter().map(|(id,p)|(id.clone(),p.manifest.clone(),p.enabled,p.compatible,p.error.clone())).collect::<Vec<_>>(),r.errors.clone())};
        ui.label(egui::RichText::new(directory.display().to_string()).small().color(super::theme::MUTED));
        for error in errors {ui.colored_label(super::theme::DANGER,error);}
        if records.is_empty(){ui.add_space(16.);ui.label(uu_i18n::text("尚未安装插件"));}
        for (id,manifest,mut enabled,compatible,error) in records {
            ui.add_space(12.);
            egui::Frame::group(ui.style()).inner_margin(16.).show(ui,|ui| {
                ui.horizontal(|ui| {
                    ui.strong(&manifest.name);ui.label(format!("v{}",manifest.version));
                    if ui.add_enabled(compatible&&self.error.is_empty(),egui::Checkbox::new(&mut enabled,uu_i18n::text("启用"))).changed() {
                        let expected=self.preferences.get(&id);let value=PluginPreferences{enabled,..expected.clone()};
                        let result=self.config.as_ref().ok_or_else(||anyhow::anyhow!("configuration directory unavailable"))
                            .and_then(|config|runtime.lock().unwrap().save(config,&id,&expected,value));
                        match result {
                            Ok(value)=> {
                                self.preferences=value;self.notice=uu_i18n::text("插件设置已保存；现有串流继续，主机入口需重启共享服务生效。").into();
                                if let Some(watch)=&self.watch {watch.epoch.fetch_add(1,Ordering::AcqRel);watch.thread.thread().unpark();}
                                ui.ctx().request_repaint();
                            },
                            Err(error)=>self.notice=error.to_string(),
                        }
                    }
                });
                if !compatible {ui.colored_label(super::theme::DANGER,uu_i18n::text("插件与当前程序不匹配，请下载匹配版本。"));}
                if !error.is_empty(){ui.colored_label(super::theme::DANGER,error);}
                if enabled&&self.enabled(&id) {
                    for page in self.pages(Placement::Settings).into_iter().filter_map(|(plugin,page)|(plugin==id).then_some(page)) {
                        ui.separator();for action in self.show_page(ui,&id,&page.id){actions.push((id.clone(),action));}
                    }
                }
            });
        }
        actions
    }
}

impl super::App {
    pub(super) fn select_plugin_page(&mut self,id:String,page:String) {
        if !self.plugins.enabled(&id){return;}
        self.select_workspace_page(super::workspace_ui::Page::Plugin);
        self.workspace.route.plugin=Some((id,page));
    }
    pub(super) fn plugin_page_ui(&mut self,ctx:&egui::Context) {
        let Some((id,page))=self.workspace.route.plugin.clone() else{return;};
        let mut actions=Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(super::theme::CANVAS).inner_margin(24.)).show(ctx,|ui| {
            egui::ScrollArea::vertical().show(ui,|ui|{actions=self.plugins.show_page(ui,&id,&page);});
        });
        self.plugin_actions(actions.into_iter().map(|a|(id.clone(),a)).collect());
    }
    pub(super) fn plugin_actions(&mut self,actions:Vec<(String,Action)>) {
        for (id,action) in actions {
            match action {
                Action::Connect{identity,target,name,view_only}=> {
                    let launch=super::sessions::StreamTarget{connection:super::sessions::ConnectionTarget::Plugin{plugin:id,identity},alias:name,view_only};
                    let result=std::env::current_exe().and_then(|exe|self.sessions.open_plugin(&exe,&launch,&target,self.platform.origin()));
                    if let Err(error)=result {self.status=error.to_string();}
                },
                Action::RefreshDevices=>{uu_core::plugins::registry().invalidate();self.workspace.reload();},
                Action::ProviderReset=>{uu_core::plugins::registry().invalidate();if self.workspace.host.uses_provider(&id){self.workspace.host.stop();}},
            }
        }
    }
}
