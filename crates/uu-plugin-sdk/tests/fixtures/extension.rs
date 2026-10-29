use uu_plugin_sdk::{anyhow::Result,Context,Plugin,Page,Placement,PageState,egui};
struct Extension;
impl Plugin for Extension {
    fn pages(&self)->Vec<Page>{vec![Page{id:"fixture".into(),title:"Fixture".into(),placement:Placement::Navigation}]}
    fn gui(&self)->Result<Box<dyn PageState>>{Ok(Box::new(State))}
    fn cli(&self,_arguments:Vec<String>)->Result<()> {
        // Resolving this handle and timer in a separately compiled DLL proves
        // that the host and extension use the same Tokio runtime instance.
        let handle=uu_plugin_sdk::tokio::runtime::Handle::try_current()?;
        uu_plugin_sdk::tokio::task::block_in_place(||handle.block_on(async {
            uu_plugin_sdk::tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }));Ok(())
    }
}
struct State;
impl PageState for State {
    fn show(&mut self,ui:&mut egui::Ui,_page:&str)->Vec<uu_plugin_sdk::Action>{ui.label("Fixture UI from a separate DLL");Vec::new()}
}
fn create(context:Context)->Result<Box<dyn Plugin>> {
    std::fs::create_dir_all(&context.data.data_dir)?;
    std::fs::write(context.data.at("loaded"),b"factory called")?;
    Ok(Box::new(Extension))
}
#[no_mangle]
pub extern "C" fn eclipse_plugin_entry_v1()->*const uu_plugin_sdk::Entry {
    static ENTRY:uu_plugin_sdk::Entry=uu_plugin_sdk::entry(create);&ENTRY
}
