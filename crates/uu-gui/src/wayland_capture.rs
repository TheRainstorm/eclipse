//! Surface-scoped Wayland keyboard shortcut inhibition. No global input hooks.
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::{
    backend::{Backend, ObjectId},
    protocol::{wl_registry, wl_seat, wl_surface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1 as Inhibitor},
};
#[derive(Default)]
struct State {
    manager: Option<Manager>,
    seats: Vec<wl_seat::WlSeat>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == Manager::interface().name && state.manager.is_none() {
                state.manager = Some(registry.bind(name, 1, qh, ()));
            } else if interface == "wl_seat" {
                state
                    .seats
                    .push(registry.bind(name, version.min(5), qh, ()));
            }
        }
    }
}
wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: ignore Manager);
impl Dispatch<Inhibitor, ()> for State {
    fn event(
        _: &mut Self,
        _: &Inhibitor,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if std::env::var_os("UU_UI_DEBUG").is_some() {
            eprintln!("wayland capture: {event:?}");
        }
    }
}
pub struct Capture {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    surface: wl_surface::WlSurface,
    inhibitors: Vec<Inhibitor>,
}
impl Capture {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let RawDisplayHandle::Wayland(display) = cc.display_handle().ok()?.as_raw() else {
            return None;
        };
        let debug = std::env::var_os("UU_UI_DEBUG").is_some();
        if debug {
            eprintln!("wayland capture: native display found");
        }
        let RawWindowHandle::Wayland(window) = cc.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: eframe owns this live display/surface and drops the App
        // (including Capture) before destroying its window. Guest mode neither
        // closes the connection nor takes ownership of the borrowed surface.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let id = unsafe {
            ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                window.surface.as_ptr().cast(),
            )
        }
        .ok()?;
        let surface = wl_surface::WlSurface::from_id(&connection, id).ok()?;
        if debug {
            eprintln!("wayland capture: native surface found");
        }
        let mut queue = connection.new_event_queue();
        let mut state = State::default();
        let _registry = connection.display().get_registry(&queue.handle(), ());
        queue.roundtrip(&mut state).ok()?;
        if debug {
            eprintln!(
                "wayland capture: manager={} seats={}",
                state.manager.is_some(),
                state.seats.len()
            );
        }
        state.manager.as_ref()?;
        Some(Self {
            connection,
            queue,
            state,
            surface,
            inhibitors: Vec::new(),
        })
    }
    pub fn update(&mut self, active: bool) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        if active && self.inhibitors.is_empty() {
            if std::env::var_os("UU_UI_DEBUG").is_some() {
                eprintln!("wayland capture: inhibit requested");
            }
            if let Some(manager) = &self.state.manager {
                for seat in &self.state.seats {
                    self.inhibitors.push(manager.inhibit_shortcuts(
                        &self.surface,
                        seat,
                        &self.queue.handle(),
                        (),
                    ));
                }
            }
        } else if !active {
            for inhibitor in self.inhibitors.drain(..) {
                inhibitor.destroy();
            }
        }
        let _ = self.connection.flush();
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        for inhibitor in self.inhibitors.drain(..) {
            inhibitor.destroy();
        }
        if let Some(manager) = self.state.manager.take() {
            manager.destroy();
        }
        for seat in self.state.seats.drain(..) {
            if seat.version() >= 5 {
                seat.release();
            }
        }
        let _ = self.connection.flush();
    }
}
