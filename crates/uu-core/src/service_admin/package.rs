//! Check a complete Windows package before requesting elevation. Installation
//! independently repeats its protected-path and payload checks afterwards.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn verify(directory: &Path, expected_revision: &str) -> Result<()> {
    let mut bytes=Vec::new();
    File::open(directory.join("version.json"))
        .context("缺少完整安装包的 version.json，请重新解压整个 Windows 安装包")?
        .take(64*1024+1).read_to_end(&mut bytes)?;
    ensure!(bytes.len()<=64*1024,"安装包清单过大");
    let manifest:serde_json::Value=serde_json::from_slice(&bytes)?;
    ensure!(expected_revision.len()==40 && manifest["source_commit"].as_str()==Some(expected_revision),
        "主窗口与安装包版本不一致，请将整个 Windows 安装包解压到同一目录后重试");
    for name in ["uu.exe","uu-gui.exe","libgcc_s_seh-1.dll","libstdc++-6.dll","libvpl.dll"] {
        let path=directory.join(name);
        let file=File::open(path).with_context(||format!("安装包缺少 {name}，请重新解压整个安装包"))?;
        ensure!(file.metadata()?.len()<=512*1024*1024,"安装包文件过大：{name}");
        let mut hash=Sha256::new();
        std::io::copy(&mut file.take(512*1024*1024+1),&mut hash)?;
        ensure!(manifest["sha256"][name].as_str()==Some(hex::encode(hash.finalize()).as_str()),
            "安装包文件版本不一致或已损坏：{name}；请同时更新 uu.exe、uu-gui.exe 和安装包中的其余文件");
    }
    Ok(())
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
