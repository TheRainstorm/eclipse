//! Application plugin policy, independent of accounts and device profiles.
use crate::{Config,device_trust::TrustStore};
use anyhow::{ensure,Context,Result};
use serde::{Deserialize,Serialize};
use std::{collections::BTreeMap,io::Read};

#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct PluginPreferences {
    pub enabled:bool,
    pub options:serde_json::Map<String,serde_json::Value>,
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct Preferences {pub plugins:BTreeMap<String,PluginPreferences>}
impl Preferences {
    pub const FILE:&'static str="plugins/settings.json";
    pub fn get(&self,id:&str)->PluginPreferences {self.plugins.get(id).cloned().unwrap_or_default()}
    pub fn load(config:&Config)->Result<Self> {
        let file=match std::fs::File::open(config.at(Self::FILE)) {
            Ok(file)=>file,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(Self::default()),
            Err(error)=>return Err(error).context("cannot read plugin settings"),
        };
        let mut bytes=Vec::new();file.take(65537).read_to_end(&mut bytes)?;
        ensure!(bytes.len()<=65536,"plugin settings exceed 64 KiB; original preserved");
        let value:Self=serde_json::from_slice(&bytes).context("invalid plugin settings; original preserved")?;
        ensure!(value.plugins.len()<=64,"too many plugin settings");
        for id in value.plugins.keys(){valid_id(id)?;}
        Ok(value)
    }
    /// Edit one plugin under a cross-process lock; stale editors cannot reverse
    /// another window's opt-in or overwrite other plugins and client settings.
    pub fn save_plugin(config:&Config,id:&str,expected:&PluginPreferences,value:PluginPreferences)->Result<Self> {
        valid_id(id)?;
        let directory=config.at("plugins");std::fs::create_dir_all(&directory)?;
        let _lock=TrustStore::lock_at(&directory)?;
        let mut latest=Self::load(config)?;
        ensure!(latest.get(id)==*expected,"{}",uu_i18n::text("插件设置已在其它窗口更改，请重新读取后保存"));
        latest.plugins.insert(id.into(),value);
        let bytes=serde_json::to_vec_pretty(&latest)?;
        ensure!(bytes.len()<=65536,"plugin settings exceed 64 KiB");
        crate::private_file::write_atomic(&config.at(Self::FILE),&bytes)?;
        Ok(latest)
    }
}
fn valid_id(id:&str)->Result<()> {
    ensure!(!id.is_empty()&&id.len()<=64&&id.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-'),"invalid plugin id");Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_off_merge_and_stale_editor_preserve_independent_decisions() {
        let root=std::env::temp_dir().join(format!("plugin-policy-{}",uuid::Uuid::new_v4()));
        let config=Config{data_dir:root.clone(),state_dir:root.join("state"),signaling_host:String::new()};
        let off=PluginPreferences::default();assert_eq!(Preferences::load(&config).unwrap().get("addon"),off);
        let on=PluginPreferences{enabled:true,..Default::default()};
        Preferences::save_plugin(&config,"addon",&off,on.clone()).unwrap();
        Preferences::save_plugin(&config,"another",&off,on.clone()).unwrap();
        assert!(Preferences::save_plugin(&config,"addon",&off,off.clone()).is_err());
        let latest=Preferences::save_plugin(&config,"addon",&on,off.clone()).unwrap();
        assert_eq!(latest.get("another"),on);assert_eq!(latest.get("addon"),off);
        std::fs::write(config.at(Preferences::FILE),b"broken").unwrap();
        assert!(Preferences::save_plugin(&config,"addon",&off,on).is_err());
        assert_eq!(std::fs::read(config.at(Preferences::FILE)).unwrap(),b"broken");
        std::fs::remove_dir_all(root).unwrap();
    }
}
