use eframe::egui;
use uu_core::{device_auth::{PairingChoice,PairingDisplay,PairingMethod},device_trust::{Permissions,PublicDevice,TrustStore}};
use zeroize::Zeroizing;

#[derive(Default)]
pub(super) struct ClientUi {
    method:PairingMethod,
    password:Zeroizing<String>,
    peer:Option<String>,
    notice:String,
}
impl ClientUi {
    /// Returns true when the user cancels this connection.
    pub fn show(&mut self,ctx:&egui::Context,display:&PairingDisplay)->bool {
        let prompt=display.prompt();
        let code=display.code();
        if prompt.is_none() && code.is_none() {self.password=Default::default();self.peer=None;return false;}
        let mut open=true;
        egui::Window::new("首次连接 · 设备配对").id("device-pair-code".into())
            .open(&mut open).collapsible(false).resizable(false)
            .anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO).default_width(460.)
            .show(ctx,|ui| {
                if let Some(prompt)=prompt {
                    let id=prompt.device.id();
                    if self.peer.as_deref()!=Some(&id) {
                        self.peer=Some(id.clone());self.notice.clear();self.password=Default::default();
                        self.method=if prompt.preauthorized{PairingMethod::PublicKey}else{PairingMethod::Code};
                    }
                    ui.strong(&prompt.device.name);
                    ui.weak("主机公钥指纹 · 首次信任前请核对目标设备");
                    ui.label(egui::RichText::new(&id).monospace().small());
                    ui.add_space(10.);
                    ui.horizontal_wrapped(|ui| {
                        for (method,label) in [(PairingMethod::Code,"配对码"),(PairingMethod::Password,"密码"),(PairingMethod::TemporaryCode,"临时配对码")] {
                            ui.selectable_value(&mut self.method,method,label);
                        }
                        ui.add_enabled_ui(prompt.preauthorized,|ui| {ui.selectable_value(&mut self.method,PairingMethod::PublicKey,"已授权公钥");});
                    });
                    ui.add_space(8.);
                    match self.method {
                        PairingMethod::Code=>{ui.label("主机输入本机显示的八位短码并批准，保存长期授权。");},
                        PairingMethod::TemporaryCode=>{ui.label("主机确认八位短码后转为本次授权；此前长期授权会撤销，断开后需重新配对。");},
                        PairingMethod::Password=>{
                            ui.label("输入主机预先设置的配对密码；验证成功后保存设备授权。");
                            ui.add(egui::TextEdit::singleline(&mut *self.password).password(true).char_limit(256).hint_text("主机配对密码"));
                        },
                        PairingMethod::PublicKey=>{ui.label("主机已预先授权本机公钥。核对上方指纹后信任并连接。");},
                    }
                    if !prompt.preauthorized {ui.weak("公钥方式需先在主机“访问权限”中添加本机公钥。");}
                    if !self.notice.is_empty(){ui.colored_label(super::theme::DANGER,&self.notice);}
                    let valid=self.method!=PairingMethod::Password || (8..=256).contains(&self.password.len());
                    if ui.add_enabled(valid,egui::Button::new(if self.method==PairingMethod::PublicKey{"信任并连接"}else{"开始配对"})).clicked() {
                        let choice=PairingChoice{method:self.method,password:std::mem::take(&mut self.password)};
                        if let Err(error)=display.choose(choice){self.notice=error.to_string();}
                    }
                }else if let Some(code)=code {
                    ui.label("请在主机输入下面的配对码，并批准本设备：");
                    ui.add_space(14.);
                    ui.label(egui::RichText::new(format!("{} {}",&code[..4],&code[4..])).size(34.).monospace().color(super::theme::ACCENT));
                    if ui.button("复制配对码").clicked(){ctx.copy_text(code);}
                    ui.weak(if display.selected_method()==PairingMethod::TemporaryCode {"临时授权只用于本次串流，断开后失效。"}else{"配对完成后，以后连接无需重复输入。"});
                    ui.spinner();ui.label("等待主机确认…");
                }
            });
        !open
    }
}

#[derive(Default)]
pub(super) struct HostUi {
    loaded:bool,
    password:Zeroizing<String>,
    configured:bool,
    password_permissions:Permissions,
    public_key:String,
    name:String,
    key_permissions:Permissions,
    notice:String,
    job:Option<std::sync::mpsc::Receiver<Result<Option<Permissions>,String>>>,
}
impl HostUi {
    pub fn show(&mut self,ui:&mut egui::Ui,store:&TrustStore,name:&str,access:Option<&std::sync::Arc<uu_core::shared_host::Access>>) {
        if !self.loaded {
            self.password_permissions=Permissions::CONTROL;self.key_permissions=Permissions::CONTROL;
            match store.pairing_password_permissions() {
                Ok(value)=>{self.configured=value.is_some();if let Some(value)=value{self.password_permissions=value;}},
                Err(error)=>self.notice=error.to_string(),
            }
            self.loaded=true;
        }
        if let Some(job)=&self.job {
            if let Ok(result)=job.try_recv() {
                self.job=None;
                match result {
                    Ok(value)=>{self.configured=value.is_some();if let Some(value)=value{self.password_permissions=value;}self.notice="配对密码设置已保存，后续新设备配对使用新设置。".into();},
                    Err(error)=>self.notice=error,
                }
            }
        }
        ui.strong("本机公钥");
        ui.weak("作为控制端时，复制此公钥到主机的授权公钥列表。");
        let card=store.card(name);
        ui.horizontal_wrapped(|ui| {
            if ui.button("复制本机公钥").clicked(){ui.ctx().copy_text(card.export_public_key());}
            if ui.button("复制公钥指纹").clicked(){ui.ctx().copy_text(card.id());}
        });
        ui.add_space(14.);
        ui.strong("密码配对");
        ui.label(if self.configured{"已设置配对密码"}else{"尚未设置配对密码"});
        ui.weak("设置后，知道密码的新设备可配对并保存授权。更改密码用于后续配对，已有设备授权仍由下方列表管理。");
        ui.add(egui::TextEdit::singleline(&mut *self.password).password(true).char_limit(256).hint_text("新配对密码 · 8–256 字节"));
        ui.horizontal_wrapped(|ui|{super::sharing_ui::permissions(ui,&mut self.password_permissions);});
        let mut change=None;
        ui.horizontal(|ui| {
            if ui.add_enabled(self.job.is_none()&&(8..=256).contains(&self.password.len()),egui::Button::new("保存配对密码")).clicked(){change=Some(true);}
            if ui.add_enabled(self.job.is_none()&&self.configured,egui::Button::new("移除配对密码")).clicked(){change=Some(false);}
            if self.job.is_some(){ui.spinner();}
        });
        if let Some(set)=change {
            let store=store.clone();let password=std::mem::take(&mut self.password);let permissions=self.password_permissions;
            let (send,receive)=std::sync::mpsc::channel();self.job=Some(receive);let ctx=ui.ctx().clone();
            std::thread::spawn(move|| {
                let result=if set{store.set_pairing_password(&password,permissions)}else{store.clear_pairing_password()};
                let result=result.and_then(|_|store.pairing_password_permissions()).map_err(|e|e.to_string());
                let _=send.send(result);ctx.request_repaint();
            });
        }
        ui.add_space(14.);
        ui.strong("预先授权控制端公钥");
        ui.add(egui::TextEdit::singleline(&mut self.name).char_limit(64).hint_text("控制端名称"));
        ui.add(egui::TextEdit::multiline(&mut self.public_key).char_limit(8192).desired_rows(2).hint_text("粘贴 Eclipse 或 SSH Ed25519 公钥"));
        ui.horizontal_wrapped(|ui|{super::sharing_ui::permissions(ui,&mut self.key_permissions);});
        if ui.button("添加授权公钥").clicked() {
            let result=PublicDevice::from_public_key(&self.public_key,&self.name).and_then(|device| {
                store.authorize(device.clone(),self.key_permissions,None)?;
                if let Some(access)=access{access.sessions.revoke(&device.id());}
                Ok(())
            });
            self.notice=match result {Ok(())=>{self.public_key.clear();self.name.clear();"公钥已授权，控制端首次连接时核对主机指纹并选择已授权公钥。".into()},Err(error)=>error.to_string()};
        }
        if !self.notice.is_empty(){ui.add_space(8.);ui.label(&self.notice);}
    }
}
