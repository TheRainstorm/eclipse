//! Shared editor for local and remotely managed host settings.
use eframe::egui;
use uu_core::device_book::{Encoding,RateMode,Sharing};
use uu_core::native::VideoCodec;

pub fn edit(ui:&mut egui::Ui,s:&mut Sharing,section:u8,monitors:&[(String,String)],portal:bool,naming:bool) {
    if section==0 {
        ui.horizontal(|ui|{ui.label("本机名称");ui.add_enabled(naming,egui::TextEdit::singleline(&mut s.name).char_limit(64));});
        ui.add_space(8.);
        egui::ComboBox::from_id_salt("server-monitor").selected_text(if s.monitor.is_empty(){"主显示器 / 系统选择"}else if s.monitor==uu_core::native_displays::ALL_MONITORS{"允许远程切换显示器"}else{&s.monitor}).show_ui(ui,|ui| {
            ui.selectable_value(&mut s.monitor,String::new(),"主显示器 / 系统选择");
            ui.selectable_value(&mut s.monitor,uu_core::native_displays::ALL_MONITORS.into(),"允许远程切换显示器");
            for (name,label) in monitors {ui.selectable_value(&mut s.monitor,name.clone(),label);}
        });
        if portal {ui.weak("屏幕选择由主机系统授权，远程重选仍需主机确认。");}
        ui.add_space(10.);ui.checkbox(&mut s.audio,"传输系统声音");
        ui.weak("帧率、输出尺寸和码率由各客户端调整。");
    } else if section==1 {
        ui.horizontal_wrapped(|ui| {
            ui.label("允许的编码");
            for (codec,label) in [(VideoCodec::H264,"H.264"),(VideoCodec::Hevc,"HEVC"),(VideoCodec::Av1,"AV1")] {
                let mut selected=s.allowed_codecs.contains(&codec);
                if ui.checkbox(&mut selected,label).changed() {if selected{s.allowed_codecs.push(codec);}else{s.allowed_codecs.retain(|c|*c!=codec);}}
            }
        });
        ui.add_space(10.);
        ui.horizontal_wrapped(|ui| {
            ui.label("编码优先级");
            egui::ComboBox::from_id_salt("server-codec").selected_text(match s.codec{None=>"自动协商",Some(VideoCodec::H264)=>"H.264",Some(VideoCodec::Hevc)=>"HEVC",Some(VideoCodec::Av1)=>"AV1"}).show_ui(ui,|ui| {
                for (codec,label) in [(None,"自动协商"),(Some(VideoCodec::H264),"H.264"),(Some(VideoCodec::Hevc),"HEVC"),(Some(VideoCodec::Av1),"AV1")] {ui.selectable_value(&mut s.codec,codec,label);}
            });
            ui.label("码率模式");
            egui::ComboBox::from_id_salt("server-rate").selected_text(match s.rate_control{RateMode::Cbr=>"CBR",RateMode::Vbr=>"VBR"}).show_ui(ui,|ui|{
                ui.selectable_value(&mut s.rate_control,RateMode::Cbr,"CBR");ui.selectable_value(&mut s.rate_control,RateMode::Vbr,"VBR");
            });
        });
        ui.weak("服务端明确选择时优先；服务端自动时使用客户端选择，双方自动按 AV1 → HEVC → H.264 协商。");
        ui.add_space(10.);
        let label=match s.encoding{Encoding::Auto=>"自动",Encoding::Hardware=>"硬件",Encoding::Intel=>"Intel QSV",Encoding::Nvidia=>"NVIDIA 硬件",Encoding::Amd=>"AMD AMF",Encoding::Software=>"软件 H.264"};
        egui::ComboBox::from_id_salt("server-encoding").selected_text(label).show_ui(ui,|ui| {
            for (value,label) in [(Encoding::Auto,"自动"),(Encoding::Hardware,"硬件"),(Encoding::Intel,"Intel QSV"),(Encoding::Nvidia,"NVIDIA 硬件"),(Encoding::Amd,"AMD AMF"),(Encoding::Software,"软件 H.264")] {ui.selectable_value(&mut s.encoding,value,label);}
        });
    } else {
        ui.checkbox(&mut s.control,"允许已授权设备控制键盘、鼠标和手柄");
        ui.checkbox(&mut s.clipboard,"允许文本剪贴板同步");
        ui.weak("每台设备还需获得对应授权；修改主机设置后下次启动生效。");
    }
}
