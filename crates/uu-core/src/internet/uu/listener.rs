//! Session routing above one UU room socket. Native authentication remains per peer.
use super::*;
use std::collections::HashMap;
use tokio::sync::mpsc;

pub struct Listener { incoming: mpsc::Receiver<Connection>, task: tokio::task::JoinHandle<()> }
struct Route { target: String, ctrl: String, incoming: mpsc::Sender<Value> }
enum Outgoing { Signal(String,Value), Close(String) }
impl Listener {
    pub async fn online(config:&Config, development_relay:bool)->Result<Self> {
        let mut host=Host::online(config,development_relay).await?;
        Ok(Self::from_socket(host.socket.take().context("UU room socket unavailable")?,development_relay))
    }
    fn from_socket(mut socket:SocketIo,development_relay:bool)->Self {
        let (outgoing,mut requests)=mpsc::channel(128);
        let (accepted,incoming)=mpsc::channel(crate::shared_host::MAX_SESSIONS);
        let task=tokio::spawn(async move {
            let mut pending:Vec<(Option<String>,InternetIce,std::time::Instant)>=Vec::new();
            let mut sessions=HashMap::<String,Route>::new();
            loop {
                tokio::select! {
                    request=requests.recv()=>{
                        match request {
                            Some(Outgoing::Signal(session,envelope))=>{
                                let Some(route)=sessions.get(&session) else{continue;};
                                let Ok(sdp)=pack_sdp(envelope) else{sessions.remove(&session);continue;};
                                let Ok(bytes)=gzip_sdp04(&sdp) else{sessions.remove(&session);continue;};
                                if socket.emit("soac",json!({"client_id":route.target,"data":{
                                    "app_control_id":route.ctrl,"ice_id":session,"type":"answer","ice_network_type":0,
                                    "sdp":"","gzip_sdp":{"_placeholder":true,"num":0}}}),&[bytes]).await.is_err(){break;}
                            },
                            Some(Outgoing::Close(session))=>{
                                if let Some(route)=sessions.remove(&session) {
                                    if socket.emit("release",json!({"app_control_id":route.ctrl,"control_id":route.ctrl,"ice_id":session}),&[]).await.is_err(){break;}
                                }
                            },
                            None=>break,
                        }
                    },
                    event=socket.recv_event()=>{
                        let Ok((event,header,attachments))=event else{break;};
                        let body=header.get("data").unwrap_or(&header);
                        pending.retain(|(_,_,at)|at.elapsed()<Duration::from_secs(30));
                        if event=="be-controlled" {
                            if pending.len()>=8 {continue;}
                            if let Ok(ice)=InternetIce::from_uu_with_policy(&header,development_relay) {
                                pending.push((body["ice_id"].as_str().filter(|id|id.len()<=128).map(str::to_owned),ice,std::time::Instant::now()));
                            }
                        }else if event=="soac" && body["type"]=="offer" {
                            let Some(session)=body["ice_id"].as_str().filter(|s|!s.is_empty()&&s.len()<=128).map(str::to_owned) else{continue;};
                            let Some(target)=header["client_id"].as_str().or_else(||body["client_id"].as_str()).filter(|s|!s.is_empty()&&s.len()<=128) else{continue;};
                            let Some(ctrl)=body["app_control_id"].as_str().filter(|s|!s.is_empty()&&s.len()<=128) else{continue;};
                            let Ok(first)=description(body,&attachments).and_then(|sdp|unpack_sdp(&sdp)) else{continue;};
                            if let Some(route)=sessions.get(&session) {
                                if route.target==target && route.ctrl==ctrl && route.incoming.try_send(first).is_err(){sessions.remove(&session);}
                                continue;
                            }
                            let Some(index)=pending.iter().position(|(id,_,_)|id.as_deref()==Some(&session)).or_else(||pending.iter().position(|(id,_,_)|id.is_none())) else{continue;};
                            let (_,ice,_)=pending.remove(index);
                            if sessions.len()>=crate::shared_host::MAX_SESSIONS {
                                let _=socket.emit("release",json!({"app_control_id":ctrl,"control_id":ctrl,"ice_id":session}),&[]).await;
                                continue;
                            }
                            let (send,receive)=mpsc::channel(64);let _=send.try_send(first);
                            sessions.insert(session.clone(),Route{target:target.into(),ctrl:ctrl.into(),incoming:send});
                            let connection=Connection{session:session.clone(),ice,transport:Box::new(Routed{session,outgoing:outgoing.clone(),incoming:receive,closed:false})};
                            if accepted.try_send(connection).is_err(){break;}
                        }else if matches!(event.as_str(),"released"|"left"|"leave"|"fail") {
                            if let Some(session)=body["ice_id"].as_str(){sessions.remove(session);}else{break;}
                        }
                    },
                }
            }
            let _=timeout(Duration::from_secs(1),socket.close()).await;
        });
        Self{incoming,task}
    }
    pub async fn accept(&mut self)->Result<Connection>{self.incoming.recv().await.context("UU signaling listener disconnected")}
    pub async fn close(&mut self){self.task.abort();let _=(&mut self.task).await;self.incoming.close();}
}
impl Drop for Listener {fn drop(&mut self){self.task.abort();}}
struct Routed {session:String,outgoing:mpsc::Sender<Outgoing>,incoming:mpsc::Receiver<Value>,closed:bool}
#[async_trait::async_trait]
impl Transport for Routed {
    async fn send(&mut self,envelope:Value)->Result<()> {
        ensure!(!self.closed,"UU signaling session closed");
        timeout(Duration::from_secs(1),self.outgoing.send(Outgoing::Signal(self.session.clone(),envelope))).await.context("UU signaling queue timed out")?.context("UU signaling listener closed")
    }
    async fn receive(&mut self)->Result<Value>{self.incoming.recv().await.context("UU native peer disconnected")}
    async fn close(&mut self){if !self.closed {self.closed=true;let _=timeout(Duration::from_secs(1),self.outgoing.send(Outgoing::Close(self.session.clone()))).await;self.incoming.close();}}
}
impl Drop for Routed {fn drop(&mut self){if !self.closed {let _=self.outgoing.try_send(Outgoing::Close(self.session.clone()));}}}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt,StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    fn envelope(n:u32)->Value {json!({"v":2,"kind":"device_bootstrap","n":n,"payload":{"sdp":{"type":"offer","sdp":"v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"}}})}
    fn event(name:&str,body:Value)->Message {Message::Text(format!("42{}",json!([name,body])).into())}
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn room_routes_two_sessions_and_releasing_one_keeps_the_other_socket_alive() {
        let tcp=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=tcp.local_addr().unwrap();
        let (commands,mut actions)=mpsc::channel::<Message>(16);let (observed,mut responses)=mpsc::channel(16);
        let server=tokio::spawn(async move {
            let (tcp,_)=tcp.accept().await.unwrap();let mut ws=tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::Text("0{}".into())).await.unwrap();ws.next().await.unwrap().unwrap();ws.send(Message::Text("40{}".into())).await.unwrap();
            loop {tokio::select! {
                msg=actions.recv()=>match msg{Some(msg)=>if ws.send(msg).await.is_err(){break;},None=>break},
                msg=ws.next()=>match msg{Some(Ok(msg))=>{if matches!(msg,Message::Close(_)){break;}if observed.send(msg).await.is_err(){break;}},_=>break},
            }}
        });
        let socket=SocketIo::connect(&format!("ws://{address}"),"generated-token").await.unwrap();
        let mut listener=Listener::from_socket(socket,false);
        for n in 1..=2 {
            commands.send(event("be-controlled",json!({"data":{"ice_id":format!("session-{n}")},"iceServers":[]}))).await.unwrap();
            commands.send(event("soac",json!({"client_id":format!("client-{n}"),"data":{"ice_id":format!("session-{n}"),"app_control_id":format!("control-{n}"),"type":"offer","sdp":pack_sdp(envelope(n)).unwrap()}}))).await.unwrap();
        }
        let mut a=timeout(Duration::from_secs(2),listener.accept()).await.unwrap().unwrap();
        let mut b=timeout(Duration::from_secs(2),listener.accept()).await.unwrap().unwrap();
        assert_eq!(a.transport.receive().await.unwrap()["n"],1);assert_eq!(b.transport.receive().await.unwrap()["n"],2);
        a.transport.close().await;
        let msg=timeout(Duration::from_secs(2),responses.recv()).await.unwrap().unwrap().into_text().unwrap();
        assert!(msg.contains("release") && msg.contains("session-1"));
        commands.send(event("released",json!({"data":{"ice_id":"session-1"}}))).await.unwrap();
        b.transport.send(envelope(9)).await.unwrap();
        let text=timeout(Duration::from_secs(2),responses.recv()).await.unwrap().unwrap().into_text().unwrap();
        assert!(text.starts_with("451-") && text.contains("session-2") && text.contains("client-2"));
        let bytes=timeout(Duration::from_secs(2),responses.recv()).await.unwrap().unwrap().into_data();
        assert_eq!(unpack_sdp(&gunzip_sdp04(&bytes).unwrap()).unwrap()["n"],9);
        commands.send(event("soac",json!({"client_id":"wrong-client","data":{"ice_id":"session-2","app_control_id":"control-2","type":"offer","sdp":pack_sdp(envelope(90)).unwrap()}}))).await.unwrap();
        assert!(timeout(Duration::from_millis(50),b.transport.receive()).await.is_err(),"another route cannot inject into this session");
        commands.send(event("soac",json!({"client_id":"client-2","data":{"ice_id":"session-2","app_control_id":"control-2","type":"offer","sdp":pack_sdp(envelope(10)).unwrap()}}))).await.unwrap();
        assert_eq!(timeout(Duration::from_secs(2),b.transport.receive()).await.unwrap().unwrap()["n"],10);
        listener.close().await;drop(commands);timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
    }
}
