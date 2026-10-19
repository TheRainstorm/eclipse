//! Interoperability: Windows hosts send CaptureChange on TEXT, independently
//! of CONTROL's echo/input traffic. Exercise real SCTP channels, not a parser call.
use super::*;
use crate::control_wire::{bytes, envelope, int};
use webrtc::{data_channel::data_channel_state::RTCDataChannelState,
    peer_connection::sdp::session_description::RTCSessionDescription};

fn rpc_reply(id: u64, case: u32, payload: &[u8]) -> Vec<u8> {
    let mut header = Vec::new(); int(1, id, &mut header);
    let mut rpc = Vec::new(); bytes(1, &header, &mut rpc); bytes(case, payload, &mut rpc);
    envelope(1, 22, &rpc)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_channel_delivers_display_changes_and_rpc_replies() {
    crate::install_crypto_provider();
    let stats = Arc::new(MediaStats::default());
    let media = ControllerMedia::new_with_frames(vec![], None, Some(stats.clone())).await.unwrap();
    let mut engine = MediaEngine::default(); engine.register_default_codecs().unwrap();
    let host = APIBuilder::new().with_media_engine(engine).build()
        .new_peer_connection(RTCConfiguration::default()).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    host.on_data_channel(Box::new(move |channel| {
        let tx = tx.clone(); Box::pin(async move { tx.send(channel).unwrap(); })
    }));
    host.set_remote_description(RTCSessionDescription::offer(media.offer().await.unwrap()).unwrap()).await.unwrap();
    let mut gathered = host.gathering_complete_promise().await;
    let answer = host.create_answer(None).await.unwrap(); host.set_local_description(answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gathered.recv()).await.unwrap();
    media.set_answer(&host.local_description().await.unwrap().sdp).await.unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(channel) = rx.recv().await {
            if channel.label() == "TEXT_DATA_CHANNEL" { return channel; }
        }
        panic!("TEXT channel missing");
    }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while text.ready_state() != RTCDataChannelState::Open {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let mut screen = Vec::new(); int(1, 7, &mut screen); int(12, 0, &mut screen);
    bytes(10, b"native display", &mut screen);
    let mut inventory = Vec::new(); bytes(1, &screen, &mut inventory); int(2, 7, &mut inventory);
    text.send(&envelope(1, 7, &inventory).into()).await.unwrap();
    let mut legacy = Vec::new(); int(1, 10001, &mut legacy); int(2, 8, &mut legacy);
    text.send(&envelope(2, 19, &legacy).into()).await.unwrap();
    text.send(&rpc_reply(10002, 11, &[]).into()).await.unwrap();
    text.send(&rpc_reply(2001, 2, &[]).into()).await.unwrap();
    let mut change = Vec::new(); int(2, 7, &mut change);
    // SCTP text/binary tagging is a transport choice; the protobuf is the same.
    text.send_text(String::from_utf8(envelope(3, 8, &change)).unwrap()).await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if *stats.desktop_capture_source.lock().unwrap() == Some(7)
                && stats.settings_response.load(Ordering::Acquire) == (2001 << 1) | 1
                && stats.remote_screens.lock().unwrap().current_id == 7
                && stats.display_responses.lock().unwrap().len() == 2 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    stats.clipboard.stopped.store(true, Ordering::Release);
    media.pc.close().await.unwrap(); let _ = host.close().await;
    received.expect("TEXT must deliver inventory, source notification and both reply formats");
    assert_eq!(stats.remote_screens.lock().unwrap().selected().unwrap().display_name, "native display");
    let replies = stats.display_responses.lock().unwrap();
    assert!(replies.contains(&(10001, 19, 0)));
    assert!(replies.contains(&(10002, 11, 0)));
}
