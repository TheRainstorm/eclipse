//! Devices are the primary object. Sharing settings and connection services
//! have their own pages and never ask for a per-device shared key file.
use super::theme;
mod device_management;
mod device_menus;
mod device_details;
mod connection_settings;
use eframe::egui::{self, RichText};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use uu_core::{
    device_book::{Book, Destination, Encoding, Profile, Route, Service, Sharing, Store},
    device_trust::TrustStore,
    native::{CaptureMonitor, HostStatus},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum Page {
    #[default]
    Devices,
    Services,
    Plugin,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) enum DeviceView {
    #[default]
    List,
    Details(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum SettingsView {
    Software(super::settings_page::SoftwareSection),
    Plugins,
    About,
}
impl Default for SettingsView {
    fn default()->Self {Self::Software(Default::default())}
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) struct HomeRoute {
    pub page: Page,
    pub device: DeviceView,
    pub plugin: Option<(String,String)>,
    pub settings: SettingsView,
    pub settings_return: Page,
}
pub(super) enum Action {
    DevicesHidden(bool),
    Open(String),
    OpenPrepared(crate::sessions::StreamTarget),
    OpenConnectionServices,
    RefreshWallpaper,
    FavoriteWallpaper,
    DownloadWallpaper,
    SelfStream,
    StartSharing,
    RestartSharing,
    SaveClientDefaults(uu_core::client_settings::ClientSettings),
}
pub(super) struct Info {
    pub wallpaper_enabled:bool,
    pub wallpaper_ready:bool,
    pub wallpaper_download_ready:bool,
    pub wallpaper_size:Option<(u32,u32)>,
    pub nearby:Vec<(String,String)>,
    pub providers:Vec<uu_core::plugins::Descriptor>,
}
#[derive(Clone)]
struct DeviceEditor {
    error: String,
    profile: Option<String>,
    route: Option<String>,
    name: String,
    kind: u8,
    provider:String,
    address: String,
    device: String,
    service: String,
}
#[derive(Clone)]
struct ClientEditor {
    device:String,
    value:uu_core::client_settings::NamedClientSettings,
    loaded:Option<(String,String)>,
    section:super::settings_page::Section,
    global:bool,
    error:String,
}
impl ClientEditor {
    fn editing_id(&self)->Option<&str> {
        self.loaded.as_ref().filter(|(_,name)|name.trim()==self.value.name.trim()).map(|(id,_)|id.as_str())
    }
    fn load(&mut self,value:uu_core::client_settings::NamedClientSettings) {
        self.loaded=Some((value.id.clone(),value.name.clone()));
        self.value=value;
        self.error.clear();
    }
}

struct PreviewSlot {revision:String,texture:Option<egui::TextureHandle>,loaded:bool}
struct PreviewJob {device:String,revision:String,rx:std::sync::mpsc::Receiver<Result<super::wallpapers::Thumbnail,String>>}
pub(super) struct Workspace {
    providers:Vec<uu_core::plugins::Descriptor>,
    #[cfg(windows)]
    pub(super) background_service:super::service_ui::Entry,
    pub route: HomeRoute,
    pub book: Book,
    pub host: super::sharing_ui::HostUi,
    pub(super) host_sessions_requested: bool,
    connection_history: super::connection_history_ui::State,
    statistics:super::stream_statistics::View,
    servers: super::connection_service_ui::Servers,
    service_config: Option<uu_core::Config>,
    history_path: Option<std::path::PathBuf>,
    pub store: Option<Store>,
    pub trust: Option<TrustStore>,
    pub feedback: super::ui_feedback::Feedback,
    draft: Sharing,
    monitors: Vec<CaptureMonitor>,
    monitor_error: String,
    host_section:u8,
    service_tab:u8,
    encoder_vendor_tab:u8,
    editor: Option<DeviceEditor>,
    suspended_editor: Option<DeviceEditor>,
    device_picker: super::connection_service_ui::device_picker::DevicePicker,
    client_editor: Option<ClientEditor>,
    pub client_defaults: uu_core::client_settings::ClientSettings,
    dark_background:bool,
    devices_hidden:bool,
    device_icon_size:f32,
    device_text_size:f32,
    management:device_management::State,
    menus:device_menus::State,
    connection_draft:Option<connection_settings::Draft>,
    last_read: Instant,
    pub pairing_visible: bool,
    pairing_settings:super::pairing_ui::HostUi,
    pairing_password_configured: Option<bool>,
    autostart_checked: bool,
    previews:std::collections::HashMap<String,PreviewSlot>,
    preview_jobs:Vec<PreviewJob>,
    presence:super::device_presence::Monitor,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_draft_preserves_a_live_game_room_policy_change() {
        let mut h=Harness::new();
        let audio=!h.workspace.draft.audio;h.workspace.draft.audio=audio;
        h.workspace.store.as_ref().unwrap().edit(|book| {book.sharing.host_input.game_room=true;Ok(())}).unwrap();
        h.workspace.refresh();
        assert_eq!(h.workspace.draft.audio,audio,"refresh must retain an edited media draft");
        assert!(h.workspace.draft.host_input.game_room);
        assert!(h.workspace.save_sharing());
        assert!(h.workspace.store.as_ref().unwrap().read().unwrap().sharing.host_input.game_room);
        assert!(!h.workspace.server_dirty());
        h.workspace.store.as_ref().unwrap().edit(|book| {book.sharing.host_input.game_room=false;Ok(())}).unwrap();
        h.workspace.refresh();
        assert!(!h.workspace.server_dirty(),"a control-center update alone must not dirty the media form");
    }
    #[test]
    fn device_management_selects_without_connecting_and_deletes_the_batch() {
        let mut h=Harness::new();
        for i in 1..=2 {
            h.workspace.store.as_ref().unwrap().save_device(Profile::new(&format!("Device {i}"),Destination::Ip{address:format!("192.0.2.{i}:48998"),legacy_device:None})).unwrap();
        }
        h.workspace.refresh();h.workspace.management.active=true;
        assert!(h.click("Device 1").is_empty());
        assert_eq!(h.workspace.management.selected.len(),1);
        h.batch("全选");assert_eq!(h.workspace.management.selected.len(),2);
        let rect=h.ctx.data(|data|data.get_temp::<egui::Rect>(egui::Id::new(("batch-button","删除所选")))).unwrap();
        assert!(rect.center().x>1000.&&rect.center().y>900.,"floating controls must stay at the bottom right: {rect:?}");
        h.batch("删除所选");h.click("确认删除");
        assert!(h.workspace.store.as_ref().unwrap().read().unwrap().devices.is_empty());
    }
    #[test]
    fn hidden_devices_can_be_restored_from_the_wallpaper_menu() {
        let mut h=Harness::new();h.workspace.devices_hidden=true;
        h.frame(vec![]);
        assert!(!h.texts.iter().any(|(s,_)|s=="尚未添加设备"));
        let position=egui::pos2(100.,100.);
        for pressed in [true,false] {
            h.frame(vec![egui::Event::PointerMoved(position),egui::Event::PointerButton{pos:position,button:egui::PointerButton::Secondary,pressed,modifiers:Default::default()}]);
        }
        assert!(matches!(&h.click("显示设备列表")[..],[Action::DevicesHidden(false)]));
        h.frame(vec![]);
        assert!(h.texts.iter().any(|(s,_)|s=="尚未添加设备"));
    }
    #[test]
    #[cfg(not(windows))]
    fn extension_settings_stay_with_their_provider_between_ip_and_self_hosted_cards() {
        let mut h=Harness::new();h.workspace.route.page=Page::Services;
        let info=Info{wallpaper_enabled:false,wallpaper_ready:false,wallpaper_download_ready:false,wallpaper_size:None,nearby:Vec::new(),providers:vec![uu_core::plugins::Descriptor{id:"example-addon",name:"Generated provider",version:"1"}]};
        let draw=|ctx:&egui::Context,workspace:&mut Workspace| {
            egui::CentralPanel::default().show(ctx,|ui| {
                workspace.show_with_connections(ui,&info,&mut |ui,id| {if let Some(id)=id{assert_eq!(id,"example-addon");ui.label("Generated account controls");}else{ui.label("Generated independent protocol");}});
            });
        };
        let input=||egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1200.,1400.))),..Default::default()};
        let _=h.ctx.run(input(),|ctx|draw(ctx,&mut h.workspace));
        let output=h.ctx.run(input(),|ctx|draw(ctx,&mut h.workspace));
        fn position(shape:&egui::Shape,label:&str)->Option<f32>{match shape {
            egui::Shape::Text(text) if text.galley.job.text==label=>Some(text.pos.y),
            egui::Shape::Vec(shapes)=>shapes.iter().find_map(|s|position(s,label)),_=>None,
        }}
        let y=|label|output.shapes.iter().find_map(|s|position(&s.shape,label)).unwrap_or_else(||panic!("missing {label}"));
        assert!(y("IP 直连")<y("Generated provider"));
        assert!(y("Generated provider")<y("Generated account controls"));
        assert!(y("Generated account controls")<y("Generated independent protocol"));
        assert!(y("IP 直连")<y("自建服务器"));
        assert!(y("自建服务器")<y("Generated provider"));
    }
    struct Harness {
        workspace: Workspace,
        ctx: egui::Context,
        root: PathBuf,
        time: f64,
        focused: bool,
        texts: Vec<(String, egui::Pos2)>,
        text_lefts: Vec<(String, f32)>,


    }
    #[test]
    fn repeated_manual_add_updates_the_existing_named_device_without_losing_its_data() {
        let mut h=Harness::new();
        for address in ["192.0.2.40:48998","192.0.2.41:48998"] {
            h.workspace.new_device(None,None);
            let editor=h.workspace.editor.as_mut().unwrap();
            editor.name="Generated repeated host".into();editor.address=address.into();
            h.click("仅保存");
            assert!(h.workspace.editor.is_none());
        }
        let profile=h.workspace.book.devices.iter().find(|profile|profile.redirect.is_none()).unwrap();
        let id=profile.id.clone();
        assert_eq!(h.workspace.book.devices.len(),1);
        assert_eq!(profile.routes.len(),2);
        assert_eq!(profile.name,"Generated repeated host");
        h.workspace.new_device(None,None);
        let editor=h.workspace.editor.as_mut().unwrap();editor.name="Other reported name".into();editor.address="192.0.2.40:48998".into();
        h.click("仅保存");
        assert_eq!(h.workspace.book.devices.len(),1);assert_eq!(h.workspace.book.devices[0].id,id);
        assert_eq!(h.workspace.book.devices[0].name,"Generated repeated host");
    }
    #[test]
    fn plugin_routes_follow_current_provider_visibility_without_deleting_saved_configuration() {
        let mut h=Harness::new();
        let profile=Profile::new("Generated host",Destination::Plugin {plugin:"example-addon".into(),device:"generated-target".into()});
        h.workspace.store.as_ref().unwrap().save_device(profile.clone()).unwrap();h.workspace.refresh();
        h.frame(Vec::new());
        assert!(h.workspace.route_visible(&profile.routes[0]));
        let info=Info {wallpaper_enabled:false,wallpaper_ready:false,wallpaper_download_ready:false,wallpaper_size:None,nearby:Vec::new(),providers:Vec::new()};
        let _=h.ctx.run(Default::default(),|ctx|{egui::CentralPanel::default().show(ctx,|ui|{h.workspace.show(ui,&info);});});
        assert!(!h.workspace.route_visible(&profile.routes[0]),"a changed enabled-provider snapshot applies in the same window");
        assert_eq!(h.workspace.book.device(&profile.id).unwrap().selected_route,profile.selected_route);
        assert_eq!(h.workspace.store.as_ref().unwrap().read().unwrap().device(&profile.id).unwrap().routes.len(),1);
        h.frame(Vec::new());
        assert!(h.workspace.route_visible(&profile.routes[0]),"re-enabling exposes the existing route again");
    }
    impl Harness {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("eclipse-workspace-ui-{}", uuid::Uuid::new_v4()));
            let config = uu_core::Config {
                data_dir: root.join("data"),
                state_dir: root.join("state"),
                signaling_host: "unreachable.example.invalid".into(),
            };
            let workspace = Workspace::load(Some(&config), (47998, 47999));
            assert!(workspace.store.is_some(), "{}", workspace.feedback.text());
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            Self {
                workspace,
                ctx,
                root,
                time: 0.,
                focused: true,
                texts: Vec::new(),
                text_lefts: Vec::new(),

            }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Action> {
            self.time += 0.02;
            let mut actions = Vec::new();
            let info = Info {
                wallpaper_enabled:true,wallpaper_ready:true,wallpaper_download_ready:true,wallpaper_size:Some((2560,1440)),nearby:vec![("Generated nearby".into(),"192.0.2.20:48998".into())],providers:vec![uu_core::plugins::Descriptor{id:"example-addon",name:"Example connection",version:"1"}],
            };
            let output = self.ctx.run(
                egui::RawInput {
                    time: Some(self.time),
                    focused: self.focused,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 1000.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        actions.extend(self.workspace.show(ui, &info));
                    });
                    actions.extend(self.workspace.dialogs(ctx, &info));
                },
            );
            fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>, lefts: &mut Vec<(String,f32)>) {
                match shape {
                    egui::Shape::Text(text) => {
                        let text_value=text.galley.text().to_owned();
                        texts.push((text_value.clone(),text.pos + text.galley.size() * 0.5));
                        lefts.push((text_value,text.pos.x));
                    }
                    egui::Shape::Vec(shapes) => {
                        for s in shapes {
                            collect(s, texts, lefts);
                        }
                    }
                    _ => {}
                }
            }
            self.texts.clear();
            self.text_lefts.clear();
            for shape in output.shapes {
                collect(&shape.shape, &mut self.texts, &mut self.text_lefts);
            }
            actions
        }
        fn click(&mut self, label: &str) -> Vec<Action> {
            for _ in 0..3 {
                self.frame(Vec::new());
            }
            let position = self
                .texts
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("missing button {label}: {:?}", self.texts))
                .1;
            self.click_at(position)
        }
        fn click_at(&mut self,position:egui::Pos2)->Vec<Action> {
            let event = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(position), event(true)]);
            self.frame(vec![event(false)])
        }
        fn submenu(&mut self, label:&str) {
            self.frame(Vec::new());
            let point=self.texts.iter().rfind(|(text,_)|text==label).unwrap_or_else(||panic!("missing submenu {label}")).1;
            self.frame(vec![egui::Event::PointerMoved(point)]);
            for _ in 0..3 {self.frame(Vec::new());}
        }
        fn batch(&mut self,label:&str) {
            for _ in 0..3 {self.frame(Vec::new());}
            let rect=self.ctx.data(|data|data.get_temp::<egui::Rect>(egui::Id::new(("batch-button",label)))).expect("floating management button");
            self.click_at(rect.center());
        }
        fn choose_preset(&mut self,label:&str) {
            for _ in 0..3 {self.frame(Vec::new());}
            let rect=self.ctx.data(|d|d.get_temp::<egui::Rect>(egui::Id::new("test-client-config-name-arrow"))).expect("name dropdown response");
            self.click_at(rect.center());self.click(label);
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            self.workspace.host.stop();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn empty_devices_offer_add_and_single_computer_test_without_starting_sharing() {
        let mut h = Harness::new();
        h.frame(vec![]);
        assert!(h.texts.iter().any(|(s, _)| s == "尚未添加设备"));
        assert!(h.click("添加第一台电脑").is_empty());
        assert!(h.workspace.editor.is_some());
        assert!(!h.workspace.host.running());
        h.workspace.editor = None;
        assert!(matches!(&h.click("本机串流测试")[..], [Action::SelfStream]));
        assert!(!h.workspace.host.running());
    }

    #[test]
    #[cfg(not(windows))]
    fn empty_devices_onboarding_stays_stable_while_password_state_is_unknown() {
        let mut h = Harness::new();
        let mut heading_y = None;
        let mut add_y = None;
        for state in [None, Some(false), Some(true), None, Some(false)] {
            h.workspace.pairing_password_configured = state;
            h.frame(vec![]);
            h.frame(vec![]);
            let y = |label: &str| {
                h.texts
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("missing onboarding action: {label}"))
                    .1
                    .y
            };
            let next_heading = y("尚未添加设备");
            let next_add = y("添加第一台电脑");
            assert_eq!(*heading_y.get_or_insert(next_heading), next_heading);
            assert_eq!(*add_y.get_or_insert(next_add), next_add);
            assert!(
                !h.texts.iter().any(|(text, _)| text == "让其它电脑安全连接本机"),
                "the async password probe must not insert a second onboarding card"
            );
        }
    }

    #[test]
    fn device_name_uses_the_saved_device_connection_action() {
        let mut h = Harness::new();
        let profile = Profile::new("Generated desktop", Destination::Ip {
            address: "192.0.2.10:48998".into(), legacy_device: None,
        });
        let id = profile.id.clone();
        h.workspace.book.devices.push(profile);
        h.workspace.pairing_password_configured = Some(false);
        let actions = h.click("Generated desktop");
        assert!(matches!(&actions[..], [Action::Open(found)] if found == &id));
        assert!(!h.workspace.host.running());
        for hidden in ["让其它电脑安全连接本机", "设置安全连接密码", "192.0.2.10:48998", "在线", "暂时离线", "在线状态待检查", "开始串流"] {
            assert!(!h.texts.iter().any(|(text, _)| text == hidden), "extra device-list content: {hidden}");
        }
        let point = h.texts.iter().find(|(text, _)| text == "Generated desktop").unwrap().1;
        for pressed in [true, false] {
            h.frame(vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
                pos: point, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default(),
            }]);
        }
        h.frame(vec![]);
        h.click("设备详情…");
        h.frame(vec![]);
        assert_eq!(h.workspace.route.device, DeviceView::Details(id));
        assert!(h.texts.iter().any(|(text, _)| text == "编辑配置"));
        assert!(h.texts.iter().any(|(text, _)| text == "移除此设备…"));
    }

    #[test]
    fn device_background_context_menu_requests_wallpaper_refresh() {
        let mut h=Harness::new();
        for _ in 0..3 {h.frame(vec![]);}
        let point=egui::pos2(900.,850.);
        for pressed in [true,false] {
            h.frame(vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Secondary,pressed,modifiers:Default::default()}]);
        }
        h.submenu("壁纸");
        assert!(matches!(&h.click("刷新壁纸")[..],[Action::RefreshWallpaper]));
    }

    #[test]
    fn f5_refreshes_only_the_focused_device_list() {
        let mut h=Harness::new();
        let key=|pressed|egui::Event::Key{key:egui::Key::F5,physical_key:Some(egui::Key::F5),pressed,repeat:false,modifiers:Default::default()};
        h.focused=false;assert!(h.frame(vec![key(true)]).is_empty());h.frame(vec![key(false)]);
        h.focused=true;assert!(matches!(&h.frame(vec![key(true)])[..],[Action::RefreshWallpaper]));h.frame(vec![key(false)]);
        h.workspace.route.page=Page::Settings;assert!(h.frame(vec![key(true)]).is_empty());
    }

    #[test]
    fn device_menu_separates_library_actions_and_removal_requires_confirmation() {
        let mut h=Harness::new();
        let profile=Profile::new("Selected host",Destination::Ip{address:"192.0.2.10:48998".into(),legacy_device:None});
        let other=Profile::new("Other host",Destination::Ip{address:"192.0.2.11:48998".into(),legacy_device:None});
        for value in [&profile,&other] {h.workspace.store.as_ref().unwrap().save_device(value.clone()).unwrap();}
        h.workspace.refresh();
        let open=|h:&mut Harness| {
            h.frame(Vec::new());
            let point=h.texts.iter().find(|(text,_)|text=="Selected host").unwrap().1;
            for pressed in [true,false] {h.frame(vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Secondary,pressed,modifiers:Default::default()}]);}
            h.frame(Vec::new());
        };
        open(&mut h);
        for label in ["连接此设备","连接方式","串流配置","刷新在线状态","设备详情…","移除此设备…"] {
            assert!(h.texts.iter().any(|(text,_)|text==label),"missing {label}");
        }
        for label in ["隐藏设备列表","批量管理设备…","配置选择","配置编辑","壁纸"] {
            assert!(!h.texts.iter().any(|(text,_)|text==label),"library action leaked: {label}");
        }
        assert!(matches!(&h.click("连接此设备")[..],[Action::Open(id)] if id==&profile.id));
        open(&mut h);h.click("移除此设备…");h.click("取消");
        assert_eq!(h.workspace.store.as_ref().unwrap().read().unwrap().devices.len(),2);
        open(&mut h);h.click("移除此设备…");h.click("确认移除");
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        assert!(book.device(&profile.id).is_err());assert!(book.device(&other.id).is_ok());
    }

    #[test]
    fn current_default_edits_global_settings_and_saved_profile_edits_only_that_device() {
        let mut h=Harness::new();
        let mut profile=Profile::new("Generated host",Destination::Ip{address:"192.0.2.10:48998".into(),legacy_device:None});
        h.workspace.edit_selected_client_config(&profile);
        assert!(h.workspace.client_editor.as_ref().unwrap().global);
        let preset=uu_core::client_settings::NamedClientSettings::new("Office",Default::default());
        profile.selected_client=Some(preset.id.clone());profile.client_configs.push(preset.clone());
        h.workspace.edit_selected_client_config(&profile);
        let editor=h.workspace.client_editor.as_ref().unwrap();
        assert!(!editor.global);assert_eq!(editor.device,profile.id);assert_eq!(editor.editing_id(),Some(preset.id.as_str()));
    }

    #[test]
    fn context_menu_actions_and_submenu_entry_share_the_same_text_origin() {
        let mut h=Harness::new();
        for _ in 0..3 {h.frame(vec![]);}
        let point=egui::pos2(900.,850.);
        for pressed in [true,false] {
            h.frame(vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Secondary,pressed,modifiers:Default::default()}]);
        }
        h.frame(vec![]);
        let x=|label:&str|h.text_lefts.iter().rfind(|(text,_)|text==label).unwrap_or_else(||panic!("missing {label}: {:?}",h.texts)).1;
        let expected=x("添加设备…");
        for label in ["添加附近设备","刷新全部在线状态","批量管理设备…","隐藏设备列表","壁纸","本机串流测试"] {
            assert!((x(label)-expected).abs()<2.,"{label} at {} is not aligned with {expected}: {:?}",x(label),h.text_lefts);
        }
    }

    #[test]
    fn wallpaper_context_menu_exposes_source_pixels_and_separate_favorite_and_download_actions() {
        for (label, download) in [("收藏当前壁纸",false),("下载当前壁纸…",true)] {
            let mut h=Harness::new();
            for _ in 0..3 {h.frame(vec![]);}
            let point=egui::pos2(900.,850.);
            for pressed in [true,false] {
                h.frame(vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Secondary,pressed,modifiers:Default::default()}]);
            }
            h.frame(vec![]);
            h.submenu("壁纸");
            assert!(h.texts.iter().any(|(text,_)|text=="原图：2560 × 1440 像素"),"{:?}",h.texts);
            let actions=h.click(label);
            assert!(if download {matches!(&actions[..],[Action::DownloadWallpaper])}else{matches!(&actions[..],[Action::FavoriteWallpaper])});
        }
    }




    #[test]
    #[cfg(not(windows))]
    fn service_management_is_separate_from_connection_routes() {
        let mut h=Harness::new();h.workspace.route.page=Page::Services;
        h.frame(vec![]);
        assert!(!h.texts.iter().any(|(s,_)|s=="本机服务已停止"));
        assert!(h.texts.iter().any(|(s,_)|s=="IP 直连"));
        h.click("服务管理");
        assert!(h.texts.iter().any(|(s,_)|s=="本机服务已停止"));
        assert!(!h.texts.iter().any(|(s,_)|s=="自建服务器"));
        h.click("连接服务");
        assert!(!h.texts.iter().any(|(s,_)|s=="本机服务已停止"));
        assert!(h.texts.iter().any(|(s,_)|s=="自建服务器"));
    }

    #[test]
    #[cfg(not(windows))]
    fn empty_connection_sessions_keep_the_section_and_show_an_empty_state() {
        let mut h=Harness::new();h.workspace.route.page=Page::Services;
        h.click("服务管理");
        h.frame(vec![]);
        assert!(h.texts.iter().any(|(s,_)|s=="会话"),"{:?}",h.texts);
        assert!(h.texts.iter().any(|(s,_)|s=="会话为空"),"{:?}",h.texts);
        assert!(!h.texts.iter().any(|(s,_)|s.starts_with("已连接 ")));
    }

    #[test]
    #[cfg(not(windows))]
    fn pairing_tab_owns_identity_and_authorization_controls() {
        let mut h=Harness::new();h.workspace.route.page=Page::Services;
        h.click("配对设置");
        assert!(h.texts.iter().any(|(s,_)|s=="设备名称"));
        assert!(h.texts.iter().any(|(s,_)|s=="密码配对"));
        assert!(!h.texts.iter().any(|(s,_)|s=="添加授权公钥"));
        h.click("公钥配对");
        assert!(h.texts.iter().any(|(s,_)|s=="添加授权公钥"));
        assert!(!h.texts.iter().any(|(s,_)|s=="保存配对密码"));
        h.click("输入");
        assert!(!h.texts.iter().any(|(s,_)|s=="密码配对"));
        h.click("音频 / 视频");
        assert!(!h.texts.iter().any(|(s,_)|s=="设备名称"));
    }

    #[test]
    fn device_editor_retains_failed_input_and_saves_multiple_routes_without_duplicates_or_key_files(
    ) {
        let mut h = Harness::new();
        h.workspace.new_device(None,None);
        let editor = h.workspace.editor.as_mut().unwrap();
        editor.name = "Generated host".into();
        editor.address = "invalid:::".into();
        h.click("仅保存");
        let editor = h
            .workspace
            .editor
            .as_ref()
            .expect("invalid save keeps the form open");
        assert_eq!(editor.address, "invalid:::");
        assert!(!editor.error.is_empty());
        assert!(h.workspace.book.devices.is_empty());
        h.workspace.editor.as_mut().unwrap().address = "192.0.2.50:47998".into();
        h.click("仅保存");
        assert!(h.workspace.editor.is_none());
        let id = h.workspace.book.devices[0].id.clone();
        for _ in 0..2 {
            h.workspace.new_device(Some(id.clone()), None);
            let editor = h.workspace.editor.as_mut().unwrap();
            editor.kind = 1;
            editor.device = "generated-uu-device".into();
            h.click("仅保存");
        }
        assert_eq!(h.workspace.book.device(&id).unwrap().routes.len(), 2);
        let service = Service {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Generated server".into(),
            address: "signal.example.invalid:8443".into(),
            token_file: "generated.token".into(),
            certificate: None,
            host_enabled: false,
            published_name: "generated-host".into(),
        };
        h.workspace
            .store
            .as_ref()
            .unwrap()
            .save_service(service.clone())
            .unwrap();
        h.workspace.refresh();
        h.workspace.new_device(Some(id.clone()), None);
        let service = h.workspace.book.services.iter().find(|item| item.id == service.id).unwrap().clone();
        let editor = h.workspace.editor.as_mut().unwrap();
        editor.kind = 2;
        editor.service = service.id.clone();
        editor.device = "other-host".into();
        h.workspace.device_picker.seed(&service, vec![uu_core::internet::self_hosted::Device { device: "other-host".into(), name: "Other host".into() }], &mut editor.device);
        let actions = h.click("保存并连接");
        assert!(matches!(&actions[..],[Action::Open(opened)] if opened==&id));
        assert_eq!(h.workspace.book.device(&id).unwrap().routes.len(), 3);
        assert!(!h
            .texts
            .iter()
            .any(|(s, _)| s.contains("配对文件") || s.contains("配对 key")));
    }
    #[test]
    fn device_server_shortcut_keeps_the_draft_and_uses_the_management_entry() {
        let mut h = Harness::new();
        let sharing = h.workspace.book.sharing.clone();
        h.workspace.new_device(None, None);
        let editor = h.workspace.editor.as_mut().unwrap();
        editor.kind = 2;
        editor.name = "Unsaved device name".into();
        let actions = h.click("添加服务器");
        assert!(matches!(&actions[..], [Action::OpenConnectionServices]));
        assert!(h.workspace.editor.is_none());
        assert_eq!(h.workspace.suspended_editor.as_ref().unwrap().name, "Unsaved device name");
        assert_eq!(h.workspace.book.sharing, sharing);
        assert!(h.workspace.book.services.is_empty());
        assert!(!h.workspace.host.running());
        #[cfg(not(windows))]
        {
            h.workspace.servers.join_mut().open();
            h.workspace.route.page = Page::Devices;
            h.frame(Vec::new());
            assert!(h.workspace.editor.is_none(), "the suspended device form cannot overlap enrollment");
            h.workspace.servers = Default::default();
            h.frame(Vec::new());
            assert_eq!(h.workspace.editor.as_ref().unwrap().name, "Unsaved device name");
            assert!(h.workspace.suspended_editor.is_none());
        }
    }
    #[test]
    fn route_names_use_local_server_aliases_and_actual_plugin_names_without_target_ids() {
        let mut h = Harness::new();
        let (servers, rows) = crate::connection_service_ui::Servers::fixture();
        h.workspace.servers = servers;
        h.workspace.book.services = rows.iter().map(|row| Service {
            id: row.id.clone(), name: row.name.clone(), address: row.address.clone(),
            token_file: "fixture-unused-token".into(), certificate: None,
            host_enabled: false, published_name: "owned-device".into(),
        }).collect();
        h.workspace.providers = vec![uu_core::plugins::Descriptor { id: "generated-plugin", name: "Generated account service", version: "1" }];
        let target = "opaque-generated-device-id-never-a-visible-name";
        let route = |destination| Route::new(destination);
        assert_eq!(h.workspace.route_label(&route(Destination::Vps { service: rows[0].id.clone(), device: target.into() })), "自建服务器 · 家里的服务器 (wss://203.0.113.10:9443 | 我的设备)");
        let fallback = h.workspace.route_label(&route(Destination::Vps { service: rows[1].id.clone(), device: target.into() }));
        assert_eq!(fallback, "自建服务器 · wss://203.0.113.10:9443 (工作设备)");
        assert!(!fallback.contains(target));
        assert_eq!(h.workspace.route_label(&route(Destination::Plugin { plugin: "generated-plugin".into(), device: target.into() })), "Generated account service");
        assert!(!h.workspace.route_visible(&route(Destination::Plugin { plugin: "missing-plugin".into(), device: target.into() })));
    }
    #[test]
    #[cfg(not(windows))]
    fn connection_cards_save_at_the_bottom_and_keep_concurrent_media_settings() {
        let mut h=Harness::new();h.workspace.route.page=Page::Services;
        h.frame(vec![]);h.frame(vec![]);
        assert_eq!(h.texts.iter().filter(|(s,_)|s=="IP 直连").count(),1);
        h.click("启用 IPv6");
        assert!(h.workspace.store.as_ref().unwrap().read().unwrap().sharing.ipv6_enabled,"editing is a draft until save");
        h.workspace.store.as_ref().unwrap().edit(|book|{book.sharing.audio=false;Ok(())}).unwrap();
        h.click("保存设置");
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        assert!(!book.sharing.ipv6_enabled && !book.sharing.audio && book.sharing.ip_enabled);
        let y=|label:&str|h.texts.iter().find(|(s,_)|s==label).unwrap().1.y;
        assert!(y("保存设置")>y("自建服务器"));
        h.click("启用 IPv6");h.click("还原修改");
        assert!(!h.workspace.store.as_ref().unwrap().read().unwrap().sharing.ipv6_enabled);
    }
    #[test]
    #[cfg(not(windows))]
    fn server_addition_uses_one_invitation_form_without_exposing_routing_fields() {
        let mut h = Harness::new();
        h.workspace.route.page = Page::Services;
        h.click("＋ 添加服务器");
        h.frame(Vec::new());
        assert!(h.workspace.servers.join_mut().is_open());
        assert!(h.texts.iter().any(|(text, _)| text == "粘贴管理员提供的接入码或链接"));
        for hidden in ["访问令牌", "服务器地址", "证书与高级设置", "本机在服务器的设备名称"] {
            assert!(!h.texts.iter().any(|(text, _)| text == hidden), "unexpected field: {hidden}");
        }
        assert!(h.workspace.book.services.is_empty());
        assert!(!h.workspace.host.running());
    }
    #[test]
    fn final_remote_image_is_persisted_and_loaded_into_the_device_card() {
        let mut h=Harness::new();
        let profile=Profile::new("Generated desktop",Destination::Ip{address:"127.0.0.1:47998".into(),legacy_device:None});
        let id=profile.id.clone();let store=h.workspace.store.clone().unwrap();store.save_device(profile).unwrap();
        let writer=super::super::device_preview::Writer::default();
        writer.submit(store.clone(),id.clone(),None);
        writer.submit(store.clone(),id.clone(),Some(egui::ColorImage::new([160,90],egui::Color32::from_rgb(40,110,190))));
        drop(writer);
        let book=store.read().unwrap();let profile=book.device(&id).unwrap();
        assert!(profile.last_connected.is_some());let preview=profile.preview.as_ref().unwrap();
        let bytes=store.preview_bytes(preview).unwrap();assert!(bytes.starts_with(&[0xff,0xd8]));
        h.workspace.refresh();let until=Instant::now()+Duration::from_secs(3);
        loop {
            h.workspace.poll_previews(&h.ctx);
            if h.workspace.previews.get(&id).and_then(|p|p.texture.as_ref()).is_some(){break;}
            assert!(Instant::now()<until,"private preview cache failed to load");std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(h.workspace.previews[&id].texture.as_ref().unwrap().size(),[160,90]);
    }
    #[test]
    fn device_preset_editor_saves_an_isolated_selected_config() {
        let mut h=Harness::new();
        let profile=Profile::new("Desk",Destination::Ip {address:"127.0.0.1:47998".into(),legacy_device:None});
        let id=profile.id.clone();
        h.workspace.store.as_ref().unwrap().save_device(profile).unwrap(); h.workspace.refresh();
        h.workspace.client_defaults.fps=120;
        let profile=h.workspace.book.device(&id).unwrap().clone();
        h.workspace.edit_client_config(&profile,None,false);
        let editor=h.workspace.client_editor.as_mut().unwrap();
        editor.value.name="移动网络".into();editor.value.settings.fps=30;editor.value.settings.bitrate=3;
        h.click("保存配置");
        assert!(h.workspace.client_editor.is_none());
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        let profile=book.device(&id).unwrap();
        assert_eq!(profile.client_configs.len(),1);
        assert_eq!(profile.selected_client.as_deref(),Some(profile.client_configs[0].id.as_str()));
        assert_eq!(profile.client_configs[0].settings.fps,30);
        assert_eq!(h.workspace.client_defaults.fps,120);
        assert_eq!(book.sharing.fps,60);
        h.workspace.route.device = DeviceView::Details(id);
        for _ in 0..3 {h.frame(Vec::new());}
        let more=h.ctx.data(|data|data.get_temp::<egui::Rect>(egui::Id::new(("config-more",&profile.client_configs[0].id)))).unwrap();
        h.click_at(more.center());h.click("复制");
        assert!(h.workspace.client_editor.as_ref().unwrap().value.name.contains("副本"));
    }
    #[test]
    fn preset_name_dropdown_loads_the_chosen_id_and_new_names_create_copies() {
        let mut h=Harness::new();
        let host=Profile::new("Desk",Destination::Ip{address:"127.0.0.1:47998".into(),legacy_device:None});
        let store=h.workspace.store.clone().unwrap();store.save_device(host.clone()).unwrap();
        let first=uu_core::client_settings::NamedClientSettings::new("桌面",uu_core::client_settings::ClientSettings{fps:120,..Default::default()});
        let second=uu_core::client_settings::NamedClientSettings::new("办公",uu_core::client_settings::ClientSettings{fps:30,..Default::default()});
        store.save_client_config(&host.id,first.clone()).unwrap();store.save_client_config(&host.id,second.clone()).unwrap();
        h.workspace.refresh();h.workspace.client_defaults.fps=75;
        let profile=h.workspace.book.device(&host.id).unwrap().clone();
        h.workspace.edit_client_config(&profile,Some(first.clone()),false);
        h.workspace.client_editor.as_mut().unwrap().value.settings.fps=90;
        h.choose_preset("办公");
        let editor=h.workspace.client_editor.as_mut().unwrap();
        assert_eq!(editor.editing_id(),Some(second.id.as_str()));assert_eq!(editor.value.settings.fps,30);
        editor.value.settings.fps=45;h.click("保存配置");
        assert!(h.workspace.client_editor.is_none());
        let profile=store.read().unwrap().device(&host.id).unwrap().clone();
        let loaded=profile.client_configs.iter().find(|c|c.id==second.id).unwrap().clone();
        assert_eq!(loaded.settings.fps,45);
        h.workspace.edit_client_config(&profile,Some(loaded),false);
        let editor=h.workspace.client_editor.as_mut().unwrap();editor.value.name="旅行".into();editor.value.settings.fps=60;
        assert!(editor.editing_id().is_none());h.click("保存配置");
        let profile=store.read().unwrap().device(&host.id).unwrap().clone();
        assert_eq!(profile.client_configs.len(),3);
        let copy=profile.client_configs.iter().find(|c|c.name=="旅行").unwrap();
        assert_ne!(copy.id,second.id);assert_eq!(copy.settings.fps,60);
        assert_eq!(profile.client_configs.iter().find(|c|c.id==second.id).unwrap().settings.fps,45);
        assert_eq!(serde_json::to_value(profile.client_configs.iter().find(|c|c.id==first.id).unwrap()).unwrap(),serde_json::to_value(&first).unwrap());
        assert_eq!(profile.selected_client.as_deref(),Some(copy.id.as_str()));
        assert_eq!(h.workspace.client_defaults.fps,75);
        h.workspace.edit_client_config(&profile,Some(copy.clone()),false);
        h.workspace.client_editor.as_mut().unwrap().value.name="未保存".into();
        h.click("删除");assert!(h.workspace.client_editor.is_some());
        assert_eq!(store.read().unwrap().device(&host.id).unwrap().client_configs.len(),3);
        h.workspace.client_editor.as_mut().unwrap().value.name="旅行".into();h.click("删除");
        assert!(h.workspace.client_editor.is_none());
        let profile=store.read().unwrap().device(&host.id).unwrap().clone();
        assert_eq!(profile.client_configs.len(),2);assert!(profile.selected_client.is_none());
    }
    #[test]
    fn preset_editor_requires_explicit_selection_for_a_conflicting_or_legacy_duplicate_name() {
        let mut h=Harness::new();
        let host=Profile::new("Desk",Destination::Ip{address:"127.0.0.1:47998".into(),legacy_device:None});
        let store=h.workspace.store.clone().unwrap();store.save_device(host.clone()).unwrap();
        let mut a=uu_core::client_settings::NamedClientSettings::new("同名",uu_core::client_settings::ClientSettings{fps:120,..Default::default()});
        let mut b=uu_core::client_settings::NamedClientSettings::new("同名",uu_core::client_settings::ClientSettings{fps:30,..Default::default()});
        a.id="aaaaaaaa-0000-4000-8000-000000000001".into();b.id="aaaaaaaa-0000-4000-8000-000000000002".into();
        store.save_client_config(&host.id,a.clone()).unwrap();store.save_client_config(&host.id,b.clone()).unwrap();h.workspace.refresh();
        let profile=h.workspace.book.device(&host.id).unwrap().clone();h.workspace.edit_client_config(&profile,None,false);
        h.workspace.client_editor.as_mut().unwrap().value.name="同名".into();
        let path=h.root.join("data/device-trust/connections.json");
        let unchanged=std::fs::read(&path).unwrap();
        h.click("保存配置");assert!(!h.workspace.client_editor.as_ref().unwrap().error.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(),unchanged);
        h.choose_preset(&format!("同名 · {}",b.id));
        let editor=h.workspace.client_editor.as_ref().unwrap();
        assert!(editor.error.is_empty());assert_eq!(editor.editing_id(),Some(b.id.as_str()));assert_eq!(editor.value.settings.fps,30);
    }
    #[test]
    fn media_draft_does_not_restore_old_connection_ports_or_enable_flags() {
        let mut h=Harness::new();
        h.workspace.draft.audio=false;
        h.workspace.update_connections(|s|{s.ip_enabled=false;s.ipv6_enabled=false;s.plugin_services=vec!["example-addon".into()];s.service_port=51001;s.discovery_port=51002;});
        assert!(h.workspace.save_sharing());
        assert!(!h.workspace.server_dirty());
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        assert!(!book.sharing.audio);
        assert!(!book.sharing.ip_enabled && book.sharing.plugin_services==vec!["example-addon".to_owned()]);
        assert!(!book.sharing.ipv6_enabled);
        assert_eq!((book.sharing.service_port,book.sharing.discovery_port),(51001,51002));
    }
    #[test]
    fn feedback_does_not_leak_between_home_pages() {
        let mut h = Harness::new();
        h.workspace.feedback.success(
            crate::ui_feedback::Scope::Devices,
            "设备页保存成功",
        );
        h.frame(Vec::new());
        assert!(h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
        h.workspace.route.page = Page::Services;
        h.frame(Vec::new());
        assert!(!h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
        h.workspace.route.page = Page::Devices;
        h.frame(Vec::new());
        assert!(h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
    }

    #[test]
    fn device_placeholders_use_a_stable_color_and_readable_initial() {
        assert_eq!(device_initial("  remote desktop"), "R");
        assert_eq!(device_initial("远程工作站"), "远");
        assert_eq!(device_initial("  "), "?");
        assert_eq!(
            device_placeholder_color("stable-device-id"),
            device_placeholder_color("stable-device-id")
        );
        assert_ne!(
            device_placeholder_color("device-a"),
            device_placeholder_color("device-b")
        );
    }

}
fn muted(ui:&mut egui::Ui,text:impl Into<String>)->egui::Response {
    ui.label(RichText::new(text).size(theme::FONT_META).color(theme::palette(ui.visuals()).muted))
}
fn device_placeholder_color(id: &str) -> egui::Color32 {
    let palette = [
        egui::Color32::from_rgb(45, 93, 155),
        egui::Color32::from_rgb(74, 119, 80),
        egui::Color32::from_rgb(150, 92, 61),
        egui::Color32::from_rgb(105, 82, 151),
        egui::Color32::from_rgb(38, 124, 137),
        egui::Color32::from_rgb(151, 82, 108),
        egui::Color32::from_rgb(74, 101, 133),
        egui::Color32::from_rgb(128, 104, 53),
    ];
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    palette[(hash as usize) % palette.len()]
}

fn device_initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|ch| {
            if ch.is_ascii_alphabetic() {
                ch.to_ascii_uppercase().to_string()
            } else {
                ch.to_string()
            }
        })
        .unwrap_or_else(|| "?".into())
}

fn last_connection(time:Option<u64>)->String {
    let Some(time)=time else{return uu_i18n::text("尚未连接").into();};
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(time);
    let seconds=now.saturating_sub(time);
    let relative=if seconds<60 {uu_i18n::text("刚刚").into()}else if seconds<3600{uu_i18n::format!("{} 分钟前",seconds/60)}else if seconds<86400{uu_i18n::format!("{} 小时前",seconds/3600)}else{uu_i18n::format!("{} 天前",seconds/86400)};
    uu_i18n::format!("上次连接 · {relative}")
}
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(theme::primary_button(text))
}
fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    theme::card(ui.visuals())
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            contents(ui);
        });
    ui.add_space(12.);
}
const COMPACT_CONTEXT_MENU_WIDTH: f32 = 190.;
fn compact_context_menu(ui: &mut egui::Ui) {
    theme::compact_menu_style(ui);
    ui.set_width(COMPACT_CONTEXT_MENU_WIDTH);
}
fn field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) {
    ui.label(label);
    theme::text_input(ui, text, hint, f32::INFINITY);
    ui.add_space(6.);
}
impl Workspace {
    pub fn load(config: Option<&uu_core::Config>, ports: (u16, u16)) -> Self {
        let loaded = (|| -> anyhow::Result<_> {
            let config = config.ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("无法确定设备配置目录")))?;
            let store = Store::open(config.at("device-trust"))?;
            let trust = TrustStore::open(config.at("device-trust"))?;
            let old = store.read()?;
            if !old.legacy_imported {
                let legacy_path = config.at("native-lan/paired-devices.json");
                let legacy = if legacy_path.exists() {
                    Some(uu_core::native_lan_store::Devices::open(&legacy_path)?)
                } else {
                    None
                };
                let internet_path = config.at("internet/settings.json");
                let internet = if internet_path.exists() {
                    Some(super::internet_ui::read_profile(&internet_path)?)
                } else {
                    None
                };
                store.edit(|book| {
                    if book.legacy_imported {
                        return Ok(());
                    }
                    book.sharing.service_port = ports.0;
                    book.sharing.discovery_port = ports.1;
                    book.sharing.name = uu_core::config::local_device_name();
                    if let Some(legacy) = &legacy {
                        for device in &legacy.devices {
                            book.devices.push(Profile::new(
                                &device.name,
                                Destination::Ip {
                                    address: device.endpoint.clone(),
                                    legacy_device: Some(device.id.clone()),
                                },
                            ));
                        }
                        for address in &legacy.manual {
                            if !legacy.devices.iter().any(|d| &d.endpoint == address) {
                                book.devices.push(Profile::new(
                                    uu_i18n::text("手动设备"),
                                    Destination::Ip {
                                        address: address.clone(),
                                        legacy_device: None,
                                    },
                                ));
                            }
                        }
                    }
                    if let Some(settings) = &internet {
                        match &settings.provider {
                            uu_core::internet::Provider::Plugin(id) => {
                                if !book.sharing.plugin_services.contains(id){book.sharing.plugin_services.push(id.clone());}
                                if !settings.device.is_empty() {
                                    book.devices.push(Profile::new(
                                        uu_i18n::text("UU 设备"),
                                        Destination::Plugin {plugin:id.clone(),
                                            device: settings.device.clone(),
                                        },
                                    ));
                                }
                            }
                            uu_core::internet::Provider::SelfHosted => {
                                if !settings.server.trim().is_empty() {
                                    let service = Service {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        name: uu_i18n::text("已导入的服务器").into(),
                                        address: settings.server.clone(),
                                        token_file: settings.server_token.clone(),
                                        certificate: settings.server_ca.clone(),
                                        host_enabled: false,
                                        published_name: format!(
                                            "device-{}",
                                            &trust.card("Eclipse").id()[..12]
                                        ),
                                    };
                                    if !settings.device.is_empty() {
                                        book.devices.push(Profile::new(
                                            uu_i18n::text("服务器设备"),
                                            Destination::Vps {
                                                service: service.id.clone(),
                                                device: settings.device.clone(),
                                            },
                                        ));
                                    }
                                    book.services.push(service);
                                }
                            }
                        }
                    }
                    book.legacy_imported = true;
                    Ok(())
                })?;
            }
            let book = store.read()?;
            Ok((store, trust, book))
        })();
        let (store, trust, book, notice) = match loaded {
            Ok((s, t, b)) => (Some(s), Some(t), b, String::new()),
            Err(e) => (
                None,
                None,
                Book::default(),
                uu_i18n::format!("无法读取设备配置：{e:#}。原文件已保留。"),
            ),
        };
        let draft = book.sharing.clone();
        let pairing_password_configured = trust.as_ref()
            .and_then(|store| store.pairing_password_permissions().ok())
            .map(|permissions| permissions.is_some());
        let mut feedback = super::ui_feedback::Feedback::default();
        if !notice.is_empty() {
            feedback.error(super::ui_feedback::Scope::Devices, notice);
        }
        Self {
            providers:Vec::new(),
            route: HomeRoute {
                page: if std::env::var_os("ECLIPSE_SHOW_SHARING").is_some() {
                    Page::Services
                } else {
                    Page::Devices
                },
                ..Default::default()
            },
            book,
            host: Default::default(),
            host_sessions_requested: false,
            connection_history: Default::default(),
            statistics:Default::default(),
            servers: Default::default(),
            service_config: config.cloned(),
            history_path: config.map(|config| config.at("device-trust/connection-history.json")),
            #[cfg(windows)]
            background_service:Default::default(),
            store,
            trust,
            feedback,
            draft,
            monitors: Vec::new(),
            monitor_error: String::new(),
            host_section:0,
            service_tab:0,
            encoder_vendor_tab:0,
            editor: None,
            suspended_editor: None,
            device_picker: Default::default(),
            client_editor: None,
            client_defaults:Default::default(),
            dark_background:false,
            devices_hidden:false,
            device_icon_size:230.0,
            device_text_size:17.0,
            management:Default::default(),
            menus:Default::default(),
            connection_draft:None,
            last_read: Instant::now(),
            pairing_visible: false,
            pairing_settings:Default::default(),
            pairing_password_configured,
            autostart_checked: false,
            previews:Default::default(),preview_jobs:Vec::new(),presence:Default::default(),
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context, window: isize) {
        #[cfg(not(windows))]
        {
            self.servers.poll(ctx, self.service_config.as_ref());
            if self.servers.take_changed() { self.refresh(); }
            if self.servers.take_config_changed() && !self.host.connection_services_current(&self.book.services) {
                self.servers.join_mut().require_activation();
            }
            if self.servers.join_mut().activation_pending() && !self.host.stopping() {
                if let (Some(config), Some(trust)) = (self.service_config.clone(), self.trust.clone()) {
                    let reserved = self.host.access.as_ref().map(|access| {
                        let token = uuid::Uuid::new_v4().to_string();
                        access.sessions.reserve_configuration_switch(&token, false).map(|_| (access.clone(), token))
                    });
                    if reserved.as_ref().is_none_or(|result| result.is_ok()) {
                        let result = if self.host.running() { self.host.restart(config, self.book.clone(), trust, ctx) }
                            else { self.host.start(config, self.book.clone(), trust, ctx) };
                        match result {
                            Ok(()) => self.servers.join_mut().activation_started(),
                            Err(error) => {
                                if let Some(Ok((access, token))) = reserved { let _ = access.sessions.reserve_configuration_switch(&token, true); }
                                self.feedback.error(super::ui_feedback::Scope::Services(0), error.to_string());
                            }
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        self.servers.labels_mut().poll(ctx, self.service_config.as_ref());
        self.feedback.tick(ctx);
        self.host.poll(ctx, window);
        #[cfg(windows)]
        {
            self.background_service.poll(ctx,&mut self.host);
            self.pairing_password_configured=self.background_service.password_configured();
        }
        self.poll_previews(ctx);
        if self.last_read.elapsed() >= Duration::from_millis(500) {
            self.last_read = Instant::now();
            self.refresh();
        }
    }
    fn poll_previews(&mut self,ctx:&egui::Context) {
        self.previews.retain(|id,_|self.book.devices.iter().any(|p|p.id==*id&&p.redirect.is_none()));
        self.preview_jobs.retain(|job| {
            match job.rx.try_recv() {
                Ok(result)=>{
                    if let Some(slot)=self.previews.get_mut(&job.device).filter(|slot|slot.revision==job.revision) {
                        slot.loaded=true;
                        if let Ok(image)=result {
                            slot.texture=Some(ctx.load_texture(format!("device-preview-{}",job.device),egui::ColorImage::from_rgba_unmultiplied([image.width as usize,image.height as usize],&image.rgba),egui::TextureOptions::LINEAR));
                        }
                    }
                    false
                },
                Err(std::sync::mpsc::TryRecvError::Empty)=>true,
                Err(_)=>{if let Some(slot)=self.previews.get_mut(&job.device){slot.loaded=true;}false},
            }
        });
        for profile in self.book.devices.iter().filter(|p|p.redirect.is_none()) {
            let Some(preview)=&profile.preview else{continue;};
            let slot=self.previews.entry(profile.id.clone()).or_insert_with(||PreviewSlot{revision:preview.id.clone(),texture:None,loaded:false});
            if slot.revision!=preview.id {slot.revision=preview.id.clone();slot.loaded=false;}
            if slot.loaded || self.preview_jobs.iter().any(|j|j.device==profile.id) || self.preview_jobs.len()>=2 {continue;}
            let Some(store)=self.store.clone() else{continue;};
            let preview=preview.clone();let device=profile.id.clone();let revision=preview.id.clone();
            let (tx,rx)=std::sync::mpsc::channel();let context=ctx.clone();let identity=device.clone();
            std::thread::spawn(move|| {
                let result=store.preview_bytes(&preview).and_then(|bytes|super::wallpapers::decode(super::wallpapers::Request{device_id:identity,url:String::new()},bytes)).map_err(|e|format!("{e:#}"));
                let _=tx.send(result);context.request_repaint();
            });
            self.preview_jobs.push(PreviewJob{device,revision,rx});
        }
        if !self.preview_jobs.is_empty(){ctx.request_repaint_after(Duration::from_millis(100));}
    }
    fn device_scope(&self) -> super::ui_feedback::Scope {
        match &self.route.device {
            DeviceView::Details(id) => super::ui_feedback::Scope::DeviceDetails(id.clone()),
            DeviceView::List => super::ui_feedback::Scope::Devices,
        }
    }

    fn service_scope(&self) -> super::ui_feedback::Scope {
        super::ui_feedback::Scope::Services(self.service_tab)
    }

    fn refresh(&mut self) {
        self.pairing_password_configured = self.trust.as_ref()
            .and_then(|store| store.pairing_password_permissions().ok())
            .map(|permissions| permissions.is_some());
        if let Some(store) = &self.store {
            match store.read() {
                Ok(book) => {
                    self.draft.host_input=self.book.sharing.host_input;
                    if serde_json::to_vec(&self.draft).ok()
                        == serde_json::to_vec(&self.book.sharing).ok()
                    {
                        self.draft = book.sharing.clone();
                    }
                    self.draft.host_input=book.sharing.host_input;
                    if self.connection_draft.as_ref().is_some_and(|draft|*draft==connection_settings::Draft::from(&self.book.sharing)) {
                        self.connection_draft=None;
                    }
                    self.book = book;
                }
                Err(e) => self.feedback.error(
                    super::ui_feedback::Scope::Devices,
                    uu_i18n::format!("无法读取设备配置：{e:#}，原文件已保留"),
                ),
            }
        }
    }
    pub(super) fn reload(&mut self){self.refresh();self.presence.refresh();}
    fn update_connections(&mut self, edit: impl FnOnce(&mut Sharing)) {
        let result = self.store.as_ref().ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用")))
            .and_then(|store| store.edit(|book| { edit(&mut book.sharing); Ok(()) }));
        if let Err(error) = result {
            self.feedback.error(
                super::ui_feedback::Scope::Services(self.service_tab),
                uu_i18n::format!("无法保存连接服务：{error:#}"),
            );
        }
        self.refresh();
    }
    fn endpoint_state(&self, ui: &mut egui::Ui, identity: &uu_core::host_runtime::EndpointIdentity) {
        for snapshot in self.host.endpoint_snapshots().into_iter().filter(|snapshot|&snapshot.identity==identity) {
            crate::connection_service_ui::endpoint::show(ui,&snapshot,self.servers.labels().names());
        }
    }
    pub(super) fn discard_server_draft(&mut self){self.draft=self.book.sharing.clone();}
    pub(super) fn server_dirty(&self)->bool {self.draft!=self.book.sharing}
    pub(super) fn save_sharing(&mut self) -> bool {
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用")))
            .and_then(|s| s.edit(|book| {
                let mut media = self.draft.clone();
                media.ip_enabled = book.sharing.ip_enabled;
                media.ipv6_enabled = book.sharing.ipv6_enabled;
                media.plugin_services = book.sharing.plugin_services.clone();
                media.service_port = book.sharing.service_port;
                media.discovery_port = book.sharing.discovery_port;
                media.host_input = book.sharing.host_input;
                book.sharing = media;
                Ok(())
            }));
        let saved = result.is_ok();
        match result {
            Ok(()) => self.feedback.success(
                super::ui_feedback::Scope::Services(self.service_tab),
                if self.host.running() {uu_i18n::text("服务端设置已保存，下次启动生效")}else{uu_i18n::text("服务端设置已保存")},
            ),
            Err(e) => self.feedback.error(
                super::ui_feedback::Scope::Services(self.service_tab),
                uu_i18n::format!("无法保存：{e:#}"),
            ),
        }
        self.refresh();
        if saved {self.draft=self.book.sharing.clone();}
        saved
    }
    fn new_device(&mut self, profile: Option<String>, route: Option<String>) {
        self.suspended_editor = None;
        self.device_picker.reset();
        let mut editor = DeviceEditor {
            error: String::new(),
            profile,
            route,
            name: String::new(),
            kind: 0,
            provider:self.providers.first().map(|p|p.id.to_owned()).unwrap_or_default(),
            address: String::new(),
            device: String::new(),
            service: self
                .book
                .services
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
        };
        if let Some(id) = &editor.profile {
            if let Ok(profile) = self.book.device(id) {
                editor.name = profile.name.clone();
                if let Some(id) = &editor.route {
                    if let Some(route) = profile.routes.iter().find(|r| &r.id == id) {
                        match &route.destination {
                            Destination::Ip { address, .. } => editor.address = address.clone(),
                            Destination::Plugin { plugin,device } => {
                                editor.kind = 1;editor.provider=plugin.clone();
                                editor.device = device.clone();
                            }
                            Destination::Vps { service, device } => {
                                editor.kind = 2;
                                editor.service = service.clone();
                                editor.device = device.clone();
                            }
                            Destination::LegacyInternet { .. } | Destination::Unavailable(_) => {editor.kind=3;}
                        }
                    }
                }
            }
        }
        self.editor = Some(editor);
    }
    fn save_device(&mut self, editor: DeviceEditor) -> anyhow::Result<String> {
        if editor.kind == 1 {anyhow::ensure!(self.providers.iter().any(|provider|provider.id==editor.provider), "{}", uu_i18n::text("请选择可用连接方式"));}
        if editor.kind == 2 {
            let service = self.book.services.iter().find(|service| service.id == editor.service);
            anyhow::ensure!(self.device_picker.valid(service, &editor.device), "{}", uu_i18n::text("请选择在线设备"));
        }
        let destination = match editor.kind {
            0 => Destination::Ip {
                address: uu_core::native_discovery::endpoint(&editor.address)?,
                legacy_device: None,
            },
            1 => Destination::Plugin {
                plugin:editor.provider,
                device: editor.device.trim().into(),
            },
            2 => Destination::Vps {
                service: editor.service,
                device: editor.device.trim().into(),
            },
            _=>anyhow::bail!("此连接方式需要安装所属插件后编辑"),
        };
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用")))?;
        if editor.profile.is_none() {
            let id=store.upsert_device(editor.name.trim(),destination,None)?;
            self.refresh();
            return Ok(id);
        }
        let mut profile = if let Some(id) = editor.profile {
            store.read()?.device(&id)?.clone()
        } else {
            let name = if editor.name.trim().is_empty() {
                uu_i18n::text("远端设备")
            } else {
                editor.name.trim()
            };
            Profile::new(name, destination.clone())
        };
        if !editor.name.trim().is_empty() {
            profile.name = editor.name.trim().into();
        }
        if let Some(id) = editor.route {
            anyhow::ensure!(
                !profile
                    .routes
                    .iter()
                    .any(|r| r.id != id && r.destination.same_target(&destination)),
                "{}", uu_i18n::format!("这个连接地址已经存在"));
            let route = profile
                .routes
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("连接地址已移除")))?;
            // A previously paired IP may change address but retains its old
            // pinned identity until the v2 migration has been verified.
            let mut updated = destination;
            if let (
                Destination::Ip {
                    legacy_device: old, ..
                },
                Destination::Ip {
                    legacy_device: new, ..
                },
            ) = (&route.destination, &mut updated)
            {
                *new = old.clone();
            }
            route.destination = updated;
        } else if let Some(route) = profile
            .routes
            .iter()
            .find(|r| r.destination.same_target(&destination))
        {
            profile.selected_route = route.id.clone();
        } else {
            let route = Route::new(destination);
            profile.selected_route = route.id.clone();
            profile.routes.push(route);
        }
        let id = profile.id.clone();
        store.save_device(profile)?;
        self.refresh();
        Ok(id)
    }
    pub fn show(&mut self, ui: &mut egui::Ui, info: &Info) -> Vec<Action> {
        self.show_with_connections(ui,info,&mut |_,_|{})
    }
    pub fn show_with_connections(&mut self, ui: &mut egui::Ui, info: &Info, connection:&mut dyn FnMut(&mut egui::Ui,Option<&str>)) -> Vec<Action> {
        self.providers=info.providers.clone();
        let mut actions = Vec::new();
        match self.route.page {
            Page::Devices => self.devices(ui, info, &mut actions),
            Page::Services => self.service_workspace(ui, info, &mut actions,connection),
            Page::Plugin => {},
            Page::Settings => {}
        }
        actions
    }
    fn devices(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        if let DeviceView::Details(id) = self.route.device.clone() {
            self.device_details(ui, &id, actions);
            return;
        }
        if ui.ctx().input(|input|input.focused) && !ui.ctx().wants_keyboard_input()
            && ui.ctx().input_mut(|input|input.consume_key(egui::Modifiers::NONE,egui::Key::F5)) {
            actions.push(Action::RefreshWallpaper);
        }
        let blank=ui.interact(ui.max_rect().intersect(ui.clip_rect()),ui.id().with("device-wallpaper"),egui::Sense::click());
        blank.context_menu(|ui| self.library_menu(ui, info, actions));
        self.feedback.show(ui, super::ui_feedback::Scope::Devices);
        if self.devices_hidden {return;}
        let profiles:Vec<_>=self.book.devices.iter().filter(|p|p.redirect.is_none()).cloned().collect();
        self.device_management_toolbar(ui,&profiles);
        if profiles.is_empty() {
            card(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(uu_i18n::text("尚未添加设备"));
                    muted(ui, uu_i18n::text("添加远端电脑，或先进行本机串流测试。"));
                    ui.add_space(8.);
                    if ui.add(theme::primary_button(uu_i18n::text("添加第一台电脑"))).clicked() {
                        self.new_device(None, None);
                    }
                    if !info.nearby.is_empty() {
                        ui.menu_button(uu_i18n::text("选择附近电脑"), |ui| {
                            for (name, address) in &info.nearby {
                                if ui.add(theme::secondary_button(name)).clicked() {
                                    self.new_device(None, None);
                                    if let Some(editor) = &mut self.editor {
                                        editor.name = name.clone();
                                        editor.address = address.clone();
                                    }
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if ui.add(theme::secondary_button(uu_i18n::text("本机串流测试"))).clicked() {
                        actions.push(Action::SelfStream);
                    }
                });
            });
        }
        let columns=((ui.available_width()+22.)/(self.device_icon_size+22.)).floor().clamp(1.,32.) as usize;
        for row in profiles.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x=22.;
                for profile in row {
                    let response=self.device_tile(ui,profile,actions);
                    self.device_management_drag(ui,&response,&profile.id);
                }
            });
            ui.add_space(22.);
        }
        if self.management.active {ui.add_space(70.);}
        self.finish_device_management();
    }
    fn device_tile(&mut self,ui:&mut egui::Ui,profile:&Profile,actions:&mut Vec<Action>)->egui::Response {
        ui.push_id(&profile.id,|ui| {
            let width=self.device_icon_size.min(ui.available_width());
            let offline=self.presence.status(&profile.id)==super::device_presence::Status::Offline;
            let color=if offline {
                if self.dark_background {egui::Color32::from_gray(200)}else{theme::palette(ui.visuals()).muted}
            }else if self.dark_background {egui::Color32::WHITE}else{theme::palette(ui.visuals()).text};
            let mut label=egui::text::LayoutJob::simple(profile.name.clone(),egui::FontId::proportional(self.device_text_size),color,width);
            label.halign=egui::Align::Center;
            let text=ui.painter().layout_job(label);
            let (rect,response)=ui.allocate_exact_size(egui::vec2(width,width*9./16.+20.+text.size().y),if self.management.active {egui::Sense::click_and_drag()}else{egui::Sense::click()});
            response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Button,ui.is_enabled(),if self.management.active {uu_i18n::format!("选择设备 {}",profile.name)} else {uu_i18n::format!("连接 {}",profile.name)}));
            if response.hovered()||response.has_focus()||response.is_pointer_button_down_on(){let p=theme::palette(ui.visuals());ui.painter().rect_filled(rect.expand(7.),theme::RADIUS,if response.is_pointer_button_down_on(){p.primary.gamma_multiply(0.35)}else{p.accent.gamma_multiply(0.18)});}
            let picture=egui::Rect::from_min_size(rect.min,egui::vec2(width,width*9./16.));
            let mut painter=ui.painter().clone();
            if offline{ui.painter().rect_filled(picture,6.,egui::Color32::from_gray(50));painter.multiply_opacity(0.35);}
            self.paint_device_picture(&painter,picture,profile);
            let pos=egui::pos2(rect.center().x,picture.bottom()+12.);
            if self.dark_background {
                ui.painter().galley_with_override_text_color(pos+egui::vec2(0.,1.),text.clone(),egui::Color32::from_black_alpha(210));
            }
            ui.painter().galley(pos,text,color);
            if self.management.active && self.management.selected.contains(&profile.id) {
                let primary=theme::palette(ui.visuals()).primary;
                ui.painter().rect_stroke(rect.expand(4.),theme::RADIUS,egui::Stroke::new(2.,primary));
                let center=picture.right_top()+egui::vec2(-10.,10.);
                ui.painter().circle_filled(center,10.,primary);
                ui.painter().line_segment([center+egui::vec2(-4.,0.),center+egui::vec2(-1.,3.)],egui::Stroke::new(2.,egui::Color32::WHITE));
                ui.painter().line_segment([center+egui::vec2(-1.,3.),center+egui::vec2(5.,-4.)],egui::Stroke::new(2.,egui::Color32::WHITE));
            }
            if response.clicked(){
                if self.management.active {
                    if !self.management.selected.remove(&profile.id) {self.management.selected.insert(profile.id.clone());}
                }else{actions.push(Action::Open(profile.id.clone()));}
            }
            response.context_menu(|ui| self.device_menu(ui, profile, actions));
            response
        }).inner
    }
    fn route_visible(&self, route: &Route) -> bool {
        match &route.destination {
            Destination::Plugin { plugin, .. } => self.providers.iter().any(|provider|provider.id==plugin),
            Destination::Unavailable(_) => false,
            _=>true,
        }
    }
    fn route_label(&self, route: &Route) -> String {
        match &route.destination {
            Destination::Ip { address, .. } => format!("{} · {address}", uu_i18n::text("IP 直连")),
            Destination::Vps { service, .. } => self.book.services.iter().find(|item| &item.id == service)
                .map(|item| crate::connection_service_ui::route_name(self.servers.labels().names(), &item.address, &item.name))
                .unwrap_or_else(|| uu_i18n::text("服务器已移除").into()),
            Destination::Plugin { plugin, .. } => self.providers.iter().find(|provider| provider.id == plugin)
                .map(|provider| provider.name.to_owned())
                .unwrap_or_else(|| uu_i18n::text("请选择可用连接方式").into()),
            _ => route.destination.label().into(),
        }
    }
    fn route_picker(&mut self,ui:&mut egui::Ui,profile:&Profile) {
                let mut selected=profile.selected_route.clone();
                ui.horizontal(|ui| {
                    ui.label(uu_i18n::text("连接方式"));
                    crate::theme::Select::from_id_salt("device-route").selected_text(profile.route().ok().filter(|route|self.route_visible(route)).map(|route|self.route_label(route)).unwrap_or_else(||uu_i18n::text("请选择可用连接方式").into())).width(360.).show_ui(ui,|ui| {
                        for route in profile.routes.iter().filter(|route|self.route_visible(route)) {crate::theme::selectable_value(ui,&mut selected,route.id.clone(),self.route_label(route));}
                    });
                });
                if selected!=profile.selected_route {
                    let mut updated=profile.clone();updated.selected_route=selected;
                    if let Some(store)=&self.store {if let Err(e)=store.save_device(updated){
                        let scope=self.device_scope();
                        self.feedback.error(scope,uu_i18n::format!("无法保存连接方式：{e:#}"));
                    }}
                    self.refresh();
                }
    }
    fn device_picture(&mut self,ui:&mut egui::Ui,profile:&Profile,width:f32) {
        let (rect,_)=ui.allocate_exact_size(egui::vec2(width,width*9./16.),egui::Sense::hover());
        self.paint_device_picture(ui.painter(),rect,profile);
    }
    fn paint_device_picture(&self,painter:&egui::Painter,rect:egui::Rect,profile:&Profile) {
        painter.rect_filled(rect,6.,theme::palette(&painter.ctx().style().visuals).sidebar);
        if let Some(texture)=self.previews.get(&profile.id).and_then(|p|p.texture.as_ref()) {
            let size=texture.size_vec2();let scale=(rect.width()/size.x).min(rect.height()/size.y);
            let mut picture=egui::epaint::RectShape::filled(egui::Rect::from_center_size(rect.center(),size*scale),6.,egui::Color32::WHITE);
            picture.fill_texture_id=texture.id();
            picture.uv=egui::Rect::from_min_max(egui::Pos2::ZERO,egui::pos2(1.,1.));
            painter.add(picture);
        }else{
            painter.rect_filled(rect, 6.0, device_placeholder_color(&profile.id));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                device_initial(&profile.name),
                egui::FontId::proportional((rect.height() * 0.40).clamp(34.0, 76.0)),
                egui::Color32::WHITE,
            );
        }
    }
    #[cfg(test)]
    pub(super) fn open_generated_preview_editor(&mut self,profile:&Profile) {
        self.edit_client_config(profile,profile.client_configs.first().cloned(),false);
    }
    #[cfg(test)]
    pub(super) fn open_generated_device_details(&mut self,ctx:&egui::Context,profile:&Profile) {
        self.open_generated_connection_preview("device-details");
        let mut profile=profile.clone();
        profile.name="Notebook · Eclipse".into();
        profile.peer=Some("ab".repeat(32));
        profile.routes[0].destination=Destination::Ip{address:"192.0.2.40:48998".into(),legacy_device:None};
        profile.routes.push(Route::new(Destination::Vps{service:self.book.services[0].id.clone(),device:"generated-target".into()}));
        profile.client_configs.push(uu_core::client_settings::NamedClientSettings::new("办公",uu_core::client_settings::ClientSettings{fps:60,..Default::default()}));
        // Synthetic desktop thumbnail; no capture, host or real device is used.
        let mut image=egui::ColorImage::new([480,270],egui::Color32::from_rgb(208,231,248));
        for y in 20..250 {for x in 24..456 {
            image.pixels[y*480+x]=if y<46 {egui::Color32::from_rgb(60,100,140)}else if x<136 {egui::Color32::from_rgb(232,241,248)}else{egui::Color32::WHITE};
        }}
        let texture=ctx.load_texture("generated-device-thumbnail",image,egui::TextureOptions::LINEAR);
        self.previews.insert(profile.id.clone(),PreviewSlot{revision:"generated".into(),texture:Some(texture),loaded:true});
        self.route.page=Page::Devices;
        self.route.device=DeviceView::Details(profile.id.clone());
        self.book.devices=vec![profile];
    }
    #[cfg(test)]
    pub(super) fn open_generated_connection_preview(&mut self, surface: &str) {
        let (servers, rows) = crate::connection_service_ui::Servers::fixture();
        self.servers = servers;
        self.book.services = rows.into_iter().map(|row| Service {
            id: row.id, name: row.name, address: row.address,
            token_file: "fixture-unused-token".into(), certificate: None,
            host_enabled: row.host_enabled, published_name: "own-device-route-id".into(),
        }).collect();
        // This fixture retains the seeded in-memory directory and never loads
        // the user's configuration or opens a signaling endpoint.
        self.service_config = None;
        self.last_read = Instant::now();
        self.service_tab = 0;
        self.route.page = Page::Services;
        if surface == "add-server" {
            self.servers.join_mut().open();
        } else if surface == "add-device" {
            self.route.page = Page::Devices;
            self.new_device(None, None);
            let service = self.book.services[0].clone();
            let editor = self.editor.as_mut().unwrap();
            editor.kind = 2;
            editor.service = service.id.clone();
            self.device_picker.seed(&service, vec![uu_core::internet::self_hosted::Device {
                device: "generated-laptop".into(), name: "笔记本电脑".into(),
            }], &mut editor.device);
        }
    }
    fn edit_client_config(&mut self,profile:&Profile, value:Option<uu_core::client_settings::NamedClientSettings>, duplicate:bool) {
        let loaded=value.as_ref().filter(|_|!duplicate).map(|v|(v.id.clone(),v.name.clone()));
        let mut value=value.unwrap_or_else(||uu_core::client_settings::NamedClientSettings::new(uu_i18n::text("新配置"),self.client_defaults.clone()));
        if duplicate { value.id=uuid::Uuid::new_v4().to_string(); value.name=uu_i18n::format!("{} 副本",value.name); }
        self.client_editor=Some(ClientEditor {device:profile.id.clone(),value,loaded,section:super::settings_page::Section::Video,global:false,error:String::new()});
    }
    fn edit_global_client_config(&mut self) {
        self.client_editor=Some(ClientEditor {
            device:String::new(),
            value:uu_core::client_settings::NamedClientSettings::new(uu_i18n::text("全局默认"),self.client_defaults.clone()),
            loaded:None,
            section:super::settings_page::Section::Video,
            global:true,
            error:String::new(),
        });
    }
    fn client_config_picker(&mut self,ui:&mut egui::Ui,profile:&Profile) {
        ui.horizontal_wrapped(|ui| {
            let mut selected=profile.selected_client.clone();
            let title=profile.client_configs.iter().find(|v|Some(&v.id)==selected.as_ref()).map(|v|v.name.as_str()).unwrap_or(uu_i18n::text("默认配置"));
            crate::theme::Select::from_id_salt(("client-config",&profile.id)).selected_text(title).width((ui.available_width()-110.).clamp(140.,240.)).show_ui(ui,|ui| {
                crate::theme::selectable_value(ui,&mut selected,None,uu_i18n::text("默认配置"));
                for value in &profile.client_configs {crate::theme::selectable_value(ui,&mut selected,Some(value.id.clone()),&value.name);}
            }).response.on_hover_text(uu_i18n::text("默认配置跟随应用设置；已保存的配置仅用于当前设备。"));
            if selected!=profile.selected_client {
                if let Some(store)=&self.store {
                    match store.select_client_config(&profile.id,selected){
                        Ok(())=>self.refresh(),
                        Err(e)=>{
                            let scope=self.device_scope();
                            self.feedback.error(scope,uu_i18n::format!("无法选择配置：{e:#}"));
                        }
                    }
                }
            }
            if ui.add(crate::theme::secondary_button(uu_i18n::text("编辑配置")).small()).clicked() {
                self.edit_selected_client_config(profile);
            }
        });
    }
    fn refresh_monitors(&mut self) {
        if uu_core::native::uses_desktop_portal() {
            self.monitors.clear();
            self.monitor_error.clear();
            if self.draft.monitor != uu_core::native_displays::ALL_MONITORS {self.draft.monitor.clear();}
            return;
        }
        match uu_core::native::monitors() {
            Ok(monitors) => {
                self.monitors = monitors;
                self.monitor_error = if self.monitors.is_empty() {
                    uu_i18n::text("当前会话没有可共享的屏幕").into()
                } else {
                    String::new()
                };
            }
            Err(e) => self.monitor_error = uu_i18n::format!("无法读取屏幕：{e}"),
        }
    }
    #[cfg(windows)]
    fn service_workspace(&mut self, ui:&mut egui::Ui, _info:&Info, _actions:&mut Vec<Action>, _connection:&mut dyn FnMut(&mut egui::Ui,Option<&str>)) {
        self.background_service.show(ui,&mut self.host,self.store.is_some());
        if self.background_service.take_session_center() {self.host_sessions_requested=true;}
    }
    #[cfg(not(windows))]
    fn service_workspace(&mut self, ui:&mut egui::Ui, info:&Info, actions:&mut Vec<Action>, connection:&mut dyn FnMut(&mut egui::Ui,Option<&str>)) {
        ui.horizontal_wrapped(|ui| {
            for (tab,label) in [(5,uu_i18n::text("服务管理")),(0,uu_i18n::text("连接服务")),(1,uu_i18n::text("输入")),(2,uu_i18n::text("音频 / 视频")),(3,uu_i18n::text("编码器")),(4,uu_i18n::text("配对设置"))] {
                crate::theme::tab_value(ui,&mut self.service_tab,tab,label);
            }
        });
        ui.add_space(14.);
        let scope = self.service_scope();
        self.feedback.show(ui, scope);
        if self.service_tab==5 {
            self.service_control(ui,actions);
        } else if self.service_tab==0 {
            self.services(ui,info,actions,connection);
        } else {
            ui.horizontal(|ui| {
                let running=self.host.running();
                if ui.add_enabled(self.server_dirty(),theme::primary_button(uu_i18n::text("保存设置"))).clicked(){self.save_sharing();}
                if running && ui.add_enabled(!self.host.stopping(),crate::theme::secondary_button(uu_i18n::text("保存并重启服务"))).clicked() {
                    if !self.server_dirty() || self.save_sharing() {actions.push(Action::RestartSharing);}
                }
                if ui.add_enabled(self.server_dirty(),crate::theme::secondary_button(uu_i18n::text("还原修改"))).clicked(){self.discard_server_draft();}
                muted(ui,if self.server_dirty(){uu_i18n::text("有未保存的修改")}else if self.host.changes_pending(&self.book.sharing){uu_i18n::text("已保存，下次启动生效")}else{uu_i18n::text("已保存")});
            });
            ui.add_space(12.);
            self.host_section=match self.service_tab {1=>2,2=>0,_=>1};
            if self.service_tab==3 {
                ui.horizontal_wrapped(|ui| {
                    for (vendor,label) in [(0,uu_i18n::text("自动")),(1,"NVIDIA"),(2,"Intel"),(3,"AMD")] {
                        crate::theme::tab_value(ui,&mut self.encoder_vendor_tab,vendor,label);
                    }
                });
                muted(ui,match self.encoder_vendor_tab {
                    1=>uu_i18n::text("NVIDIA：Windows 使用驱动提供的 Media Foundation 编码，可调整画质与速度；驱动须支持所选参数。"),
                    2=>uu_i18n::text("Intel QSV 使用 oneVPL，可调整 Target Usage 与低功耗引擎；驱动需支持所选组合。"),
                    3=>uu_i18n::text("AMD：Windows 使用原生 AMF 低延迟编码，支持驱动可用的 H.264 / HEVC / AV1。"),
                    _=>uu_i18n::text("编码参数应用于当前主机。自动模式按已安装驱动选择可用后端；客户端仍需支持所选格式。"),
                });
                let choice=match self.encoder_vendor_tab {1=>Some(Encoding::Nvidia),2=>Some(Encoding::Intel),3=>Some(Encoding::Amd),_=>None};
                if let Some(choice)=choice {
                    let mut selected=self.draft.encoding==choice;
                    if crate::theme::switch(ui,&mut selected,uu_i18n::text("使用此厂商硬件编码")).changed() {
                        self.draft.encoding=if selected {choice}else{Encoding::Auto};
                    }
                }
                ui.add_space(10.);
            }
            self.server_settings(ui,info,actions);
        }
    }
    fn service_control(&mut self, ui:&mut egui::Ui, actions:&mut Vec<Action>) {
        if self.connection_history.open {
            if let Some(path)=&self.history_path {self.connection_history.local(path.clone(),self.host.access.as_ref().map(|a|&a.sessions.history),ui.ctx());}
            card(ui,|ui|{self.connection_history.show(ui);});
            return;
        }
        #[cfg(windows)]
        if self.background_service.show(ui,&mut self.host,self.store.is_some()) {
            self.book.sharing.auto_start=false;
            self.draft.auto_start=false;
        }
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if self.host.running() {
                        if self.host.stopping() {
                            uu_i18n::text("正在停止服务…")
                        } else {
                            uu_i18n::text("本机服务运行中")
                        }
                    } else {
                        uu_i18n::text("本机服务已停止")
                    })
                    .size(18.)
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.host.running() {
                        if ui.add_enabled(!self.host.stopping(),crate::theme::secondary_button(uu_i18n::text("停止服务"))).clicked(){self.host.stop();}
                        if ui.add_enabled(!self.host.stopping(),crate::theme::secondary_button(uu_i18n::text("保存并重启服务"))).clicked() {
                            if !self.server_dirty() || self.save_sharing() {actions.push(Action::RestartSharing);}
                        }
                    } else if ui.add_enabled(self.store.is_some(),theme::primary_button(uu_i18n::text("启动服务"))).clicked() {
                        if !self.server_dirty() || self.save_sharing() { actions.push(Action::StartSharing); }
                    }
                });
            });
            if !self.host.notice.is_empty() {
                let error = self.host.notice_is_error();
                let mut dismiss = false;
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&self.host.notice).color(if error {
                            theme::palette(ui.visuals()).danger
                        } else {
                            theme::palette(ui.visuals()).success
                        }),
                    );
                    if error && ui.add(crate::theme::secondary_button(uu_i18n::text("关闭")).small()).clicked() {
                        dismiss = true;
                    }
                });
                if dismiss {
                    self.host.clear_notice();
                }
            }
            muted(ui,uu_i18n::text("仅在本软件运行时共享；退出软件后停止。"));
            let mut auto = self.book.sharing.auto_start;
            if crate::theme::switch(ui,&mut auto, uu_i18n::text("打开软件时自动启动服务")).changed() {
                self.update_connections(|s| s.auto_start = auto);
                self.draft.auto_start = auto;
            }
            if self.host.running() {
                ui.add_space(12.);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(uu_i18n::text("新设备可直接发起配对")).color(theme::palette(ui.visuals()).success));
                    if self
                        .host
                        .access
                        .as_ref()
                        .is_some_and(|a| a.sessions.active_peer().is_some())
                        && ui.add(crate::theme::secondary_button(uu_i18n::text("断开全部串流"))).clicked()
                    {
                        self.host.disconnect();
                    }
                });
            }
        });
        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        let sessions=self.host.sessions();
        card(ui,|ui| {
            ui.horizontal(|ui| {
                ui.strong(uu_i18n::text("会话"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                    self.connection_history.button(ui);
                    if ui.add(crate::theme::secondary_button(uu_i18n::text("悬浮会话中心"))).clicked() {self.host_sessions_requested=true;}
                });
            });
            if sessions.is_empty() {
                muted(ui,uu_i18n::text("会话为空"));
                return;
            }
            ui.label(uu_i18n::format!("已连接 {} 个会话",sessions.len()));
            muted(ui,uu_i18n::text("多台设备可同时观看同一屏幕；每台会话由本机单独控制键盘、鼠标、手柄、显示器和画质，键鼠仍由当前取得控制权的一台设备操作。"));
            for session in sessions {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(&session.name);
                        let mut input=session.input;
                        if super::sharing_ui::input_permissions(ui,&mut input,session.max_input) {
                            match self.host.set_session_input(&session.id,input) {
                                Ok(())=>self.feedback.success(
                                    super::ui_feedback::Scope::Services(5),
                                    uu_i18n::text("已更新此会话的键盘、鼠标和手柄权限"),
                                ),
                                Err(error)=>self.feedback.error(
                                    super::ui_feedback::Scope::Services(5),
                                    uu_i18n::format!("无法更新会话输入权限：{error:#}"),
                                ),
                            }
                        }
                        if ui.add(crate::theme::secondary_button(uu_i18n::text("断开此会话")).small()).clicked(){self.host.disconnect_session(&session.id);}
                    });
                    if matches!(session.status,HostStatus::Streaming{..}) {
                        ui.horizontal_wrapped(|ui| {
                            if !session.displays.is_empty() {
                                let mut selected=session.display;
                                let label=session.display.and_then(|id|session.displays.iter().find(|s|s.id==id).map(|s|s.display_name.clone())).unwrap_or_else(||uu_i18n::text("当前显示器").into());
                                crate::theme::Select::from_id_salt(("host-session-display",session.id.clone())).selected_text(label).show_ui(ui,|ui|{
                                    for screen in &session.displays {
                                        crate::theme::selectable_value(ui,&mut selected,Some(screen.id),screen.display_name.clone());
                                    }
                                });
                                if selected!=session.display {
                                    if let Some(id)=selected {
                                        if let Err(error)=self.host.set_session_display(&session.id,id) {
                                            self.feedback.error(super::ui_feedback::Scope::Services(5),uu_i18n::format!("无法切换显示器：{error:#}"));
                                        }
                                    }
                                }
                            }
                        });
                        if session.fps>0 && session.bitrate_kbps>0 {
                            let draft_id=egui::Id::new(("host-session-quality",&session.id));
                            let (mut fps,mut bitrate)=ui.data_mut(|data|data.get_temp::<(u32,u32)>(draft_id)).unwrap_or((session.fps,session.bitrate_kbps));
                            ui.horizontal_wrapped(|ui| {
                                ui.label(uu_i18n::text("帧率"));
                                crate::theme::number_input(ui,egui::DragValue::new(&mut fps).range(5..=240).suffix(" FPS"));
                                ui.label(uu_i18n::text("码率"));
                                crate::theme::number_input(ui,egui::DragValue::new(&mut bitrate).range(64..=1_000_000).suffix(" kbps"));
                                if ui.add(crate::theme::secondary_button(uu_i18n::text("应用此会话画质")).small()).clicked() {
                                    ui.data_mut(|data|data.insert_temp(draft_id,(fps,bitrate)));
                                    let update=uu_core::native_quality::Update{fps,bitrate_kbps:Some(bitrate),rate_control:None};
                                    match self.host.set_session_quality(&session.id,update) {
                                        Ok(())=>self.feedback.success(super::ui_feedback::Scope::Services(5),uu_i18n::text("已提交此会话画质更新")),
                                        Err(error)=>self.feedback.error(super::ui_feedback::Scope::Services(5),uu_i18n::format!("无法更新会话画质：{error:#}")),
                                    }
                                }
                            });
                            ui.data_mut(|data|data.insert_temp(draft_id,(fps,bitrate)));
                        }
                    }
                    muted(ui,match session.status {
                        HostStatus::Starting|HostStatus::Preparing=>uu_i18n::text("正在准备串流…").into(),
                        HostStatus::Waiting{..}=>uu_i18n::text("等待控制端建立媒体连接…").into(),
                        HostStatus::Streaming{width,height,fps,encoder,audio}=>format!("{width}×{height} · {fps} FPS · {encoder} · {audio}"),
                    });
            }
        });
    }
    pub(super) fn server_settings(&mut self, ui: &mut egui::Ui, info: &Info, _actions: &mut Vec<Action>) {
        if self.service_tab==4 {
            card(ui,|ui| {
                ui.strong(uu_i18n::text("设备身份"));
                super::server_config_ui::device_name(ui,&mut self.draft,true);
                muted(ui,uu_i18n::text("名称用于原生设备配对和公钥备注，保存后使用新名称。"));
            });
            if let Some(trust)=&self.trust {
                card(ui,|ui|{self.pairing_settings.show(ui,trust,&self.book.sharing.name,self.host.access.as_ref());});
            }
            self.grants(ui);
            return;
        }

        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        let running = self.host.running();
        card(ui, |ui| {
            ui.strong(match self.host_section {0=>uu_i18n::text("采集与声音"),1=>uu_i18n::text("编码"),_=>uu_i18n::text("访问权限")});
            ui.add_space(10.);
            let monitors:Vec<_>=self.monitors.iter().map(|m|(m.name.clone(),format!("{} · {}×{}",m.name,m.width,m.height))).collect();
            if self.host_section==0 {super::server_config_ui::capture(ui,&mut self.draft,&monitors,uu_core::native::uses_desktop_portal());}
            else {super::server_config_ui::edit(ui,&mut self.draft,self.host_section,&monitors,uu_core::native::uses_desktop_portal(),true,true);}
            if !self.monitor_error.is_empty(){ui.colored_label(theme::palette(ui.visuals()).danger,&self.monitor_error);}
            if running {
                muted(ui,uu_i18n::text("运行期间可编辑并保存；下次启动生效，或保存后重启服务。重启会断开当前串流。"));
            }
        });
        ui.add_space(14.);
    }

    fn grants(&mut self, ui: &mut egui::Ui) {
        let peers = match &self.trust {
            Some(store) => match store.peers() {
                Ok(peers) => peers,
                Err(e) => {
                    ui.colored_label(theme::palette(ui.visuals()).danger, uu_i18n::format!("无法读取授权：{e}"));
                    return;
                }
            },
            None => return,
        };
        card(ui, |ui| {
            ui.strong(uu_i18n::text("已授权的控制端"));
            muted(ui,uu_i18n::text("这些设备通过 IP、UU 或 VPS 连接时，都使用相同的身份和权限。"));
            ui.add_space(10.);
            let active: Vec<_> = peers.iter().filter(|p| p.inbound.is_some()).collect();
            if active.is_empty() {
                muted(ui,uu_i18n::text("还没有已授权的控制端。可预先添加公钥，或在启动服务后发起配对。"));
            }
            for peer in active {
                ui.push_id(peer.device.id(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(&peer.device.name);
                        let mut permission=peer.inbound.unwrap_or(uu_core::device_trust::Permissions::VIEW);
                        if super::sharing_ui::permissions(ui,&mut permission) {
                            let result = self.trust.as_ref().unwrap().authorize(
                                peer.device.clone(),
                                permission,
                                None,
                            );
                            if result.is_ok() {
                                if let Some(access) = &self.host.access {
                                    access.sessions.revoke(&peer.device.id());
                                }
                            }
                            match result {
                                Ok(()) => self.feedback.success(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    uu_i18n::text("权限已保存，重新连接后生效"),
                                ),
                                Err(e) => self.feedback.error(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    uu_i18n::format!("无法修改权限：{e}"),
                                ),
                            }
                        }
                        if ui.add(crate::theme::secondary_button(uu_i18n::text("撤销授权")).small()).clicked() {
                            let result = if let Some(access) = &self.host.access {
                                access.revoke(&peer.device.id())
                            } else {
                                self.trust.as_ref().unwrap().revoke(&peer.device.id())
                            };
                            match result {
                                Ok(()) => self.feedback.success(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    uu_i18n::text("已撤销设备授权"),
                                ),
                                Err(e) => self.feedback.error(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    uu_i18n::format!("撤销失败：{e}"),
                                ),
                            }
                        }
                    });
                });
            }
            for legacy in self
                .host
                .legacy_clients()
                .into_iter()
                .filter(|p| !peers.iter().any(|peer| peer.legacy_ids.contains(&p.id)))
            {
                ui.horizontal(|ui| {
                    ui.label(&legacy.name);
                    muted(ui,uu_i18n::text("旧版 IP 配对"));
                    if ui.add(crate::theme::secondary_button(uu_i18n::text("撤销授权")).small()).clicked() {
                        self.host.revoke_legacy(&legacy.id);
                    }
                });
            }
        });
    }
    pub fn dialogs(&mut self, ctx: &egui::Context, info: &Info) -> Vec<Action> {
        let mut actions = Vec::new();
        #[cfg(windows)]
        {
            if let Some(target) = self.background_service.take_open() { actions.push(Action::OpenPrepared(target)); }
            self.background_service.dialogs(ctx);
        }
        #[cfg(not(windows))]
        {
            if let Some(target) = self.servers.take_open() { actions.push(Action::OpenPrepared(target)); }
            self.servers.dialogs(ctx, self.service_config.as_ref());
        }
        self.statistics.show(ctx,self.service_config.as_ref(),&self.book);
        self.host.dialogs(ctx);
        self.device_menu_dialogs(ctx);
        if let Some(mut editor)=self.client_editor.take() {
            let mut open=true; let mut save=false; let mut cancel=false; let mut remove=false;
            egui::Window::new(if editor.global {uu_i18n::text("全局默认客户端配置")} else {uu_i18n::text("设备客户端配置")}).id("device-client-editor".into()).open(&mut open)
                .default_width(720.).default_height(600.).max_height((ctx.screen_rect().height()-70.).max(200.)).max_width((ctx.screen_rect().width()-40.).max(280.))
                .collapsible(false).show(ctx,|ui| {
                    if !editor.error.is_empty(){ui.colored_label(theme::palette(ui.visuals()).danger,&editor.error);}
                    if !editor.global && ui.add(crate::theme::secondary_button(uu_i18n::text("编辑全局默认")).small()).clicked() {
                        editor.global = true;
                        editor.device.clear();
                        editor.loaded=None;
                        editor.value = uu_core::client_settings::NamedClientSettings::new(
                            uu_i18n::text("全局默认"),
                            self.client_defaults.clone(),
                        );
                    }
                    if editor.global {
                        muted(ui,uu_i18n::text("全局默认会用于未指定独立配置的设备；修改只影响下一次连接。"));
                    } else {
                        ui.label(uu_i18n::text("配置名称"));
                        ui.horizontal(|ui| {
                            theme::text_input(ui,&mut editor.value.name,uu_i18n::text("例如：桌面 4K / 游戏 120 FPS"),300.);
                            let configs=self.book.device(&editor.device).map(|p|p.client_configs.clone()).unwrap_or_default();
                            ui.add_enabled_ui(!configs.is_empty(),|ui| {
                                let response=crate::theme::Select::from_id_salt(("client-config-name",&editor.device)).selected_text("").width(28.).show_ui(ui,|ui| {
                                    for value in &configs {
                                        let duplicate=configs.iter().any(|c|c.id!=value.id&&c.name.trim()==value.name.trim());
                                        let short=value.id.get(..8).unwrap_or(&value.id);
                                        let suffix=if configs.iter().any(|c|c.id!=value.id&&c.id.starts_with(short)){value.id.as_str()}else{short};
                                        let label=if duplicate {format!("{} · {}",value.name,suffix)}else{value.name.clone()};
                                        if crate::theme::selectable_label(ui,editor.editing_id()==Some(value.id.as_str()),label).clicked(){editor.load(value.clone());ui.close_menu();}
                                    }
                                }).response.on_hover_text(uu_i18n::text("配置选择"));
                                #[cfg(test)]
                                ui.ctx().data_mut(|d|d.insert_temp(egui::Id::new("test-client-config-name-arrow"),response.rect));
                                #[cfg(not(test))]
                                let _=response;
                            });
                        });
                        muted(ui,uu_i18n::text("选择已有配置会加载其参数；输入新名称会用当前参数另存配置。"));
                        muted(ui,uu_i18n::text("仅用于这台设备，不改变本机共享设置和其它设备。"));
                    }
                    ui.separator();
                    egui::ScrollArea::vertical().id_salt("device-client-fields").max_height((ui.available_height()-70.).max(80.).min((ctx.screen_rect().height()-260.).max(80.))).show(ui,|ui| {
                        super::settings_page::client_editor(ui,&mut editor.value.settings,&mut editor.section);
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        save=primary(ui,uu_i18n::text("保存配置")).clicked();
                        if !editor.global {remove=ui.add_enabled(editor.editing_id().is_some(),crate::theme::secondary_button(uu_i18n::text("删除"))).clicked();}
                        cancel=ui.add(crate::theme::secondary_button(uu_i18n::text("取消"))).clicked();
                    });
                });
            if save {
                if editor.global {
                    actions.push(Action::SaveClientDefaults(editor.value.settings.clone()));
                } else {
                    let result=self.store.as_ref().ok_or_else(||anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用"))).and_then(|store| {
                        store.save_named_client_config(&editor.device,editor.editing_id(),&editor.value.name,editor.value.settings.clone()).map(|_|())
                    });
                    match result {
                        Ok(())=>{
                            let scope=self.device_scope();
                            self.feedback.success(scope,uu_i18n::text("设备配置已保存；下次连接生效"));
                            self.refresh();
                        }
                        Err(e)=>{editor.error=uu_i18n::format!("无法保存：{e:#}");self.client_editor=Some(editor);}
                    }
                }
            }else if remove {
                let result=self.store.as_ref().ok_or_else(||anyhow::anyhow!("{}",uu_i18n::text("设备配置不可用"))).and_then(|store| {
                    store.remove_client_config(&editor.device,editor.editing_id().expect("delete requires a selected preset"))
                });
                match result {
                    Ok(())=>{self.feedback.success(self.device_scope(),uu_i18n::text("配置已删除；当前连接不受影响。"));self.refresh();}
                    Err(e)=>{editor.error=uu_i18n::format!("无法删除配置：{e:#}");self.client_editor=Some(editor);}
                }
            }else if open && !cancel {self.client_editor=Some(editor);}
        }
        #[cfg(windows)]
        let connection_dialog_open = self.background_service.servers_dialog_open();
        #[cfg(not(windows))]
        let connection_dialog_open = self.servers.is_open();
        if self.route.page == Page::Devices && matches!(self.route.device, DeviceView::List)
            && self.editor.is_none() && !connection_dialog_open {
            self.editor = self.suspended_editor.take();
            if let Some(editor) = self.editor.as_mut().filter(|editor| editor.profile.is_none() && editor.kind == 2 && editor.service.is_empty()) {
                if self.book.services.len() == 1 { editor.service = self.book.services[0].id.clone(); }
            }
        }
        if let Some(mut editor) = self.editor.take() {
            let mut open = true;
            let mut save = false;
            let mut connect = false;
            let mut cancel = false;
            let mut open_services = false;
            egui::Window::new(if editor.profile.is_none() {
                uu_i18n::text("添加设备")
            } else if editor.route.is_some() {
                uu_i18n::text("修改连接地址")
            } else {
                uu_i18n::text("添加连接地址")
            })
            .id("device-editor".into()).open(&mut open).collapsible(false).resizable(false)
            .default_width(460.).default_pos(ctx.screen_rect().center() - egui::vec2(240., 200.))
            .show(ctx, |ui| {
                if !editor.error.is_empty() { ui.colored_label(theme::palette(ui.visuals()).danger, &editor.error); }
                field(ui, uu_i18n::text("设备名称（可选）"), &mut editor.name, uu_i18n::text("例如：工作电脑"));
                let previous = (editor.kind, editor.provider.clone());
                ui.horizontal(|ui| {
                    crate::theme::tab_value(ui,&mut editor.kind, 0, uu_i18n::text("IP 直连"));
                    for provider in &info.providers {if crate::theme::selectable_tab(ui,editor.kind==1&&editor.provider==provider.id,provider.name).clicked(){editor.kind=1;editor.provider=provider.id.into();}}
                    crate::theme::tab_value(ui,&mut editor.kind, 2, uu_i18n::text("自建服务器"));
                });
                if previous != (editor.kind, editor.provider.clone()) { editor.device.clear(); self.device_picker.reset(); }
                ui.add_space(12.);
                match editor.kind {
                    0 => {
                        field(ui, uu_i18n::text("对方电脑 IP 地址"), &mut editor.address, uu_i18n::text("IPv4 或 IPv6，例如 192.168.1.100、2001:db8::10"));
                        muted(ui, uu_i18n::text("支持 IPv4、IPv6 和主机名。自定义端口使用 IP:端口，IPv6 使用 [地址]:端口，例如 [2001:db8::10]:48998。"));
                    },
                    1 => {
                        if self.providers.iter().any(|provider|provider.id==editor.provider) {field(ui,uu_i18n::text("目标设备编号"),&mut editor.device,uu_i18n::text("由连接插件提供的设备标识"));}
                        else {ui.weak(uu_i18n::text("请选择可用连接方式"));}
                    },
                    2 => {
                        ui.label(uu_i18n::text("连接服务器"));
                        ui.horizontal(|ui| {
                            let label = self.book.services.iter().find(|service| service.id == editor.service)
                                .map(|service| self.servers.labels().display_name(&service.address))
                                .unwrap_or_else(|| uu_i18n::text(if self.book.services.is_empty() { "请先添加服务器" } else { "请选择服务器" }).into());
                            crate::theme::Select::from_id_salt("route-service").selected_text(label)
                                .width((ui.available_width() - 130.).max(160.)).show_ui(ui, |ui| {
                                    for service in &self.book.services {
                                        crate::theme::selectable_value(ui, &mut editor.service, service.id.clone(),
                                            self.servers.labels().display_name(&service.address));
                                    }
                                });
                            if ui.add(crate::theme::secondary_button(uu_i18n::text("添加服务器"))).clicked() { open_services = true; }
                        });
                        let selected = self.book.services.iter().find(|service| service.id == editor.service).cloned();
                        ui.add_space(8.); ui.label(uu_i18n::text("目标设备"));
                        if let Some(name) = self.device_picker.show(ui, selected.as_ref(), &mut editor.device) {
                            if editor.name.trim().is_empty() { editor.name = name; }
                        }
                    },
                    _ => { ui.weak(uu_i18n::text("此连接方式需要安装所属插件后编辑")); },
                }
                muted(ui,uu_i18n::text("首次连接时确认设备身份，以后不再重复配对。"));
                ui.add_space(12.);
                let ready = editor.kind == 0 || (editor.kind == 1 && self.providers.iter().any(|provider|provider.id==editor.provider)) || (editor.kind == 2 && self.device_picker.valid(
                    self.book.services.iter().find(|service| service.id == editor.service), &editor.device));
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, theme::primary_button(uu_i18n::text("保存并连接"))).clicked() { save = true; connect = true; }
                    if ui.add_enabled(ready, crate::theme::secondary_button(uu_i18n::text("仅保存"))).clicked() { save = true; }
                    if ui.add(crate::theme::secondary_button(uu_i18n::text("取消"))).clicked() { cancel = true; }
                });
            });
            if open_services {
                self.device_picker.reset();
                self.suspended_editor = Some(editor);
                actions.push(Action::OpenConnectionServices);
            } else if save {
                match self.save_device(editor.clone()) {
                    Ok(id) => { if connect { actions.push(Action::Open(id)); } self.device_picker.reset(); },
                    Err(e) => { editor.error = uu_i18n::format!("无法保存设备：{e:#}"); self.editor = Some(editor); },
                }
            } else if open && !cancel { self.editor = Some(editor); }
            else { self.device_picker.reset(); }
        }

        actions
    }
}

impl super::App {
    #[cfg(windows)]
    pub(super) fn toggle_shared_host(&mut self, _ctx:&egui::Context) {self.workspace.background_service.toggle();}
    #[cfg(not(windows))]
    pub(super) fn toggle_shared_host(&mut self, ctx:&egui::Context) {
        if self.workspace.host.running() { self.workspace.host.stop(); }
        else { self.workspace_actions(vec![Action::StartSharing],ctx); }
    }
    #[cfg(windows)]
    pub(super) fn autostart_shared_host(&mut self, _ctx:&egui::Context) {}
    #[cfg(not(windows))]
    pub(super) fn autostart_shared_host(&mut self, ctx:&egui::Context) {
        if self.workspace.autostart_checked {return;}
        self.workspace.autostart_checked=true;
        if self.stream_target.is_none() && self.workspace.book.sharing.auto_start && !self.exit_requested {
            self.workspace_actions(vec![Action::StartSharing],ctx);
        }
    }
    pub(super) fn select_workspace_page(&mut self, page: Page) {
        let previous = self.workspace.route.page;
        if previous == Page::Settings && page != Page::Settings {
            self.discard_home_settings();
        }
        if page == Page::Settings && previous != Page::Settings {
            self.workspace.route.settings_return = previous;
            self.prepare_home_settings();
        }
        self.workspace.route.page = page;
        self.workspace.route.device = DeviceView::List;
        self.workspace.editor = None;
        self.workspace.device_picker.reset();
        self.context.request_repaint();
    }
    pub(super) fn leave_settings(&mut self) {
        let return_page = self.workspace.route.settings_return;
        self.discard_home_settings();
        self.workspace.route.page = return_page;
        self.workspace.route.device = DeviceView::List;
        self.context.request_repaint();
    }
    pub(super) fn workspace_info(&self)->Info {
        Info {wallpaper_enabled:self.preferences.wallpaper_enabled,wallpaper_ready:self.background.has_wallpaper(),wallpaper_download_ready:self.background.can_download(),wallpaper_size:self.background.original_size(),nearby:self.lan.nearby(self.workspace.host.server_id()),providers:uu_core::plugins::registry().descriptors()}
    }
    pub(super) fn workspace_home(&mut self, ctx: &egui::Context) {
        let info = self.workspace_info();
        let wallpaper_root = self.workspace.route.page == Page::Devices
            && matches!(self.workspace.route.device, DeviceView::List)
            && self.preferences.wallpaper_enabled;
        self.workspace.client_defaults = self.preferences.client.clone();
        self.workspace.dark_background = wallpaper_root;
        self.workspace.devices_hidden = self.preferences.devices_hidden;
        self.workspace.device_icon_size = self.preferences.device_icon_size;
        self.workspace.device_text_size = self.preferences.device_text_size;
        let mut actions = Vec::new();
        let mut extensions=Vec::new();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(if wallpaper_root { egui::Color32::TRANSPARENT } else { theme::palette(&ctx.style().visuals).base })
                    .inner_margin(24.),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    actions = self.workspace.show_with_connections(ui, &info,&mut |ui,provider| {
                        let placement=if provider.is_some(){uu_plugin_sdk::Placement::Connections}else{uu_plugin_sdk::Placement::Services};
                        for (id,page) in self.plugins.pages(placement) {
                            if provider.is_some_and(|provider|id!=provider){continue;}
                            if provider.is_none() {
                                card(ui,|ui|{ui.strong(&page.title);for action in self.plugins.show_page(ui,&id,&page.id){extensions.push((id.clone(),action));}});
                            }else{for action in self.plugins.show_page(ui,&id,&page.id){extensions.push((id.clone(),action));}}
                        }
                    });
                    if wallpaper_root && !self.background.notice.is_empty() {
                        self.background.notice.show(ui,super::ui_feedback::Scope::Wallpaper);
                    }
                });
            });
        self.workspace_actions(actions, ctx);self.plugin_actions(extensions);
    }
    pub(super) fn workspace_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::DevicesHidden(hidden) => {
                    self.preferences.devices_hidden=hidden;
                    self.persist_preferences();
                }
                Action::OpenConnectionServices => {
                    if self.workspace.suspended_editor.is_none() { self.workspace.suspended_editor = self.workspace.editor.take(); }
                    self.workspace.device_picker.reset();
                    self.workspace.route.page = Page::Services;
                    self.workspace.service_tab = 0;
                    #[cfg(windows)] self.workspace.background_service.open_servers();
                    #[cfg(not(windows))] self.workspace.servers.join_mut().open();
                    ctx.request_repaint();
                }
                Action::Open(id) => self.open_device_profile(&id),
                Action::OpenPrepared(target) => {
                    if let Err(error) = std::env::current_exe().and_then(|exe| self.sessions.open(&exe, &target, self.platform.origin())) {
                        self.workspace.feedback.error(super::ui_feedback::Scope::Devices, error.to_string());
                    }
                }
                Action::SelfStream => {
                    let target=super::sessions::StreamTarget{connection:super::sessions::ConnectionTarget::NativeLoopback,
                        alias:uu_i18n::text("本机串流测试").into(),view_only:true};
                    let result=std::env::current_exe().and_then(|exe|self.sessions.open(&exe,&target,self.platform.origin()));
                    if let Err(e)=result {self.workspace.feedback.error(super::ui_feedback::Scope::Devices,uu_i18n::format!("无法连接设备：{e:#}"));}
                }
                Action::SaveClientDefaults(settings) => {
                    self.preferences.client = settings;
                    self.workspace.client_defaults = self.preferences.client.clone();
                    self.persist_preferences();
                    let scope = self.workspace.device_scope();
                    self.workspace.feedback.success(scope, uu_i18n::text("全局默认客户端配置已保存；下次连接生效"));
                }
                Action::FavoriteWallpaper => self.background.favorite(ctx),
                Action::DownloadWallpaper => self.background.download(ctx),
                Action::RefreshWallpaper => {
                    if self.preferences.wallpaper_enabled {self.background.sync(&self.preferences,ctx);self.background.refresh(ctx);}
                }
                // The start button is dispatched after rendering to keep one
                // owner of the runtime and immutable sharing configuration.
                Action::StartSharing | Action::RestartSharing => {
                    #[cfg(windows)] {self.workspace.background_service.toggle();}
                    #[cfg(not(windows))] {
                    let restart=matches!(action,Action::RestartSharing);
                    let result = (|| -> anyhow::Result<()> {
                        let config = self
                            .config
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备目录不可用")))?;
                        let book = self
                            .workspace
                            .store
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用")))?
                            .read()?;
                        let trust = self
                            .workspace
                            .trust
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备身份不可用")))?;
                        if restart {self.workspace.host.restart(config,book,trust,ctx)}
                        else {self.workspace.host.start(config, book, trust, ctx)}
                    })();
                    if let Err(e) = result {
                        self.workspace.feedback.error(
                            super::ui_feedback::Scope::Services(0),
                            uu_i18n::format!("无法开始共享：{e:#}"),
                        );
                    }
                    }
                }
            }
        }
    }
    fn open_device_profile(&mut self, id: &str) {
        let result = (|| -> anyhow::Result<()> {
            let store = self
                .workspace
                .store
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("设备配置不可用")))?;
            let launch = store.launch(id)?;
            if let Destination::Plugin{plugin,..}=&launch.route.destination {anyhow::ensure!(self.plugins.enabled(plugin),"请先安装并启用此连接方式的插件");}
            let snapshot = store.publish_launch_with_defaults(id,&self.preferences.client)?;
            let target = super::sessions::StreamTarget {
                connection: super::sessions::ConnectionTarget::NativeProfile {
                    id: launch.profile,
                    snapshot,
                },
                alias: launch.name,
                view_only: false,
            };
            self.sessions
                .open(&std::env::current_exe()?, &target, self.platform.origin())?;
            Ok(())
        })();
        if let Err(e) = result {
            self.workspace.feedback.error(
                self.workspace.device_scope(),
                uu_i18n::format!("无法连接设备：{e:#}"),
            );
        }
    }
    pub(super) fn workspace_poll(&mut self, ctx: &egui::Context) {
        if self.stream_target.is_some() {
            return;
        }
        self.workspace.poll(ctx, self.platform.window_id());
        self.workspace.presence.poll(ctx,self.config.as_ref(),&self.workspace.book,uu_core::plugins::registry().generation());
    }
    pub(super) fn workspace_dialogs(&mut self, ctx: &egui::Context) {
        let info = self.workspace_info();
        let actions = self.workspace.dialogs(ctx, &info);
        self.workspace_actions(actions, ctx);
    }
    pub(super) fn device_pairing_ui(&mut self,ctx:&egui::Context) {
        let Some(controller)=self.controller.as_ref() else {self.workspace.pairing_visible=false;return;};
        let display=&controller.stats.pairing;
        self.workspace.pairing_visible=display.prompt().is_some()||display.code().is_some();
        if self.pairing_ui.show(ctx,display){self.disconnect();}
    }
}
