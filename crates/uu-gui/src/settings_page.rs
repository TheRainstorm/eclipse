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
    Audio,
    Input,
    Stream,
    Server,
    Application,
}
impl Section {
    pub const ALL: [Self; 6] = [
        Self::Video,
        Self::Audio,
        Self::Input,
        Self::Stream,
        Self::Server,
        Self::Application,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "客户端 · 画面与显示",
            Self::Audio => "客户端 · 声音",
            Self::Input => "客户端 · 键盘与鼠标",
            Self::Stream => "客户端 · 串流信息",
            Self::Server => "服务端设置",
            Self::Application => "应用与诊断",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Video => "调整清晰度、帧率与呈现方式。",
            Self::Audio => "设置默认音量与声音播放。",
            Self::Input => "配置键鼠、手柄、运动传感器与输入归属。",
            Self::Stream => "设置串流菜单与统计信息。",
            Self::Server => "管理本机采集、编码、权限与服务运行状态。",
            Self::Application => "管理串流界面、默认偏好与软件信息。",
        }
    }
}
pub struct State<'a> {
    pub streaming: bool,
    pub dirty: bool,
    pub storage_error: bool,
    pub notice: &'a str,
    pub presentation_vsync: bool,
    pub gamepad_status: Option<&'a str>,
}
#[derive(Default)]
pub struct Actions {
    pub back: bool,
    pub apply: bool,
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
    egui::ComboBox::from_id_salt(id)
        .width(210.0)
        .selected_text(selected)
        .show_ui(ui, body);
}
fn video(ui: &mut egui::Ui, s: &mut Settings, state: &State<'_>) {
    group(ui, "串流画质", |ui| {
        egui::Grid::new("settings-video-grid")
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label("目标帧率");
                ui.add(
                    egui::DragValue::new(&mut s.fps)
                        .range(5..=144)
                        .suffix(" FPS"),
                );
                ui.end_row();
                ui.label("画质");
                combo(
                    ui,
                    "settings-bitrate",
                    super::BITRATE_PRESETS[s.bitrate].0,
                    |ui| {
                        for (i, (label, _, _)) in super::BITRATE_PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut s.bitrate, i, *label);
                        }
                    },
                );
                ui.end_row();
                if s.bitrate == 4 {
                    ui.label("自定义码率");
                    ui.add(egui::Slider::new(&mut s.custom_mbps, 1..=100).suffix(" Mbps"));
                    ui.end_row();
                }
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
                ui.label("视频编码");
                combo(ui,"settings-codec",s.codec.label(),|ui| {
                    for codec in super::settings::CodecPreference::ALL {
                        if ui.selectable_value(&mut s.codec,codec,codec.label()).changed(){s.prefer_hevc=false;}
                    }
                });
                ui.end_row();
            });
        ui.add_space(8.0);
        note(
            ui,
            "输出分辨率只调整串流编码尺寸，不修改远端桌面。实际帧率取决于被控端、屏幕和网络。",
        );
        note(
            ui,
            "原生连接按两端能力协商，明确选择不可用时会提示；UU 兼容模式支持 H.264 或优先 HEVC，暂不支持 AV1。切换编码需要重连。",
        );
    });
    group(ui, "呈现与解码", |ui| {
        ui.checkbox(&mut s.vsync, "垂直同步");
        note(
            ui,
            "关闭可减少呈现等待，也可能出现撕裂；重新打开串流窗口后生效。",
        );
        if state.streaming && s.vsync != state.presentation_vsync {
            ui.label(
                RichText::new("当前窗口仍使用启动时的垂直同步设置")
                    .color(theme::ACCENT)
                    .size(12.0),
            );
        }
        ui.add_space(8.0);
        ui.checkbox(&mut s.software_decoder, "使用软件解码");
        note(ui, "默认优先使用硬件解码。更改后重新打开软件生效。");
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
fn audio(ui: &mut egui::Ui, s: &mut Settings) {
    group(ui, "声音播放", |ui| {
        ui.checkbox(&mut s.muted, "静音");
        ui.add_space(8.0);
        ui.add(
            egui::Slider::new(&mut s.volume, 0..=100)
                .suffix("%")
                .text("音量"),
        );
        note(ui, "当前串流和全局默认分别保存；设备配置可覆盖默认值。");
    });
}
fn input(ui: &mut egui::Ui, s: &mut Settings, state:&State<'_>) {
    group(ui, "光标显示", |ui| {
        for mode in CursorMode::ALL {
            ui.radio_value(&mut s.cursor_mode, mode, mode.label());
        }
        note(
            ui,
            "Ctrl+Alt+C 在两种光标显示间切换。原生共享目前由控制端绘制本地光标。",
        );
    });
    group(ui, "指针移动", |ui| {
        ui.radio_value(&mut s.relative_mouse, false, "桌面 · 绝对定位");
        ui.radio_value(&mut s.relative_mouse, true, "游戏 · 相对移动");
        note(ui,"Ctrl+Alt+P 切换移动方式。游戏模式单击画面捕获鼠标，Ctrl+Alt+Z 释放或重新捕获；菜单和失焦时自动释放。");
    });
    group(ui, "手柄与运动传感器", |ui| {
        ui.checkbox(&mut s.gamepad, "转发本地手柄和陀螺仪");
        if let Some(status)=state.gamepad_status{ui.label(status);}
        note(ui, "原生串流支持最多 4 个手柄，仅在画面获得输入焦点和控制权时转发；失焦或断线后释放。运动传感器需要实体手柄支持，Windows 被控端需要 ViGEmBus，Linux 被控端需要 UHID 访问权限。");
    });
    group(ui, "键盘与协作", |ui| {
        ui.checkbox(&mut s.capture_keyboard, "全屏时将系统快捷键交给远端");
        note(ui,"系统键跟随全屏画面的输入焦点。Ctrl+左 Alt+F / X 切换全屏，Q 退出串流，Z 释放或捕获输入；这些本地命令始终由客户端处理。Ctrl+Alt+Del 由 Windows 保留。");
        ui.add_space(8.0);
        ui.checkbox(&mut s.clipboard, "同步文本剪贴板");
        note(
            ui,
            "仅在当前前台串流同步有界文本；原生共享还需主机许可。图片和文件不传输。",
        );
    });
}
fn stream(ui:&mut egui::Ui,s:&mut Settings) {
    group(ui, "串流界面", |ui| {
        ui.checkbox(&mut s.floating_menu, "全屏显示悬浮菜单");
        note(
            ui,
            "默认位于右边缘、距顶部 15%。悬停展开、离开收起，Ctrl+Alt+H 隐藏或显示。",
        );
        ui.add_space(8.0);
        ui.checkbox(&mut s.hud, "显示串流统计");
        note(
            ui,
            "Ctrl+Alt+S 隐藏或显示。统计中的已知段合计不等于完整输入到屏幕延迟。",
        );
    });
}
fn application(ui: &mut egui::Ui, s: &mut Settings, actions: &mut Actions) {
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
    group(ui, "软件信息", |ui| {
        ui.label(format!("uu-client · {}", env!("CARGO_PKG_VERSION")));
        note(ui, "软件信息、源提交与版本核验可在“关于软件”查看。");
        if ui.button("关于软件").clicked() {
            actions.about = true;
        }
    });
}

pub fn show(
    ctx: &egui::Context,
    s: &mut Settings,
    section: &mut Section,
    state: State<'_>,
    mut server: impl FnMut(&mut egui::Ui),
) -> Actions {
    let mut actions = Actions::default();
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
                    RichText::new("应用设置")
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
                if *section != Section::Server && ui.button("恢复默认").clicked() {
                    actions.reset = true;
                }
                if state.dirty && *section!=Section::Server {
                    ui.label(
                        RichText::new("有未保存的更改")
                            .color(theme::MUTED)
                            .size(12.0),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if *section != Section::Server && ui
                        .add(
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
    if ctx.screen_rect().width() >= 760.0 {
        egui::SidePanel::left("settings-navigation")
            .exact_width(184.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(theme::SIDEBAR).inner_margin(14.0))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                note(ui, "偏好设置");
                ui.add_space(12.0);
                for item in Section::ALL {
                    if ui
                        .add_sized(
                            [ui.available_width(), 42.0],
                            egui::SelectableLabel::new(*section == item, item.label()),
                        )
                        .clicked()
                    {
                        *section = item;
                    }
                    ui.add_space(3.0);
                }
            });
    } else {
        egui::TopBottomPanel::top("settings-compact-navigation")
            .frame(egui::Frame::none().fill(theme::SIDEBAR).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for item in Section::ALL {
                        ui.selectable_value(section, item, item.label());
                    }
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
                    if !matches!(section,Section::Server|Section::Application) {
                        note(ui,if state.streaming {"仅应用到当前串流；设备配置与全局默认不受影响。"}else{"全局默认：未指定独立配置的设备使用这里的设置。"});
                    }
                    ui.add_space(if compact { 10.0 } else { 18.0 });
                    if state.storage_error && *section!=Section::Server {
                        group(ui, "设置文件需要处理", |ui| {
                            ui.colored_label(theme::DANGER, state.notice);
                            note(ui, "异常原件会保留。可先备份，再保存当前设置。");
                            if ui.button("备份异常文件并保存").clicked() {
                                actions.apply = true;
                                actions.recover = true;
                            }
                        });
                    } else if !state.notice.is_empty() && *section!=Section::Server {
                        ui.label(RichText::new(state.notice).color(theme::SUCCESS));
                        ui.add_space(12.0);
                    }
                    match section {
                        Section::Video => video(ui, s, &state),
                        Section::Audio => audio(ui, s),
                        Section::Input => input(ui, s, &state),
                        Section::Stream => stream(ui,s),
                        Section::Server => server(ui),
                        Section::Application => application(ui, s, &mut actions),
                    }
                });
        });
    actions
}

/// The device editor uses exactly the same client fields as global/session settings.
pub(super) fn client_editor(ui:&mut egui::Ui, settings:&mut super::settings::ClientSettings, section:&mut Section) {
    ui.horizontal_wrapped(|ui| {
        for item in [Section::Video,Section::Audio,Section::Input,Section::Stream] {
            ui.selectable_value(section,item,item.label().trim_start_matches("客户端 · "));
        }
    });
    ui.add_space(12.);
    let mut wrapper=Settings {client:settings.clone(),..Default::default()};
    let state=State { streaming:false,dirty:false,storage_error:false,notice:"",presentation_vsync:settings.vsync,gamepad_status:None };
    match section {Section::Audio=>audio(ui,&mut wrapper),Section::Input=>input(ui,&mut wrapper,&state),Section::Stream=>stream(ui,&mut wrapper),_=>video(ui,&mut wrapper,&state)}
    *settings=wrapper.client;
}
