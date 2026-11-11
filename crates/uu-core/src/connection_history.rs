//! Bounded host audit data. Persistence owns one worker, never a media task.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::Read,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
};
use tokio::sync::broadcast;

pub const LIMIT: usize = 200;
const MAX_FILE: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source { Ip, Internet, #[default] Other }
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome { Connecting, Connected, Disconnected, Failed, Interrupted }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub peer: String,
    pub name: String,
    pub source: Source,
    pub address: Option<IpAddr>,
    pub requested_at: u64,
    pub connected_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub outcome: Outcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub records: VecDeque<Record>,
    pub error: Option<String>,
}
impl Snapshot {
    /// Used only when no host owns this log. A process crash has no witnessed
    /// end time, so an archived in-progress record must not look live.
    pub fn inactive(mut self) -> Self {
        let mut changed = false;
        for record in &mut self.records {
            if matches!(record.outcome, Outcome::Connecting | Outcome::Connected) {
                record.outcome = Outcome::Interrupted;
                record.ended_at = None;
                changed = true;
            }
        }
        if changed { self.revision += 1; }
        self
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File { version: u32, revision: u64, records: VecDeque<Record> }
struct State { snapshot: Snapshot, dirty: bool, stopped: bool }
struct Data { state: Mutex<State>, wake: Condvar }
struct Inner {
    data: Arc<Data>,
    notices: broadcast::Sender<Record>,
    writer: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.data.state.lock().unwrap().stopped = true;
        self.data.wake.notify_one();
        if let Some(writer) = self.writer.get_mut().unwrap().take() { let _ = writer.join(); }
    }
}
#[derive(Clone)]
pub struct History(Arc<Inner>);
fn clean(value: &str, max_chars: usize) -> String {
    value.chars().filter(|c| !c.is_control()).take(max_chars).collect()
}
impl Default for History {
    fn default() -> Self { Self::memory(Snapshot { revision: 0, records: VecDeque::new(), error: None }) }
}
impl History {
    /// Flush while the host still owns its configuration, even if a frontend
    /// keeps an Access handle briefly after listeners have retired.
    pub fn close(&self) {
        self.0.data.state.lock().unwrap().stopped = true;
        self.0.data.wake.notify_one();
        if let Some(writer) = self.0.writer.lock().unwrap().take() { let _ = writer.join(); }
    }
    fn memory(snapshot: Snapshot) -> Self {
        let (notices, _) = broadcast::channel(32);
        Self(Arc::new(Inner { data: Arc::new(Data {
            state: Mutex::new(State { snapshot, dirty: false, stopped: false }), wake: Condvar::new(),
        }), notices, writer: Mutex::new(None) }))
    }
    pub fn read(path: &Path) -> anyhow::Result<Snapshot> {
        let mut bytes = Vec::new();
        match std::fs::File::open(path) {
            Ok(file) => { file.take(MAX_FILE + 1).read_to_end(&mut bytes)?; }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default().snapshot()),
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(bytes.len() as u64 <= MAX_FILE, "connection history exceeds size limit");
        let file: File = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(file.version == 1 && file.records.len() <= LIMIT, "invalid connection history version or size");
        for record in &file.records {
            anyhow::ensure!(uuid::Uuid::parse_str(&record.id).is_ok() && record.peer.chars().count() <= 128
                && record.name.chars().count() <= 64, "invalid connection history record");
        }
        Ok(Snapshot { revision: file.revision, records: file.records, error: None })
    }
    /// Unreadable history is preserved. Sharing still works with a visible
    /// persistence error and an in-memory log for this host run.
    pub fn open(path: PathBuf) -> Self {
        let (snapshot, interrupted) = match Self::read(&path) {
            Ok(snapshot) => {
                let revision = snapshot.revision;
                let snapshot = snapshot.inactive();
                let interrupted = snapshot.revision != revision;
                (snapshot, interrupted)
            },
            Err(error) => return Self::memory(Snapshot { revision: 0, records: VecDeque::new(), error: Some(format!("{error:#}")) }),
        };
        let history = Self::memory(snapshot);
        history.0.data.state.lock().unwrap().dirty = interrupted;
        let data = history.0.data.clone();
        let writer = std::thread::Builder::new().name("host-history".into()).spawn(move || {
            loop {
                let snapshot = {
                    let mut state = data.state.lock().unwrap();
                    while !state.dirty && !state.stopped { state = data.wake.wait(state).unwrap(); }
                    if !state.dirty { break; }
                    state.dirty = false;
                    state.snapshot.clone()
                };
                let result = serde_json::to_vec(&File { version: 1, revision: snapshot.revision, records: snapshot.records })
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| crate::private_file::write_atomic(&path, &bytes).map_err(Into::into));
                let mut state = data.state.lock().unwrap();
                let error = result.err().map(|error| format!("{error:#}"));
                if state.snapshot.error != error {
                    state.snapshot.error = error;
                    state.snapshot.revision += 1;
                }
            }
        });
        match writer {
            Ok(writer) => *history.0.writer.lock().unwrap() = Some(writer),
            Err(error) => history.0.data.state.lock().unwrap().snapshot.error = Some(error.to_string()),
        }
        history
    }
    pub fn snapshot(&self) -> Snapshot { self.0.data.state.lock().unwrap().snapshot.clone() }
    pub fn changed_since(&self, revision: Option<u64>) -> Option<Snapshot> {
        let state = self.0.data.state.lock().unwrap();
        (revision != Some(state.snapshot.revision)).then(|| state.snapshot.clone())
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Record> { self.0.notices.subscribe() }
    fn edit(&self, edit: impl FnOnce(&mut VecDeque<Record>) -> bool) {
        let mut state = self.0.data.state.lock().unwrap();
        if edit(&mut state.snapshot.records) {
            state.snapshot.revision += 1; state.dirty = true; self.0.data.wake.notify_one();
        }
    }
    pub(crate) fn begin(&self, id: &str, peer: &str, name: &str) -> Record {
        let record = Record { id: id.into(), peer: clean(peer, 128), name: clean(name, 64),
            source: Source::Other, address: None, requested_at: crate::time::now_ms(),
            connected_at: None, ended_at: None, outcome: Outcome::Connecting };
        self.edit(|records| {
            records.push_front(record.clone());
            records.truncate(LIMIT); true
        });
        record
    }
    fn update(&self, record: &Record) {
        self.edit(|records| {
            let Some(saved) = records.iter_mut().find(|r| r.id == record.id) else { return false; };
            *saved = record.clone(); true
        });
    }
    pub(crate) fn source(&self, record: &mut Record, source: Source, address: Option<IpAddr>) {
        if record.source == source && record.address == address { return; }
        record.source = source; record.address = address; self.update(record);
    }
    pub(crate) fn connected(&self, record: &mut Record, notify: bool) {
        if record.connected_at.is_some() || record.ended_at.is_some() { return; }
        record.connected_at = Some(crate::time::now_ms()); record.outcome = Outcome::Connected;
        self.update(record);
        if notify { let _ = self.0.notices.send(record.clone()); }
    }
    pub(crate) fn end(&self, record: &mut Record, successful: bool) {
        if record.ended_at.is_some() { return; }
        record.ended_at = Some(crate::time::now_ms());
        record.outcome = if successful && record.connected_at.is_some() { Outcome::Disconnected } else { Outcome::Failed };
        self.update(record);
    }
}

#[cfg(test)]
mod tests;
