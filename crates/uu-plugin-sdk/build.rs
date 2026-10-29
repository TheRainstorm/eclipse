use sha2::{Digest,Sha256};
use std::{path::{Path,PathBuf},process::Command};

fn sources(directory:&Path,files:&mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("SDK source directory") {
        let path=entry.expect("SDK source entry").path();
        if path.is_dir(){sources(&path,files);}else if matches!(path.extension().and_then(|s|s.to_str()),Some("rs"|"toml"|"c"|"cpp"|"h"|"json")){files.push(path);}
    }
}
fn main() {
    println!("cargo:rerun-if-env-changed=ECLIPSE_PLUGIN_ENGINE_SOURCE");
    let root=std::env::var_os("ECLIPSE_PLUGIN_ENGINE_SOURCE").map(PathBuf::from).unwrap_or_else(||PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..")).canonicalize().unwrap();
    let mut files=Vec::new();
    for name in ["uu-plugin-sdk","uu-core","uu-media","uu-capture","uu-input","uu-clipboard","uu-proto","uu-i18n","uu-i18n-macros"] {
        println!("cargo:rerun-if-changed={}",root.join("crates").join(name).display());
        let directory=root.join("crates").join(name);
        sources(&directory.join("src"),&mut files);
        for filename in ["Cargo.toml","build.rs"] {let file=directory.join(filename);if file.is_file(){files.push(file);}}
    }
    files.sort();
    // Vendored engine patches participate in the same binary contract.
    sources(&root.join("vendor"),&mut files);
    files.push(root.join("Cargo.toml"));files.sort();
    files.push(root.join("crates/uu-gui/Cargo.toml"));files.push(root.join("crates/uu-cli/Cargo.toml"));files.sort();
    let mut hash=Sha256::new();hash.update(b"eclipse-native-plugin-api-v1\0");
    hash.update(std::env::var("CARGO_PKG_VERSION").unwrap());hash.update(std::env::var("TARGET").unwrap());
    let compiler=Command::new(std::env::var_os("RUSTC").unwrap()).args(["--version","--verbose"]).output().expect("Rust compiler identity");
    assert!(compiler.status.success());hash.update(compiler.stdout);
    hash.update(std::env::var("CARGO_FEATURE_VAAPI").unwrap_or_default());
    for name in ["PROFILE","OPT_LEVEL","DEBUG","CARGO_CFG_TARGET_FEATURE","CARGO_ENCODED_RUSTFLAGS"] {hash.update(name);hash.update(std::env::var(name).unwrap_or_default());hash.update([0]);}
    // Even when a separately checked-out plugin builds against these sources,
    // this is the engine checkout's lock, not the plugin workspace's lock.
    println!("cargo:rerun-if-changed={}",root.join("Cargo.lock").display());
    hash.update(std::fs::read(root.join("Cargo.lock")).expect("engine dependency lock"));
    for path in files {
        println!("cargo:rerun-if-changed={}",path.display());
        hash.update(path.strip_prefix(&root).unwrap().to_string_lossy().as_bytes());hash.update([0]);
        hash.update(std::fs::read(path).unwrap());hash.update([0]);
    }
    let id=format!("{:x}",hash.finalize());
    println!("cargo:rustc-env=ECLIPSE_PLUGIN_SDK_ID={id}");
    let target=std::env::var("TARGET").unwrap();
    let output=PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("sdk.json");
    std::fs::write(output,format!("{{\"api_version\":1,\"sdk_id\":\"{id}\",\"target\":\"{target}\"}}\n")).expect("SDK build metadata");
}
