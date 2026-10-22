//! AV1 RTP aggregation and fragmentation, per AOMedia's RTP payload format.
//! Restore explicit low-overhead OBU sizes only after a complete OBU arrives.
use crate::video_receiver::MAX_AU;
#[derive(Default)]
pub(crate) struct Assembler {data:Vec<u8>,fragment:Option<Vec<u8>>,frame:bool,count:usize}
fn length(input:&[u8],offset:&mut usize)->Result<usize,()>{
    let mut value=0usize;
    for index in 0..8 {let byte=*input.get(*offset).ok_or(())?;*offset+=1;
        value=value.checked_add(usize::from(byte&127).checked_shl(index*7).ok_or(())?).ok_or(())?;
        if byte&128==0{return Ok(value);}
    }Err(())
}
fn write_length(mut value:usize,out:&mut Vec<u8>){loop{let mut byte=(value&127)as u8;value>>=7;if value!=0{byte|=128;}out.push(byte);if value==0{break;}}}
impl Assembler {
    fn obu(&mut self,bytes:&[u8])->Result<(),()>{
        let header=*bytes.first().ok_or(())?;
        if header&0x81!=0{return Err(());}
        let header_size=if header&4!=0{2}else{1};
        if bytes.len()<header_size || (header_size==2 && bytes[1]&7!=0){return Err(());}
        let kind=(header>>3)&15;
        if self.data.len()+bytes.len()+8>MAX_AU || self.count>=4096{return Err(());}
        self.count+=1;
        // A size field is permitted by the RTP specification, although senders
        // should omit it. Validate it rather than adding a second size field.
        if header&2!=0 {
            let units=uu_media::av1::obus(bytes).map_err(|_|())?;if units.len()!=1{return Err(());}
            self.data.extend_from_slice(bytes);
        }else{
            self.data.push(header|2);if header_size==2{self.data.push(bytes[1]);}
            write_length(bytes.len()-header_size,&mut self.data);self.data.extend_from_slice(&bytes[header_size..]);
        }
        self.frame|=matches!(kind,3|4|6);Ok(())
    }
    pub fn push(&mut self,payload:&[u8])->Result<(),()>{
        if payload.is_empty(){return Ok(());}
        let header=payload[0];let z=header&128!=0;let y=header&64!=0;let w=(header>>4)&3;
        if header&7!=0 || (header&8!=0 && z) || z!=self.fragment.is_some(){return Err(());}
        let mut at=1;let mut elements=Vec::new();
        while at<payload.len(){
            let size=if w!=0 && elements.len()+1==w as usize{payload.len()-at}else{length(payload,&mut at)?};
            if size==0 || elements.len()>=4096{return Err(());}
            let end=at.checked_add(size).filter(|end|*end<=payload.len()).ok_or(())?;
            elements.push(&payload[at..end]);at=end;
            if w!=0 && elements.len()==w as usize{break;}
        }
        if elements.is_empty() || at!=payload.len() || (w!=0 && elements.len()!=w as usize){return Err(());}
        for (index,element) in elements.iter().enumerate(){
            let continues=y && index+1==elements.len();
            if index==0 && z {
                let fragment=self.fragment.as_mut().ok_or(())?;
                if self.data.len()+fragment.len()+element.len()>MAX_AU{return Err(());}
                fragment.extend_from_slice(element);
                if !continues{let bytes=self.fragment.take().ok_or(())?;self.obu(&bytes)?;}
            }else if continues{
                if self.fragment.is_some() || self.data.len()+element.len()>MAX_AU{return Err(());}
                self.fragment=Some(element.to_vec());
            }else{self.obu(element)?;}
        }
        Ok(())
    }
    pub fn finish(self)->Result<(Vec<u8>,bool),()>{if self.fragment.is_some(){Err(())}else{Ok((self.data,self.frame))}}
}
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use webrtc::rtp::packetizer::Payloader;
    #[test]
    fn actual_av1_packetizer_roundtrips_fragmented_generated_sequence(){
        for mtu in [5,9,16,1200]{
            let mut payloader=webrtc::rtp::codecs::av1::Av1Payloader::default();
            for source in uu_media::av1::PROBE_UNITS {
                let packets=payloader.payload(mtu,&Bytes::from_static(source)).unwrap();
                let mut assembly=Assembler::default();for packet in packets{assembly.push(&packet).unwrap();}
                let (data,frame)=assembly.finish().unwrap();assert!(frame);
                let expected:Vec<_>=uu_media::av1::obus(source).unwrap().into_iter()
                    .filter(|obu| !matches!((obu[0]>>3)&15,2|15)).collect();
                assert_eq!(uu_media::av1::obus(&data).unwrap(),expected,"I/P OBU payload changed at MTU {mtu}");
                assert_eq!(uu_media::av1::is_keyframe(&data),uu_media::av1::is_keyframe(source));
                assert_eq!(uu_media::av1::sequence(&data),uu_media::av1::sequence(source));
            }
        }
    }
    #[test]
    fn missing_fragments_malformed_lengths_and_double_size_fields_are_rejected(){
        let mut a=Assembler::default();assert!(a.push(&[0x90,0x32,1]).is_err());
        assert!(a.push(&[0x10,0x32,9,1]).is_err()); // internal size exceeds element
        assert!(a.push(&[0x00,0x80]).is_err());
        let mut a=Assembler::default();a.push(&[0x50,0x30,1]).unwrap();assert!(a.finish().is_err());
        let mut a=Assembler::default();a.push(&[0x10,0x32,1,7]).unwrap();let (data,frame)=a.finish().unwrap();assert!(frame);assert_eq!(data,[0x32,1,7]);
    }
}
