//! Full-page preferences. Rendering emits actions; App owns applying changes.
use super::{
    settings::{CursorMode, Settings},
    theme,
    ui::{self, Icon},
};
use eframe::egui::{self, RichText};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Section {
    #[default]
    Video,
    Encoding,
    Audio,
    Input,
    Stream,
    Application,
    Connections,
    Server,
}
impl Section {
    pub const ALL: [Self; 6] = [
        Self::Video,
        Self::Encoding,
        Self::Audio,
        Self::Input,
        Self::Stream,
        Self::Application,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "画面",
            Self::Encoding => "编码",
            Self::Audio => "声音",
            Self::Input => "输入",
            Self::Stream => "串流信息",
            Self::Application => "应用与诊断",
            Self::Connections=>"连接信息",
            Self::Server=>"服务端设置",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Video => "调整分辨率、帧率与呈现方式。",
            Self::Encoding => "选择编码格式、码率并查看当前编码。",
            Self::Audio => "设置默认音量与声音播放。",
            Self::Input => "配置键鼠、手柄、运动传感器与输入归属。",
            Self::Stream => "设置串流菜单与统计信息。",
            Self::Application => "管理串流界面、默认偏好与软件信息。",
            Self::Connections=>"查看连接到主机的设备及其授权。",
            Self::Server=>"修改远端服务下次启动使用的配置。",
        }
    }
}
pub struct State<'a> {
    pub native:bool,
    pub native_cursor:bool,
    pub streaming: bool,
    pub can_save_profile: bool,
    pub dirty: bool,
    pub storage_error: bool,
    pub notice: &'a str,
    pub presentation_vsync: bool,
    pub gamepad_status: Option<&'a str>,
    pub actual_codec: Option<&'a str>,
    pub actual_size: Option<[u32;2]>,
}
#[derive(Default)]
pub struct Actions {
    pub back: bool,
    pub apply: bool,
    pub save_profile: bool,
    pub recover: bool,
    pub reset: bool,
    pub about: bool,
    pub host: bool,
    pub maximize: bool,
    pub refresh_wallpaper:bool,
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(theme::MUTED)).wrap());
}
fn group(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .rounding(12.0)
        .inner_margin(if ui.ctx().screen_rect().height() < 520.0 {
            12.0
        } else {
            18.0
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16.0).strong().color(theme::INK));
            ui.add_space(8.0);
            body(ui);
        });
    ui.add_space(12.0);
}
fn combo(ui: &mut egui::Ui, id: &str, selected: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::ComboBox::from_id_salt((ui.ctx().viewport_id(),id))
        .width(210.0)
        .selected_text(selected)
        .show_ui(ui, body);
}
fn video(ui: &mut egui::Ui, s: &mut Settings, state: &State<'_>) {
    group(ui, "画面", |ui| {
        egui::Grid::new("settings-video-grid")
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label("目标帧率");
                egui::ComboBox::from_id_salt("settings-frame-rate").selected_text(format!("{} FPS",s.fps)).show_ui(ui,|ui| {
                    for fps in [30,60,120,165] {ui.selectable_value(&mut s.fps,fps,format!("{fps} FPS"));}
                    ui.menu_button("其它帧数",|ui| {
                        for fps in [24,90,120,144] {ui.selectable_value(&mut s.fps,fps,format!("{fps} FPS"));}
                        ui.separator();ui.label("自定义帧数");ui.add(egui::DragValue::new(&mut s.fps).range(5..=240).suffix(" FPS"));
                    });
                });
                ui.end_row();
                ui.label("输出分辨率");
                let labels = [
                    "保持远端分辨率",
                    "1920 × 1080",
                    "2560 × 1440",
                    "3840 × 2160",
                ];
                combo(ui, "settings-resolution", labels[s.resolution], |ui| {
                    for (i, label) in labels.iter().enumerate() {
                        ui.selectable_value(&mut s.resolution, i, *label);
                    }
                });
                ui.end_row();

            });
        ui.add_space(8.0);
        note(
            ui,
            "输出分辨率只调整串流编码尺寸，不修改远端桌面。实际帧率取决于被控端、屏幕和网络。",
        );

    });
    group(ui, "呈现与解码", |ui| {
        ui.add_enabled_ui(cfg!(windows),|ui|{ui.checkbox(&mut s.hdr,"原生串流：优先 HDR");});
        note(ui,"双方显示器均启用 HDR 且支持 10 位 HEVC / AV1 时自动协商。HDR 和编码切换需要重新连接；其它情况使用 SDR。当前 Linux 客户端以 SDR 呈现。");
        ui.checkbox(&mut s.vsync, "垂直同步");
        note(
            ui,
            "保存后立即应用到当前窗口。关闭可减少呈现等待，也可能出现撕裂。",
        );
        if state.streaming && s.vsync != state.presentation_vsync {
            ui.label(
                    RichText::new("保存后应用新的垂直同步设置")
                    .color(theme::ACCENT)
                    .size(12.0),
            );
        }
        ui.add_space(8.0);
        ui.checkbox(&mut s.software_decoder, "使用软件解码");
        note(ui, "默认优先使用硬件解码。更改后重新连接此设备生效。");
    });
    group(ui, "远端显示器布局", |ui| {
        for mode in [
            uu_core::screen_control::DisplayMode::Preserve,
            uu_core::screen_control::DisplayMode::ExclusiveVirtual,
        ] {
            ui.radio_value(&mut s.display_mode, mode, mode.label());
        }
        note(ui,"下次连接生效。保持现状不切换主屏或关闭显示器；独占虚拟屏会请求关闭其它屏幕，退出时请求恢复。");
    });
}
fn encoding(ui:&mut egui::Ui,s:&mut Settings,state:&State<'_>) {
    group(ui,"编码与码率",|ui| {
        egui::Grid::new("settings-encoding-grid").spacing([24.0,14.0]).show(ui,|ui| {
                ui.label("视频编码");
                combo(ui,"settings-codec",s.codec.label(),|ui| {
                    for codec in super::settings::CodecPreference::ALL {
                        if ui.selectable_value(&mut s.codec,codec,codec.label()).changed(){s.prefer_hevc=false;
                            if codec==super::settings::CodecPreference::Pyrowave{s.rate_control=Some(uu_core::native_quality::RateMode::Cbr);}}
                    }
                });
                ui.end_row();
                ui.label("原生码率模式");
                combo(ui,"settings-rate-control",s.rate_control.map_or("跟随主机",|r|r.label()),|ui| {
                    use uu_core::native_quality::RateMode;
                    ui.selectable_value(&mut s.rate_control,None,"跟随主机");
                    ui.selectable_value(&mut s.rate_control,Some(RateMode::Cbr),"CBR · 固定码率");
                    ui.add_enabled_ui(s.codec!=super::settings::CodecPreference::Pyrowave,|ui|{ui.selectable_value(&mut s.rate_control,Some(RateMode::Vbr),"VBR · 可变码率");});
                });
                ui.end_row();
                ui.label("码率");
                combo(
                    ui,
                    "settings-bitrate",
                    if state.streaming {super::settings::bitrate_label(s.bitrate,state.native)}else{super::BITRATE_PRESETS[s.bitrate].0},
                    |ui| {
                        for (i, (label, _, _)) in super::BITRATE_PRESETS.iter().enumerate() {
                            if (state.native&&i==0)||(!state.native&&state.streaming&&i==5){continue;}
                            if state.native&&i==5 {
                                if ui.selectable_label(matches!(s.bitrate,0|5),*label).clicked(){s.bitrate=5;}
                            }else if !state.native&&state.streaming&&i==0 {
                                if ui.selectable_label(matches!(s.bitrate,0|5),*label).clicked(){s.bitrate=0;}
                            }else{ui.selectable_value(&mut s.bitrate, i, *label);}
                        }
                    },
                );
                ui.end_row();
                if s.bitrate == 4 {
                    ui.label("自定义码率");
                    ui.add(egui::Slider::new(&mut s.custom_mbps, 1..=1000).suffix(" Mbps"));
                    ui.end_row();
                }
        });
        if state.native||!state.streaming {
            match super::settings::recommended_bitrate(&s.client,state.actual_size,state.actual_codec) {
                Some(kbps)=>{ui.label(format!("LAN Quality 推荐约 {:.1} Mbps",kbps as f64/1000.));},
                None=>{ui.label("推荐码率将在连接后按实际分辨率、帧率和协商编码计算。");},
            }
            if super::settings::recommended_bitrate(&s.client,state.actual_size,state.actual_codec)==Some(uu_core::native_quality::MAX_BITRATE_KBPS) {
                note(ui,"推荐值已达到当前 1000 Mbps 上限；此组合需要降低分辨率或 FPS，或自行调整画质目标。");
            }
            note(ui,"推荐档随分辨率、FPS 和编码更新；以良好画质为目标，复杂场景可手动提高。不是网络测速或所有画面的最低码率保证。UU 兼容模式的自动档由官方主机决定。");
        }
        if let Some(codec)=state.actual_codec.filter(|c|!c.is_empty()){ui.label(format!("当前编码：{}",codec.trim_start_matches("video/")));}
        note(
            ui,
            "原生连接按两端能力协商，明确选择不可用时会提示；UU 兼容模式支持 H.264 或优先 HEVC。切换编码需要重连。",
        );
        if s.codec==super::settings::CodecPreference::Pyrowave {
            note(ui,"PyroWave 当前支持 SDR、CBR；60 FPS 下推荐约 1080p 170 Mbps、4K 288 Mbps，帧率翻倍时码率约翻倍。还需预留协议与纠错带宽；主机需允许此编码。");
        }
    });
}
fn audio(ui: &mut egui::Ui, s: &mut Settings) {
    group(ui, "声音播放", |ui| {
        ui.checkbox(&mut s.muted, "静音");
        ui.add_space(8.0);
        super::audio_controls::volume_slider(ui, &mut s.volume);
        note(ui, "当前串流和全局默认分别保存；设备配置可覆盖默认值。");
    });
}
fn input(ui: &mut egui::Ui, s: &mut Settings, state:&State<'_>) {
    group(ui, "光标显示", |ui| {
        for mode in CursorMode::ALL {
            ui.radio_value(&mut s.cursor_mode, mode, mode.label());
        }
        if state.native&&state.streaming&&!state.native_cursor {note(ui,"主机未提供独立远端光标，当前保留本地指针。");}
        note(
            ui,
            &format!("{} 切换光标显示。新 Windows 原生主机独立传输远端光标的位置与形状；仅远端模式隐藏客户端光标。旧主机需更新后使用。",s.hotkeys.label(uu_core::hotkeys::Action::CursorMode)),
        );
    });
    group(ui, "指针移动", |ui| {
        ui.radio_value(&mut s.relative_mouse, false, "桌面 · 绝对定位");
        ui.radio_value(&mut s.relative_mouse, true, "游戏 · 相对移动");
        note(ui,&format!("{} 切换移动方式。游戏模式单击画面捕获鼠标，{} 释放或重新捕获；菜单和失焦时自动释放。",s.hotkeys.label(uu_core::hotkeys::Action::MouseMode),s.hotkeys.label(uu_core::hotkeys::Action::Capture)));
    });
    group(ui, "手柄与运动传感器", |ui| {
        ui.checkbox(&mut s.gamepad, "转发本地手柄和陀螺仪");
        if let Some(status)=state.gamepad_status{ui.label(status);}
        note(ui, "原生串流支持最多 4 个手柄，仅在画面获得输入焦点和控制权时转发；失焦或断线后释放。Xbox 模拟为 Xbox，NS/PS 模拟为 DS4 并保留实体支持的运动传感器。Windows 被控端需要 ViGEmBus；Linux 的 Xbox 需要 uinput、DS4 需要 UHID 访问权限。");
    });
    group(ui, "键盘与协作", |ui| {
        ui.checkbox(&mut s.capture_keyboard, "全屏时将系统快捷键交给远端");
        ui.checkbox(&mut s.nested_shortcuts, "将客户端快捷键交给远端（嵌套串流）");
        note(ui,&format!("{} 随时切换嵌套透传；其它客户端命令发送到远端。",s.hotkeys.label(uu_core::hotkeys::Action::ToggleNested)));
        note(ui,"系统键跟随全屏画面的输入焦点。客户端命令可在下方设置；Ctrl+Alt+Del 由 Windows 保留。");
        ui.add_space(8.0);
        if state.native {
            ui.checkbox(&mut s.native_clipboard, "同步剪贴板（文本和图片）");
        } else {
            ui.checkbox(&mut s.native_clipboard, "原生连接：同步文本和图片剪贴板");
            ui.checkbox(&mut s.clipboard, "兼容连接：同步文本剪贴板");
        }
        note(
            ui,
            "仅同步当前前台串流；原生共享需主机许可，支持最多30 KiB文本和8 MiB PNG图片，文件不传输。兼容连接只支持文本。",
        );
    });
    group(ui,"客户端快捷键",|ui|super::shortcuts_ui::editor(ui,&mut s.hotkeys));
}
fn stream(ui:&mut egui::Ui,s:&mut Settings) {
    group(ui, "串流界面", |ui| {
        ui.label("全屏悬浮入口默认开启");
        note(
            ui,
            &format!("默认位于右边缘、距顶部 15%。悬停展开、离开收起，{} 隐藏或显示。",s.hotkeys.label(uu_core::hotkeys::Action::Menu)),
        );
        ui.add_space(8.0);
        ui.checkbox(&mut s.hud, "Show HUD");
        ui.horizontal(|ui| {
            for level in super::settings::HudDetail::ALL {ui.radio_value(&mut s.hud_detail,level,level.label());}
        });
        note(
            ui,
            &format!("{} toggles HUD. Estimates are pipeline stage means, not input-to-display latency.",s.hotkeys.label(uu_core::hotkeys::Action::Stats)),
        );
    });
}
pub(super) fn software(ui: &mut egui::Ui, s: &mut Settings, actions: &mut Actions) {
    group(ui,"常规",|ui| {
        ui.horizontal(|ui| {ui.label("界面语言");ui.label("简体中文");});
        ui.checkbox(&mut s.app_autostart,"登录系统时自动打开软件");
        note(ui,"本机服务是否随软件启动，在服务管理的连接服务页设置。");
    });
    group(ui,"设备页外观",|ui| {
        ui.checkbox(&mut s.wallpaper_enabled,"显示背景壁纸");
        ui.label("壁纸图片接口");
        ui.add(egui::TextEdit::singleline(&mut s.wallpaper_api).desired_width(f32::INFINITY).hint_text("https://picsum.photos/1920/1080"));
        note(ui,"兼容 Moonlight V+ 使用的图片接口，也可填返回 JPEG / PNG 的图片 URL。缓存每日更新，获取失败继续使用已有壁纸。");
        if ui.button("刷新壁纸").clicked(){actions.refresh_wallpaper=true;}
    });
    group(ui,"设备发现",|ui| {
        ui.checkbox(&mut s.lan_discovery,"自动查找局域网设备");
        note(ui,"关闭后仍可使用保存的地址或手动添加设备；本机服务入口在连接服务中管理。");
    });
}

pub fn show(
    ctx: &egui::Context,
    s: &mut Settings,
    section: &mut Section,
    state: State<'_>,
    mut extra:impl FnMut(&mut egui::Ui,Section),
) -> Actions {
    let mut actions = Actions::default();
    let hotkeys_valid=s.hotkeys.issues().is_empty();
    if !state.streaming { *section=Section::Application; }
    let compact = ctx.screen_rect().height() < 520.0;
    egui::TopBottomPanel::top("settings-title")
        .exact_height(58.0)
        .frame(
            egui::Frame::none()
                .fill(theme::SURFACE)
                .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                if ui
                    .button(if state.streaming {
                        "‹ 返回串流"
                    } else {
                        "‹ 返回主页"
                    })
                    .clicked()
                {
                    actions.back = true;
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if state.streaming {"串流设置"}else{"应用设置"})
                        .size(18.0)
                        .strong()
                        .color(theme::INK),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::icon_button(ui, Icon::Close, "关闭软件").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui::icon_button(ui, Icon::Max, "最大化 / 还原").clicked() {
                        actions.maximize = true;
                    }
                    if ui::icon_button(ui, Icon::Min, "最小化").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    let response = ui.allocate_response(ui.available_size(), egui::Sense::drag());
                    if response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                });
            });
        });
    if !matches!(*section,Section::Connections|Section::Server) {
    egui::TopBottomPanel::bottom("settings-actions")
        .exact_height(if compact { 54.0 } else { 66.0 })
        .frame(
            egui::Frame::none()
                .fill(theme::SURFACE)
                .inner_margin(egui::Margin::symmetric(
                    20.0,
                    if compact { 8.0 } else { 14.0 },
                )),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                if ui.button("恢复默认").clicked() {
                    actions.reset = true;
                }
                if state.dirty {
                    ui.label(
                        RichText::new("有未保存的更改")
                            .color(theme::MUTED)
                            .size(12.0),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.can_save_profile
                        && ui
                            .add_enabled(hotkeys_valid,egui::Button::new("保存到设备配置"))
                            .on_hover_text("将当前串流设置为该设备的命名配置，下次连接继续使用")
                            .clicked()
                    {
                        actions.apply = true;
                        actions.save_profile = true;
                    }
                    if ui
                        .add_enabled(hotkeys_valid,
                            egui::Button::new(
                                RichText::new("保存并应用").color(egui::Color32::WHITE),
                            )
                            .fill(theme::ACCENT),
                        )
                        .clicked()
                    {
                        actions.apply = true;
                    }
                    if ui.button("取消").clicked() {
                        actions.back = true;
                    }
                });
            });
        });
    }
    if state.streaming && ctx.screen_rect().width() >= 760.0 {
        egui::SidePanel::left("settings-navigation")
            .exact_width(184.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(theme::SIDEBAR).inner_margin(14.0))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                for (title, items) in [
                    ("客户端", &[Section::Video, Section::Encoding, Section::Audio, Section::Input, Section::Stream][..]),

                ] {
                    ui.label(RichText::new(title).size(12.0).strong().color(theme::MUTED));
                    ui.add_space(6.0);
                    for &item in items {
                        if ui.add_sized([ui.available_width(), 38.0],
                            egui::SelectableLabel::new(*section == item, item.label())).clicked() {
                            *section = item;
                        }
                        ui.add_space(3.0);
                    }
                    ui.add_space(18.0);
                }
                if state.native {
                    ui.weak("主机");
                    for item in [Section::Connections,Section::Server] {
                        if ui.add_sized([ui.available_width(),38.],egui::SelectableLabel::new(*section==item,item.label())).clicked(){*section=item;}
                    }
                }
            });
    } else if state.streaming {
        egui::TopBottomPanel::top("settings-compact-navigation")
            .frame(egui::Frame::none().fill(theme::SIDEBAR).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for item in [Section::Video,Section::Encoding,Section::Audio,Section::Input,Section::Stream] {
                        ui.selectable_value(section, item, item.label());
                    }
                    if state.native {for item in [Section::Connections,Section::Server] {ui.selectable_value(section,item,item.label());}}
                });
            });
    }
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(theme::CANVAS)
                .inner_margin(if compact { 12.0 } else { 22.0 }),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("settings-content", *section as u8))
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    ui.set_width(viewport.width().min(860.0));
                    ui.label(
                        RichText::new(section.label())
                            .size(if compact { 20.0 } else { 24.0 })
                            .strong()
                            .color(theme::INK),
                    );
                    if !compact { note(ui, section.description()); }
                    if !matches!(*section,Section::Application|Section::Connections|Section::Server) {
                        note(ui,if state.streaming {
                            if state.can_save_profile {
                                "保存并应用只影响当前串流；“保存到设备配置”会更新这台设备当前选择的命名配置。"
                            } else if state.native {
                                "仅应用到当前串流；设备配置与全局默认不受影响。"
                            } else {
                                "UU 兼容串流设置会保存，下次连接继续使用。"
                            }
                        }else{"全局默认：未指定独立配置的设备使用这里的设置。"});
                    }
                    ui.add_space(if compact { 10.0 } else { 18.0 });
                    if state.storage_error {
                        group(ui, "设置文件需要处理", |ui| {
                            ui.colored_label(theme::DANGER, state.notice);
                            note(ui, "异常原件会保留。可先备份，再保存当前设置。");
                            if ui.button("备份异常文件并保存").clicked() {
                                actions.apply = true;
                                actions.recover = true;
                            }
                        });
                    } else if !state.notice.is_empty() {
                        ui.label(RichText::new(state.notice).color(theme::SUCCESS));
                        ui.add_space(12.0);
                    }
                    match section {
                        Section::Video => video(ui, s, &state),
                        Section::Encoding => encoding(ui,s,&state),
                        Section::Audio => audio(ui, s),
                        Section::Input => input(ui, s, &state),
                        Section::Stream => stream(ui,s),
                        Section::Application => software(ui, s, &mut actions),
                        Section::Connections|Section::Server=>extra(ui,*section),
                    }
                });
        });
    actions
}

/// The device editor uses exactly the same client fields as global/session settings.
pub(super) fn client_editor(ui:&mut egui::Ui, settings:&mut super::settings::ClientSettings, section:&mut Section) {
    ui.horizontal_wrapped(|ui| {
        for item in [Section::Video,Section::Encoding,Section::Audio,Section::Input,Section::Stream] {
            ui.selectable_value(section,item,item.label());
        }
    });
    ui.add_space(12.);
    let mut wrapper=Settings {client:settings.clone(),..Default::default()};
    let state=State { native:false,native_cursor:false,streaming:false,can_save_profile:false,dirty:false,storage_error:false,notice:"",presentation_vsync:settings.vsync,gamepad_status:None,actual_codec:None,actual_size:None };
    match section {Section::Encoding=>encoding(ui,&mut wrapper,&state),Section::Audio=>audio(ui,&mut wrapper),Section::Input=>input(ui,&mut wrapper,&state),Section::Stream=>stream(ui,&mut wrapper),_=>video(ui,&mut wrapper,&state)}
    *settings=wrapper.client;
}
