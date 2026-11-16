//! Bounded session diagnostics: keep the initial segment and a recent tail.
use std::{ffi::OsString,fs::{File,OpenOptions},io::{self,Write},path::PathBuf};

const SEGMENT_BYTES:usize=256*1024;
const TAIL_SEGMENTS:usize=7;
const MAX_RECORD:usize=32*1024;
const STDERR_BUFFER_BYTES:usize=64*1024;

/// Large opt-in terminal reports must not turn every JSON punctuation fragment
/// into an unbuffered OS stderr write. Diagnostic I/O never fails a session.
pub(crate) fn stderr_json(value:&impl serde::Serialize) {
    let _=write_json_line(io::stderr().lock(),value);
}
fn write_json_line(writer:impl Write,value:&impl serde::Serialize)->io::Result<()> {
    let mut writer=io::BufWriter::with_capacity(STDERR_BUFFER_BYTES,writer);
    serde_json::to_writer(&mut writer,value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(crate) struct Writer {
    file:File,
    initial:PathBuf,
    size:usize,
    segment:usize,
    limit:usize,
    tails:usize,
}
impl Writer {
    pub fn new(file:File,initial:PathBuf)->Self {
        Self{file,initial,size:0,segment:0,limit:SEGMENT_BYTES,tails:TAIL_SEGMENTS}
    }
    pub fn write(&mut self,line:&[u8])->io::Result<()> {
        if line.len()>MAX_RECORD || line.len()>self.limit {return Ok(());}
        if self.size+line.len()>self.limit {
            self.segment=self.segment.saturating_add(1);
            let slot=(self.segment-1)%self.tails;
            let mut name=OsString::from(self.initial.file_stem().ok_or_else(||io::Error::other("diagnostic name is missing"))?);
            name.push(format!(".tail-{slot}.jsonl"));
            let path=self.initial.with_file_name(name);
            let mut options=OpenOptions::new();options.write(true);
            if self.segment>self.tails {
                // Replace our previous slot atomically. Concurrent readers keep
                // their complete old segment rather than observing truncation.
                crate::private_file::write_atomic(&path,b"")?;
                options.append(true);
            }else{options.create_new(true);}
            #[cfg(unix)] {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            self.file=options.open(path)?;
            self.size=0;
        }
        self.file.write_all(line)?;self.size+=line.len();Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct CountingWriter {bytes:Vec<u8>,writes:usize,flushes:usize}
    impl Write for CountingWriter {
        fn write(&mut self,bytes:&[u8])->io::Result<usize> {
            self.writes+=1;self.bytes.extend_from_slice(bytes);Ok(bytes.len())
        }
        fn flush(&mut self)->io::Result<()> {self.flushes+=1;Ok(())}
    }
    #[test]
    fn large_terminal_json_preserves_one_line_and_bounds_os_writes() {
        let frames:Vec<_>=(0..4096).map(|n|serde_json::json!({
            "frame_id":n,"pts_us":n*16667u64,"events_us":[n*17,n*17+1,n*17+9],
            "source":{"generation":1,"duplicate":false},"label":"owned profiling fixture"})).collect();
        let value=serde_json::json!({"event":"host_continuous_trace","report":{"frames":frames}});
        // The direct serializer is an independent control for the original
        // fragment-by-fragment sink, not an assumed number of JSON fields.
        let mut direct=CountingWriter::default();
        serde_json::to_writer(&mut direct,&value).unwrap();direct.write_all(b"\n").unwrap();
        let mut buffered=CountingWriter::default();
        write_json_line(&mut buffered,&value).unwrap();
        assert_eq!(buffered.bytes,direct.bytes);
        assert_eq!(buffered.bytes.iter().filter(|b|**b==b'\n').count(),1);
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&buffered.bytes).unwrap(),value);
        assert!(buffered.bytes.len()>512*1024,"exercise a real large report");
        assert!(buffered.writes<=32,"large report must use bounded chunk writes: {}",buffered.writes);
        assert!(direct.writes>buffered.writes*100,"buffering must remove fragmented OS I/O");
        assert_eq!(buffered.flushes,1);
    }
    #[test]
    fn terminal_json_write_failure_is_a_returned_diagnostic_error() {
        struct Failed;
        impl Write for Failed {
            fn write(&mut self,_:&[u8])->io::Result<usize> {Err(io::ErrorKind::BrokenPipe.into())}
            fn flush(&mut self)->io::Result<()> {Ok(())}
        }
        assert!(write_json_line(Failed,&serde_json::json!({"event":"fixture"})).is_err());
    }
    #[test]
    fn long_sessions_keep_initial_metadata_and_recent_events_with_bounded_private_storage() {
        let root=std::env::temp_dir().join(format!("eclipse-diagnostics-rotation-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();let initial=root.join("owned-session.jsonl");
        crate::private_file::write_atomic(&initial,b"").unwrap();
        let file=OpenOptions::new().write(true).open(&initial).unwrap();
        let mut writer=Writer{file,initial:initial.clone(),size:0,segment:0,limit:192,tails:2};
        for index in 0..1000 {
            let mut line=serde_json::to_vec(&serde_json::json!({"event":"fixture","index":index})).unwrap();line.push(b'\n');
            writer.write(&line).unwrap();
        }
        writer.write(&vec![b'x';MAX_RECORD+1]).unwrap();drop(writer);
        let files:Vec<_>=std::fs::read_dir(&root).unwrap().map(|e|e.unwrap().path()).collect();
        assert_eq!(files.len(),3);
        assert!(std::fs::read_to_string(&initial).unwrap().contains("\"index\":0"));
        let mut latest=0;
        for path in files {
            assert!(std::fs::metadata(&path).unwrap().len()<=192);
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o600);
            }
            for line in std::fs::read_to_string(path).unwrap().lines() {
                let value:serde_json::Value=serde_json::from_str(line).unwrap();
                latest=latest.max(value["index"].as_u64().unwrap());
            }
        }
        assert_eq!(latest,999);
        std::fs::remove_dir_all(root).unwrap();
    }
}
