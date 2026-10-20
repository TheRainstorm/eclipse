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
            let mut directory = std::fs::DirBuilder::new();
            directory.recursive(true);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
                directory.mode(0o700);
                options.mode(0o600);
            }
            directory.create(&dir).ok()?;
            options.open(dir.join(&name)).ok()
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
        if !value.is_object() {
            return;
        }
        if let Some(tx) = self.0.get() {
            value["at_ms"] = crate::control_wire::now_ms().into();
            let _ = tx.try_send(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_are_private_and_ignore_non_object_events_without_panicking() {
        let root =
            std::env::temp_dir().join(format!("uu-owned-diagnostics-{}", uuid::Uuid::new_v4()));
        let directory = root.join("private");
        let logger = SessionDiagnostics::default();
        logger.start([directory.clone()]);
        logger.record(serde_json::json!(42));
        logger.record(serde_json::json!(["not an event"]));
        logger.record(serde_json::json!({"event":"generated_fixture"}));
        drop(logger);
        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let content = loop {
            let text = std::fs::read_to_string(&files[0]).unwrap();
            if text.contains("generated_fixture") {
                break text;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned diagnostics worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let lines: Vec<_> = content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0]["at_ms"].is_number());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
