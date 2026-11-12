//! Real coturn and WSS on owned loopback sockets. No desktop or firewall changes.
use super::*;
use std::{path::PathBuf, process::{Child, Command, Stdio}};
use uu_media::{RawFrame, Timestamp, VideoEncoder};

struct Fixture { child: Child, root: PathBuf }
impl Drop for Fixture {
    fn drop(&mut self) {
        let _=self.child.kill();let _=self.child.wait();let _=std::fs::remove_dir_all(&self.root);
    }
}
async fn coturn() -> anyhow::Result<(Fixture,SocketAddr,PathBuf)> {
    let executable=std::env::var_os("ECLIPSE_TEST_COTURN").context("set ECLIPSE_TEST_COTURN to an owned coturn executable")?;
    let root=std::env::temp_dir().join(format!("eclipse-owned-coturn-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let reservation=std::net::UdpSocket::bind("127.0.0.1:0")?;let address=reservation.local_addr()?;drop(reservation);
    let secret=uuid::Uuid::new_v4().simple().to_string();
    let secret_file=root.join("turn-secret");crate::private_file::write_atomic(&secret_file,secret.as_bytes())?;
    let configuration=format!("listening-ip=127.0.0.1\nrelay-ip=127.0.0.1\nlistening-port={}\nmin-port=53000\nmax-port=53127\nno-tcp\nno-tls\nno-dtls\nno-cli\nfingerprint\nuse-auth-secret\nstatic-auth-secret={secret}\nrealm=eclipse-owned\nallow-loopback-peers\nno-multicast-peers\nrelay-threads=0\nlog-file={}\nsimple-log\nno-stdout-log\npidfile={}\nuserdb={}\n",address.port(),root.join("coturn.log").display(),root.join("coturn.pid").display(),root.join("db.sqlite").display());
    let file=root.join("coturn.conf");crate::private_file::write_atomic(&file,configuration.as_bytes())?;
    let mut command=Command::new(executable);
    command.arg("-c").arg(&file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(library)=std::env::var_os("ECLIPSE_TEST_COTURN_LIB") {
        let mut paths=vec![PathBuf::from(library)];
        if let Some(old)=std::env::var_os("LD_LIBRARY_PATH") {paths.extend(std::env::split_paths(&old));}
        command.env("LD_LIBRARY_PATH",std::env::join_paths(paths)?);
    }
    let fixture=Fixture{child:command.spawn()?,root};
    let socket=UdpSocket::bind("127.0.0.1:0").await?;
    let mut binding=[0u8;20];binding[..2].copy_from_slice(&1u16.to_be_bytes());binding[4..8].copy_from_slice(&0x2112a442u32.to_be_bytes());
    binding[8..].copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..12]);
    timeout(Duration::from_secs(4),async {
        let mut buffer=[0u8;1024];
        loop {
            socket.send_to(&binding,address).await?;
            if let Ok(Ok((size,from)))=timeout(Duration::from_millis(100),socket.recv_from(&mut buffer)).await {
                if from==address && size>=20 && buffer[..2]==[1,1] && buffer[8..20]==binding[8..] {break;}
            }
        }
        Ok::<_,anyhow::Error>(())
    }).await.context("owned coturn did not answer STUN")??;
    Ok((fixture,address,secret_file))
}

#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore = "requires explicit ECLIPSE_TEST_COTURN; uses only owned loopback services"]
async fn owned_coturn_credentials_from_wss_choose_direct_then_relay_and_decode_video() {
    crate::install_crypto_provider();
    let (fixture,turn_address,secret_file)=coturn().await.unwrap();
    let certificate=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let cert=fixture.root.join("cert.pem");let key=fixture.root.join("key.pem");let token=fixture.root.join("token");
    crate::private_file::write_atomic(&cert,certificate.cert.pem().as_bytes()).unwrap();
    crate::private_file::write_atomic(&key,certificate.key_pair.serialize_pem().as_bytes()).unwrap();
    let group = crate::internet::server::groups::Group::new("Generated TURN group".into()).unwrap();
    let group_id = group.id;
    crate::private_file::write_atomic(&token, group.token.as_bytes()).unwrap();
    let groups_file = fixture.root.join("groups.json");
    crate::internet::server::groups::Groups { version: 1, groups: vec![group] }.save(&groups_file).unwrap();
    let reservation=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=reservation.local_addr().unwrap();drop(reservation);
    let (stop,rx)=tokio::sync::watch::channel(false);
    let serving=tokio::spawn(crate::internet::server::serve(crate::internet::server::Config {
        bind:address,certificate:cert.clone(),private_key:key,groups_file,stun:vec![format!("stun:{turn_address}")],
        turn:Some(crate::internet::server::TurnConfig{urls:vec![format!("turn:{turn_address}?transport=udp")],secret_file,credential_lifetime:Duration::from_secs(3600)}),
    },rx));
    let endpoint=crate::internet::self_hosted::Config{url:crate::internet::enrollment::group_url(&format!("wss://{address}/v1/signal"), &group_id).unwrap(),token_file:token,ca_file:Some(cert)};
    let mut listener=timeout(Duration::from_secs(4),async {
        loop {match crate::internet::self_hosted::Listener::online_named(&endpoint,"owned-host","Owned host").await {
            Ok(listener)=>break listener,Err(_)=>tokio::time::sleep(Duration::from_millis(10)).await,
        }}
    }).await.unwrap();
    for force_relay in [false,true] {
        let mut viewer_connection=crate::internet::self_hosted::join(&endpoint,"owned-host").await.unwrap();
        let mut host_connection=listener.accept().await.unwrap();
        let result=generated_video(&mut host_connection,&mut viewer_connection,force_relay,16,12).await;
        viewer_connection.transport.close().await;host_connection.transport.close().await;
        let report=result.unwrap();
        assert_eq!(report["path"]["direct"],!force_relay,"owned loopback ICE selection");
        println!("owned WSS+coturn {report}");
    }
    listener.close().await;stop.send_replace(true);serving.await.unwrap().unwrap();
}

/// An explicit, reusable WAN acceptance probe. Both generated peers run in this
/// process: All may choose same-machine host candidates, while Relay proves
/// actual traffic traverses the supplied remote TURN service. It cannot prove
/// two independent NATs can punch through. No desktop, input, audio or service
/// configuration is opened or changed; only this probe's random room is closed.
#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore = "requires explicit ECLIPSE_TEST_SIGNAL_ENDPOINT and ECLIPSE_TEST_SIGNAL_TOKEN_FILE"]
async fn remote_wss_turn_generated_media_probe() {
    crate::install_crypto_provider();
    #[derive(serde::Deserialize)]
    struct Endpoint { url:String, ca_file:Option<PathBuf> }
    let file=std::env::var_os("ECLIPSE_TEST_SIGNAL_ENDPOINT").expect("set endpoint JSON path");
    let bytes=std::fs::read(&file).unwrap();assert!(bytes.len()<=64*1024,"endpoint configuration exceeds limit");
    let configured:Endpoint=serde_json::from_slice(&bytes).expect("endpoint JSON needs url and optional ca_file");
    let endpoint=crate::internet::self_hosted::Config {
        url:configured.url,
        token_file:std::env::var_os("ECLIPSE_TEST_SIGNAL_TOKEN_FILE").expect("set token file path").into(),
        ca_file:std::env::var_os("ECLIPSE_TEST_SIGNAL_CA_FILE").map(PathBuf::from).or(configured.ca_file),
    };
    endpoint.validate_local().unwrap();
    let room=format!("generated-probe-{}",uuid::Uuid::new_v4().simple());
    let mut listener=crate::internet::self_hosted::Listener::online_named(&endpoint,&room,"Generated acceptance probe").await.unwrap();
    let result=async {
        let listed=crate::internet::self_hosted::directory(&endpoint).await.context("query remote authenticated directory")?;
        ensure!(listed.iter().any(|device|device.device==room),"probe room missing from authenticated directory");
        for force_relay in [false,true] {
            let mut viewer_connection=crate::internet::self_hosted::join(&endpoint,&room).await.context("join remote probe room")?;
            let mut host_connection=listener.accept().await.context("accept remote probe Session")?;
            let result=generated_video(&mut host_connection,&mut viewer_connection,force_relay,48,24).await;
            viewer_connection.transport.close().await;host_connection.transport.close().await;
            println!("remote WSS+TURN {}",result.with_context(||format!("generated media force_relay={force_relay}"))?);
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    listener.close().await;
    result.unwrap();
}

async fn generated_video(host_connection:&mut crate::internet::Connection,viewer_connection:&mut crate::internet::Connection,
    force_relay:bool,frame_count:u64,minimum_decoded:u64)->anyhow::Result<serde_json::Value> {
    let started=std::time::Instant::now();
    let host_ice=crate::direct_ice::InternetIce::from_servers(host_connection.ice.configuration().ice_servers,true,force_relay)?;
    let viewer_ice=crate::direct_ice::InternetIce::from_servers(viewer_connection.ice.configuration().ice_servers,true,force_relay)?;
    for ice in [&host_ice,&viewer_ice] {
        ensure!(ice.configuration().ice_servers.iter().any(|s|!s.username.is_empty() && !s.credential.is_empty()),
            "connection service did not supply per-session TURN credentials");
    }
    let stats=Arc::new(MediaStats::default());stats.decoder_preference.set(uu_media::DecoderKind::Software)
        .map_err(|_|anyhow::anyhow!("decoder preference already set"))?;
    let mut host=NativePeer::new_direct(&host_ice,None,true).await?;
    let mut viewer=match NativePeer::new_direct(&viewer_ice,Some(stats.clone()),false).await {
        Ok(peer)=>peer,Err(error)=>{host.close().await;return Err(error);},
    };
    let result=async {
        let (frames,mut images)=crate::media::frame_channel();viewer.receiver(frames,stats.clone()).await?;
        let mut sender=host.sender_for_color(Arc::new(AtomicBool::new(false)),VideoCodec::H264,false).await?;
        // Send both descriptions through the actual authenticated WSS routes.
        // Neither description nor its credentials are printed or saved.
        let offer=viewer.description(true).await.context("gather viewer ICE candidates")?;
        viewer_connection.transport.send(serde_json::to_value(offer)?).await.context("send offer over WSS")?;
        host.remote_direct(serde_json::from_value(host_connection.transport.receive().await.context("receive offer over WSS")?)?).await?;
        let answer=host.description(false).await.context("gather host ICE candidates")?;
        host_connection.transport.send(serde_json::to_value(answer)?).await.context("send answer over WSS")?;
        viewer.remote_direct(serde_json::from_value(viewer_connection.transport.receive().await.context("receive answer over WSS")?)?).await?;
        tokio::try_join!(host.connected(),viewer.connected()).context("establish generated-media ICE path")?;
        let path=viewer.direct_path().await?;
        if force_relay {
            ensure!(path["direct"]==false && path["local_type"]=="relay" && path["remote_type"]=="relay",
                "forced TURN did not select a relay pair: {path}");
        }
        let mut encoder=uu_media::H264Encoder::with_frame_rate(300,60)?;
        for index in 0..frame_count {
            let frame=RawFrame::new_i420(64,64,vec![128;64*64*3/2],Timestamp(index*16_666));
            for encoded in encoder.encode(&frame)? {sender.frame(&encoded).await?;}
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
        timeout(Duration::from_secs(8),async {
            while stats.decoded_total.load(Ordering::Acquire)<minimum_decoded {
                while images.try_recv_image().is_ok(){}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.context("generated video did not reach the decode threshold")?;
        ensure!(stats.errors.load(Ordering::Acquire)==0,"generated TURN video decode errors");
        Ok::<_,anyhow::Error>(serde_json::json!({"force_relay":force_relay,"decoded":stats.decoded_total.load(Ordering::Acquire),
            "decode_errors":stats.errors.load(Ordering::Acquire),"elapsed_ms":started.elapsed().as_millis(),"path":path}))
    }.await;
    host.close().await;viewer.close().await;
    result
}
