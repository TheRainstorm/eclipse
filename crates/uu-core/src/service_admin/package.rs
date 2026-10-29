//! Check a complete Windows package before requesting elevation. Installation
//! independently repeats its protected-path and payload checks afterwards.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn verify(directory: &Path, expected_revision: &str) -> Result<()> {
    let mut bytes=Vec::new();
    File::open(directory.join("version.json"))
        .context(uu_i18n::text("缺少完整安装包的 version.json，请重新解压整个 Windows 安装包"))?
        .take(64*1024+1).read_to_end(&mut bytes)?;
    ensure!(bytes.len()<=64*1024,"{}", uu_i18n::format!("安装包清单过大"));
    let manifest:serde_json::Value=serde_json::from_slice(&bytes)?;
    ensure!(expected_revision.len()==40 && manifest["source_commit"].as_str()==Some(expected_revision),
        "{}", uu_i18n::format!("主窗口与安装包版本不一致，请将整个 Windows 安装包解压到同一目录后重试"));
    for name in executable_files(&manifest)? {
        let path=directory.join(name);
        let file=File::open(path).with_context(||uu_i18n::format!("安装包缺少 {name}，请重新解压整个安装包"))?;
        ensure!(file.metadata()?.len()<=512*1024*1024,"{}", uu_i18n::format!("安装包文件过大：{name}"));
        let mut hash=Sha256::new();
        std::io::copy(&mut file.take(512*1024*1024+1),&mut hash)?;
        ensure!(manifest["sha256"][name].as_str()==Some(hex::encode(hash.finalize()).as_str()),
            "{}", uu_i18n::format!("安装包文件版本不一致或已损坏：{name}；请完整更新 Eclipse 安装包"));
    }
    Ok(())
}

/// The elevated installer uses the same bounded allowlist as its preflight.
pub fn executable_files(manifest:&serde_json::Value)->Result<Vec<&str>> {
    let version=match manifest.get("format_version") {None=>1,Some(value)=>value.as_u64().context("invalid package format")?};
    ensure!((1..=5).contains(&version),"unsupported package format");
    let mut files=vec!["uu.exe","uu-gui.exe","libgcc_s_seh-1.dll","libstdc++-6.dll","libvpl.dll"];
    if version==5 {files[0]="eclipse-cli.exe";files[1]="eclipse.exe";}
    if manifest["format_version"].as_u64().unwrap_or(1)>=3 {files.push("libpyrowave-shared-1.dll");}
    if version>=4 {
        let runtime=manifest["plugin_runtime"].as_array().context("missing SDK runtime list")?;
        ensure!(runtime.len()==2&&runtime[0]=="uu_plugin_sdk.dll","invalid SDK runtime list");
        let standard=runtime[1].as_str().context("invalid compiler runtime")?;
        let hash=standard.strip_prefix("std-").and_then(|s|s.strip_suffix(".dll")).context("invalid compiler runtime name")?;
        ensure!(!hash.is_empty()&&hash.len()<=64&&hash.bytes().all(|c|c.is_ascii_hexdigit()),"invalid compiler runtime hash");
        let sdk=manifest["plugin_sdk_id"].as_str().context("missing SDK identity")?;
        ensure!(sdk.len()==64&&sdk.bytes().all(|c|c.is_ascii_hexdigit()),"invalid SDK identity");
        files.extend(["uu_plugin_sdk.dll",standard]);
    }
    Ok(files)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn incomplete_or_mixed_packages_fail_before_any_privileged_operation(){
        let root=std::env::temp_dir().join(format!("service-package-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let revision="a".repeat(40);
        let mut hashes=serde_json::Map::new();
        for name in ["uu.exe","uu-gui.exe","libgcc_s_seh-1.dll","libstdc++-6.dll","libvpl.dll"] {
            std::fs::write(root.join(name),name).unwrap();hashes.insert(name.into(),hex::encode(Sha256::digest(name)).into());
        }
        std::fs::write(root.join("version.json"),serde_json::to_vec(&serde_json::json!({"source_commit":revision,"sha256":hashes})).unwrap()).unwrap();
        verify(&root,&revision).unwrap();
        assert!(verify(&root,&"b".repeat(40)).is_err());
        std::fs::write(root.join("uu.exe"),"old CLI").unwrap();
        assert!(verify(&root,&revision).unwrap_err().to_string().contains("uu.exe"));
        std::fs::remove_file(root.join("uu.exe")).unwrap();
        assert!(verify(&root,&revision).unwrap_err().to_string().contains("缺少"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
