//! UU room adapter. Only normal authenticated sessions; never allocates TURN.
use super::{pack_sdp, unpack_sdp, Connection, Transport};
use crate::{
    api::UuApi,
    config::Config,
    direct_ice::InternetIce,
    signal::SignalClient,
    socketio::{gunzip_sdp04, gzip_sdp04, SocketIo},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::timeout;

#[derive(Clone, Copy)]
enum RoomRequest { Create, Join }

fn room_data(response: Value, request: RoomRequest) -> Result<Value> {
    let code=response["code"].as_i64().unwrap_or(-1);
    if code==2001 && matches!(request,RoomRequest::Join) {
        bail!("目标设备未上线，无法发起远控（UU 2001）。请先在目标设备的“共享本机”中勾选 UU 连接服务并点击“开始共享”，保持共享运行后再连接。更换账号或配对文件不能解决设备离线。");
    }
    ensure!(
        code == 0,
        "UU {}失败（错误码 {}）",
        if matches!(request,RoomRequest::Create) {"创建主机房间"}else{"加入目标主机房间"}, code
    );
    Ok(response["data"].clone())
}
async fn connect(data: &Value) -> Result<SocketIo> {
    let server = data["signaling_server"]
        .as_str()
        .context("UU room missing signaling endpoint")?;
    ensure!(server.starts_with("wss://"), "UU signaling must use WSS");
    let token = data["token"]
        .as_str()
        .context("UU room missing session token")?;
    let url = format!(
        "{}/socket.io/?EIO=4&transport=websocket",
        server.trim_end_matches('/')
    );
    timeout(Duration::from_secs(15), SocketIo::connect(&url, token))
        .await
        .context("UU signaling connect timed out")?
}

pub struct Host {
    socket: Option<SocketIo>,
    development_relay: bool,
}
impl Host {
    pub async fn online(config: &Config, development_relay: bool) -> Result<Self> {
        let api = UuApi::new(SignalClient::new(config)?, config.at("state.json"))?;
        let data = room_data(api.room_create().await?,RoomRequest::Create)?;
        let mut socket = connect(&data).await?;
        socket.request_ack("room_info", 1).await?;
        tracing::info!("UU native host online; direct connections only");
        Ok(Self {
            socket: Some(socket),
            development_relay,
        })
    }
    /// One active native viewer. Recreate the room after this connection closes.
    pub async fn accept(&mut self) -> Result<Connection> {
        let socket = self
            .socket
            .as_mut()
            .context("UU host connection already consumed")?;
        let mut requests: Vec<(Option<String>, InternetIce)> = Vec::new();
        loop {
            let (event, header, attachments) = socket.recv_event().await?;
            if event == "be-controlled" {
                ensure!(requests.len() < 8, "too many pending UU requests");
                let body = header.get("data").unwrap_or(&header);
                let id = body["ice_id"].as_str().map(str::to_owned);
                let ice = InternetIce::from_uu_with_policy(&header, self.development_relay)?;
                requests.push((id, ice));
            } else if event == "soac" {
                let body = header.get("data").unwrap_or(&header);
                if body["type"] != "offer" {
                    continue;
                }
                let session = body["ice_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .context("UU offer missing session id")?
                    .to_owned();
                let index = requests
                    .iter()
                    .position(|(id, _)| id.as_deref() == Some(&session))
                    .or_else(|| requests.iter().position(|(id, _)| id.is_none()))
                    .context("UU offer without matching connection request")?;
                let (_, ice) = requests.remove(index);
                let target = header["client_id"]
                    .as_str()
                    .or_else(|| body["client_id"].as_str())
                    .context("UU offer missing controller route")?
                    .to_owned();
                let ctrl = body["app_control_id"]
                    .as_str()
                    .context("UU offer missing control id")?
                    .to_owned();
                let first = unpack_sdp(&description(body, &attachments)?)?;
                return Ok(Connection {
                    transport: Box::new(UuTransport {
                        socket: self.socket.take().unwrap(),
                        session: session.clone(),
                        target,
                        ctrl,
                        host: true,
                        first: Some(first),
                    }),
                    session,
                    ice,
                });
            } else if matches!(event.as_str(), "fail" | "released") {
                bail!("UU host request ended before native offer");
            }
        }
    }
    pub async fn close(&mut self) {
        if let Some(socket) = &mut self.socket {
            socket.close().await;
        }
    }
}

pub async fn join(config: &Config, device: &str, development_relay: bool) -> Result<Connection> {
    ensure!(
        !device.is_empty() && device.len() <= 128,
        "invalid UU target device"
    );
    let api = UuApi::new(SignalClient::new(config)?, config.at("state.json"))?;
    ensure!(api.state.device_id.as_deref()!=Some(device),
        "目标编号与本机 UU 编号相同。控制端应填写另一台设备的编号；更换 UU 账号不会更改本机设备编号。");
    let data = room_data(api.room_join(device, false).await?,RoomRequest::Join)?;
    let mut socket = connect(&data).await?;
    let ctrl = uuid::Uuid::new_v4().to_string();
    let app = crate::socketio::encode_app_data(api.state.device_id.as_deref().unwrap_or_default());
    let streamer = json!({"control_id":ctrl,"device_capability":{
        "display_info":[{"id":0,"fps":60,"type":0,"hdr":-2}],
        "video_codec_capability":[{"video_codec":1,"width":3840,"height":2160,"chroma_sampling":1,"bit_depth":8,"codec_impl":32}],
        "ice_id":""}}).to_string();
    socket
        .emit(
            "control",
            json!({"app_control_id":ctrl,"app_data":{"_placeholder":true,"num":0},
        "geo_exit_ip":crate::config::geo_exit_ip(),"streamer_data":streamer}),
            &[app],
        )
        .await?;
    let negotiation = timeout(Duration::from_secs(30), async {
        loop {
            let (event, header, _) = socket.recv_event().await?;
            match event.as_str() {
                "success" => {
                    let ice = InternetIce::from_uu_with_policy(&header, development_relay)?;
                    let session = header["ice_id"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .context("UU success missing session")?
                        .to_owned();
                    let target = header["client_id"]
                        .as_str()
                        .context("UU success missing host route")?
                        .to_owned();
                    return Ok::<_, anyhow::Error>((ice, session, target));
                }
                "fail" | "released" => bail!("UU control request rejected"),
                _ => {}
            }
        }
    })
    .await;
    match negotiation {
        Ok(Ok((ice, session, target))) => Ok(Connection {
            transport: Box::new(UuTransport {
                socket,
                session: session.clone(),
                target,
                ctrl,
                host: false,
                first: None,
            }),
            session,
            ice,
        }),
        result => {
            let _ = socket
                .emit(
                    "release",
                    json!({"app_control_id":ctrl,"control_id":ctrl}),
                    &[],
                )
                .await;
            socket.close().await;
            result.context("UU control request timed out")??;
            unreachable!()
        }
    }
}

fn description(body: &Value, attachments: &[Vec<u8>]) -> Result<String> {
    if let Some(bytes) = attachments.first() {
        return gunzip_sdp04(bytes);
    }
    body["sdp"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .context("UU soac missing SDP")
}
struct UuTransport {
    socket: SocketIo,
    session: String,
    target: String,
    ctrl: String,
    host: bool,
    first: Option<Value>,
}
#[async_trait::async_trait]
impl Transport for UuTransport {
    async fn send(&mut self, envelope: Value) -> Result<()> {
        let sdp = pack_sdp(envelope)?;
        self.socket.emit("soac",json!({"client_id":self.target,"data":{
            "app_control_id":self.ctrl,"ice_id":self.session,"type":if self.host {"answer"}else{"offer"},
            "ice_network_type":0,"sdp":"","gzip_sdp":{"_placeholder":true,"num":0}}}),&[gzip_sdp04(&sdp)?]).await
    }
    async fn receive(&mut self) -> Result<Value> {
        if let Some(first) = self.first.take() {
            return Ok(first);
        }
        loop {
            let (event, header, attachments) = self.socket.recv_event().await?;
            let body = header.get("data").unwrap_or(&header);
            // Completed SDP contains authenticated candidates. Never apply an
            // unauthenticated trickle candidate delivered outside that envelope.
            if event == "soac"
                && body["ice_id"].as_str() == Some(&self.session)
                && body["type"].as_str() == Some(if self.host { "offer" } else { "answer" })
            {
                return unpack_sdp(&description(body, &attachments)?);
            }
            if matches!(event.as_str(), "released" | "left" | "leave" | "fail") {
                let id = body.get("ice_id");
                if id.is_none() || id.and_then(Value::as_str) == Some(&self.session) {
                    bail!("UU signaling session ended");
                }
            }
        }
    }
    async fn close(&mut self) {
        let _ = timeout(
            Duration::from_secs(1),
            self.socket.emit(
                "release",
                json!({
            "app_control_id":self.ctrl,"control_id":self.ctrl,"ice_id":self.session}),
                &[],
            ),
        )
        .await;
        let _ = timeout(Duration::from_secs(1), self.socket.close()).await;
    }
}

#[cfg(test)]
mod room_error_tests {
    use super::*;
    #[test]
    fn offline_join_explains_host_action_and_is_not_reported_as_identity_or_pairing_failure() {
        let response=json!({"code":2001,"msg":"被控设备不在线，无法发起远控"});
        let error=room_data(response.clone(),RoomRequest::Join).unwrap_err().to_string();
        assert!(error.contains("目标设备未上线")&&error.contains("开始共享")&&error.contains("2001"));
        let create=room_data(response,RoomRequest::Create).unwrap_err().to_string();
        assert!(create.contains("创建主机房间"));assert!(!create.contains("目标设备未上线"));
        assert_eq!(room_data(json!({"code":0,"data":{"fixture":true}}),RoomRequest::Join).unwrap()["fixture"],true);
    }
}
