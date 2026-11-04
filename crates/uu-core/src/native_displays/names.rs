//! Optional presentation metadata. Stable source names and the strict display
//! catalog stay unchanged, so old peers can ignore this session SDP attribute.
use super::*;

const ATTRIBUTE:&str="x-eclipse-display-names";
const LIMIT:usize=16*1024;
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {source:String,label:String,hdr:bool}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Names {version:u8,entries:Vec<Name>}
impl Default for Names {
    fn default()->Self {Self{version:1,entries:Vec::new()}}
}
impl Names {
    pub fn from_monitors(monitors:&[CaptureMonitor])->Self {
        let clean=|name:&str|name.chars().filter(|c|!c.is_control()).take(64).collect::<String>().trim().to_owned();
        Self{version:1,entries:monitors.iter().take(MAX_DISPLAYS).map(|m|Name {
            source:display_name(&m.name),
            label:clean(m.friendly_name.as_deref().filter(|n|!n.trim().is_empty()).unwrap_or(&m.name)),
            hdr:m.hdr_enabled==Some(true),
        }).collect()}
    }
    fn validate(&self)->Result<()> {
        ensure!(self.entries.len()<=MAX_DISPLAYS,"too many display labels");
        for (index,name) in self.entries.iter().enumerate() {
            ensure!(valid_source(&name.source,false) && valid_source(&name.label,false)
                && !self.entries[..index].iter().any(|old|old.source==name.source),"invalid display label");
        }
        Ok(())
    }
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {
        if self.entries.is_empty(){return Ok(());}
        self.validate()?;
        let value=serde_json::to_vec(self)?;
        ensure!(value.len()<=LIMIT,"display labels exceed limit");
        let at=sdp.sdp.find("m=").unwrap_or(sdp.sdp.len());
        sdp.sdp.insert_str(at,&format!("a={ATTRIBUTE}:{}\r\n",hex::encode(value)));
        Ok(())
    }
    pub fn read(sdp:&RTCSessionDescription)->Result<Self> {
        let prefix=format!("a={ATTRIBUTE}:");
        let mut lines=sdp.sdp.lines().filter_map(|line|line.strip_prefix(&prefix));
        let Some(value)=lines.next()else{return Ok(Self::default());};
        ensure!(lines.next().is_none() && value.len()<=LIMIT*2,"duplicate or oversized display labels");
        let value:Self=serde_json::from_slice(&hex::decode(value)?)?;
        if value.version!=1 {return Ok(Self::default());}
        value.validate()?;
        Ok(value)
    }
    pub fn label(&self,source:&str)->Option<String> {
        let name=self.entries.iter().find(|name|name.source==source)?;
        let hdr=if name.hdr && !name.label.split_whitespace().any(|word|word.eq_ignore_ascii_case("HDR")){" HDR"}else{""};
        Some(format!("{}{hdr}",name.label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sdp()->RTCSessionDescription {RTCSessionDescription::answer("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n".into()).unwrap()}
    #[test]
    fn names_roundtrip_without_changing_the_capture_source_and_clear_for_old_hosts() {
        let names=Names{version:1,entries:vec![Name{source:r"\\.\DISPLAY2".into(),label:"Zako".into(),hdr:true}]};
        let mut answer=sdp();names.add(&mut answer).unwrap();
        let read=Names::read(&answer).unwrap();
        assert_eq!(read.label(r"\\.\DISPLAY2").as_deref(),Some("Zako HDR"));
        assert!(read.label(r"\\.\DISPLAY1").is_none());
        assert!(Names::read(&sdp()).unwrap().label(r"\\.\DISPLAY2").is_none());
        let mut future=names.clone();future.version=2;let mut answer=sdp();future.add(&mut answer).unwrap();
        assert!(Names::read(&answer).unwrap().entries.is_empty());
    }
    #[test]
    fn labels_reject_duplicate_sources_attributes_and_control_characters() {
        let mut names=Names{version:1,entries:vec![Name{source:"source".into(),label:"Zako HDR".into(),hdr:true}]};
        assert_eq!(names.label("source").as_deref(),Some("Zako HDR"));
        let mut answer=sdp();names.add(&mut answer).unwrap();names.add(&mut answer).unwrap();
        assert!(Names::read(&answer).is_err());
        names.entries.push(names.entries[0].clone());assert!(names.validate().is_err());
        names.entries.pop();names.entries[0].label.push('\n');assert!(names.validate().is_err());
    }
}
