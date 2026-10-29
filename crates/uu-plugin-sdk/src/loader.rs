//! Discover any provider manifest next to the application; no built-in IDs.
use crate::{EntryPoint,Manifest,Plugin,API_VERSION,SDK_ID};
use anyhow::{ensure,Context,Result};
use std::{io::Read,path::{Path,PathBuf}};
use sha2::{Digest,Sha256};

pub struct Installed {pub manifest:Manifest,pub directory:PathBuf}
pub struct Loaded {
    pub plugin:Box<dyn Plugin>,
    pub manifest:Manifest,
    // Session futures, page state and provider vtables can outlive discovery
    // owners. Successful libraries stay mapped until process exit.
    _library:std::mem::ManuallyDrop<libloading::Library>,
}
pub struct Discovery {pub installed:Vec<Installed>,pub errors:Vec<String>}
pub fn discover(directory:&Path)->Discovery {
    let mut result=Discovery{installed:Vec::new(),errors:Vec::new()};
    let entries=match std::fs::read_dir(directory) {Ok(entries)=>entries,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return result,Err(e)=>{result.errors.push(e.to_string());return result;}};
    let mut paths:Vec<_>=entries.filter_map(Result::ok).map(|e|e.path()).filter(|p|p.is_dir()).collect();paths.sort();
    let mut duplicates=std::collections::BTreeSet::new();
    for directory in paths.into_iter().take(128) {
        match installed(&directory) {
            Ok(value)=> {
                if duplicates.contains(&value.manifest.id){continue;}
                if result.installed.iter().any(|p|p.manifest.id==value.manifest.id) {
                    result.errors.push(format!("duplicate plugin id: {}",value.manifest.id));
                    result.installed.retain(|p|p.manifest.id!=value.manifest.id);duplicates.insert(value.manifest.id);
                }
                else {result.installed.push(value);}
            },
            Err(error)=>result.errors.push(format!("{}: {error:#}",directory.display())),
        }
    }
    result
}
fn installed(directory:&Path)->Result<Installed> {
    let mut bytes=Vec::new();std::fs::File::open(directory.join("plugin.json"))?.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len()<=65536,"plugin manifest exceeds 64 KiB");
    let manifest:Manifest=serde_json::from_slice(&bytes).context("invalid plugin manifest")?;
    ensure!(!manifest.id.is_empty() && manifest.id.len()<=64 && manifest.id.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-'),"invalid plugin id");
    ensure!(!manifest.name.is_empty()&&manifest.name.len()<=256&&manifest.version.len()<=128,"invalid plugin display metadata");
    ensure!(manifest.library_sha256.len()==64&&manifest.library_sha256.bytes().all(|b|b.is_ascii_hexdigit()),"invalid plugin library hash");
    ensure!(manifest.data_files.len()<=32,"too many plugin data files");
    for name in &manifest.data_files {ensure!(name.len()<=128&&Path::new(name).components().count()==1&&matches!(Path::new(name).components().next(),Some(std::path::Component::Normal(_))),"plugin data files must be filenames in the plugin namespace");}
    let path=Path::new(&manifest.library);
    ensure!(path.components().count()==1 && matches!(path.components().next(),Some(std::path::Component::Normal(_))),"plugin library must be a filename in its own directory");
    ensure!(path.extension().and_then(|s|s.to_str())==Some(std::env::consts::DLL_EXTENSION),"plugin library targets another platform");
    Ok(Installed{manifest,directory:directory.into()})
}
impl Installed {
    pub fn compatible(&self)->bool {self.manifest.api_version==API_VERSION&&self.manifest.sdk_id==SDK_ID}
    pub fn verify_library(&self)->Result<Vec<u8>> {
        let mut bytes=Vec::new();std::fs::File::open(self.directory.join(&self.manifest.library))?.take(256*1024*1024+1).read_to_end(&mut bytes)?;
        ensure!(bytes.len()<=256*1024*1024,"plugin library exceeds size limit");
        ensure!(format!("{:x}",Sha256::digest(&bytes))==self.manifest.library_sha256.to_ascii_lowercase(),"plugin library hash mismatch");Ok(bytes)
    }
    pub fn load(self,context:crate::Context)->Result<Loaded> {
        ensure!(self.compatible(),"plugin requires a different application SDK; install a matching plugin build");
        self.verify_library()?;
        let path=self.directory.join(&self.manifest.library).canonicalize().context("plugin library missing")?;
        // SAFETY: installed native code is trusted. Both its declared manifest
        // and exported fixed-size metadata must match this exact SDK.
        unsafe {
            let library=libloading::Library::new(path)?;
            let entry=library.get::<EntryPoint>(b"eclipse_plugin_entry_v1\0")?();
            ensure!(!entry.is_null(),"plugin returned no metadata");
            #[repr(C)]
            struct Header {api_version:u32,struct_bytes:u32}
            let header=&*entry.cast::<Header>();
            ensure!(header.api_version==API_VERSION&&header.struct_bytes==std::mem::size_of::<crate::Entry>() as u32,"plugin metadata layout does not match the installed SDK");
            let entry=&*entry;
            ensure!(entry.sdk_id==SDK_ID.as_bytes(),"plugin metadata does not match the installed SDK");
            let plugin=(entry.create)(context)?;
            Ok(Loaded{plugin,manifest:self.manifest,_library:std::mem::ManuallyDrop::new(library)})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifests_are_provider_neutral_and_incompatible_code_is_never_loaded() {
        let root=std::env::temp_dir().join(format!("extension-discovery-{}",std::process::id()));
        let addon=root.join("example-addon");std::fs::create_dir_all(&addon).unwrap();
        let mut manifest=Manifest{api_version:API_VERSION,sdk_id:"different-sdk".into(),id:"example-addon".into(),name:"Example addon".into(),version:"1".into(),library:format!("example.{}",std::env::consts::DLL_EXTENSION),library_sha256:"0".repeat(64),data_files:Vec::new()};
        std::fs::write(addon.join("plugin.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
        let mut found=discover(&root);assert!(found.errors.is_empty());assert_eq!(found.installed.len(),1);
        let candidate=found.installed.pop().unwrap();assert!(!candidate.compatible());
        let config=crate::engine::Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:String::new()};
        assert!(candidate.load(crate::Context::new(&config,"example-addon")).err().unwrap().to_string().contains("different application SDK"));
        manifest.sdk_id=SDK_ID.into();manifest.library="../outside.dll".into();
        std::fs::write(addon.join("plugin.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
        let found=discover(&root);assert!(found.installed.is_empty());assert_eq!(found.errors.len(),1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
