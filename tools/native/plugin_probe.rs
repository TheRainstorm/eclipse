//! Standalone host compiled against the application's actual shared SDK.
//! Invoke only with a generated configuration directory and explicit plugin ID.
use uu_plugin_sdk::{anyhow::{ensure,Result},engine::{Config,plugins::PluginPreferences},runtime::Runtime};
fn main()->Result<()> {
    let arguments=std::env::args().collect::<Vec<_>>();
    ensure!(arguments.len()==4,"expected plugin directory, generated config directory and plugin ID");
    let config=Config{data_dir:arguments[2].clone().into(),state_dir:std::path::PathBuf::from(&arguments[2]).join("state"),signaling_host:String::new()};
    ensure!(!config.data_dir.exists(),"probe configuration must be a new generated directory");
    let id=&arguments[3];
    let mut runtime=Runtime::new(arguments[1].clone().into(),config.clone());
    ensure!(runtime.get(id).is_err(),"extension was enabled without opt-in");
    runtime.save(&config,id,&Default::default(),PluginPreferences{enabled:true,..Default::default()})?;
    let loaded=runtime.get(id)?;
    let pages=loaded.plugin.pages();
    if let Some(page)=pages.iter().find(|p|p.placement==uu_plugin_sdk::Placement::Settings) {
        let mut gui=loaded.plugin.gui()?;let ctx=uu_plugin_sdk::egui::Context::default();
        let _=ctx.run(Default::default(),|ctx|{uu_plugin_sdk::egui::CentralPanel::default().show(ctx,|ui|{gui.show(ui,&page.id);});});
    }
    runtime.save(&config,id,&PluginPreferences{enabled:true,..Default::default()},Default::default())?;
    ensure!(runtime.get(id).is_err(),"disabled extension still admits new requests");
    ensure!(loaded.plugin.pages().len()==pages.len(),"disabling invalidated existing extension objects");
    println!("independent library: discovery, default-off, enable, UI, disable and retained objects passed");Ok(())
}
