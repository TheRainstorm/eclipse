use eframe::egui;
use uu_core::{host_admin::{Client,Request,Response,ServerSettings},media::MediaStats};
use std::time::{Duration,Instant};

#[derive(Default)]
pub(super) struct RemoteAdminUi {
    base:Option<ServerSettings>,
    pending:Option<(u64,Instant)>,
    seen:u64,
    notice:String,
}
impl RemoteAdminUi {
    fn send(&mut self,client:&Client,request:Request) {
        match client.request(request) {
            Ok(id)=>{self.pending=Some((id,Instant::now()));self.notice=uu_i18n::text("正在等待主机…").into();},
            Err(error)=>self.notice=error.to_string(),
        }
    }
    pub fn show(&mut self,ui:&mut egui::Ui,section:super::settings_page::Section,stats:Option<&MediaStats>) {
        let Some(stats)=stats else {ui.label(uu_i18n::text("串流已断开，请重新连接主机。"));return;};
        let client=&stats.host_admin;
        let response=client.response.lock().unwrap().clone();
        if let Some((id,response))=response.filter(|(id,_)|*id>self.seen) {
            self.seen=id;self.pending=None;
            match response {
                Response::Snapshot{snapshot}=>{
                    self.base=snapshot.server;
                    self.notice.clear();
                },
                Response::Saved{restart}=>{
                    self.notice=if restart {uu_i18n::text("配置已保存，主机正在重启；服务就绪后可重新连接。")}else{uu_i18n::text("配置已保存，下次启动服务生效。")}.into();
                    if !restart {self.base=None;}
                },
                Response::Applied=>{self.notice=uu_i18n::text("连接权限已更新；受影响设备需重新连接。").into();},
                Response::Error{message}=>self.notice=message,
                Response::SessionControls{..}=>{},
            }
        }
        if self.pending.is_some_and(|(_,at)|at.elapsed()>Duration::from_secs(10)) {
            self.pending=None;self.notice=uu_i18n::text("主机没有返回管理结果，请检查连接后重新读取。").into();
        }
        let available=client.available.load(std::sync::atomic::Ordering::Acquire);
        let snapshot=client.snapshot.lock().unwrap().clone();
        if available&&snapshot.is_none()&&self.pending.is_none() {self.send(client,Request::Inspect);}
        ui.horizontal(|ui| {
            if ui.add_enabled(available&&self.pending.is_none(),crate::theme::secondary_button(uu_i18n::text("重新读取主机信息"))).clicked(){self.send(client,Request::Inspect);}
            if self.pending.is_some(){ui.spinner();}
        });
        if !self.notice.is_empty(){ui.label(&self.notice);}
        if !available {ui.weak(uu_i18n::text("主机未提供连接管理，请更新两端并重新连接。"));return;}
        let Some(snapshot)=snapshot else{return;};
        let manage=snapshot.own_permissions.details().manage_server;
        ui.add_space(12.);
        if section==super::settings_page::Section::Connections {
            ui.label(uu_i18n::format!("已连接 {} 个会话",snapshot.connections.len()));
            ui.weak(uu_i18n::text("客户端只能查看会话状态；键盘、鼠标、手柄和画质由被控端服务面板控制。"));
            for connection in snapshot.connections {
                ui.push_id(&connection.id,|ui| {
                    ui.group(|ui| {
                        ui.strong(&connection.name);ui.weak(&connection.status);
                    });
                });
            }
            if !manage {ui.weak(uu_i18n::text("本机没有主机查看授权；被控端可调整设备授权。"));}
        }else{
            if !manage {ui.label(uu_i18n::text("房主尚未授予管理服务配置的权限。"));return;}
            ui.label(uu_i18n::text("主机配置只能在被控端修改；此处仅显示主机当前提供的只读信息。"));
            if let Some(base)=&self.base {
                ui.add_space(8.);
                ui.weak(uu_i18n::format!("当前显示器：{} 个",base.monitors.len()));
                ui.weak(uu_i18n::text("服务端可通过会话面板控制每个客户端的输入、显示器和画质。"));
            }
        }
    }
}
