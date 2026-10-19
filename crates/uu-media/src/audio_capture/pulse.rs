//! Record the default output's monitor. Never select the default input source.
use anyhow::{ensure, Context as _, Result};
use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{Context, State as ContextState},
    def::BufferAttr,
    mainloop::standard::Mainloop,
    sample::{Format, Spec},
    stream::{PeekResult, State as StreamState, Stream},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) struct Capture {
    stream: Stream,
    context: Context,
    mainloop: Mainloop,
}
fn wait(
    mainloop: &mut Mainloop,
    stop: &AtomicBool,
    mut ready: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready()? {
        ensure!(!stop.load(Ordering::Acquire), "system audio stopped");
        ensure!(
            Instant::now() < deadline,
            "system-output monitor initialization timed out"
        );
        ensure!(
            mainloop.iterate(false).is_success(),
            "system audio event loop failed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}
impl Capture {
    pub fn new(stop: &AtomicBool) -> Result<Self> {
        let mut mainloop = Mainloop::new().context("cannot create system audio event loop")?;
        let mut context = Context::new(&mainloop, "uu-client output sharing")
            .context("cannot create system audio connection")?;
        context.connect(None, pulse::context::FlagSet::NOAUTOSPAWN, None)?;
        wait(&mut mainloop, stop, || {
            ensure!(
                !matches!(
                    context.get_state(),
                    ContextState::Failed | ContextState::Terminated
                ),
                "system audio server is unavailable"
            );
            Ok(context.get_state() == ContextState::Ready)
        })?;
        let sink = Rc::new(RefCell::new(None));
        let result = sink.clone();
        let request = context.introspect().get_server_info(move |info| {
            *result.borrow_mut() =
                Some(info.default_sink_name.as_ref().map(|name| name.to_string()));
        });
        wait(&mut mainloop, stop, || Ok(sink.borrow().is_some()))?;
        drop(request);
        let sink = sink
            .borrow_mut()
            .take()
            .flatten()
            .context("no default system-output device")?;
        let monitor = Rc::new(RefCell::new(None));
        let result = monitor.clone();
        let request = context
            .introspect()
            .get_sink_info_by_name(&sink, move |info| match info {
                ListResult::Item(info) => {
                    *result.borrow_mut() =
                        Some(info.monitor_source_name.as_ref().map(|s| s.to_string()))
                }
                ListResult::End | ListResult::Error => {
                    if result.borrow().is_none() {
                        *result.borrow_mut() = Some(None);
                    }
                }
            });
        wait(&mut mainloop, stop, || Ok(monitor.borrow().is_some()))?;
        drop(request);
        let monitor = monitor
            .borrow_mut()
            .take()
            .flatten()
            .context("system-output monitor is unavailable")?;
        let spec = Spec {
            format: Format::FLOAT32NE,
            channels: 2,
            rate: 48_000,
        };
        let mut stream = Stream::new(&mut context, "Shared system output", &spec, None)
            .context("cannot create output-monitor stream")?;
        let buffer = BufferAttr {
            maxlength: 38_400,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 3840,
        };
        stream.connect_record(
            Some(&monitor),
            Some(&buffer),
            pulse::stream::FlagSet::ADJUST_LATENCY,
        )?;
        wait(&mut mainloop, stop, || {
            ensure!(
                !matches!(
                    stream.get_state(),
                    StreamState::Failed | StreamState::Terminated
                ),
                "system-output monitor was rejected"
            );
            Ok(stream.get_state() == StreamState::Ready)
        })?;
        Ok(Self {
            stream,
            context,
            mainloop,
        })
    }
    pub fn poll(&mut self) -> Result<Vec<Vec<f32>>> {
        ensure!(
            self.mainloop.iterate(false).is_success(),
            "system audio event loop failed"
        );
        ensure!(
            self.context.get_state() == ContextState::Ready
                && self.stream.get_state() == StreamState::Ready,
            "system-output monitor disconnected"
        );
        let mut blocks = Vec::new();
        for _ in 0..8 {
            let block = match self.stream.peek()? {
                PeekResult::Empty => break,
                PeekResult::Hole(bytes) => vec![0.0; (bytes / 4).min(5760) / 2 * 2],
                PeekResult::Data(bytes) => {
                    ensure!(
                        bytes.len() % 8 == 0 && bytes.len() <= 384_000,
                        "invalid system-output audio fragment"
                    );
                    bytes
                        .chunks_exact(4)
                        .map(|b| f32::from_ne_bytes(b.try_into().unwrap()))
                        .collect()
                }
            };
            self.stream.discard()?;
            blocks.push(block);
        }
        Ok(blocks)
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
        self.context.disconnect();
    }
}
