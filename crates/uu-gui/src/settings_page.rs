//! Full-page preferences. Rendering emits actions; App owns applying changes.
use super::{
    settings::{CursorMode, Settings, WallpaperSource},
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
            Self::Video => uu_i18n::text("画面"),
            Self::Encoding => uu_i18n::text("编码"),
            Self::Audio => uu_i18n::text("声音"),
            Self::Input => uu_i18n::text("输入"),
            Self::Stream => uu_i18n::text("串流信息"),
            Self::Application => uu_i18n::text("应用与诊断"),
            Self::Connections=>uu_i18n::text("连接信息"),
            Self::Server=>uu_i18n::text("服务端设置"),
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Video => uu_i18n::text("调整分辨率、帧率与呈现方式。"),
            Self::Encoding => uu_i18n::text("选择编码格式、码率并查看当前编码。"),
            Self::Audio => uu_i18n::text("设置默认音量与声音播放。"),
            Self::Input => uu_i18n::text("配置键鼠、手柄、运动传感器与输入归属。"),
            Self::Stream => uu_i18n::text("设置串流菜单与统计信息。"),
            Self::Application => uu_i18n::text("管理串流界面、默认偏好与软件信息。"),
            Self::Connections=>uu_i18n::text("查看连接到主机的设备及其授权。"),
            Self::Server=>uu_i18n::text("修改远端服务下次启动使用的配置。"),
        }
    }
}
pub struct State<'a> {
    pub native:bool,
    pub host_managed_bitrate:bool,
    pub max_fps:u32,
    pub codecs:Option<&'a [uu_core::native::VideoCodec]>,
    pub rate_control_available:bool,
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
    ui.add(egui::Label::new(RichText::new(text).size(theme::FONT_META).color(theme::palette(ui.visuals()).muted)).wrap());
}
fn group(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    theme::card(ui.visuals())
        .inner_margin(if ui.ctx().screen_rect().height() < 520.0 {
            12.0
        } else {
            theme::SPACE_4
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(theme::FONT_SECTION).strong().color(theme::palette(ui.visuals()).text));
            ui.add_space(8.0);
            body(ui);
        });
    ui.add_space(12.0);
}
fn combo(ui: &mut egui::Ui, id: &str, selected: &str, body: impl FnOnce(&mut egui::Ui)) {
    crate::theme::Select::from_id_salt((ui.ctx().viewport_id(),id))
        .width(210.0)
        .selected_text(selected)
        .show_ui(ui, body);
}
fn video(ui: &mut egui::Ui, s: &mut Settings, state: &State<'_>) {
    group(ui, uu_i18n::text("画面"), |ui| {
        egui::Grid::new("settings-video-grid")
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(uu_i18n::text("目标帧率"));
                crate::theme::Select::from_id_salt("settings-frame-rate").selected_text(if s.follow_remote_fps {uu_i18n::text("保持远端帧率").to_owned()}else{format!("{} FPS",s.fps)}).show_ui(ui,|ui| {
                    if crate::theme::selectable_label(ui,s.follow_remote_fps,uu_i18n::text("保持远端帧率")).clicked(){s.follow_remote_fps=true;}
                    for fps in [30,60,120,165].into_iter().filter(|fps|*fps<=state.max_fps) {
                        if crate::theme::selectable_label(ui,!s.follow_remote_fps&&s.fps==fps,format!("{fps} FPS")).clicked(){s.fps=fps;s.follow_remote_fps=false;}
                    }
                    ui.menu_button(uu_i18n::text("其它帧数"),|ui| {
                        for fps in [24,90,120,144].into_iter().filter(|fps|*fps<=state.max_fps) {
                            if crate::theme::selectable_label(ui,!s.follow_remote_fps&&s.fps==fps,format!("{fps} FPS")).clicked(){s.fps=fps;s.follow_remote_fps=false;}
                        }
                        ui.separator();ui.label(uu_i18n::text("自定义帧数"));
                        if crate::theme::number_input(ui,egui::DragValue::new(&mut s.fps).range(5..=state.max_fps.clamp(5,240)).suffix(" FPS")).changed(){s.follow_remote_fps=false;}
                    });
                });
                ui.end_row();
                ui.label(uu_i18n::text("输出分辨率"));
                let labels = [
                    uu_i18n::text("保持远端分辨率"),
                    "1920 × 1080",
                    "2560 × 1440",
                    "3840 × 2160",
                ];
                combo(ui, "settings-resolution", labels[s.resolution], |ui| {
                    for (i, label) in labels.iter().enumerate() {
                        crate::theme::selectable_value(ui,&mut s.resolution, i, *label);
                    }
                });
                ui.end_row();

            });
        ui.add_space(8.0);
        note(
            ui,
            uu_i18n::text("输出分辨率只调整串流编码尺寸，不修改远端桌面。实际帧率取决于被控端、屏幕和网络。"),
        );

    });
    group(ui, uu_i18n::text("呈现与解码"), |ui| {
        ui.add_enabled_ui(cfg!(windows),|ui|{theme::switch(ui,&mut s.hdr,uu_i18n::text("原生串流：优先 HDR"));});
        note(ui,uu_i18n::text("双方显示器均启用 HDR 且支持 10 位 HEVC / AV1 时自动协商。HDR 和编码切换需要重新连接；其它情况使用 SDR。当前 Linux 客户端以 SDR 呈现。"));
        theme::switch(ui,&mut s.vsync, uu_i18n::text("垂直同步"));
        note(
            ui,
            uu_i18n::text("保存后立即应用到当前窗口。关闭可减少呈现等待，也可能出现撕裂。"),
        );
        if state.streaming && s.vsync != state.presentation_vsync {
            ui.label(
                    RichText::new(uu_i18n::text("保存后应用新的垂直同步设置"))
                    .color(theme::palette(ui.visuals()).accent)
                    .size(12.0),
            );
        }
        ui.add_space(8.0);
        theme::switch(ui,&mut s.software_decoder, uu_i18n::text("使用软件解码"));
        note(ui, uu_i18n::text("默认优先使用硬件解码。更改后重新连接此设备生效。"));
    });
}
fn encoding(ui:&mut egui::Ui,s:&mut Settings,state:&State<'_>) {
    group(ui,uu_i18n::text("编码与码率"),|ui| {
        egui::Grid::new("settings-encoding-grid").spacing([24.0,14.0]).show(ui,|ui| {
                ui.label(uu_i18n::text("视频编码"));
                combo(ui,"settings-codec",s.codec.label(),|ui| {
                    for codec in super::settings::CodecPreference::ALL {
                        use uu_core::native::VideoCodec as V;
                        let wire=match codec {super::settings::CodecPreference::Auto=>None,super::settings::CodecPreference::H264=>Some(V::H264),super::settings::CodecPreference::Hevc=>Some(V::Hevc),super::settings::CodecPreference::Av1=>Some(V::Av1),super::settings::CodecPreference::Pyrowave=>Some(V::Pyrowave)};
                        if state.codecs.is_some_and(|codecs|wire.is_some_and(|value|!codecs.contains(&value))){continue;}
                        if crate::theme::selectable_value(ui,&mut s.codec,codec,codec.label()).changed(){s.prefer_hevc=false;
                            if codec==super::settings::CodecPreference::Pyrowave{s.rate_control=Some(uu_core::native_quality::RateMode::Cbr);}}
                    }
                });
                ui.end_row();
                ui.label(uu_i18n::text("原生码率模式"));
                ui.add_enabled_ui(state.rate_control_available,|ui|{combo(ui,"settings-rate-control",s.rate_control.map_or(uu_i18n::text("跟随主机"),|r|r.label()),|ui| {
                    use uu_core::native_quality::RateMode;
                    crate::theme::selectable_value(ui,&mut s.rate_control,None,uu_i18n::text("跟随主机"));
                    crate::theme::selectable_value(ui,&mut s.rate_control,Some(RateMode::Cbr),uu_i18n::text("CBR · 固定码率"));
                    ui.add_enabled_ui(s.codec!=super::settings::CodecPreference::Pyrowave,|ui|{crate::theme::selectable_value(ui,&mut s.rate_control,Some(RateMode::Vbr),uu_i18n::text("VBR · 可变码率"));});
                });
                });
                ui.end_row();
                ui.label(uu_i18n::text("码率"));
                combo(
                    ui,
                    "settings-bitrate",
                    if state.streaming {super::settings::bitrate_label(s.bitrate,!state.host_managed_bitrate)}else{uu_i18n::text(super::BITRATE_PRESETS[s.bitrate])},
                    |ui| {
                        for i in [5,0,1,2,3,4] {
                            let label=uu_i18n::text(super::BITRATE_PRESETS[i]);
                            if (!state.host_managed_bitrate&&i==0)||(state.host_managed_bitrate&&state.streaming&&i==5){continue;}
                            if !state.host_managed_bitrate&&i==5 {
                                if crate::theme::selectable_label(ui,matches!(s.bitrate,0|5),label).clicked(){s.bitrate=5;}
                            }else if state.host_managed_bitrate&&state.streaming&&i==0 {
                                if crate::theme::selectable_label(ui,matches!(s.bitrate,0|5),label).clicked(){s.bitrate=0;}
                            }else{crate::theme::selectable_value(ui,&mut s.bitrate, i, label);}
                        }
                    },
                );
                ui.end_row();
                if s.bitrate == 4 {
                    ui.label(uu_i18n::text("自定义码率"));
                    crate::theme::number_input(ui,egui::DragValue::new(&mut s.custom_mbps).range(1..=1000).suffix(" Mbps"));
                    ui.end_row();
                }
        });
        if state.native||!state.streaming {
            match super::settings::recommended_bitrate(&s.client,state.actual_size,state.actual_codec) {
                Some(kbps)=>{ui.label(uu_i18n::format!("LAN Quality 推荐约 {:.1} Mbps",kbps as f64/1000.));},
                None=>{ui.label(uu_i18n::text("推荐码率将在连接后按实际分辨率、帧率和协商编码计算。"));},
            }
            if super::settings::recommended_bitrate(&s.client,state.actual_size,state.actual_codec)==Some(uu_core::native_quality::MAX_BITRATE_KBPS) {
                note(ui,uu_i18n::text("推荐值已达到当前 1000 Mbps 上限；此组合需要降低分辨率或 FPS，或自行调整画质目标。"));
            }
            note(ui,uu_i18n::text("推荐档随分辨率、FPS 和编码更新；以良好画质为目标，复杂场景可手动提高。不是网络测速或所有画面的最低码率保证。UU 兼容模式的自动档由官方主机决定。"));
        }
        if let Some(codec)=state.actual_codec.filter(|c|!c.is_empty()){ui.label(uu_i18n::format!("当前编码：{}",codec.trim_start_matches("video/")));}
        note(
            ui,
            uu_i18n::text("原生连接按两端能力协商，明确选择不可用时会提示；UU 兼容模式支持 H.264 或优先 HEVC。切换编码需要重连。"),
        );
        if s.codec==super::settings::CodecPreference::Pyrowave {
            note(ui,uu_i18n::text("PyroWave 当前支持 SDR、CBR；60 FPS 下推荐约 1080p 170 Mbps、4K 288 Mbps，帧率翻倍时码率约翻倍。还需预留协议与纠错带宽；主机需允许此编码。"));
        }
    });
}
fn audio(ui: &mut egui::Ui, s: &mut Settings) {
    group(ui, uu_i18n::text("声音播放"), |ui| {
        theme::switch(ui,&mut s.muted, uu_i18n::text("静音"));
        ui.add_space(8.0);
        super::audio_controls::volume_slider(ui, &mut s.volume);
        note(ui, uu_i18n::text("当前串流和全局默认分别保存；设备配置可覆盖默认值。"));
    });
}
fn input(ui: &mut egui::Ui, s: &mut Settings, state:&State<'_>) {
    group(ui, uu_i18n::text("光标显示"), |ui| {
        for mode in CursorMode::ALL {
            crate::theme::radio_value(ui,&mut s.cursor_mode, mode, mode.label());
        }
        if state.native&&state.streaming&&!state.native_cursor {note(ui,uu_i18n::text("主机未提供独立远端光标，当前保留本地指针。"));}
        note(
            ui,
            &uu_i18n::format!("{} 切换光标显示。新 Windows 原生主机独立传输远端光标的位置与形状；仅远端模式隐藏客户端光标。旧主机需更新后使用。",s.hotkeys.label(uu_core::hotkeys::Action::CursorMode)),
        );
    });
    group(ui, uu_i18n::text("指针移动"), |ui| {
        crate::theme::radio_value(ui,&mut s.relative_mouse, false, uu_i18n::text("桌面 · 绝对定位"));
        crate::theme::radio_value(ui,&mut s.relative_mouse, true, uu_i18n::text("游戏 · 相对移动"));
        note(ui,&uu_i18n::format!("{} 切换移动方式。游戏模式单击画面捕获鼠标，{} 释放或重新捕获；菜单和失焦时自动释放。",s.hotkeys.label(uu_core::hotkeys::Action::MouseMode),s.hotkeys.label(uu_core::hotkeys::Action::Capture)));
    });
    group(ui, uu_i18n::text("手柄与运动传感器"), |ui| {
        theme::switch(ui,&mut s.gamepad, uu_i18n::text("转发本地手柄和陀螺仪"));
        if let Some(status)=state.gamepad_status{ui.label(status);}
        note(ui, uu_i18n::text("原生串流支持最多 4 个手柄，仅在画面获得输入焦点和控制权时转发；失焦或断线后释放。Xbox 模拟为 Xbox，NS/PS 模拟为 DS4 并保留实体支持的运动传感器。Windows 被控端需要 ViGEmBus；Linux 的 Xbox 需要 uinput、DS4 需要 UHID 访问权限。"));
    });
    group(ui, uu_i18n::text("键盘与协作"), |ui| {
        theme::switch(ui,&mut s.capture_keyboard, uu_i18n::text("全屏时将系统快捷键交给远端"));
        theme::switch(ui,&mut s.nested_shortcuts, uu_i18n::text("将客户端快捷键交给远端（嵌套串流）"));
        note(ui,&uu_i18n::format!("{} 随时切换嵌套透传；其它客户端命令发送到远端。",s.hotkeys.label(uu_core::hotkeys::Action::ToggleNested)));
        note(ui,&uu_i18n::format!("{} 隐藏所有串流窗口并返回本机，再按一次恢复；此全局命令在嵌套模式下仍由本机处理。",s.hotkeys.label(uu_core::hotkeys::Action::ToggleLocal)));
        note(ui,uu_i18n::text("系统键跟随全屏画面的输入焦点。客户端命令可在下方设置；Ctrl+Alt+Del 由 Windows 保留。"));
        ui.add_space(8.0);
        if state.native {
            theme::switch(ui,&mut s.native_clipboard, uu_i18n::text("同步剪贴板（文本和图片）"));
        } else {
            theme::switch(ui,&mut s.native_clipboard, uu_i18n::text("原生连接：同步文本和图片剪贴板"));
            theme::switch(ui,&mut s.clipboard, uu_i18n::text("兼容连接：同步文本剪贴板"));
        }
        note(
            ui,
            uu_i18n::text("仅同步当前前台串流；原生共享需主机许可，支持最多30 KiB文本和8 MiB PNG图片，文件不传输。兼容连接只支持文本。"),
        );
    });
}
fn stream(ui:&mut egui::Ui,s:&mut Settings) {
    group(ui, uu_i18n::text("串流界面"), |ui| {
        ui.label(uu_i18n::text("全屏悬浮入口默认开启"));
        note(
            ui,
            &uu_i18n::format!("默认位于右边缘、距顶部 15%。悬停展开、离开收起，{} 隐藏或显示。",s.hotkeys.label(uu_core::hotkeys::Action::Menu)),
        );
        ui.add_space(8.0);
        theme::switch(ui,&mut s.hud, uu_i18n::text("启用 HUD"));
        crate::theme::slider(ui,egui::Slider::new(&mut s.hud_appearance.vertical_position,0.0..=1.0).step_by(0.001).text(uu_i18n::text("HUD垂直位置")));
        note(ui,uu_i18n::text("0为画面顶部，1为底部；默认在1080高度下距顶部约36点，HUD始终保持在画面内。"));
        crate::theme::slider(ui,egui::Slider::new(&mut s.hud_appearance.opacity,0.0..=1.0).step_by(0.01).text(uu_i18n::text("HUD背景不透明度")));
        crate::theme::slider(ui,egui::Slider::new(&mut s.hud_appearance.diagnostic_opacity,0.0..=1.0).step_by(0.01).text(uu_i18n::text("诊断背景不透明度")));
        if ui.add(crate::theme::secondary_button(uu_i18n::text("恢复HUD外观默认值")).small()).clicked(){s.hud_appearance=Default::default();}
        note(
            ui,
            &format!("{} toggles HUD. Estimates are pipeline stage means, not input-to-display latency.",s.hotkeys.label(uu_core::hotkeys::Action::Stats)),
        );
    });
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash)]
pub(super) enum SoftwareSection { #[default] General, Appearance, Shortcuts, Discovery, VirtualDisplays }
impl SoftwareSection {
    pub const ALL:[Self;5]=[Self::General,Self::Appearance,Self::Shortcuts,Self::Discovery,Self::VirtualDisplays];
    pub fn label(self)->&'static str {uu_i18n::text(match self {
        Self::General=>"常规",Self::Appearance=>"外观",Self::Shortcuts=>"快捷键",Self::Discovery=>"设备发现",Self::VirtualDisplays=>"虚拟显示器规格",
    })}
    pub fn reset(self,s:&mut Settings) {
        let defaults=Settings::default();
        match self {
            Self::General=>{s.language=defaults.language;s.app_autostart=defaults.app_autostart;s.close_to_tray=defaults.close_to_tray;}
            Self::Appearance=>{s.theme_preference=defaults.theme_preference;s.color_theme=defaults.color_theme;s.wallpaper_enabled=defaults.wallpaper_enabled;s.wallpaper_source=defaults.wallpaper_source;s.wallpaper_favorite=defaults.wallpaper_favorite;s.wallpaper_api=defaults.wallpaper_api;s.show_input_status=defaults.show_input_status;}
            Self::Shortcuts=>s.application_hotkeys=Some(Default::default()),
            Self::Discovery=>s.lan_discovery=defaults.lan_discovery,
            Self::VirtualDisplays=>s.virtual_display_presets=defaults.virtual_display_presets,
        }
    }
}
pub(super) fn software(ui: &mut egui::Ui, s: &mut Settings, actions: &mut Actions,section:SoftwareSection) {
    match section {
    SoftwareSection::VirtualDisplays=>group(ui,uu_i18n::text("虚拟显示器规格"),|ui| {
        super::virtual_display_presets::editor(ui,&mut s.virtual_display_presets);
    }),
    SoftwareSection::Shortcuts=>group(ui,uu_i18n::text("客户端快捷键"),|ui|{
        let mut bindings=s.application_hotkeys().clone();
        super::shortcuts_ui::editor(ui,&mut bindings);
        if &bindings!=s.application_hotkeys(){s.application_hotkeys=Some(bindings);}
        note(ui,uu_i18n::text("快捷键属于本机应用，所有串流窗口共用；保存后会更新已打开的新版窗口。设备画质配置不覆盖快捷键。"));
    }),
    SoftwareSection::General=>group(ui,uu_i18n::text("常规"),|ui| {
        ui.horizontal(|ui| {ui.label(uu_i18n::text("界面语言"));
            crate::theme::Select::from_id_salt("application-language").selected_text(s.language.label()).show_ui(ui,|ui|{
                for language in uu_i18n::Language::ALL {crate::theme::selectable_value(ui,&mut s.language,language,language.label());}
            });});
        crate::theme::switch(ui,&mut s.app_autostart,uu_i18n::text("登录系统时自动打开软件"));
        note(ui,uu_i18n::text("本机服务是否随软件启动，在服务管理的连接服务页设置。"));
        if super::tray::supported() {
            ui.add_space(8.0);
            crate::theme::switch(ui,&mut s.close_to_tray,uu_i18n::text("关闭主窗口时最小化到系统托盘"));
            note(ui,uu_i18n::text("共享服务继续运行；从托盘菜单重新打开或退出 Eclipse。"));
        }
    }),
    SoftwareSection::Appearance=>{
        group(ui,uu_i18n::text("界面主题"),|ui| {
            ui.add_enabled_ui(!s.color_theme.light_only(),|ui| {
                combo(ui,"application-theme",theme::preference_label(if s.color_theme.light_only(){egui::ThemePreference::Light}else{s.theme_preference}),|ui| {
                    for preference in theme::PREFERENCES {
                        crate::theme::selectable_value(ui,&mut s.theme_preference,preference,theme::preference_label(preference));
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(uu_i18n::text("主题配色"));
                combo(ui,"application-color-theme",theme::color_theme_label(s.color_theme),|ui| {
                    for color in theme::COLOR_THEMES {
                        crate::theme::selectable_value(ui,&mut s.color_theme,color,theme::color_theme_label(color));
                    }
                });
            });
            if s.color_theme.light_only() { note(ui,uu_i18n::text("天蓝色使用浅色外观。")); }
            note(ui,uu_i18n::text("保存后应用到主窗口和串流窗口。"));
        });
        group(ui,uu_i18n::text("设备页外观"),|ui| {
        crate::theme::switch(ui,&mut s.wallpaper_enabled,uu_i18n::text("显示背景壁纸"));
        ui.horizontal(|ui| {
            ui.label(uu_i18n::text("壁纸图片来源"));
            crate::theme::Select::from_id_salt("wallpaper-source").selected_text(s.wallpaper_source.label()).show_ui(ui,|ui| {
                for source in WallpaperSource::ALL {crate::theme::selectable_value(ui,&mut s.wallpaper_source,source,source.label());}
            });
        });
        if s.wallpaper_source == WallpaperSource::CustomApi {
            ui.label(uu_i18n::text("壁纸图片接口"));
            ui.add(crate::theme::Input::new(&mut s.wallpaper_api).desired_width(f32::INFINITY).hint_text("https://example.com/image"));
        }
        note(ui,uu_i18n::text("摄影使用 Lorem Picsum，二次元使用 Pipw；自定义 API 可填返回 JPEG / PNG / WebP 的图片地址。缓存每日更新，获取失败继续使用已有壁纸。"));
        if ui.add(crate::theme::secondary_button(uu_i18n::text("刷新壁纸"))).clicked(){actions.refresh_wallpaper=true;}
    });
        group(ui,uu_i18n::text("串流界面"),|ui| {
            crate::theme::switch(ui,&mut s.show_input_status,uu_i18n::text("输入状态显示"));
            note(ui,uu_i18n::text("全屏串流失去焦点时，显示“点击画面以获取键盘控制”提示。"));
        });
    },
    SoftwareSection::Discovery=>group(ui,uu_i18n::text("设备发现"),|ui| {
        crate::theme::switch(ui,&mut s.lan_discovery,uu_i18n::text("自动查找局域网设备"));
        note(ui,uu_i18n::text("关闭后仍可使用保存的地址或手动添加设备；本机服务入口在连接服务中管理。"));
    }),
    }
}

pub fn show(
    ctx: &egui::Context,
    s: &mut Settings,
    section: &mut Section,
    state: State<'_>,
    mut extra:impl FnMut(&mut egui::Ui,Section),
) -> Actions {
    let mut actions = Actions::default();
    let hotkeys_valid=if state.streaming {true}else{s.application_hotkeys().issues().is_empty()};
    if !state.streaming { *section=Section::Application; }
    else if *section==Section::Application {*section=Section::Stream;}
    let compact = ctx.screen_rect().height() < 520.0;
    egui::TopBottomPanel::top("settings-title")
        .exact_height(58.0)
        .frame(
            egui::Frame::none()
                .fill(theme::palette(&ctx.style().visuals).card)
                .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                if ui.add(theme::toolbar_button(if state.streaming {
                        uu_i18n::text("‹ 返回串流")
                    } else {
                        uu_i18n::text("‹ 返回主页")
                    },theme::ButtonKind::Ghost))
                    .clicked()
                {
                    actions.back = true;
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if state.streaming {uu_i18n::text("串流设置")}else{uu_i18n::text("应用设置")})
                        .size(18.0)
                        .strong()
                        .color(theme::palette(ui.visuals()).text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::icon_button(ui, Icon::Close, uu_i18n::text("关闭软件")).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui::icon_button(ui, Icon::Max, uu_i18n::text("最大化 / 还原")).clicked() {
                        actions.maximize = true;
                    }
                    if ui::icon_button(ui, Icon::Min, uu_i18n::text("最小化")).clicked() {
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
        .min_height(if compact { 54.0 } else { 66.0 })
        .frame(
            egui::Frame::none()
                .fill(theme::palette(&ctx.style().visuals).card)
                .inner_margin(egui::Margin::symmetric(
                    20.0,
                    if compact { 8.0 } else { 14.0 },
                )),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                if ui.add(crate::theme::secondary_button(uu_i18n::text("恢复默认"))).clicked() {
                    actions.reset = true;
                }
                if state.dirty {
                    ui.label(
                        RichText::new(uu_i18n::text("有未保存的更改"))
                            .color(theme::palette(ui.visuals()).muted)
                            .size(12.0),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.can_save_profile
                        && ui
                            .add_enabled(hotkeys_valid,crate::theme::secondary_button(uu_i18n::text("保存到设备配置")))
                            .on_hover_text(uu_i18n::text("将当前串流设置为该设备的命名配置，下次连接继续使用"))
                            .clicked()
                    {
                        actions.apply = true;
                        actions.save_profile = true;
                    }
                    if ui
                        .add_enabled(hotkeys_valid,
                            theme::primary_button(uu_i18n::text("保存并应用")),
                        )
                        .clicked()
                    {
                        actions.apply = true;
                    }
                    if ui.add(theme::ghost_button(uu_i18n::text("取消"))).clicked() {
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
            .frame(egui::Frame::none().fill(theme::palette(&ctx.style().visuals).sidebar).inner_margin(14.0))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                for (title, items) in [
                    (uu_i18n::text("客户端"), &[Section::Video, Section::Encoding, Section::Audio, Section::Input, Section::Stream][..]),

                ] {
                    ui.label(RichText::new(title).size(12.0).strong().color(theme::palette(ui.visuals()).muted));
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
                    ui.weak(uu_i18n::text("主机"));
                    for item in [Section::Connections,Section::Server] {
                        if ui.add_sized([ui.available_width(),38.],egui::SelectableLabel::new(*section==item,item.label())).clicked(){*section=item;}
                    }
                }
            });
    } else if state.streaming {
        egui::TopBottomPanel::top("settings-compact-navigation")
            .frame(egui::Frame::none().fill(theme::palette(&ctx.style().visuals).sidebar).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for item in [Section::Video,Section::Encoding,Section::Audio,Section::Input,Section::Stream] {
                        crate::theme::tab_value(ui,section, item, item.label());
                    }
                    if state.native {for item in [Section::Connections,Section::Server] {crate::theme::tab_value(ui,section,item,item.label());}}
                });
            });
    }
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(theme::palette(&ctx.style().visuals).base)
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
                            .color(theme::palette(ui.visuals()).text),
                    );
                    if !compact { note(ui, section.description()); }
                    if !matches!(*section,Section::Application|Section::Connections|Section::Server) {
                        note(ui,if state.streaming {
                            if state.can_save_profile {
                                uu_i18n::text("保存并应用只影响当前串流；“保存到设备配置”会更新这台设备当前选择的命名配置。")
                            } else if state.native {
                                uu_i18n::text("仅应用到当前串流；设备配置与全局默认不受影响。")
                            } else {
                                uu_i18n::text("UU 兼容串流设置会保存，下次连接继续使用。")
                            }
                        }else{uu_i18n::text("全局默认：未指定独立配置的设备使用这里的设置。")});
                    }
                    ui.add_space(if compact { 10.0 } else { 18.0 });
                    if state.storage_error {
                        group(ui, uu_i18n::text("设置文件需要处理"), |ui| {
                            ui.colored_label(theme::palette(ui.visuals()).danger, state.notice);
                            note(ui, uu_i18n::text("异常原件会保留。可先备份，再保存当前设置。"));
                            if ui.add(crate::theme::secondary_button(uu_i18n::text("备份异常文件并保存"))).clicked() {
                                actions.apply = true;
                                actions.recover = true;
                            }
                        });
                    } else if !state.notice.is_empty() {
                        ui.label(RichText::new(state.notice).color(theme::palette(ui.visuals()).success));
                        ui.add_space(12.0);
                    }
                    match section {
                        Section::Video => video(ui, s, &state),
                        Section::Encoding => encoding(ui,s,&state),
                        Section::Audio => audio(ui, s),
                        Section::Input => input(ui, s, &state),
                        Section::Stream => stream(ui,s),
                        Section::Application => software(ui, s, &mut actions,SoftwareSection::General),
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
            crate::theme::tab_value(ui,section,item,item.label());
        }
    });
    ui.add_space(12.);
    let mut wrapper=Settings {client:settings.clone(),..Default::default()};
    let state=State { native:false,host_managed_bitrate:false,max_fps:240,codecs:None,rate_control_available:true,native_cursor:false,streaming:false,can_save_profile:false,dirty:false,storage_error:false,notice:"",presentation_vsync:settings.vsync,gamepad_status:None,actual_codec:None,actual_size:None };
    match section {Section::Encoding=>encoding(ui,&mut wrapper,&state),Section::Audio=>audio(ui,&mut wrapper),Section::Input=>input(ui,&mut wrapper,&state),Section::Stream=>stream(ui,&mut wrapper),_=>video(ui,&mut wrapper,&state)}
    *settings=wrapper.client;
}

#[cfg(test)]
mod language_tests {
    #[test]
    fn opening_software_settings_does_not_create_unsaved_hotkey_changes() {
        let ctx=eframe::egui::Context::default();let mut settings=super::Settings::default();let before=settings.clone();
        for section in super::SoftwareSection::ALL {
            let _=ctx.run(eframe::egui::RawInput{screen_rect:Some(eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO,eframe::egui::vec2(900.,900.))),..Default::default()},|ctx|{
                eframe::egui::CentralPanel::default().show(ctx,|ui|super::software(ui,&mut settings,&mut super::Actions::default(),section));
            });
        }
        assert_eq!(settings,before);
    }
}

#[cfg(test)]
mod parameter_tests {
    use super::*;

    #[test]
    fn device_parameters_are_visible_without_expanding_and_preserve_saved_values() {
        let mut settings = super::super::settings::ClientSettings {
            fps: 177,
            resolution: 3,
            bitrate: 4,
            custom_mbps: 79,
            codec: super::super::settings::CodecPreference::Av1,
            prefer_hevc: true,
            hdr: false,
            vsync: false,
            ..Default::default()
        };
        let before = settings.clone();
        for color in theme::COLOR_THEMES {
            for (mut section, labels) in [
                (Section::Video, vec!["目标帧率", "输出分辨率", "垂直同步", "使用软件解码"]),
                (Section::Encoding, vec!["视频编码", "原生码率模式", "自定义码率"]),
            ] {
                let ctx = egui::Context::default();
                theme::apply_color_theme(&ctx,color);
                let output = ctx.run(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000., 1800.))),
                    ..Default::default()
                }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| client_editor(ui, &mut settings, &mut section));
                });
                fn collect(shape: &egui::Shape, text: &mut Vec<String>) {
                    match shape {
                        egui::Shape::Text(value) => text.push(value.galley.job.text.clone()),
                        egui::Shape::Vec(shapes) => for shape in shapes { collect(shape, text); },
                        _ => {},
                    }
                }
                let mut text = Vec::new();
                for shape in output.shapes { collect(&shape.shape, &mut text); }
                for label in labels {
                    assert!(text.iter().any(|text| text == uu_i18n::text(label)), "parameter {label} must be visible on first paint");
                }
                for label in ["极速流畅", "平衡", "极致画质", "画质偏好", "视频兼容性", "高级设置"] {
                    assert!(!text.iter().any(|text| text == uu_i18n::text(label)), "removed shortcut {label} must not hide actual parameters");
                }
                assert_eq!(settings, before, "opening the editor must preserve all saved client values");
            }
        }
    }
}
