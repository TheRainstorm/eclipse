//! Bounded session diagnostics: keep the initial segment and a recent tail.
use std::{ffi::OsString,fs::{File,OpenOptions},io::{self,Write},path::PathBuf};

const SEGMENT_BYTES:usize=256*1024;
const TAIL_SEGMENTS:usize=7;
const MAX_RECORD:usize=32*1024;
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
