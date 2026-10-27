//! Shared editor for local and remotely managed host settings.
use eframe::egui;
use uu_core::device_book::{Encoding,RateMode,Sharing};
use uu_core::native::VideoCodec;

pub fn edit(ui:&mut egui::Ui,s:&mut Sharing,section:u8,monitors:&[(String,String)],portal:bool,naming:bool) {
    if section==0 {
        device_name(ui,s,naming);
        capture(ui,s,monitors,portal);
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
            ui.label("默认码率模式");
            egui::ComboBox::from_id_salt("server-rate").selected_text(match s.rate_control{RateMode::Cbr=>"CBR",RateMode::Vbr=>"VBR"}).show_ui(ui,|ui|{
                ui.selectable_value(&mut s.rate_control,RateMode::Cbr,"CBR");ui.selectable_value(&mut s.rate_control,RateMode::Vbr,"VBR");
            });
        });
        ui.weak("服务端明确选择时优先；服务端自动时使用客户端选择，双方自动按 AV1 → HEVC → H.264 协商。");
        ui.weak("码率模式默认由主机提供；客户端可为当前串流切换 CBR / VBR，不改写主机配置。VBR 峰值随客户端目标码率调整为两倍。");
        ui.add_space(10.);
        let label=match s.encoding{Encoding::Auto=>"自动",Encoding::Hardware=>"硬件",Encoding::Intel=>"Intel QSV",Encoding::Nvidia=>"NVIDIA 硬件",Encoding::Amd=>"AMD AMF",Encoding::Software=>"软件 H.264"};
        egui::ComboBox::from_id_salt("server-encoding").selected_text(label).show_ui(ui,|ui| {
            for (value,label) in [(Encoding::Auto,"自动"),(Encoding::Hardware,"硬件"),(Encoding::Intel,"Intel QSV"),(Encoding::Nvidia,"NVIDIA 硬件"),(Encoding::Amd,"AMD AMF"),(Encoding::Software,"软件 H.264")] {ui.selectable_value(&mut s.encoding,value,label);}
        });
        ui.add_space(10.);
        ui.weak("编码参数在下次启动服务后生效；默认优先低延迟。提高画质可能增加编码耗时。仅应用实际使用的 GPU 参数。");
        let tuning=&mut s.encoder_tuning;
        if matches!(s.encoding,Encoding::Auto|Encoding::Hardware|Encoding::Amd) {
            egui::CollapsingHeader::new("AMD AMF").id_salt("amd-tuning").show(ui,|ui| {
                use uu_core::device_book::AmdQuality;
                ui.horizontal(|ui| {
                    ui.label("画质预设");
                    for (value,label) in [(AmdQuality::Speed,"速度"),(AmdQuality::Balanced,"均衡"),(AmdQuality::Quality,"画质")] {
                        ui.selectable_value(&mut tuning.amd_quality,value,label);
                    }
                });
                ui.weak("Windows 原生 AMF，保持超低延迟用法与无 B 帧。");
            });
        }
        if matches!(s.encoding,Encoding::Auto|Encoding::Hardware|Encoding::Intel) {
            egui::CollapsingHeader::new("Intel QSV").id_salt("intel-tuning").show(ui,|ui| {
                use uu_core::device_book::LowPower;
                ui.add(egui::Slider::new(&mut tuning.intel_target_usage,1..=7).text("Target Usage · 1 画质 / 7 速度"));
                ui.horizontal(|ui| {
                    ui.label("低功耗引擎");
                    for (value,label) in [(LowPower::Auto,"自动"),(LowPower::On,"启用"),(LowPower::Off,"停用")] {
                        ui.selectable_value(&mut tuning.intel_low_power,value,label);
                    }
                });
                ui.weak("自动优先固定功能引擎；驱动不支持明确选择的参数时连接会说明原因。");
            });
        }
        if matches!(s.encoding,Encoding::Auto|Encoding::Hardware|Encoding::Nvidia) {
            egui::CollapsingHeader::new("NVIDIA · Windows").id_salt("nvidia-tuning").show(ui,|ui| {
                let mut custom=tuning.nvidia_quality_vs_speed.is_some();
                if ui.checkbox(&mut custom,"自定义画质 / 速度").changed() {
                    tuning.nvidia_quality_vs_speed=custom.then_some(0);
                }
                if let Some(value)=&mut tuning.nvidia_quality_vs_speed {
                    ui.add(egui::Slider::new(value,0..=100).text("0 速度 / 100 画质"));
                }
                ui.weak("通过显卡驱动的 Media Foundation 接口设置；驱动须支持并回报该值。");
            });
        }
    } else {
        ui.checkbox(&mut s.control,"允许已授权设备控制键盘、鼠标和手柄");
        ui.checkbox(&mut s.clipboard,"允许文本和图片剪贴板同步");
        ui.weak("每台设备还需获得对应授权；修改主机设置后下次启动生效。");
    }
}

pub fn device_name(ui:&mut egui::Ui,s:&mut Sharing,enabled:bool) {
    ui.horizontal(|ui|{ui.label("设备名称");ui.add_enabled(enabled,egui::TextEdit::singleline(&mut s.name).char_limit(64));});
}
pub fn capture(ui:&mut egui::Ui,s:&mut Sharing,monitors:&[(String,String)],portal:bool) {
        ui.add_space(8.);
        ui.label("初始显示器");
        let default_label=if portal {"系统选择"} else {"主显示器"};
        egui::ComboBox::from_id_salt("server-monitor").selected_text(if s.monitor.is_empty()||s.monitor==uu_core::native_displays::ALL_MONITORS{default_label}else{&s.monitor}).show_ui(ui,|ui| {
            ui.selectable_value(&mut s.monitor,String::new(),default_label);
            for (name,label) in monitors {ui.selectable_value(&mut s.monitor,name.clone(),label);}
        });
        if portal {ui.weak("屏幕选择由主机系统授权，远程重选仍需主机确认。");}
        else {ui.weak("连接后可在客户端切换显示器。");}
        ui.add_space(10.);ui.checkbox(&mut s.audio,"传输系统声音");
        ui.weak("帧率、输出尺寸和码率由各客户端调整。");

}
