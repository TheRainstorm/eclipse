//! Parse UU room ICE JSON before applying engine-owned ICE policy.
use anyhow::{ensure,Result};
use serde_json::Value;
use webrtc::ice_transport::ice_server::RTCIceServer;
pub fn from_room(header:&Value,relay:bool)->Result<uu_core::direct_ice::InternetIce>{
    let nested=header.get("data").unwrap_or(header);
    let field=|key|header.get(key).or_else(||nested.get(key));
    let mut servers=Vec::new();
    if let Some(entries)=field("iceServers").and_then(Value::as_array){
        ensure!(entries.len()<=32,"too many session ICE services");
        for entry in entries {
            let urls=match entry.get("urls").or_else(||entry.get("url")) {
                Some(Value::String(url))=>vec![url.clone()],
                Some(Value::Array(urls))=>{ensure!(urls.len()<=32,"too many session ICE URLs");urls.iter().filter_map(Value::as_str).map(str::to_owned).collect()},
                _=>continue,
            };
            servers.push(RTCIceServer {urls,username:entry["username"].as_str().unwrap_or_default().into(),credential:entry["credential"].as_str().unwrap_or_default().into(),..Default::default()});
        }
    }
    uu_core::direct_ice::InternetIce::from_servers(servers,relay,field("force_relay").and_then(Value::as_bool).unwrap_or(false))
}
