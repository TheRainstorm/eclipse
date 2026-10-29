//! One process owns extension registration; pages keep their own UI state.
use crate::{loader::{self,Installed,Loaded},Manifest};
use anyhow::{ensure,Context,Result};
use std::{collections::BTreeMap,path::PathBuf,sync::{Arc,Mutex,OnceLock}};
use engine::{Config,plugins::{Preferences,PluginPreferences}};
use crate::engine;

pub struct Record {
    pub manifest:Manifest,
    pub compatible:bool,
    pub enabled:bool,
    pub error:String,
    candidate:Option<Installed>,
    loaded:Option<Arc<Loaded>>,
    registered:bool,
}
pub struct Runtime {
    config:Config,
    pub directory:PathBuf,
    pub records:BTreeMap<String,Record>,
    pub errors:Vec<String>,
}
impl Runtime {
    pub fn new(directory:PathBuf,config:Config)->Self {
        let mut value=Self{config,directory,records:BTreeMap::new(),errors:Vec::new()};value.rescan();value
    }
    pub fn rescan(&mut self) {
        let found=loader::discover(&self.directory);self.errors=found.errors;
        for candidate in found.installed {
            let id=candidate.manifest.id.clone();
            if let Some(old)=self.records.get(&id).filter(|r|r.loaded.is_some()) {
                if old.manifest.sdk_id!=candidate.manifest.sdk_id||old.manifest.version!=candidate.manifest.version {
                    self.errors.push(format!("plugin '{id}' was updated; restart the application to load its new version"));
                }
                continue;
            }
            self.records.insert(id,Record{compatible:candidate.compatible(),manifest:candidate.manifest.clone(),enabled:false,error:String::new(),candidate:Some(candidate),loaded:None,registered:false});
        }
    }
    fn load(&mut self,id:&str)->Result<()> {
        let record=self.records.get_mut(id).context("plugin is not installed")?;
        if record.loaded.is_none() {
            let installed=record.candidate.take().context("plugin could not be loaded; rescan before retrying")?;
            let loaded=Arc::new(installed.load(crate::Context::new(&self.config,id))?);
            if let Some(service)=loaded.plugin.service() {
                ensure!(service.descriptor().id==id,"plugin service ID differs from its manifest");
                engine::plugins::registry().register(service)?;record.registered=true;
            }
            record.loaded=Some(loaded);
        }
        Ok(())
    }
    pub fn apply(&mut self,preferences:&Preferences) {
        let ids:Vec<_>=self.records.keys().cloned().collect();
        for id in ids {
            let enabled=preferences.get(&id).enabled;
            if enabled {
                if let Err(error)=self.load(&id) {
                    let record=self.records.get_mut(&id).unwrap();record.enabled=false;record.error=format!("{error:#}");continue;
                }
            }
            let record=self.records.get_mut(&id).unwrap();
            if !enabled&&record.registered {let _=engine::plugins::registry().set_enabled(&id,false);}
            if record.enabled!=enabled {
                if let Some(loaded)=&record.loaded {if let Err(error)=loaded.plugin.set_enabled(enabled){record.error=error.to_string();record.enabled=false;continue;}}
            }
            if record.registered {
                if let Err(error)=engine::plugins::registry().set_enabled(&id,enabled){record.error=error.to_string();record.enabled=false;continue;}
            }
            record.enabled=enabled;record.error.clear();
        }
    }
    pub fn save(&mut self,config:&Config,id:&str,expected:&PluginPreferences,value:PluginPreferences)->Result<Preferences> {
        if value.enabled {self.load(id)?;}
        let preferences=Preferences::save_plugin(config,id,expected,value)?;
        self.apply(&preferences);Ok(preferences)
    }
    pub fn get(&self,id:&str)->Result<Arc<Loaded>> {
        self.records.get(id).filter(|r|r.enabled).and_then(|r|r.loaded.clone()).context("plugin is not installed or enabled")
    }
    pub fn active(&self)->Vec<(String,Arc<Loaded>)> {
        self.records.iter().filter(|(_,r)|r.enabled).filter_map(|(id,r)|r.loaded.clone().map(|p|(id.clone(),p))).collect()
    }
}
/// GUI instances and the CLI use the same directory rule. No plugin is bundled
/// or enabled implicitly; installed manifest IDs are the only discovery keys.
pub fn shared(config:&Config)->Arc<Mutex<Runtime>> {
    static RUNTIME:OnceLock<Arc<Mutex<Runtime>>>=OnceLock::new();
    RUNTIME.get_or_init(|| {
        let directory=std::env::current_exe().ok().and_then(|p|p.parent().map(|p|p.join("plugins"))).unwrap_or_default();
        let mut runtime=Runtime::new(directory,config.clone());
        match Preferences::load(config) {Ok(value)=>runtime.apply(&value),Err(error)=>runtime.errors.push(error.to_string())}
        Arc::new(Mutex::new(runtime))
    }).clone()
}
