//! Connection drafts own only listener fields; media drafts cannot replay them.
use super::{card, egui, muted, theme, Action, Info, RichText, Sharing, Workspace};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Draft {
    ipv6_enabled: bool,
    service_port: u16,
    discovery_port: u16,
    plugin_services: Vec<String>,
}
impl From<&Sharing> for Draft {
    fn from(s: &Sharing) -> Self {
        Self {
            ipv6_enabled: s.ipv6_enabled,
            service_port: s.service_port,
            discovery_port: s.discovery_port,
            plugin_services: s.plugin_services.clone(),
        }
    }
}
impl Workspace {
    fn save_connection_settings(&mut self) -> bool {
        let Some(draft) = self.connection_draft.clone() else {
            return true;
        };
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(uu_i18n::text("设备配置不可用")))
            .and_then(|store| {
                store.edit(|book| {
                    book.sharing.ip_enabled = true;
                    book.sharing.ipv6_enabled = draft.ipv6_enabled;
                    book.sharing.service_port = draft.service_port;
                    book.sharing.discovery_port = draft.discovery_port;
                    book.sharing.plugin_services = draft.plugin_services;
                    Ok(())
                })
            });
        let saved = result.is_ok();
        match result {
            Ok(()) => {
                self.connection_draft = None;
                self.feedback.success(
                    self.service_scope(),
                    uu_i18n::text("服务端设置已保存，下次启动生效"),
                );
            }
            Err(error) => self.feedback.error(
                self.service_scope(),
                uu_i18n::format!("无法保存连接服务：{error:#}"),
            ),
        }
        self.refresh();
        saved
    }
    pub(super) fn services(
        &mut self,
        ui: &mut egui::Ui,
        info: &Info,
        actions: &mut Vec<Action>,
        connection: &mut dyn FnMut(&mut egui::Ui, Option<&str>),
    ) {
        let locked = self.host.stopping();
        #[cfg(not(windows))]
        {
        ui.horizontal(|ui| {
            if ui.add_enabled(!locked, crate::theme::primary_button(uu_i18n::text("加入连接服务"))).clicked() { self.enrollment.open(); }
            ui.weak(uu_i18n::text("使用同一个接入码，让两台电脑互相发现并连接。"));
        });
        self.enrollment.status(ui);
        card(ui, |ui| {
            if let Some(target) = self.service_directory.show(ui, self.service_config.as_ref(), &self.book) { actions.push(Action::OpenPrepared(target)); }
        });
        }
        let mut draft = self
            .connection_draft
            .clone()
            .unwrap_or_else(|| Draft::from(&self.book.sharing));
        if self.host.running() {
            muted(
                ui,
                uu_i18n::text("连接入口修改后保存，下次启动服务生效。可保存并重启服务立即应用。"),
            );
        }
        card(ui, |ui| {
            ui.strong(uu_i18n::text("IP 直连"));
            muted(
                ui,
                uu_i18n::text("同一网络或已有路由时直接连接，默认开启。"),
            );
            ui.add_enabled_ui(!locked, |ui| {
                crate::theme::switch(ui, &mut draft.ipv6_enabled, uu_i18n::text("启用 IPv6"))
                    .on_hover_text(uu_i18n::text(
                    "允许通过 IPv6 地址直连，默认开启。关闭后仅监听 IPv4；保存并重启服务后生效。",
                ));
                egui::Grid::new("connection-ip-ports")
                    .spacing([20., 10.])
                    .show(ui, |ui| {
                        ui.label(uu_i18n::text("服务端口 · TCP"));
                        crate::theme::number_input(
                            ui,
                            egui::DragValue::new(&mut draft.service_port).range(1..=65535),
                        );
                        ui.end_row();
                        ui.label(uu_i18n::text("发现端口 · UDP"));
                        crate::theme::number_input(
                            ui,
                            egui::DragValue::new(&mut draft.discovery_port).range(1..=65535),
                        );
                        ui.end_row();
                    });
            });
            self.endpoint_state(ui, uu_i18n::text("IP 直连"));
            ui.collapsing(uu_i18n::text("本机地址"), |ui| {
                if let Ok(addresses) = crate::native_interfaces::enumerate() {
                    for address in addresses
                        .into_iter()
                        .filter(|a| a.ip.is_ipv4() || draft.ipv6_enabled)
                    {
                        let endpoint =
                            std::net::SocketAddr::new(address.ip, draft.service_port).to_string();
                        ui.horizontal(|ui| {
                            ui.monospace(&endpoint);
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("复制")).small())
                                .clicked()
                            {
                                ui.ctx().copy_text(endpoint);
                            }
                        });
                    }
                }
                muted(
                    ui,
                    uu_i18n::text("手动输入可达地址支持跨网段；自动发现通常不跨路由。"),
                );
            });
        });
        for provider in &info.providers {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(provider.name);
                    let mut enabled = draft.plugin_services.iter().any(|id| id == provider.id);
                    if ui
                        .add_enabled_ui(!locked, |ui| {
                            crate::theme::switch(ui, &mut enabled, uu_i18n::text("启用"))
                        })
                        .inner
                        .changed()
                    {
                        draft.plugin_services.retain(|id| id != provider.id);
                        if enabled {
                            draft.plugin_services.push(provider.id.into());
                        }
                    }
                });
                self.endpoint_state(ui, provider.name);
                connection(ui, Some(provider.id));
            });
        }
        connection(ui, None);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(uu_i18n::text("自建服务器")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!locked, crate::theme::primary_button("＋"))
                        .on_hover_text(uu_i18n::text("添加服务器"))
                        .clicked()
                    {
                        self.edit_service(None);
                    }
                });
            });
            ui.add_space(10.);
            if self.book.services.is_empty() {
                muted(ui, uu_i18n::text("还没有服务器，可点击右上角加号添加。"));
            }
            for service in self.book.services.clone() {
                ui.push_id(&service.id, |ui| {
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&service.name).size(17.).strong());
                                muted(ui, &service.address);
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !locked,
                                            crate::theme::secondary_button(uu_i18n::text("编辑")),
                                        )
                                        .clicked()
                                    {
                                        self.edit_service(Some(service.clone()));
                                    }
                                    if ui
                                        .add_enabled(
                                            !locked,
                                            crate::theme::secondary_button(uu_i18n::text("移除")),
                                        )
                                        .clicked()
                                    {
                                        if let Some(store) = &self.store {
                                            match store.remove_service(&service.id) {
                                                Ok(()) => self.refresh(),
                                                Err(e) => self.feedback.error(
                                                    self.device_scope(),
                                                    uu_i18n::format!("无法移除：{e}"),
                                                ),
                                            }
                                        }
                                    }
                                },
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(uu_i18n::text("本机在此服务的设备名称"));
                            ui.monospace(&service.published_name);
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("复制")).small())
                                .clicked()
                            {
                                ui.ctx().copy_text(service.published_name.clone());
                            }
                        });
                        let mut enabled = service.host_enabled;
                        if ui
                            .add_enabled_ui(!locked, |ui| {
                                crate::theme::switch(
                                    ui,
                                    &mut enabled,
                                    uu_i18n::text("启用此连接服务"),
                                )
                            })
                            .inner
                            .changed()
                        {
                            if let Some(store) = &self.store {
                                let result = store.edit(|book| {
                                    let item = book
                                        .services
                                        .iter_mut()
                                        .find(|s| s.id == service.id)
                                        .ok_or_else(|| {
                                            anyhow::anyhow!("{}", uu_i18n::format!("服务器已移除"))
                                        })?;
                                    item.host_enabled = enabled;
                                    Ok(())
                                });
                                if let Err(e) = result {
                                    self.feedback.error(
                                        crate::ui_feedback::Scope::Services(0),
                                        uu_i18n::format!("无法保存：{e:#}"),
                                    );
                                }
                            }
                            self.refresh();
                        }
                        self.endpoint_state(ui, &service.name);
                        muted(
                            ui,
                            if service.host_enabled {
                                uu_i18n::text("共享开启时，会在此服务器发布本机")
                            } else {
                                uu_i18n::text("仅用于连接其它设备")
                            },
                        );
                    });
                });
            }
        });
        self.connection_draft = Some(draft);
        ui.add_space(12.);
        let dirty = self
            .connection_draft
            .as_ref()
            .is_some_and(|d| *d != Draft::from(&self.book.sharing))
            || !self.book.sharing.ip_enabled;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !locked && dirty,
                    theme::primary_button(uu_i18n::text("保存设置")),
                )
                .clicked()
            {
                self.save_connection_settings();
            }
            if ui
                .add_enabled(
                    !locked && dirty,
                    theme::secondary_button(uu_i18n::text("还原修改")),
                )
                .clicked()
            {
                self.connection_draft = None;
            }
            if ui
                .add_enabled(
                    !locked,
                    theme::secondary_button(uu_i18n::text("保存并重启服务")),
                )
                .clicked()
                && self.save_connection_settings()
            {
                actions.push(if self.host.running() {
                    Action::RestartSharing
                } else {
                    Action::StartSharing
                });
            }
        });
        if dirty {
            muted(ui, uu_i18n::text("有未保存的修改"));
        }
        if self.host.running() {
            muted(
                ui,
                uu_i18n::text("已运行的服务继续使用启动时配置；服务器配置更改在下次启动生效。"),
            );
        }
    }
}
