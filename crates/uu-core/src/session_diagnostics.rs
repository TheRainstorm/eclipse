//! Bounded metadata-only diagnostics. Call sites never pass input, clipboard,
//! authentication, SDP or network candidate payloads.
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        OnceLock,
    },
};

#[derive(Default)]
pub struct SessionDiagnostics(OnceLock<SyncSender<Value>>);
impl SessionDiagnostics {
    /// Attach a bounded metadata receiver instead of a file writer. A session
    /// has one sink; slow diagnostic consumers never block media preparation.
    pub fn capture(&self) -> Option<Receiver<Value>> {
        let (tx, rx) = sync_channel(64);
        self.0.set(tx).ok()?;
        Some(rx)
    }
    pub fn start(&self, directories: impl IntoIterator<Item = PathBuf>) {
        if self.0.get().is_some() {
            return;
        }
        let name = format!(
            "uu-session-{}-{}-{}.jsonl",
            std::process::id(),
            crate::control_wire::now_ms(),
            uuid::Uuid::new_v4()
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
            let path=dir.join(&name);
            options.open(&path).ok().map(|file|(file,path))
        });
        let Some((file,path)) = file else {
            return;
        };
        let Some(rx) = self.capture() else { return; };
        let _ = std::thread::Builder::new()
            .name("uu-diagnostics".into())
            .spawn(move || {
                let mut writer=crate::diagnostic_writer::Writer::new(file,path);
                for value in rx {
                    let Ok(mut line) = serde_json::to_vec(&value) else {
                        continue;
                    };
                    line.push(b'\n');
                    if writer.write(&line).is_err() {
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
    fn in_memory_reports_are_bounded_and_have_one_sink() {
        let logger = SessionDiagnostics::default();
        let events = logger.capture().unwrap();
        assert!(logger.capture().is_none());
        for number in 0..1000 {logger.record(serde_json::json!({"event":"generated","number":number}));}
        let events:Vec<_> = events.try_iter().collect();
        assert_eq!(events.len(),64);
        assert_eq!(events[0]["number"],0);
        assert_eq!(events[63]["number"],63);
        assert!(events.iter().all(|event|event["at_ms"].is_number()));
    }
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
