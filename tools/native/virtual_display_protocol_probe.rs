//! Explicit Windows desktop protocol probe, linked against the production SDK.
//! No display is created/prepared, no input is injected, and no pixels are saved.
//! Compile with rustc --test against the application's actual uu_plugin_sdk DLL.
#[cfg(windows)]
mod windows {
    use engine::{
        capabilities::Capability,
        device_auth::PairingControl,
        device_trust::{PermissionDetails, Permissions, TrustStore},
        media::{FrameReceiver, MediaStats},
        native, native_lan,
        native_virtual_display::{Request, Response},
        shared_host::{Access, SessionGate},
        virtual_display::{Manager, Spec},
    };
    use std::{
        path::PathBuf,
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };
    use tokio::sync::{mpsc, watch};
    use uu_plugin_sdk::{
        anyhow::{ensure, Context, Result},
        engine, tokio,
    };

    type Geometry = Vec<(String, i32, i32, u32, u32, bool, Option<u32>)>;
    fn geometry() -> Result<Geometry> {
        let mut monitors = engine::native::monitors()?
            .into_iter()
            .map(|m| (m.name, m.x, m.y, m.width, m.height, m.primary, m.refresh_hz))
            .collect::<Vec<_>>();
        monitors.sort();
        Ok(monitors)
    }
    type Slots = Vec<(String, bool, bool, Option<Spec>)>;
    fn slots() -> Result<Slots> {
        let mut slots = Manager::shared()
            .slots()?
            .into_iter()
            .map(|s| (s.id, s.active, s.leased, s.actual))
            .collect::<Vec<_>>();
        slots.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(slots)
    }
    struct Viewer {
        stats: Arc<MediaStats>,
        frames: FrameReceiver,
        input: Option<engine::native_input::NativeInputSender>,
        stop: watch::Sender<bool>,
        task: Option<tokio::task::JoinHandle<Result<()>>>,
    }
    impl Viewer {
        async fn wait(
            &mut self,
            label: &str,
            predicate: impl Fn(&MediaStats) -> bool,
        ) -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(25);
            loop {
                while self.frames.try_recv().is_ok() {} // Drop decoded pixels immediately.
                if predicate(&self.stats) {
                    return Ok(());
                }
                ensure!(Instant::now() < deadline, "timeout waiting for {label}");
                if self.task.as_ref().is_some_and(|task| task.is_finished()) {
                    if let Some(task) = self.task.take() {
                        task.await.context("viewer task panicked")??;
                    }
                    return anyhow_failure("viewer ended before requested observation");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        async fn request(&mut self, request: Request) -> Result<Response> {
            let id = self.stats.virtual_displays.request(request)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                while self.frames.try_recv().is_ok() {}
                if let Some(response) = self.stats.virtual_displays.take_response(id) {
                    return Ok(response);
                }
                ensure!(
                    Instant::now() < deadline,
                    "virtual-display request timed out"
                );
                ensure!(
                    !self.task.as_ref().is_some_and(|t| t.is_finished()),
                    "viewer ended during virtual-display request"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        fn readonly(&self) -> Result<()> {
            ensure!(
                self.stats.input_messages_sent.load(Ordering::Acquire) == 0,
                "unexpected keyboard/mouse message"
            );
            for capability in [Capability::Input, Capability::Audio, Capability::Clipboard] {
                ensure!(
                    !self.stats.capabilities.allows(capability),
                    "unexpected input/audio/clipboard capability"
                );
            }
            ensure!(
                !self.stats.native_input_available.load(Ordering::Acquire),
                "input backend was enabled"
            );
            ensure!(
                self.stats.errors.load(Ordering::Acquire) == 0,
                "video decode errors"
            );
            let (width, height) = self.stats.res();
            ensure!(
                width > 0 && height > 0 && width <= 1280 && height <= 720,
                "video bounds exceeded 1280x720"
            );
            ensure!(
                self.stats.configured_fps.load(Ordering::Acquire) == 30,
                "host did not accept 30 fps"
            );
            ensure!(
                self.stats
                    .codec
                    .lock()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("h264"),
                "unexpected video codec"
            );
            let path = self
                .stats
                .connection
                .request()
                .selected
                .context("selected ICE path unavailable")?;
            ensure!(
                path.local.address.ip().is_loopback()
                    && path.remote.address.ip().is_loopback()
                    && !path.relayed(),
                "media path escaped loopback"
            );
            Ok(())
        }
        async fn finish(mut self) -> Result<u64> {
            self.stop.send_replace(true);
            if let Some(mut task) = self.task.take() {
                match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                    Ok(joined) => joined.context("viewer task panicked")??,
                    Err(error) => {
                        task.abort();
                        let _ = task.await;
                        return Err(error).context("viewer failed to close");
                    }
                }
            }
            ensure!(
                !self
                    .stats
                    .virtual_displays
                    .available
                    .load(Ordering::Acquire),
                "virtual-display client remained available after close"
            );
            ensure!(
                self.stats.native_media_retired.load(Ordering::Acquire),
                "video lifetime was not retired"
            );
            Ok(self.stats.decoded_total.load(Ordering::Acquire))
        }
    }
    impl Drop for Viewer {
        fn drop(&mut self) {
            self.stop.send_replace(true);
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }
    fn anyhow_failure<T>(message: &str) -> Result<T> {
        Err(uu_plugin_sdk::anyhow::anyhow!("{message}"))
    }

    struct Fixture {
        access: Arc<Access>,
        viewer: TrustStore,
        endpoint: String,
        _handle: native_lan::Handle,
        _pairing: mpsc::UnboundedReceiver<engine::device_auth::PairRequest>,
        events: mpsc::UnboundedReceiver<native_lan::Event>,
        stop: watch::Sender<bool>,
        server: Option<tokio::task::JoinHandle<Result<()>>>,
        initial_geometry: Geometry,
        initial_slots: Slots,
        ended: usize,
    }
    impl Fixture {
        async fn start(root: PathBuf) -> Result<Self> {
            ensure!(
                root.is_absolute() && !root.exists(),
                "private fixture root must be a new absolute directory"
            );
            std::fs::create_dir(&root)?;
            let initial_geometry = geometry()?;
            ensure!(
                initial_geometry.len() >= 2,
                "media-epoch probe requires at least two existing desktop outputs"
            );
            let manager = Manager::shared();
            let status = manager.status()?;
            ensure!(
                status.supported && status.driver_present,
                "supported virtual display driver is required"
            );
            ensure!(
                status.available_slots == 0,
                "probe refuses Create when an inactive virtual slot is available"
            );
            ensure!(
                manager.list()?.is_empty(),
                "probe process must not begin with existing leases"
            );
            let initial_slots = slots()?;
            let host = TrustStore::open(root.join("host-trust"))?;
            let viewer = TrustStore::open(root.join("viewer-trust"))?;
            ensure!(
                host.card("Protocol host").id() != viewer.card("Protocol viewer").id(),
                "fixture reused the same device identity"
            );
            viewer.remember(host.card("Protocol host"))?;
            host.authorize(viewer.card("Protocol viewer"), Permissions::VIEW, None)?;
            let (pairing, receive) = mpsc::unbounded_channel();
            let access = Arc::new(Access {
                store: host,
                name: "Protocol host".into(),
                pairing: Arc::new(PairingControl::new(pairing)),
                sessions: Arc::new(SessionGate::default()),
            });
            let (stop, receiver) = watch::channel(false);
            let (events, event_rx) = mpsc::unbounded_channel();
            let config = native_lan::Config {
                encoder_tuning: Default::default(),
                video_policy: Some(engine::native_quality::Policy {
                    codecs: vec![native::VideoCodec::H264],
                    preferred_codec: Some(native::VideoCodec::H264),
                }),
                codec: native::VideoCodec::H264,
                rate_control: Default::default(),
                name: "Protocol host".into(),
                bind: ([127, 0, 0, 1], 0).into(),
                discovery_bind: None,
                data_dir: root.join("broker"),
                source: native::CaptureSource::Desktop {
                    initial_monitor: None,
                },
                fps: 30,
                bitrate_kbps: 2000,
                encoder: native::EncoderPreference::Software,
                allow_input: false,
                audio: None,
                clipboard: None,
            };
            let (server, handle) =
                native_lan::Server::bind_with_access(config, receiver, events, access.clone())
                    .await?;
            let endpoint = server.address()?.to_string();
            let task = tokio::spawn(server.run());
            Ok(Self {
                access,
                viewer,
                endpoint,
                _handle: handle,
                _pairing: receive,
                events: event_rx,
                stop,
                server: Some(task),
                initial_geometry,
                initial_slots,
                ended: 0,
            })
        }
        async fn viewer(&self) -> Result<Viewer> {
            let expected = self.access.store.card("Protocol host").id();
            let connection = tokio::time::timeout(
                Duration::from_secs(10),
                native_lan::connect_device(
                    &self.endpoint,
                    &self.viewer,
                    "Protocol viewer",
                    Some(&expected),
                    None,
                    true,
                    &Default::default(),
                ),
            )
            .await??;
            let invitation = connection
                .invitation
                .context("authenticated device connection did not return media")?;
            ensure!(
                invitation.address().ip().is_loopback(),
                "broker invitation escaped loopback"
            );
            let stats = Arc::new(MediaStats::default());
            let _ = stats
                .decoder_preference
                .set(engine::media::DecoderKind::Software);
            let _ = stats
                .native_codec_capabilities
                .set(vec![native::VideoCodec::H264]);
            let _ = stats
                .native_quality_request
                .set(engine::native_quality::Request {
                    fps: 30,
                    bounds: Some([1280, 720]),
                    bitrate_kbps: Some(2000),
                    codec: Some(native::VideoCodec::H264),
                });
            stats.audio.output_enabled.store(false, Ordering::Release);
            stats.clipboard.enabled.store(false, Ordering::Release);
            let (frames, receiver) = engine::media::frame_channel();
            let (input, input_rx) = engine::native_input::channel(false);
            let (stop, rx) = watch::channel(false);
            let observed = stats.clone();
            let task = tokio::spawn(async move {
                native::view_with_input(&invitation, frames, observed, rx, input_rx).await
            });
            Ok(Viewer {
                stats,
                frames: receiver,
                input: Some(input),
                stop,
                task: Some(task),
            })
        }
        async fn session_closed(&mut self) -> Result<()> {
            let expected = self.ended + 1;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                while let Ok(event) = self.events.try_recv() {
                    if matches!(event, native_lan::Event::SessionEnded(_)) {
                        self.ended += 1;
                    }
                }
                if self.access.sessions.sessions().is_empty() && self.ended >= expected {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "authenticated session or normal SessionEnded did not close"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            ensure!(
                Manager::shared().list()?.is_empty(),
                "virtual-display lease remained after close"
            );
            Ok(())
        }
        async fn finish(&mut self) -> Result<()> {
            self.stop.send_replace(true);
            if let Some(mut task) = self.server.take() {
                match tokio::time::timeout(Duration::from_secs(12), &mut task).await {
                    Ok(joined) => joined.context("host task panicked")??,
                    Err(error) => {
                        task.abort();
                        let _ = task.await;
                        return Err(error).context("host did not stop");
                    }
                }
            }
            ensure!(
                self.access.sessions.sessions().is_empty(),
                "authenticated sessions remain after listener close"
            );
            ensure!(
                Manager::shared().list()?.is_empty(),
                "virtual leases remain after listener close"
            );
            ensure!(
                geometry()? == self.initial_geometry && slots()? == self.initial_slots,
                "desktop layout or virtual target state changed"
            );
            Ok(())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.send_replace(true);
            if let Some(task) = &self.server {
                task.abort();
            }
        }
    }

    async fn exercise(fixture: &mut Fixture) -> Result<String> {
        let mut viewer = fixture.viewer().await?;
        let view_only: Result<()> = async {
            viewer
                .wait("VIEW media/control", |s| {
                    s.decoded_total.load(Ordering::Acquire) >= 20
                        && s.control_open.load(Ordering::Acquire) != 0
                        && s.connection.request().selected.is_some()
                })
                .await?;
            viewer.readonly()?;
            ensure!(
                !viewer
                    .stats
                    .virtual_displays
                    .available
                    .load(Ordering::Acquire),
                "VIEW-only peer received virtual-display capability"
            );
            ensure!(
                viewer
                    .stats
                    .virtual_displays
                    .request(Request::Inspect)
                    .is_err(),
                "VIEW-only peer queued a management request"
            );
            Ok(())
        }
        .await;
        let view_closed = viewer.finish().await;
        fixture.session_closed().await?;
        view_only?;
        let view_frames = view_closed?;
        let permissions = Permissions::VIEW.with_details(PermissionDetails {
            manage_server: true,
            ..Default::default()
        });
        fixture.access.store.authorize(
            fixture.viewer.card("Protocol viewer"),
            permissions,
            None,
        )?;
        let mut viewer = fixture.viewer().await?;
        let managed:Result<(u64,u64)>=async {
            viewer.wait("managed media/channel",|s|s.decoded_total.load(Ordering::Acquire)>=20&&s.virtual_displays.available.load(Ordering::Acquire)&&s.capabilities.allows(Capability::Displays)&&s.connection.request().selected.is_some()).await?;
            viewer.readonly()?;
            let sessions=fixture.access.sessions.sessions();ensure!(sessions.len()==1,"unexpected authenticated connection count");let session=sessions[0].0.clone();
            match viewer.request(Request::Inspect).await? {
                Response::Snapshot{status,displays,slots}=>{ensure!(status.supported&&status.driver_present&&status.available_slots==0,"unexpected driver inventory");ensure!(displays.is_empty()&&slots.is_empty(),"remote inspect exposed unrelated leases or local preparation slots");},
                other=>return Err(uu_plugin_sdk::anyhow::anyhow!("unexpected Inspect result: {other:?}")),
            }
            // InvalidSpec is rejected before driver provisioning. A zero-slot
            // inventory alone no longer makes Create read-only: it can enable
            // an eligible disabled adapter in the production backend.
            match viewer.request(Request::Create{spec:Spec{width:1,height:720,refresh_millihz:30_000,hdr:false}}).await? {
                Response::Error{message}=>ensure!(message==engine::native_virtual_display::error_message(&engine::virtual_display::Error::InvalidSpec.into()),"invalid Create failed for a different reason: {message}"),
                other=>return Err(uu_plugin_sdk::anyhow::anyhow!("unexpected invalid Create result: {other:?}")),
            }
            ensure!(geometry()?==fixture.initial_geometry&&slots()?==fixture.initial_slots,"invalid Create changed the display layout");
            let epoch=viewer.stats.native_media_epoch.load(Ordering::Acquire);
            let catalog=viewer.stats.remote_screens.lock().unwrap().clone();
            let target=catalog.screens.iter().find(|screen|screen.id!=catalog.current_id&&screen.width as u64*9==screen.height as u64*16)
                .or_else(||catalog.screens.iter().find(|screen|screen.id!=catalog.current_id)).context("second existing capture source unavailable")?.id;
            viewer.input.as_ref().context("display-control queue missing")?.select_display(901,target)?;
            let frames=viewer.stats.decoded_total.load(Ordering::Acquire);
            viewer.wait("second media epoch on existing display",|s|s.native_media_epoch.load(Ordering::Acquire)>epoch&&s.decoded_total.load(Ordering::Acquire)>=frames+15&&s.virtual_displays.available.load(Ordering::Acquire)&&s.remote_screens.lock().unwrap().current_id==target&&s.connection.request().selected.is_some()).await?;
            viewer.readonly()?;
            ensure!(fixture.access.sessions.sessions().as_slice()==[(session,fixture.viewer.card("Protocol viewer").id())],"display switch replaced the authenticated connection");
            ensure!(viewer.stats.native_displays.catalog_epoch()==viewer.stats.native_media_epoch.load(Ordering::Acquire),"catalog belongs to a stale media epoch");
            match viewer.request(Request::Inspect).await? {Response::Snapshot{displays,..}=>ensure!(displays.is_empty(),"lease appeared after media epoch"),_=>return anyhow_failure("Inspect failed after media epoch")}
            Ok((epoch,viewer.stats.native_media_epoch.load(Ordering::Acquire)))
        }.await;
        let managed_closed = viewer.finish().await;
        fixture.session_closed().await?;
        let (earlier_epoch, later_epoch) = managed?;
        let managed_frames = managed_closed?;
        ensure!(
            fixture.ended == 2,
            "normal SessionEnded events were not observed for both authenticated sessions"
        );
        Ok(format!("{{\"ok\":true,\"sdk_id\":\"{}\",\"view_frames\":{view_frames},\"managed_frames\":{managed_frames},\"view_only_capability_absent\":true,\"manage_capability_present\":true,\"invalid_spec_error\":true,\"epoch_before\":{earlier_epoch},\"epoch_after\":{later_epoch},\"same_authenticated_connection\":true,\"created_targets\":0,\"physical_target_removal_exercised\":false,\"input_messages\":0,\"audio_enabled\":false,\"clipboard_enabled\":false,\"layout_unchanged\":true,\"normal_sessions_ended\":2,\"source\":\"real Windows Desktop, software H264, requested 1280x720@30\"}}",uu_plugin_sdk::SDK_ID))
    }
    fn quoted(value: &str) -> String {
        let mut text = String::from("\"");
        for ch in value.chars() {
            match ch {
                '"' => text.push_str("\\\""),
                '\\' => text.push_str("\\\\"),
                c if c.is_control() => text.push_str(&format!("\\u{:04x}", c as u32)),
                c => text.push(c),
            }
        }
        text.push('"');
        text
    }
    #[test]
    #[ignore = "explicit Windows interactive desktop, private identities, no unused virtual slots; use owned runner"]
    fn production_virtual_display_protocol_without_topology_changes() -> Result<()> {
        ensure!(
            std::env::var("ECLIPSE_VIRTUAL_PROTOCOL_DESKTOP")
                .ok()
                .as_deref()
                == Some("1"),
            "explicit desktop probe opt-in is required"
        );
        engine::install_crypto_provider();
        let root = PathBuf::from(
            std::env::var_os("ECLIPSE_VIRTUAL_PROTOCOL_ROOT")
                .context("private fixture root missing")?,
        );
        let report = PathBuf::from(
            std::env::var_os("ECLIPSE_VIRTUAL_PROTOCOL_REPORT")
                .context("new report path missing")?,
        );
        ensure!(
            report.is_absolute() && !report.exists(),
            "report must be a new absolute path"
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            let mut fixture = Fixture::start(root.clone()).await?;
            let result = exercise(&mut fixture).await;
            let cleanup = fixture.finish().await;
            if let Err(error) = &cleanup {
                eprintln!("owned protocol cleanup: {error:#}");
            }
            cleanup?;
            result
        });
        let text = match &result {
            Ok(text) => text.clone(),
            Err(error) => format!(
                "{{\"ok\":false,\"sdk_id\":\"{}\",\"error\":{}}}",
                uu_plugin_sdk::SDK_ID,
                quoted(&format!("{error:#}"))
            ),
        };
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(report)?;
        output.write_all(text.as_bytes())?;
        output.sync_all()?;
        eprintln!("protocol probe result: {text}");
        result.map(|_| ())
    }
}
