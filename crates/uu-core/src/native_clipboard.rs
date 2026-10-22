//! Opt-in text on a separate ordered SCTP stream; never reads an OS clipboard.
use crate::{
    capabilities::Capability,
    clipboard::{Clipboard, MAX_TEXT},
    media::MediaStats,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use webrtc::data_channel::{data_channel_state::RTCDataChannelState, RTCDataChannel};

pub(crate) const LABEL: &str = "eclipse-clipboard-v1";
const LEASE: Duration = Duration::from_secs(3);
const MAX_PACKET: usize = MAX_TEXT + 10;
enum Packet<'a> {
    Request {
        epoch: u64,
        enabled: bool,
    },
    Status {
        epoch: u64,
        available: bool,
        enabled: bool,
    },
    Text {
        epoch: u64,
        value: &'a str,
    },
}
fn parse(bytes: &[u8]) -> Option<Packet<'_>> {
    if bytes.len() > MAX_PACKET || bytes.first() != Some(&1) {
        return None;
    }
    let epoch = u64::from_le_bytes(bytes.get(2..10)?.try_into().ok()?);
    if epoch == 0 {
        return None;
    }
    match bytes.get(1)? {
        1 if bytes.len() == 11 && bytes[10] <= 1 => Some(Packet::Request {
            epoch,
            enabled: bytes[10] != 0,
        }),
        2 if bytes.len() == 12
            && bytes[10] <= 1
            && bytes[11] <= 1
            && (bytes[10] != 0 || bytes[11] == 0) =>
        {
            Some(Packet::Status {
                epoch,
                available: bytes[10] != 0,
                enabled: bytes[11] != 0,
            })
        }
        3 => {
            let value = std::str::from_utf8(&bytes[10..]).ok()?;
            (!value.contains('\0')).then_some(Packet::Text { epoch, value })
        }
        _ => None,
    }
}
fn encode(packet: Packet<'_>) -> Option<Vec<u8>> {
    let (kind, epoch, data) = match packet {
        Packet::Request { epoch, enabled } => (1, epoch, vec![enabled as u8]),
        Packet::Status {
            epoch,
            available,
            enabled,
        } => (2, epoch, vec![available as u8, enabled as u8]),
        Packet::Text { epoch, value } => {
            if value.len() > MAX_TEXT || value.contains('\0') {
                return None;
            }
            (3, epoch, value.as_bytes().to_vec())
        }
    };
    if epoch == 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(data.len() + 10);
    bytes.extend([1, kind]);
    bytes.extend(epoch.to_le_bytes());
    bytes.extend(data);
    Some(bytes)
}
async fn send(channel: &RTCDataChannel, packet: Packet<'_>) -> bool {
    let Some(data) = encode(packet) else {
        return false;
    };
    if channel.buffered_amount().await > 64 * 1024 {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_millis(200), channel.send(&data.into())).await,
        Ok(Ok(_))
    )
}
fn clear_pending(control: &Clipboard) {
    control.local.lock().unwrap().take();
    control.remote.lock().unwrap().take();
}
pub(crate) fn suspend(control:&Clipboard){
    control.active.store(false,Ordering::Release);
    control.supported.store(false,Ordering::Release);
    clear_pending(control);
}
pub(crate) fn stop(control: &Clipboard) {
    control.stopped.store(true, Ordering::Release);
    control.active.store(false, Ordering::Release);
    control.supported.store(false, Ordering::Release);
    clear_pending(control);
}
struct Shared {
    closed: AtomicBool,
    remote_enabled: AtomicBool,
    epoch: AtomicU64,
    local_generation: AtomicU64,
    seen: Mutex<Instant>,
}
impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
            remote_enabled: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            local_generation: AtomicU64::new(0),
            seen: Mutex::new(Instant::now()),
        })
    }
}

/// The GUI supplies an explicitly permitted backend. None never opens a device.
pub(crate) fn host(channel: Arc<RTCDataChannel>, control: Option<Arc<Clipboard>>) {
    let transport=control.as_ref().map_or(0,|c|c.transport_epoch.fetch_add(1,Ordering::AcqRel).wrapping_add(1));
    let shared = Shared::new();
    let closed = shared.clone();
    let state = control.clone();
    channel.on_close(Box::new(move || {
        closed.closed.store(true, Ordering::Release);
        if let Some(state) = state.as_ref().filter(|s|s.transport_epoch.load(Ordering::Acquire)==transport) {
            suspend(state);
        }
        Box::pin(async {})
    }));
    let incoming = shared.clone();
    let state = control.clone();
    let weak = Arc::downgrade(&channel);
    channel.on_message(Box::new(move |message| {
        let incoming = incoming.clone();
        let state = state.clone();
        let weak = weak.clone();
        Box::pin(async move {
            if state.as_ref().is_some_and(|s|s.transport_epoch.load(Ordering::Acquire)!=transport){return;}
            let Some(channel) = weak.upgrade() else {
                return;
            };
            let Some(packet) = parse(&message.data) else {
                let _ = channel.close().await;
                return;
            };
            match packet {
                Packet::Request { epoch, enabled } => {
                    let previous = incoming.epoch.load(Ordering::Acquire);
                    if epoch < previous {
                        return;
                    }
                    if epoch != previous {
                        if let Some(state) = &state {
                            clear_pending(state);
                        }
                    }
                    incoming.epoch.store(epoch, Ordering::Release);
                    *incoming.seen.lock().unwrap() = Instant::now();
                    let available = state.as_ref().is_some_and(|s| {
                        s.available.load(Ordering::Acquire) && !s.stopped.load(Ordering::Acquire)
                    });
                    let enabled = enabled
                        && available
                        && state
                            .as_ref()
                            .is_some_and(|s| s.enabled.load(Ordering::Acquire));
                    incoming.remote_enabled.store(enabled, Ordering::Release);
                    if let Some(state) = &state {
                        state.active.store(enabled, Ordering::Release);
                        if !enabled {
                            clear_pending(state);
                        }
                    }
                    if !send(
                        &channel,
                        Packet::Status {
                            epoch,
                            available,
                            enabled,
                        },
                    )
                    .await
                    {
                        let _ = channel.close().await;
                    }
                }
                Packet::Text { epoch, value } => {
                    if epoch == incoming.epoch.load(Ordering::Acquire)
                        && incoming.remote_enabled.load(Ordering::Acquire)
                        && incoming.seen.lock().unwrap().elapsed() < LEASE
                    {
                        if let Some(state) = state.filter(|s| s.ready()) {
                            *state.remote.lock().unwrap() = Some(value.into());
                            state.received.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                _ => {
                    let _ = channel.close().await;
                }
            }
        })
    }));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if control.as_ref().is_some_and(|s|s.transport_epoch.load(Ordering::Acquire)!=transport)
                || shared.closed.load(Ordering::Acquire)
                || channel.ready_state() == RTCDataChannelState::Closed
            {
                break;
            }
            if shared.seen.lock().unwrap().elapsed() > LEASE {
                shared.remote_enabled.store(false, Ordering::Release);
                if let Some(state) = &control {
                    state.active.store(false, Ordering::Release);
                    clear_pending(state);
                }
            }
            if shared.remote_enabled.load(Ordering::Acquire) {
                if let Some(state) = control.as_ref().filter(|s| s.ready()) {
                    let value = state.local.lock().unwrap().take();
                    if let Some(value) = value {
                        if !send(
                            &channel,
                            Packet::Text {
                                epoch: shared.epoch.load(Ordering::Acquire),
                                value: &value,
                            },
                        )
                        .await
                        {
                            break;
                        }
                        state.sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        if let Some(state) = control.as_ref().filter(|s|s.transport_epoch.load(Ordering::Acquire)==transport) {
            suspend(state);
        }
        let _ = channel.close().await;
    });
}

pub(crate) fn viewer(channel: Arc<RTCDataChannel>, stats: Arc<MediaStats>) {
    let transport=stats.clipboard.transport_epoch.fetch_add(1,Ordering::AcqRel).wrapping_add(1);
    let shared = Shared::new();
    let closed = shared.clone();
    let state = stats.clone();
    channel.on_close(Box::new(move || {
        closed.closed.store(true, Ordering::Release);
        if state.clipboard.transport_epoch.load(Ordering::Acquire)==transport {
            state.capabilities.enable(Capability::Clipboard, false);
            suspend(&state.clipboard);
        }
        Box::pin(async {})
    }));
    let incoming = shared.clone();
    let state = stats.clone();
    let weak = Arc::downgrade(&channel);
    channel.on_message(Box::new(move |message| {
        let incoming = incoming.clone();
        let state = state.clone();
        let weak = weak.clone();
        Box::pin(async move {
            if state.clipboard.transport_epoch.load(Ordering::Acquire)!=transport{return;}
            match parse(&message.data) {
                Some(Packet::Status {
                    epoch,
                    available,
                    enabled,
                }) => {
                    if epoch != incoming.epoch.load(Ordering::Acquire)
                        || state.clipboard.generation.load(Ordering::Acquire)
                            != incoming.local_generation.load(Ordering::Acquire)
                    {
                        return;
                    }
                    let enabled = enabled && state.clipboard.ready();
                    state.capabilities.enable(Capability::Clipboard, available);
                    incoming.remote_enabled.store(enabled, Ordering::Release);
                    *incoming.seen.lock().unwrap() = Instant::now();
                    state
                        .clipboard
                        .supported
                        .store(available, Ordering::Release);
                    if state.clipboard.available.load(Ordering::Acquire) {
                        state.clipboard.status(if enabled {
                            "文本剪贴板已开启"
                        } else if available {
                            "本次同步默认关闭，可在菜单中开启"
                        } else {
                            "主机未允许文本剪贴板"
                        });
                    }
                    if !enabled {
                        clear_pending(&state.clipboard);
                    }
                }
                Some(Packet::Text { epoch, value })
                    if epoch == incoming.epoch.load(Ordering::Acquire)
                        && state.clipboard.generation.load(Ordering::Acquire)
                            == incoming.local_generation.load(Ordering::Acquire)
                        && state.clipboard.ready()
                        && incoming.remote_enabled.load(Ordering::Acquire)
                        && incoming.seen.lock().unwrap().elapsed() < LEASE =>
                {
                    *state.clipboard.remote.lock().unwrap() = Some(value.into());
                    state.clipboard.received.fetch_add(1, Ordering::Relaxed);
                }
                Some(Packet::Text { .. }) => {}
                _ => {
                    if let Some(channel) = weak.upgrade() {
                        let _ = channel.close().await;
                    }
                }
            }
        })
    }));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut next_request = Instant::now();
        let mut previous = None;
        loop {
            interval.tick().await;
            if stats.clipboard.transport_epoch.load(Ordering::Acquire)!=transport
                || shared.closed.load(Ordering::Acquire)
                || stats.clipboard.stopped.load(Ordering::Acquire)
                || channel.ready_state() == RTCDataChannelState::Closed
            {
                break;
            }
            if channel.ready_state() != RTCDataChannelState::Open {
                continue;
            }
            let desired = stats.clipboard.ready();
            let generation = stats.clipboard.generation.load(Ordering::Acquire);
            let changed = generation != shared.local_generation.load(Ordering::Acquire);
            if Instant::now() >= next_request || Some(desired) != previous || changed {
                if Some(desired) != previous || changed {
                    shared.local_generation.store(generation, Ordering::Release);
                    shared.epoch.fetch_add(1, Ordering::AcqRel);
                    shared.remote_enabled.store(false, Ordering::Release);
                }
                if !desired {
                    shared.remote_enabled.store(false, Ordering::Release);
                    clear_pending(&stats.clipboard);
                }
                if !send(
                    &channel,
                    Packet::Request {
                        epoch: shared.epoch.load(Ordering::Acquire),
                        enabled: desired,
                    },
                )
                .await
                {
                    break;
                }
                previous = Some(desired);
                next_request = Instant::now() + Duration::from_millis(500);
            }
            if stats.clipboard.read_only.load(Ordering::Acquire) {
                stats.clipboard.local.lock().unwrap().take();
            }
            if desired
                && !stats.clipboard.read_only.load(Ordering::Acquire)
                && shared.remote_enabled.load(Ordering::Acquire)
                && shared.seen.lock().unwrap().elapsed() < LEASE
            {
                let value = stats.clipboard.local.lock().unwrap().take();
                if let Some(value) = value {
                    if !send(
                        &channel,
                        Packet::Text {
                            epoch: shared.epoch.load(Ordering::Acquire),
                            value: &value,
                        },
                    )
                    .await
                    {
                        break;
                    }
                    stats.clipboard.sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if stats.clipboard.transport_epoch.load(Ordering::Acquire)==transport {
            stats.capabilities.enable(Capability::Clipboard, false);
            suspend(&stats.clipboard);
        }
        let _ = channel.close().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_binary_text_preserves_unicode_without_allowing_unknown_or_partial_packets() {
        for text in ["", "中文🙂\r\ntext", &"a".repeat(MAX_TEXT)] {
            let bytes = encode(Packet::Text {
                epoch: 1,
                value: text,
            })
            .unwrap();
            assert!(matches!(parse(&bytes),Some(Packet::Text{epoch:1,value}) if value==text));
        }
        assert!(encode(Packet::Text {
            epoch: 1,
            value: &"a".repeat(MAX_TEXT + 1)
        })
        .is_none());
        let mut malformed = encode(Packet::Text {
            epoch: 1,
            value: "x",
        })
        .unwrap();
        malformed[10] = 0xff;
        assert!(parse(&malformed).is_none());
        malformed[10] = 0;
        assert!(parse(&malformed).is_none());
        assert!(encode(Packet::Request {
            epoch: 0,
            enabled: true
        })
        .is_none());
        for bytes in [
            vec![],
            vec![1],
            vec![2, 3],
            vec![1, 7],
            vec![1, 1, 2],
            vec![1, 2, 1, 1, 0],
        ] {
            assert!(parse(&bytes).is_none());
        }
        assert!(parse(&vec![b'a'; MAX_PACKET + 1]).is_none());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encrypted_channel_rejects_text_from_an_earlier_focus_generation() {
        use crate::native_media::NativePeer;
        use anyhow::{ensure, Context};
        let ip = "127.0.0.1".parse().unwrap();
        let stats = Arc::new(MediaStats::default());
        stats.clipboard.available.store(true, Ordering::Release);
        stats.clipboard.enabled.store(true, Ordering::Release);
        stats.clipboard.active.store(true, Ordering::Release);
        stats.clipboard.generation.store(1, Ordering::Release);
        let mut host = NativePeer::new(ip, None, true).await.unwrap();
        let mut client = NativePeer::new(ip, Some(stats.clone()), false)
            .await
            .unwrap();
        let (opened, mut channels) = tokio::sync::mpsc::channel(1);
        let (requests, mut messages) = tokio::sync::mpsc::channel(16);
        host.pc.on_data_channel(Box::new(move |channel| {
            let requests = requests.clone();
            let opened = opened.clone();
            Box::pin(async move {
                channel.on_message(Box::new(move |message| {
                    let _ = requests.try_send(message.data);
                    Box::pin(async {})
                }));
                let _ = opened.send(channel).await;
            })
        }));
        let channel = client.pc.create_data_channel(LABEL, None).await.unwrap();
        viewer(channel, stats.clone());
        let validation = async {
            host.remote(client.description(true).await?, ip).await?;
            client.remote(host.description(false).await?, ip).await?;
            tokio::try_join!(host.connected(), client.connected())?;
            let remote = tokio::time::timeout(Duration::from_secs(3), channels.recv())
                .await?
                .context("clipboard channel not opened")?;
            let request = tokio::time::timeout(Duration::from_secs(3), messages.recv())
                .await?
                .context("clipboard request missing")?;
            let Some(Packet::Request {
                epoch: old,
                enabled: true,
            }) = parse(&request)
            else {
                anyhow::bail!("expected enabled request");
            };
            ensure!(
                send(
                    &remote,
                    Packet::Status {
                        epoch: old,
                        available: true,
                        enabled: true
                    }
                )
                .await,
                "status send failed"
            );
            ensure!(
                send(
                    &remote,
                    Packet::Text {
                        epoch: old,
                        value: "first generated text"
                    }
                )
                .await,
                "text send failed"
            );
            tokio::time::timeout(Duration::from_secs(3), async {
                while stats.clipboard.received.load(Ordering::Acquire) < 1 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            stats.clipboard.remote.lock().unwrap().take();
            // Focus left and returned between transport ticks; current readiness
            // is still true, but the frontend generation has changed.
            stats.clipboard.generation.fetch_add(1, Ordering::AcqRel);
            let current = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let bytes = messages.recv().await.context("clipboard request ended")?;
                    if let Some(Packet::Request {
                        epoch,
                        enabled: true,
                    }) = parse(&bytes)
                    {
                        if epoch > old {
                            return Ok::<_, anyhow::Error>(epoch);
                        }
                    }
                }
            })
            .await??;
            ensure!(
                send(
                    &remote,
                    Packet::Status {
                        epoch: current,
                        available: true,
                        enabled: true
                    }
                )
                .await,
                "new status send failed"
            );
            ensure!(
                send(
                    &remote,
                    Packet::Text {
                        epoch: old,
                        value: "stale text must be ignored"
                    }
                )
                .await,
                "old text send failed"
            );
            ensure!(
                send(
                    &remote,
                    Packet::Text {
                        epoch: current,
                        value: "current generated text"
                    }
                )
                .await,
                "current text send failed"
            );
            tokio::time::timeout(Duration::from_secs(3), async {
                while stats.clipboard.remote.lock().unwrap().as_deref()
                    != Some("current generated text")
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            ensure!(
                stats.clipboard.received.load(Ordering::Relaxed) == 2,
                "stale activation text was accepted"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        client.close().await;
        host.close().await;
        validation.unwrap();
    }
}
