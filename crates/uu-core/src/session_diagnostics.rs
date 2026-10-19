//! Bounded metadata-only diagnostics. Call sites never pass input, clipboard,
//! authentication, SDP or network candidate payloads.
use serde_json::Value;
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        mpsc::{sync_channel, SyncSender},
        OnceLock,
    },
};

#[derive(Default)]
pub struct SessionDiagnostics(OnceLock<SyncSender<Value>>);
impl SessionDiagnostics {
    pub fn start(&self, directories: impl IntoIterator<Item = PathBuf>) {
        if self.0.get().is_some() {
            return;
        }
        let name = format!(
            "uu-session-{}-{}.jsonl",
            std::process::id(),
            crate::control_wire::now_ms()
        );
        let file = directories.into_iter().find_map(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(&name))
                .ok()
        });
        let Some(mut file) = file else {
            return;
        };
        let (tx, rx) = sync_channel::<Value>(64);
        if self.0.set(tx).is_err() {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("uu-diagnostics".into())
            .spawn(move || {
                let mut size = 0;
                for value in rx {
                    let Ok(mut line) = serde_json::to_vec(&value) else {
                        continue;
                    };
                    line.push(b'\n');
                    size += line.len();
                    if size > 256 * 1024 || file.write_all(&line).is_err() {
                        break;
                    }
                }
            });
    }
    pub fn record(&self, mut value: Value) {
        if let Some(tx) = self.0.get() {
            value["at_ms"] = crate::control_wire::now_ms().into();
            let _ = tx.try_send(value);
        }
    }
}
