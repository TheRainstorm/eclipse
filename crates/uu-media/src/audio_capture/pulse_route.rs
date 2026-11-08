//! Pulse monitors follow sink mute. Capture an owned silent sink instead, and
//! return only its remaining playback streams when the last capture closes.
use super::pulse::wait;
use anyhow::{ensure, Context as _, Result};
use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{Context, State},
    mainloop::standard::Mainloop,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

struct Connection {
    context: Context,
    mainloop: Mainloop,
}
impl Connection {
    fn new(stop: &AtomicBool) -> Result<Self> {
        let mut mainloop = Mainloop::new().context("cannot create host audio routing loop")?;
        let mut context = Context::new(&mainloop, "Eclipse streaming audio output")
            .context("cannot create host audio routing context")?;
        context.connect(None, pulse::context::FlagSet::NOAUTOSPAWN, None)?;
        wait(&mut mainloop, stop, || {
            ensure!(
                !matches!(context.get_state(), State::Failed | State::Terminated),
                "host audio routing server is unavailable"
            );
            Ok(context.get_state() == State::Ready)
        })?;
        Ok(Self { context, mainloop })
    }
    fn value<T>(&mut self, value: Rc<RefCell<Option<T>>>) -> Result<T> {
        wait(&mut self.mainloop, &AtomicBool::new(false), || {
            ensure!(
                self.context.get_state() == State::Ready,
                "host audio routing server disconnected"
            );
            Ok(value.borrow().is_some())
        })?;
        let value = value.borrow_mut().take().unwrap();
        Ok(value)
    }
    fn default_sink(&mut self) -> Result<String> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op = self.context.introspect().get_server_info(move |info| {
            *result.borrow_mut() = Some(info.default_sink_name.as_ref().map(ToString::to_string))
        });
        let value = self.value(value)?;
        drop(op);
        value.context("no default host output")
    }
    fn sink(&mut self, name: &str) -> Result<(u32, Option<u32>)> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op = self
            .context
            .introspect()
            .get_sink_info_by_name(name, move |info| match info {
                ListResult::Item(info) => {
                    *result.borrow_mut() = Some(Some((info.index, info.owner_module)))
                }
                ListResult::End | ListResult::Error => {
                    if result.borrow().is_none() {
                        *result.borrow_mut() = Some(None);
                    }
                }
            });
        let value = self.value(value)?;
        drop(op);
        value.context("streaming host output is unavailable")
    }
    fn inputs(&mut self) -> Result<Vec<(u32, u32)>> {
        let rows = Rc::new(RefCell::new(Vec::new()));
        let result = rows.clone();
        let done = Rc::new(RefCell::new(None));
        let ready = done.clone();
        let op = self
            .context
            .introspect()
            .get_sink_input_info_list(move |info| match info {
                ListResult::Item(info) => result.borrow_mut().push((info.index, info.sink)),
                ListResult::End => *ready.borrow_mut() = Some(true),
                ListResult::Error => *ready.borrow_mut() = Some(false),
            });
        let success = self.value(done)?;
        drop(op);
        ensure!(success, "cannot enumerate host playback streams");
        let rows = rows.borrow().clone();
        Ok(rows)
    }
    fn set_default(&mut self, name: &str) -> Result<()> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op = self
            .context
            .set_default_sink(name, move |ok| *result.borrow_mut() = Some(ok));
        let ok = self.value(value)?;
        drop(op);
        ensure!(ok, "cannot select streaming host output");
        Ok(())
    }
    fn move_input(&mut self, index: u32, sink: &str) -> Result<()> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op = self.context.introspect().move_sink_input_by_name(
            index,
            sink,
            Some(Box::new(move |ok| *result.borrow_mut() = Some(ok))),
        );
        let ok = self.value(value)?;
        drop(op);
        // Applications may close while routing. A vanished stream needs no move.
        ensure!(
            ok || !self.inputs()?.iter().any(|(id, _)| *id == index),
            "cannot move a host playback stream"
        );
        Ok(())
    }
    fn load(&mut self, name: &str) -> Result<u32> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op=self.context.introspect().load_module("module-null-sink",&format!("sink_name={name} rate=48000 channels=2 sink_properties=device.description=Eclipse_Streaming"),move|id|*result.borrow_mut()=Some(id));
        let id = self.value(value)?;
        drop(op);
        ensure!(
            id != pulse::def::INVALID_INDEX,
            "cannot create silent streaming output"
        );
        Ok(id)
    }
    fn unload(&mut self, id: u32) -> Result<()> {
        let value = Rc::new(RefCell::new(None));
        let result = value.clone();
        let op = self
            .context
            .introspect()
            .unload_module(id, move |ok| *result.borrow_mut() = Some(ok));
        let ok = self.value(value)?;
        drop(op);
        ensure!(ok, "cannot remove silent streaming output");
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}
struct Route {
    original: String,
    sink: String,
    index: u32,
    module: u32,
    users: usize,
}
fn registry() -> &'static Mutex<HashMap<String, Route>> {
    static ROUTES: OnceLock<Mutex<HashMap<String, Route>>> = OnceLock::new();
    ROUTES.get_or_init(Default::default)
}
impl Route {
    fn restore(&self, connection: &mut Connection) -> Result<()> {
        // Both name and module identity must still match our exact output.
        let (index, module) = connection.sink(&self.sink)?;
        ensure!(
            index == self.index && module == Some(self.module),
            "streaming output ownership changed"
        );
        let current = connection.default_sink()?;
        let target = if current == self.sink {
            self.original.clone()
        } else {
            current
        };
        connection.sink(&target)?;
        if connection.default_sink()? == self.sink {
            connection.set_default(&target)?;
        }
        for (id, sink) in connection.inputs()? {
            if sink == self.index {
                connection.move_input(id, &target)?;
            }
        }
        connection.unload(self.module)
    }
}
pub(super) struct Guard {
    connection: Connection,
    server: String,
    sink: String,
}
impl Guard {
    pub fn sink(&self) -> &str {
        &self.sink
    }
    pub fn acquire(stop: &AtomicBool) -> Result<Self> {
        let mut connection = Connection::new(stop)?;
        let server = connection
            .context
            .get_server()
            .context("host audio server identity unavailable")?
            .to_string();
        let mut routes = registry().lock().unwrap();
        if let Some(route) = routes.get(&server).filter(|route| route.users == 0) {
            // A failed rollback remains one owned intent. Retrying audio must
            // never create another output while its cleanup is still pending.
            route
                .restore(&mut connection)
                .context("previous host audio output cleanup remains pending")?;
            routes.remove(&server);
        }
        if let Some(route) = routes.get_mut(&server) {
            ensure!(
                connection.sink(&route.sink)? == (route.index, Some(route.module)),
                "streaming output ownership changed"
            );
            route.users += 1;
            return Ok(Self {
                connection,
                server,
                sink: route.sink.clone(),
            });
        }
        let original = connection.default_sink()?;
        let original_index = connection.sink(&original)?.0;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let identity = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let sink = format!(
            "eclipse_stream_{}_{identity}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let module = connection.load(&sink)?;
        let index = match connection.sink(&sink) {
            Ok((index, Some(owner))) if owner == module => index,
            _ => {
                let _ = connection.unload(module);
                anyhow::bail!("silent output did not become ready");
            }
        };
        let route = Route {
            original,
            sink: sink.clone(),
            index,
            module,
            users: 1,
        };
        let prepared = (|| -> Result<()> {
            ensure!(
                connection.default_sink()? == route.original,
                "default output changed while preparing streaming audio"
            );
            connection.set_default(&sink)?;
            for (id, target) in connection.inputs()? {
                if target == original_index {
                    connection.move_input(id, &sink)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = prepared {
            if let Err(cleanup) = route.restore(&mut connection) {
                let mut pending = route;
                pending.users = 0;
                routes.insert(server, pending);
                anyhow::bail!("{error}; host audio output cleanup remains pending: {cleanup}");
            }
            return Err(error);
        }
        routes.insert(server.clone(), route);
        Ok(Self {
            connection,
            server,
            sink,
        })
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut routes = registry().lock().unwrap();
        let Some(route) = routes.get_mut(&self.server) else {
            return;
        };
        route.users -= 1;
        if route.users == 0 {
            match route.restore(&mut self.connection) {
                Ok(()) => {
                    routes.remove(&self.server);
                }
                Err(error) => tracing::warn!(%error,"host audio routing restoration failed"),
            }
        }
    }
}
