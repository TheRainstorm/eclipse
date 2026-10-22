//! Explicit connection-details dialog, separate from the stream HUD.
use eframe::egui::{self, RichText};
use uu_core::connection_info::{Endpoint, Kind, Snapshot};

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Interface => "网卡地址",
        Kind::StunMapping => "STUN 映射",
        Kind::PeerMapping => "对端观测",
        Kind::Relay => "TURN 中继",
    }
}
fn address(ui: &mut egui::Ui, endpoint: &Endpoint) {
    ui.label(RichText::new(endpoint.address.to_string()).monospace());
    ui.weak(format!(
        "{} · {}",
        kind_label(endpoint.kind),
        endpoint.protocol.to_uppercase()
    ));
}
fn list(ui: &mut egui::Ui, candidates: &[Endpoint], kinds: &[Kind], empty: &str) {
    let mut count = 0;
    for endpoint in candidates.iter().filter(|v| kinds.contains(&v.kind)) {
        if count > 0 {
            ui.add_space(5.);
        }
        address(ui, endpoint);
        count += 1;
    }
    if count == 0 {
        ui.weak(empty);
    }
}
fn grid(ui: &mut egui::Ui, snapshot: &Snapshot) {
    egui::Grid::new("connection-endpoint-grid")
        .num_columns(3)
        .spacing([20., 14.])
        .striped(true)
        .show(ui, |ui| {
            ui.strong("地址信息");
            ui.strong("本机（控制端）");
            ui.strong("远端（被控端）");
            ui.end_row();
            ui.label("实际使用的端点");
            if let Some(path) = &snapshot.selected {
                ui.vertical(|ui| address(ui, &path.local));
                ui.vertical(|ui| address(ui, &path.remote));
            } else {
                ui.weak("尚未选中");
                ui.weak("尚未选中");
            }
            ui.end_row();
            ui.label("内网 / 网卡候选");
            ui.vertical(|ui| list(ui, &snapshot.local, &[Kind::Interface], "未收到网卡地址"));
            ui.vertical(|ui| {
                list(
                    ui,
                    &snapshot.remote,
                    &[Kind::Interface],
                    "对端未提供网卡地址",
                )
            });
            ui.end_row();
            ui.label("公网 / 映射候选");
            ui.vertical(|ui| {
                list(
                    ui,
                    &snapshot.local,
                    &[Kind::StunMapping, Kind::PeerMapping],
                    "未发现 STUN 或对端观测映射",
                )
            });
            ui.vertical(|ui| {
                list(
                    ui,
                    &snapshot.remote,
                    &[Kind::StunMapping, Kind::PeerMapping],
                    "未收到 STUN 或对端观测映射",
                )
            });
            ui.end_row();
            if let Some(path) = &snapshot.selected {
                if path.local.base.is_some() || path.remote.base.is_some() {
                    ui.label("当前端点关联地址");
                    ui.monospace(
                        path.local
                            .base
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "未提供".into()),
                    );
                    ui.monospace(
                        path.remote
                            .base
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "未提供".into()),
                    );
                    ui.end_row();
                }
            }
            if snapshot
                .local
                .iter()
                .chain(&snapshot.remote)
                .any(|v| v.kind == Kind::Relay)
            {
                ui.label("中继候选");
                ui.vertical(|ui| list(ui, &snapshot.local, &[Kind::Relay], "无"));
                ui.vertical(|ui| list(ui, &snapshot.remote, &[Kind::Relay], "无"));
                ui.end_row();
            }
        });
}

pub(super) fn show(ctx: &egui::Context, open: &mut bool, snapshot: &Snapshot) {
    let width = (ctx.screen_rect().width() - 48.).clamp(300., 900.);
    egui::Window::new("连接详情").id("stream-connection-details".into()).open(open)
        .collapsible(false).resizable(true).default_width(width.min(780.))
        .max_width(width).max_height((ctx.screen_rect().height()-48.).max(160.))
        .anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO)
        .frame(egui::Frame::window(&ctx.style()).fill(super::theme::SURFACE).rounding(12.).inner_margin(18.))
        .show(ctx,|ui|{
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new(snapshot.service.as_str()).strong());
                ui.separator();
                if let Some(path)=&snapshot.selected {
                    ui.label(RichText::new(if path.relayed(){"媒体路径：中继"}else{"媒体路径：直连"}).color(super::theme::ACCENT));
                }else{ui.label("媒体路径：等待 ICE 选路");}
                ui.label(format!("连接 {} · ICE {}",snapshot.peer_state,snapshot.ice_state));
            });
            ui.add_space(12.);
            egui::ScrollArea::both().max_height((ctx.screen_rect().height()-240.).max(100.)).show(ui,|ui|grid(ui,snapshot));
            ui.add_space(12.);ui.separator();
            ui.weak("候选地址用于尝试连接；顶部实际端点才是当前媒体使用的路径。映射地址可能仍属于上级内网，未发现映射不表示连接失败。");
            ui.weak("网卡候选也可能含公网 IPv6；中继端点属于 TURN 服务器。这里显示的是媒体端点，不是信令服务器地址。");
            if ui.button("复制连接信息").clicked(){ui.output_mut(|o|o.copied_text=snapshot.copy_text());}
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        *open = false;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
}

impl super::App {
    pub(super) fn open_connection_details(&mut self) {
        self.connection_details_open = true;
        self.stream_menu.open = false;
        self.displays.menu_open = false;
        self.context.memory_mut(|m| m.close_popup());
        self.release_all_keys();
        self.release_pointer_capture(&self.context.clone());
        self.context.request_repaint();
    }
    pub(super) fn connection_details_ui(&mut self, ctx: &egui::Context) {
        if !self.connection_details_open {
            return;
        }
        if self.stream_target.is_none() {
            self.connection_details_open = false;
            return;
        }
        let snapshot = self
            .controller
            .as_ref()
            .map(|h| h.stats.connection.request())
            .unwrap_or_default();
        show(ctx, &mut self.connection_details_open, &snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out)
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for shape in shapes {
            collect(&shape.shape, &mut out);
        }
        out
    }
    #[test]
    fn unused_relay_does_not_label_a_direct_path_as_relay_and_ipv6_keeps_the_port() {
        let ctx = egui::Context::default();
        super::super::theme::apply(&ctx);
        let local = Endpoint {
            address: "[2001:db8::10]:4567".parse().unwrap(),
            protocol: "udp".into(),
            kind: Kind::Interface,
            base: None,
        };
        let remote = Endpoint {
            address: "192.0.2.20:5678".parse().unwrap(),
            protocol: "udp".into(),
            kind: Kind::Interface,
            base: None,
        };
        let relay = Endpoint {
            address: "203.0.113.1:6789".parse().unwrap(),
            protocol: "udp".into(),
            kind: Kind::Relay,
            base: None,
        };
        let snapshot = Snapshot {
            service: "生成连接".into(),
            peer_state: "connected".into(),
            ice_state: "connected".into(),
            local: vec![local.clone(), relay],
            remote: vec![remote.clone()],
            selected: Some(uu_core::connection_info::SelectedPath { local, remote }),
            ..Default::default()
        };
        let mut open = true;
        let mut rendered = Vec::new();
        for _ in 0..3 {
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280., 800.),
                    )),
                    ..Default::default()
                },
                |ctx| show(ctx, &mut open, &snapshot),
            );
            rendered = texts(&out.shapes);
        }
        assert!(rendered.iter().any(|s| s == "连接详情"));
        assert!(rendered.iter().any(|s| s == "媒体路径：直连"));
        assert!(!rendered.iter().any(|s| s == "媒体路径：中继"));
        assert!(rendered.iter().any(|s| s == "[2001:db8::10]:4567"));
        assert!(rendered.iter().any(|s| s.contains("未发现 STUN")));
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| show(ctx, &mut open, &snapshot));
        assert!(!open);
    }
}
