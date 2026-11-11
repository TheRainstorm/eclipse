//! Passive grabs leave ordinary input alone; XKB suppresses repeat activations.
use super::Event;
use std::{
    collections::HashSet,
    os::fd::{AsRawFd, BorrowedFd},
};
use tokio::sync::{mpsc, oneshot};
use uu_core::hotkeys::Chord;
use x11rb::{
    connection::Connection,
    protocol::{
        xkb::{self, ConnectionExt as _},
        xproto::{ConnectionExt as _, GrabMode, ModMask},
        Event as XEvent,
    },
    rust_connection::RustConnection,
};

pub(super) fn keysym_name(key: u32) -> String {
    match key {
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(key).unwrap().to_string(),
        0x70..=0x87 => format!("F{}", key - 0x6f),
        _ => match key {
            8 => "BackSpace",
            9 => "Tab",
            13 => "Return",
            27 => "Escape",
            32 => "space",
            33 => "Prior",
            34 => "Next",
            35 => "End",
            36 => "Home",
            37 => "Left",
            38 => "Up",
            39 => "Right",
            40 => "Down",
            45 => "Insert",
            46 => "Delete",
            0xba => "semicolon",
            0xbb => "equal",
            0xbc => "comma",
            0xbd => "minus",
            0xbe => "period",
            0xbf => "slash",
            0xc0 => "grave",
            0xdb => "bracketleft",
            0xdc => "backslash",
            0xdd => "bracketright",
            0xde => "apostrophe",
            _ => "VoidSymbol",
        }
        .into(),
    }
}
fn keysym(key: u32) -> u32 {
    match key {
        0x30..=0x39 | 0x41..=0x5a => key,
        0x70..=0x87 => 0xffbe + key - 0x70,
        _ => match key {
            8 => 0xff08,
            9 => 0xff09,
            13 => 0xff0d,
            27 => 0xff1b,
            32 => 32,
            33 => 0xff55,
            34 => 0xff56,
            35 => 0xff57,
            36 => 0xff50,
            37 => 0xff51,
            38 => 0xff52,
            39 => 0xff53,
            40 => 0xff54,
            45 => 0xff63,
            46 => 0xffff,
            0xba => 59,
            0xbb => 61,
            0xbc => 44,
            0xbd => 45,
            0xbe => 46,
            0xbf => 47,
            0xc0 => 96,
            0xdb => 91,
            0xdc => 92,
            0xdd => 93,
            0xde => 39,
            _ => 0,
        },
    }
}
fn grab(connection: &RustConnection, root: u32, keys: &[Chord]) -> anyhow::Result<HashSet<u8>> {
    let setup = connection.setup();
    let min = setup.min_keycode;
    let mapping = connection
        .get_keyboard_mapping(min, setup.max_keycode - min + 1)?
        .reply()?;
    let width = usize::from(mapping.keysyms_per_keycode);
    anyhow::ensure!(width > 0, "keyboard has no symbols");
    let symbol_for = |code: u8| {
        mapping
            .keysyms
            .get(
                usize::from(code.saturating_sub(min)) * width
                    ..usize::from(code.saturating_sub(min)) * width + width,
            )
            .unwrap_or(&[])
    };
    let modifiers = connection.get_modifier_mapping()?.reply()?;
    let per = modifiers.keycodes.len() / 8;
    let num_lock = if per > 0 {
        modifiers
            .keycodes
            .chunks(per)
            .position(|codes| {
                codes
                    .iter()
                    .any(|code| *code >= min && symbol_for(*code).contains(&0xff7f))
            })
            .map_or(0, |i| 1u16 << i)
    } else {
        0
    };
    let mut grabbed = HashSet::new();
    for chord in keys {
        if chord.modifiers == Default::default() {
            continue;
        }
        let Some(index) = mapping
            .keysyms
            .chunks(width)
            .position(|symbols| symbols.contains(&keysym(chord.key)))
        else {
            continue;
        };
        let code = min + index as u8;
        let m = chord.modifiers;
        let mask = if m.ctrl {
            u16::from(ModMask::CONTROL)
        } else {
            0
        } | if m.alt { u16::from(ModMask::M1) } else { 0 }
            | if m.shift {
                u16::from(ModMask::SHIFT)
            } else {
                0
            }
            | if m.meta { u16::from(ModMask::M4) } else { 0 };
        let variants: HashSet<_> = [
            mask,
            mask | u16::from(ModMask::LOCK),
            mask | num_lock,
            mask | num_lock | u16::from(ModMask::LOCK),
        ]
        .into_iter()
        .collect();
        let mut owned = vec![];
        let mut complete = true;
        for modifiers in variants {
            if connection
                .grab_key(
                    false,
                    root,
                    modifiers.into(),
                    code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )?
                .check()
                .is_ok()
            {
                owned.push(modifiers);
            } else {
                complete = false;
                break;
            }
        }
        if complete {
            grabbed.insert(code);
        } else {
            for modifiers in owned {
                connection
                    .ungrab_key(code, root, modifiers.into())?
                    .check()?;
            }
        }
    }
    connection.flush()?;
    anyhow::ensure!(
        !grabbed.is_empty(),
        "boss key is unavailable or already grabbed"
    );
    Ok(grabbed)
}
pub(super) async fn listen(
    keys: Vec<Chord>,
    events: mpsc::UnboundedSender<Event>,
    mut cancel: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let (connection, screen) = RustConnection::connect(None)?;
    let root = connection.setup().roots[screen].root;
    anyhow::ensure!(
        connection.xkb_use_extension(1, 0)?.reply()?.supported,
        "XKB is unavailable"
    );
    let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
    let repeat = connection
        .xkb_per_client_flags(
            xkb::ID::USE_CORE_KBD.into(),
            flag,
            flag,
            0u32.into(),
            0u32.into(),
            0u32.into(),
        )?
        .reply()?;
    anyhow::ensure!(
        repeat.value & flag == flag,
        "detectable auto repeat is unavailable"
    );
    let grabbed = grab(&connection, root, &keys)?;
    // The connection outlives the readiness registration and owns this fd.
    let fd = tokio::io::unix::AsyncFd::new(unsafe {
        BorrowedFd::borrow_raw(connection.stream().as_raw_fd())
    })?;
    let _ = events.send(Event::Ready);
    let mut held = HashSet::new();
    loop {
        while let Some(event) = connection.poll_for_event()? {
            match event {
                XEvent::KeyPress(key) if grabbed.contains(&key.detail) => {
                    if held.insert(key.detail) {
                        let _ = events.send(Event::Activated(String::new()));
                    }
                }
                XEvent::KeyRelease(key) => {
                    held.remove(&key.detail);
                }
                _ => {}
            }
        }
        tokio::select! {_=&mut cancel=>return Ok(()),ready=fd.readable()=>{ready?.clear_ready();}}
    }
}
