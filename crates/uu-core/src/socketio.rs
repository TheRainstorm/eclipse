//! Minimal Socket.IO (Engine.IO v4) client used by UU's signalling server.
//!
//! Recovered from the Windows client:
//! * URL: `wss://<server>/socket.io/?EIO=4&transport=websocket&t=<unix>`
//! * auth: HTTP header `X-NRD-AUTH: <room token>`
//! * binary events use the `45<n>-` packet followed by `n` binary frames;
//!   UU prefixes every attachment with one byte `0x04`.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub struct SocketIo {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl SocketIo {
    /// Connect and complete the Engine.IO/Socket.IO handshake.
    pub async fn connect(url: &str, auth: &str) -> Result<Self> {
        let mut req = url.into_client_request().context("bad websocket url")?;
        let h = req.headers_mut();
        h.insert(HeaderName::from_static("x-nrd-auth"), HeaderValue::from_str(auth)?);
        h.insert(
            HeaderName::from_static("streamer_flag"),
            HeaderValue::from_static(r#"{"sdp_flags":{"gzip_sdp":true}}"#),
        );
        h.insert(HeaderName::from_static("streamer_version"), HeaderValue::from_static("V4.7.3"));
        h.insert(HeaderName::from_static("user-agent"), HeaderValue::from_static("WebSocket++/0.8.2"));

        let (mut ws, _) = connect_async(req).await.context("websocket connect")?;
        // Engine.IO open
        let first = next_text(&mut ws).await?;
        if !first.starts_with('0') {
            bail!("expected engine.io open, got {first:?}");
        }
        ws.send(Message::Text("40".into())).await?;
        let ack = next_text(&mut ws).await?;
        if !ack.starts_with("40") {
            bail!("socket.io connect rejected: {ack}");
        }
        Ok(Self { ws })
    }

    /// Send a Socket.IO event, optionally with binary attachments.
    pub async fn emit(&mut self, event: &str, data: Value, attachments: &[Vec<u8>]) -> Result<()> {
        let payload = serde_json::to_string(&serde_json::json!([event, data]))?;
        if attachments.is_empty() {
            self.ws.send(Message::Text(format!("42{payload}").into())).await?;
        } else {
            self.ws
                .send(Message::Text(format!("45{}-{payload}", attachments.len()).into()))
                .await?;
            for a in attachments {
                self.ws.send(Message::Binary(a.clone().into())).await?;
            }
        }
        Ok(())
    }

    /// Leave the Socket.IO room and close the socket (so the server clears the
    /// session and the controlled end stops showing "connected").
    pub async fn close(&mut self) {
        let _ = self.ws.send(Message::Text("41".into())).await;
        let _ = self.ws.close(None).await;
    }

    /// Receive the next Socket.IO event, skipping pings/open packets.
    pub async fn recv_event(&mut self) -> Result<(String, Value, Vec<Vec<u8>>)> {
        loop {
            let msg = self.ws.next().await.ok_or_else(|| anyhow!("websocket closed"))??;
            match msg {
                Message::Text(t) => {
                    let t = t.as_str();
                    if t == "2" {
                        self.ws.send(Message::Text("3".into())).await?;
                        continue;
                    }
                    if t.starts_with('0') || t.starts_with("40") {
                        continue;
                    }
                    let (prefix, body) = if let Some(rest) = t.strip_prefix("45") {
                        let (n, json) = rest.split_once('-').context("bad binary event")?;
                        let n: usize = n.parse().context("bad attachment count")?;
                        let json = &json[json.find('[').context("no json")?..];
                        (Some(n), json)
                    } else if let Some(rest) = t.strip_prefix("42") {
                        (None, rest)
                    } else if let Some(rest) = t.strip_prefix("43") {
                        let rest = &rest[rest.find('[').context("no json")?..];
                        (None, rest)
                    } else {
                        continue;
                    };
                    let arr: Value = serde_json::from_str(body).context("bad socket.io json")?;
                    let mut atts = Vec::new();
                    if let Some(n) = prefix {
                        for _ in 0..n {
                            match self.ws.next().await {
                                Some(Ok(Message::Binary(b))) => atts.push(b.to_vec()),
                                other => bail!("expected binary attachment, got {other:?}"),
                            }
                        }
                    }
                    let event = arr.get(0).and_then(Value::as_str).unwrap_or_default().to_string();
                    let data = arr.get(1).cloned().unwrap_or(Value::Null);
                    return Ok((event, data, atts));
                }
                Message::Binary(_) => continue,
                Message::Ping(p) => self.ws.send(Message::Pong(p)).await?,
                Message::Close(_) => bail!("websocket closed by peer"),
                _ => continue,
            }
        }
    }
}

async fn next_text(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Result<String> {
    loop {
        match ws.next().await.ok_or_else(|| anyhow!("websocket closed"))?? {
            Message::Text(t) => return Ok(t.to_string()),
            Message::Ping(p) => ws.send(Message::Pong(p)).await?,
            Message::Close(_) => bail!("websocket closed during handshake"),
            _ => continue,
        }
    }
}

/// Minimal protobuf `app_data` accepted by the signalling service.
///
/// The Windows client sends a 177-byte message; only the identity/version
/// fields are required for the server to route the session. The leading
/// `0x04` is UU's binary-attachment prefix.
pub fn encode_app_data(device_id: &str) -> Vec<u8> {
    encode_app_data_at_fps(device_id, crate::control_wire::CaptureFrameRate::from_env())
}

pub fn encode_app_data_at_fps(device_id: &str, frame_rate: crate::control_wire::CaptureFrameRate) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let mut out = vec![0x04];
    out.extend_from_slice(&[0x08, 0x01]); // field 1 = 1
    out.extend_from_slice(&[0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
    // CaptureParams must carry the initial target too; no host-default 30 fps.
    let mut params = Vec::new();
    for (field, value) in [(1, frame_rate.option()), (2, 5), (4, 3), (7, 1), (11, frame_rate.fps() as u64)] {
        crate::control_wire::int(field, value, &mut params);
    }
    crate::control_wire::bytes(3, &params, &mut out);
    // ConnectOptions field 4 = DecoderCap {fps, codec, width, height, chroma}.
    // The old empty list plus 1080p streamer JSON restricted the initial stream.
    let mut cap=Vec::new();
    for (field,value) in [(1,crate::control_wire::CaptureFrameRate::MAX as u64),(2,1),(3,3840),(4,2160),(5,1)] {
        crate::control_wire::int(field,value,&mut cap);
    }
    crate::control_wire::bytes(4,&cap,&mut out);
    out.extend_from_slice(&[0x40, 0x03]); // field 8 = 3
    out.push(0x4a); // field 9, string
    varint(&mut out, device_id.len() as u64);
    out.extend_from_slice(device_id.as_bytes());
    out.extend_from_slice(&[0x50, 0x01]); // field 10 = 1
    let ver = b"4.40.1.2090";
    out.push(0x62); // field 12, string
    varint(&mut out, ver.len() as u64);
    out.extend_from_slice(ver);
    out
}

/// gzip an SDP and prepend UU's binary-attachment byte (`0x04`).
pub fn gzip_sdp04(sdp: &str) -> Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(sdp.as_bytes())?;
    let mut out = vec![0x04];
    out.extend(enc.finish()?);
    Ok(out)
}

/// Undo `0x04` + gzip.
pub fn gunzip_sdp04(data: &[u8]) -> Result<String> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let body = data.strip_prefix(&[0x04]).unwrap_or(data);
    let mut s = String::new();
    GzDecoder::new(body).read_to_string(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod frame_rate_tests {
    use super::*;
    use crate::control_wire::{CaptureFrameRate, decode_wire, WireValue};
    #[test]
    fn connect_advertises_144_and_initial_capture_uses_requested_120() {
        let app = encode_app_data_at_fps("test-device", CaptureFrameRate::new(120).unwrap());
        assert_eq!(app[0], 4);
        let fields = decode_wire(&app[1..]).unwrap();
        let nested = |field| decode_wire(fields.iter().find_map(|(n, v)| match v {
            WireValue::Bytes(payload) if *n == field => Some(*payload), _ => None,
        }).expect("initial capture parameters and decoder capability must be present")).unwrap();
        let params = nested(3);
        assert!(params.contains(&(11, WireValue::Varint(120))));
        assert!(params.contains(&(1, WireValue::Varint(0))));
        assert!(nested(4).contains(&(1, WireValue::Varint(144))));
    }
}
