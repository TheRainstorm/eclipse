use eframe::egui;
use uu_core::{host_admin::{Client,Request,Response,ServerSettings},device_book::Sharing,media::MediaStats};
use std::time::{Duration,Instant};

#[derive(Default)]
pub(super) struct RemoteAdminUi {
    base:Option<ServerSettings>,
    draft:Option<Sharing>,
    pending:Option<(u64,Instant)>,
    seen:u64,
    notice:String,
    tab:u8,
}
impl RemoteAdminUi {
    fn send(&mut self,client:&Client,request:Request) {
        match client.request(request) {
            Ok(id)=>{self.pending=Some((id,Instant::now()));self.notice="正在等待主机…".into();},
            Err(error)=>self.notice=error.to_string(),
        }
    }
    pub fn show(&mut self,ui:&mut egui::Ui,section:super::settings_page::Section,stats:Option<&MediaStats>) {
        let Some(stats)=stats else {ui.label("串流已断开，请重新连接主机。");return;};
        let client=&stats.host_admin;
        let response=client.response.lock().unwrap().clone();
        if let Some((id,response))=response.filter(|(id,_)|*id>self.seen) {
            self.seen=id;self.pending=None;
            match response {
                Response::Snapshot{snapshot}=>{
                    if let Some(server)=snapshot.server {
                        let dirty=self.base.as_ref().zip(self.draft.as_ref()).is_some_and(|(base,draft)|&base.saved!=draft);
                        if !dirty {self.draft=Some(server.saved.clone());self.base=Some(server);}
                    }
                    self.notice.clear();
                },
                Response::Saved{restart}=>{
                    self.notice=if restart {"配置已保存，主机正在重启；服务就绪后可重新连接。"}else{"配置已保存，下次启动服务生效。"}.into();
                    if !restart {self.base=None;self.draft=None;}
                },
                Response::Applied=>{self.notice="连接权限已更新；受影响设备需重新连接。".into();},
                Response::Error{message}=>self.notice=message,
            }
        }
        if self.pending.is_some_and(|(_,at)|at.elapsed()>Duration::from_secs(10)) {
            self.pending=None;self.notice="主机没有返回管理结果，请检查连接后重新读取。".into();
        }
        let available=client.available.load(std::sync::atomic::Ordering::Acquire);
        let snapshot=client.snapshot.lock().unwrap().clone();
        if available&&snapshot.is_none()&&self.pending.is_none() {self.send(client,Request::Inspect);}
        ui.horizontal(|ui| {
            if ui.add_enabled(available&&self.pending.is_none(),egui::Button::new("重新读取主机信息")).clicked(){self.send(client,Request::Inspect);}
            if self.pending.is_some(){ui.spinner();}
        });
        if !self.notice.is_empty(){ui.label(&self.notice);}
        if !available {ui.weak("主机未提供连接管理，请更新两端并重新连接。");return;}
        let Some(snapshot)=snapshot else{return;};
        let manage=snapshot.own_permissions.details().manage_server;
        ui.add_space(12.);
        if section==super::settings_page::Section::Connections {
            ui.label(format!("已连接 {} / {} 个会话",snapshot.connections.len(),uu_core::shared_host::MAX_SESSIONS));
            ui.weak("修改授权会断开对应设备的会话并释放输入，重连后使用新权限。");
            for connection in snapshot.connections {
                ui.push_id(&connection.id,|ui| {
                    ui.group(|ui| {
                        ui.strong(&connection.name);ui.weak(&connection.status);
                        let mut permissions=connection.permissions;
                        ui.add_enabled_ui(manage&&self.pending.is_none(),|ui| {
                            ui.horizontal_wrapped(|ui| {
                                if super::sharing_ui::permissions(ui,&mut permissions){self.send(client,Request::Permissions{peer:connection.peer.clone(),permissions});}
                                if ui.button("断开此会话").clicked(){self.send(client,Request::Disconnect{session:connection.id.clone()});}
                            });
                        });
                    });
                });
            }
            if !manage {ui.weak("本机没有主机管理授权；房主可在服务管理的设备授权中授予。");}
        }else{
            if !manage {ui.label("房主尚未授予管理服务配置的权限。");return;}
            if self.base.is_none()&&self.pending.is_none(){self.send(client,Request::Inspect);}
            let (Some(base),Some(draft))=(&self.base,&mut self.draft) else{return;};
            ui.horizontal_wrapped(|ui|{for (tab,label) in [(0,"采集与声音"),(1,"编码"),(2,"访问权限")] {ui.selectable_value(&mut self.tab,tab,label);}});
            ui.add_space(10.);
            super::server_config_ui::edit(ui,draft,self.tab,&base.monitors,base.portal,true);
            ui.add_space(12.);
            if base.active!=base.saved {ui.weak("已保存配置与当前运行配置不同，下次启动生效。");}
            ui.weak("保存供下次启动使用；保存并重启会断开主机上的全部串流。修改连接端口后，请同步更新设备地址。");
            let mut save=None;
            ui.horizontal(|ui| {
                if ui.add_enabled(self.pending.is_none()&&draft!=&base.saved,egui::Button::new("保存，下次启动生效")).clicked(){save=Some(false);}
                if ui.add_enabled(self.pending.is_none(),egui::Button::new("保存并重启服务")).clicked(){save=Some(true);}
            });
            if let Some(restart)=save {let request=Request::Save{revision:base.revision.clone(),settings:draft.clone(),restart};self.send(client,request);}
        }
    }
}
