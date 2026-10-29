//! Actual native loading; no desktop, remote credentials or network service.
#![cfg(target_os="linux")]
use sha2::{Digest,Sha256};
use uu_plugin_sdk::{engine::{Config,plugins::PluginPreferences},Manifest,API_VERSION,SDK_ID,runtime::Runtime};
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn separate_library_loads_only_after_opt_in_and_shares_async_and_ui_owners() {
    let root=std::env::temp_dir().join(format!("eclipse-dll-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();
    let deps=std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let addon=root.join("plugins/example-addon");std::fs::create_dir_all(&addon).unwrap();
    let library=addon.join("libexample.so");
    let output=std::process::Command::new("rustc").args(["--edition=2021","--crate-type=cdylib","-C","prefer-dynamic"])
        .arg("-L").arg(format!("dependency={}",deps.display())).arg("--extern")
        .arg(format!("uu_plugin_sdk={}",deps.join("libuu_plugin_sdk.so").display()))
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extension.rs"))
        .arg("-o").arg(&library).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let manifest=Manifest{api_version:API_VERSION,sdk_id:SDK_ID.into(),id:"example-addon".into(),name:"Example addon".into(),version:"1".into(),library:"libexample.so".into(),library_sha256:format!("{:x}",Sha256::digest(std::fs::read(&library).unwrap())),data_files:Vec::new()};
    std::fs::write(addon.join("plugin.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
    let config=Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:String::new()};
    let marker=config.at("plugins/example-addon/loaded");
    let mut runtime=Runtime::new(root.join("plugins"),config.clone());
    runtime.apply(&Default::default());assert!(!marker.exists());assert!(runtime.get("example-addon").is_err());
    runtime.save(&config,"example-addon",&Default::default(),PluginPreferences{enabled:true,..Default::default()}).unwrap();
    assert!(marker.is_file());
    let running=runtime.get("example-addon").unwrap();
    running.plugin.cli(Vec::new()).unwrap();
    let mut page=running.plugin.gui().unwrap();
    let ctx=uu_plugin_sdk::egui::Context::default();
    let _=ctx.run(Default::default(),|ctx|{uu_plugin_sdk::egui::CentralPanel::default().show(ctx,|ui|{assert!(page.show(ui,"fixture").is_empty());});});
    let enabled=PluginPreferences{enabled:true,..Default::default()};
    runtime.save(&config,"example-addon",&enabled,Default::default()).unwrap();
    assert!(runtime.get("example-addon").is_err());assert_eq!(running.plugin.pages()[0].id,"fixture");
    drop(runtime);drop(running);drop(page);std::fs::remove_dir_all(root).unwrap();
}
