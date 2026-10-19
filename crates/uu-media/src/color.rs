//! H264 VUI colour metadata. Legacy callers retain full-range BT.601.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum YuvColor { #[default] Full601, Limited601, Full709, Limited709 }
impl YuvColor {
    pub(crate) fn coefficients(self) -> [i32; 6] {
        match self {
            Self::Full601 => [0,65536,91881,22554,46802,116130],
            Self::Limited601 => [16,76309,104597,25675,53279,132201],
            Self::Full709 => [0,65536,103206,12276,30679,121609],
            Self::Limited709 => [16,76309,117489,13975,34925,138438],
        }
    }
    pub(crate) fn rgb(self,y:i32,u:i32,v:i32)->(i32,i32,i32) {
        if self==Self::Full601 {return (y+((91881*v)>>16), y-((22554*u+46802*v)>>16),y+((116130*u)>>16));}
        let [offset,m,r,gu,gv,b]=self.coefficients();let y=(y-offset)*m;
        ((y+r*v)>>16,(y-gu*u-gv*v)>>16,(y+b*u)>>16)
    }
}
pub fn from_h264(data:&[u8])->Option<YuvColor> {
    #[cfg(all(target_os="linux",feature="vaapi"))] {
        use cros_codecs::codec::h264::parser::{Nalu,NaluType,Parser};
        let mut cursor=std::io::Cursor::new(data);let mut parser=Parser::default();
        while let Ok(nal)=Nalu::next(&mut cursor) {
            if nal.header.type_==NaluType::Sps {
                let sps=parser.parse_sps(&nal).ok()?;let vui=&sps.vui_parameters;
                let full=sps.vui_parameters_present_flag && vui.video_signal_type_present_flag && vui.video_full_range_flag;
                let hd=sps.vui_parameters_present_flag && vui.colour_description_present_flag && vui.matrix_coefficients==1;
                return Some(match (full,hd){(true,false)=>YuvColor::Full601,(false,false)=>YuvColor::Limited601,(true,true)=>YuvColor::Full709,(false,true)=>YuvColor::Limited709});
            }
        }
    }
    let _=data;None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limited_black_white_and_709_red_match_reference_levels() {
        assert_eq!(YuvColor::Limited709.rgb(16,0,0),(0,0,0));
        let white=YuvColor::Limited709.rgb(235,0,0);assert!((white.0-255).abs()<=1);
        let red=YuvColor::Limited709.rgb(63,-26,112);
        assert!((red.0-255).abs()<=2 && red.1.abs()<=2 && red.2.abs()<=2);
        assert_eq!(YuvColor::Full601.rgb(16,0,0),(16,16,16));
    }
    #[test]
    fn colored_nv12_and_i420_match_scalar_with_stride_and_vector_tail() {
        use crate::{RawFrame,PixelFormat,Timestamp};
        for color in [YuvColor::Limited601,YuvColor::Limited709,YuvColor::Full709] {
            for nv12 in [true,false] {
                let (w,h,s)=(18usize,4usize,20usize);
                let len=if nv12{s*h*3/2}else{s*h+w*h/2};
                let data:Vec<u8>=(0..len).map(|i|((i*139+31)%256) as u8).collect();
                let f=RawFrame{width:w as u32,height:h as u32,stride:s as u32,format:if nv12{PixelFormat::Nv12}else{PixelFormat::I420},data:data.into(),pts:Timestamp(0),gpu:false};
                let actual=crate::convert::frame_to_rgba_with_color(&f,color);
                for y in 0..h {for x in 0..w {
                    let (u,v)=if nv12{let i=s*h+y/2*s+x/2*2;(f.data[i],f.data[i+1])}else{let i=y/2*(w/2)+x/2;(f.data[s*h+i],f.data[s*h+w*h/4+i])};
                    let (r,g,b)=color.rgb(f.data[y*s+x] as i32,u as i32-128,v as i32-128);
                    assert_eq!(&actual[(y*w+x)*4..(y*w+x+1)*4],&[r.clamp(0,255) as u8,g.clamp(0,255) as u8,b.clamp(0,255) as u8,255]);
                }}
            }
        }
    }
}
