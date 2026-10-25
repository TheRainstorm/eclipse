//! Opt-in bounded observation. No encoder policy, real-time I/O or data mutation.
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
const MAX_ROWS: usize = 8192;
const MAX_CLIP_FRAMES: usize = 512;
const MAX_CLIP_BYTES: usize = 64 * 1024 * 1024;
const CLIP_US: u64 = 8_000_000;
#[derive(Serialize, Debug)]
pub(super) struct Composition {
    pub total_bytes: usize,
    pub bytes: [u64; 16],
    pub counts: [u64; 16],
    pub unparsed_bytes: usize,
    pub error: Option<String>,
}
pub(super) fn composition(data: &[u8]) -> Composition {
    let mut c = Composition {
        total_bytes: data.len(),
        bytes: [0; 16],
        counts: [0; 16],
        unparsed_bytes: 0,
        error: None,
    };
    match crate::av1::obus(data) {
        Ok(units) => {
            for u in units {
                let k = ((u[0] >> 3) & 15) as usize;
                c.bytes[k] += u.len() as u64;
                c.counts[k] += 1
            }
        }
        Err(e) => {
            c.unparsed_bytes = data.len();
            c.error = Some(e)
        }
    }
    c
}
#[derive(Serialize)]
struct Output {
    input_sequence: u64,
    output_sequence: u64,
    pts_us: u64,
    keyframe: bool,
    native: Composition,
    prepared: Composition,
    header_bytes_added: i64,
}
#[derive(Serialize)]
struct ClipIndex {
    output_sequence: u64,
    pts_us: u64,
    keyframe: bool,
    offset: usize,
    size: usize,
}
#[derive(Serialize)]
struct Snapshot {
    phase: String,
    output_sequence: u64,
    pts_us: Option<u64>,
    properties: serde_json::Value,
}
#[derive(Serialize)]
pub(super) struct Observation {
    generation: u64,
    inputs: u64,
    outputs: u64,
    rows: Vec<Output>,
    overflow: u64,
    pts_errors: u64,
    last_pts: Option<u64>,
    snapshots: Vec<Snapshot>,
    snapshot_overflow: u64,
    updates: Vec<serde_json::Value>,
    update_overflow: u64,
    excerpt_index: Vec<ClipIndex>,
    excerpt_skipped_before_key: u64,
    excerpt_limit: Option<String>,
    excerpt_complete: bool,
    excerpt_max_bytes: usize,
    diagnostic_cpu_wall_ns: u128,
    property_cpu_wall_ns: u128,
    #[serde(skip)]
    prefix: PathBuf,
    #[serde(skip)]
    excerpt: Vec<u8>,
    #[serde(skip)]
    excerpt_start: Option<u64>,
}
impl Observation {
    pub fn enabled(generation: u64, budget: usize) -> Option<Self> {
        std::env::var_os("UU_AMF_WORKLOAD_DIAGNOSTIC")
            .filter(|v| !v.is_empty())
            .map(|v| {
                let mut d = Self::new(PathBuf::from(v), generation);
                d.excerpt_max_bytes = budget.min(MAX_CLIP_BYTES);
                d
            })
    }
    fn new(prefix: PathBuf, generation: u64) -> Self {
        Self {
            generation,
            prefix,
            inputs: 0,
            outputs: 0,
            rows: Vec::new(),
            overflow: 0,
            pts_errors: 0,
            last_pts: None,
            snapshots: Vec::new(),
            snapshot_overflow: 0,
            updates: Vec::new(),
            update_overflow: 0,
            excerpt: Vec::new(),
            excerpt_index: Vec::new(),
            excerpt_start: None,
            excerpt_skipped_before_key: 0,
            excerpt_limit: None,
            excerpt_complete: false,
            excerpt_max_bytes: MAX_CLIP_BYTES,
            diagnostic_cpu_wall_ns: 0,
            property_cpu_wall_ns: 0,
        }
    }
    pub fn retained_bytes(&self) -> usize {
        self.excerpt.capacity()
    }
    pub fn input(&mut self) {
        self.inputs += 1
    }
    pub fn wants_snapshot(&self) -> bool {
        self.outputs == 1 || self.outputs > 0 && self.outputs % 60 == 0
    }
    pub fn snapshot(
        &mut self,
        phase: &str,
        pts: Option<u64>,
        properties: serde_json::Value,
        elapsed: u128,
    ) {
        self.property_cpu_wall_ns += elapsed;
        if self.snapshots.len() < 256 {
            self.snapshots.push(Snapshot {
                phase: phase.into(),
                output_sequence: self.outputs,
                pts_us: pts,
                properties,
            })
        } else {
            self.snapshot_overflow += 1
        }
    }
    pub fn update(&mut self, kbps: u32, result: i32) {
        if self.updates.len() < 256 {
            self.updates.push(serde_json::json!({"after_output":self.outputs,"requested_kbps":kbps,"result":result}))
        } else {
            self.update_overflow += 1
        }
    }
    pub fn output(&mut self, raw: &[u8], prepared: &[u8], pts: u64, key: bool) {
        let start = Instant::now();
        self.outputs += 1;
        if self.last_pts.is_some_and(|last| pts <= last) {
            self.pts_errors += 1;
            self.excerpt_limit = Some("nonmonotonic/duplicate PTS".into())
        }
        self.last_pts = Some(pts);
        if self.rows.len() < MAX_ROWS {
            self.rows.push(Output {
                input_sequence: self.inputs,
                output_sequence: self.outputs,
                pts_us: pts,
                keyframe: key,
                native: composition(raw),
                prepared: composition(prepared),
                header_bytes_added: prepared.len() as i64 - raw.len() as i64,
            })
        } else {
            self.overflow += 1
        }
        if self.excerpt_start.is_none() && !key {
            self.excerpt_skipped_before_key += 1
        } else if self.excerpt_limit.is_none() && !self.excerpt_complete {
            let first = *self.excerpt_start.get_or_insert(pts);
            if pts.saturating_sub(first) >= CLIP_US {
                self.excerpt_complete = true
            } else if self.excerpt_index.len() >= MAX_CLIP_FRAMES {
                self.excerpt_limit = Some("frame cap".into())
            } else if prepared.len() > self.excerpt_max_bytes - self.excerpt.len() {
                self.excerpt_limit = Some("byte cap".into())
            } else {
                if self.excerpt.capacity() == 0 {
                    self.excerpt.reserve_exact(self.excerpt_max_bytes);
                }
                let offset = self.excerpt.len();
                self.excerpt.extend_from_slice(prepared);
                self.excerpt_index.push(ClipIndex {
                    output_sequence: self.outputs,
                    pts_us: pts,
                    keyframe: key,
                    offset,
                    size: prepared.len(),
                })
            }
        }
        self.diagnostic_cpu_wall_ns += start.elapsed().as_nanos();
    }
    pub fn finish(self) -> Result<(), String> {
        // A distinct generation suffix and create_new prohibit overwriting evidence.
        let base = format!("{}.g{}", self.prefix.to_string_lossy(), self.generation);
        let clip = PathBuf::from(format!("{base}.obu"));
        let index = PathBuf::from(format!("{base}.json"));
        write_new(&clip, &self.excerpt)?;
        let value = serde_json::json!({"version":1,"observation":self,"excerpt_path":clip,"limits":{"generations":4,"rows_per_generation":MAX_ROWS,"clip_frames":MAX_CLIP_FRAMES,"clip_bytes":MAX_CLIP_BYTES,"clip_nominal_us":CLIP_US},"scope":"native output vs headers.prepare output; OBU entire units including headers/length; type15 padding; no GPU timing or quality equivalence claim"});
        write_new(
            &index,
            &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
        )
    }
}
fn write_new(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(data)
        .map_err(|e| format!("write {}: {e}", path.display()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_obus_are_accounted_with_unknown_numeric_types_and_padding() {
        let data = [0x12, 0, 0x7a, 3, 1, 2, 3, 0x3a, 1, 4];
        let c = composition(&data);
        assert!(c.error.is_none());
        assert_eq!(c.bytes.iter().sum::<u64>(), data.len() as u64);
        assert_eq!((c.bytes[15], c.counts[15]), (5, 1));
        assert_eq!(c.bytes[7], 3);
        for end in [1, 3, 5, 9] {
            assert!(composition(&data[..end]).error.is_some())
        }
        assert!(composition(&[0x80]).error.is_some())
    }
    #[test]
    fn clip_identity_pts_caps_and_generation_are_explicit() {
        let mut d = Observation::new(PathBuf::from("unused"), 9);
        d.input();
        d.output(&[0x12, 0], &[0x12, 0], 0, false);
        assert_eq!(d.excerpt_skipped_before_key, 1);
        for i in 1..=513 {
            d.input();
            d.output(&[0x12, 0], &[0x12, 0], i, true)
        }
        assert_eq!(d.excerpt_index.len(), 512);
        assert_eq!(d.excerpt_limit.as_deref(), Some("frame cap"));
        d.output(&[], &[], 513, false);
        assert_eq!(d.pts_errors, 1);
        assert_eq!(d.generation, 9);
        let mut b = Observation::new(PathBuf::new(), 10);
        b.excerpt.resize(MAX_CLIP_BYTES, 0);
        b.output(&[], &[1], 0, true);
        assert_eq!(b.excerpt_limit.as_deref(), Some("byte cap"));
        for i in 0..MAX_ROWS + 2 {
            b.output(&[], &[], i as u64 + 1, false)
        }
        assert_eq!(b.rows.len(), MAX_ROWS);
        assert!(b.overflow > 0)
    }
    #[test]
    fn excerpt_duration_and_create_new_failure_do_not_fake_success() {
        let p = std::env::temp_dir().join(format!("amf-observation-test-{}", std::process::id()));
        let mut d = Observation::new(p.clone(), 123);
        d.output(&[0x12, 0], &[0x12, 0], 100, true);
        d.output(&[], &[], 8_000_100, false);
        assert!(d.excerpt_complete);
        let clip = PathBuf::from(format!("{}.g123.obu", p.display()));
        let index = PathBuf::from(format!("{}.g123.json", p.display()));
        let _ = std::fs::remove_file(&clip);
        let _ = std::fs::remove_file(&index);
        d.finish().unwrap();
        assert!(Observation::new(p, 123).finish().is_err());
        std::fs::remove_file(clip).unwrap();
        std::fs::remove_file(index).unwrap();
        assert!(write_new(Path::new("missing-parent-for-amf-test/clip"), &[]).is_err())
    }
}
