//! Reproduce the reader/FSM ordering without timing-dependent packet loss.
use super::*;
use crate::cipher_suite::cipher_suite_aes_128_gcm_sha256::CipherSuiteAes128GcmSha256;
use crate::handshake::handshake_message_server_hello_done::HandshakeMessageServerHelloDone;
use crate::handshake::HandshakeMessage;
use util::conn::conn_pipe::pipe;

fn cipher(client: bool) -> Box<dyn CipherSuite + Send + Sync> {
    let mut cipher = CipherSuiteAes128GcmSha256::new(false);
    cipher
        .init(&[0x41; 48], &[0x52; 32], &[0x63; 32], client)
        .unwrap();
    Box::new(cipher)
}

#[tokio::test]
async fn post_handshake_completion_releases_reader_and_delivers_encrypted_input() {
    let (source, sink) = pipe();
    let source: Arc<dyn util::Conn + Send + Sync> = Arc::new(source);
    let sink: Arc<dyn util::Conn + Send + Sync> = Arc::new(sink);
    let (decrypted_tx, mut decrypted_rx) = mpsc::channel(1);
    let (handshake_tx, mut handshake_rx) = mpsc::channel(1);
    let (finished_tx, finished_rx) = mpsc::channel(1);
    let (_queue_tx, mut queue_rx) = mpsc::channel(1);
    let (packet_tx, _packet_rx) = mpsc::channel(1);
    let mut ctx = ConnReaderContext {
        is_client: true,
        replay_protection_window: DEFAULT_REPLAY_PROTECTION_WINDOW,
        replay_detector: Vec::new(),
        decrypted_tx,
        encrypted_packets: Vec::new(),
        fragment_buffer: FragmentBuffer::new(),
        cache: HandshakeCache::new(),
        cipher_suite: Arc::new(Mutex::new(Some(cipher(true)))),
        remote_epoch: Arc::new(AtomicU16::new(1)),
        handshake_tx,
        handshake_done_rx: finished_rx,
        packet_tx: Arc::new(packet_tx),
    };
    let completed = Arc::new(AtomicBool::new(false));
    let reader_completed = completed.clone();
    let reader = tokio::spawn(async move {
        let local_epoch = Arc::new(AtomicU16::new(1));
        let mut buf = [0u8; 2048];
        // A final/retransmitted handshake record gets sent to the FSM first.
        DTLSConn::read_and_buffer(
            &mut ctx,
            &sink,
            &mut queue_rx,
            &mut buf,
            &local_epoch,
            &reader_completed,
        )
        .await
        .unwrap();
        // SCTP arrives as encrypted application data on the same reader.
        DTLSConn::read_and_buffer(
            &mut ctx,
            &sink,
            &mut queue_rx,
            &mut buf,
            &local_epoch,
            &reader_completed,
        )
        .await
        .unwrap();
    });
    let mut handshake = Vec::new();
    RecordLayer::new(
        PROTOCOL_VERSION1_2,
        0,
        Content::Handshake(Handshake::new(HandshakeMessage::ServerHelloDone(
            HandshakeMessageServerHelloDone {},
        ))),
    )
    .marshal(&mut handshake)
    .unwrap();
    source.send(&handshake).await.unwrap();
    // This barrier proves the outer select already chose notification. Keep
    // its ack sender alive, like the exited FSM's retained receiver does.
    let pending_ack = tokio::time::timeout(Duration::from_secs(1), handshake_rx.recv())
        .await
        .unwrap()
        .unwrap();
    completed.store(true, Ordering::SeqCst);
    drop(finished_tx);

    let input = b"synthetic-sctp-init";
    let record = RecordLayer::new(
        PROTOCOL_VERSION1_2,
        1,
        Content::ApplicationData(ApplicationData {
            data: input.to_vec(),
        }),
    );
    let mut raw = Vec::new();
    record.marshal(&mut raw).unwrap();
    let encrypted = cipher(false)
        .encrypt(&record.record_layer_header, &raw)
        .unwrap();
    source.send(&encrypted).await.unwrap();
    let delivered = tokio::time::timeout(Duration::from_millis(300), decrypted_rx.recv()).await;
    reader.abort(); // Also clean up when the unpatched implementation stalls.
    let _ = reader.await;
    drop(pending_ack);
    assert_eq!(
        delivered
            .expect("completed DTLS must continue reading SCTP")
            .expect("reader stays alive")
            .unwrap(),
        input
    );
}
