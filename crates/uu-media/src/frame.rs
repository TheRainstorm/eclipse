//! Frame representations.

use bytes::Bytes;

use crate::Timestamp;

/// Pixel layouts carried by capture and decoder frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit BGRA, the DXGI/GPU-native layout on Windows (and GDI capture).
    Bgra8,
    /// Linear scRGB BT.709, IEEE half-float RGBA (1.0 = 80 cd/m²).
    ScRgb16,
    /// 10-bit 4:2:0, MSB aligned in 16-bit Y and interleaved UV samples.
    P010,
    /// Semi-planar YUV 4:2:0 (NV12); what VA-API/QSV and DXGI video produce.
    Nv12,
    /// Planar YUV 4:2:0 (I420); the layout OpenH264 consumes.
    I420,
}

impl PixelFormat {
    pub fn row_bytes(self,width:u32)->u32 {
        width*match self {Self::Bgra8=>4,Self::ScRgb16=>8,Self::P010=>2,_=>1}
    }
    fn packed(self)->bool {matches!(self,Self::Bgra8|Self::ScRgb16)}
}

/// A captured but not-yet-encoded frame.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Bytes per row of the *first* plane. Not `width * bpp` in general:
    /// capture APIs align rows to 64/256 bytes.
    pub stride: u32,
    pub data: Bytes,
    pub pts: Timestamp,
    /// Pixels reside in a typed GPU lease below; `data` is empty.
    /// Conversion and copies stay on the GPU until explicit readback.
    pub gpu: bool,
    /// Typed, leased NV12 GPU storage; CPU frames leave this empty.
    pub dma_buf: Option<std::sync::Arc<crate::dma_buf::Frame>>,
    /// Windows decoder sample lease; retains its texture-array slot.
    pub d3d11: Option<std::sync::Arc<crate::d3d11_frame::Frame>>,
}

impl RawFrame {
    /// Remove codec padding at the right/bottom using authenticated display
    /// geometry. GPU views retain their backing leases; CPU planes are repacked
    /// independently so chroma still starts after the original luma plane.
    pub(crate) fn crop_decoded(mut self, visible: [u32;2]) -> Result<Self,crate::CodecError> {
        let fail = ||crate::CodecError::Backend("decoded picture does not contain the negotiated display area".into());
        let [w,h]=visible;
        if w<2 || h<2 || w%2!=0 || h%2!=0 || w>self.width || h>self.height
            || self.width>8192 || self.height>8192 || !self.is_consistent() {return Err(fail());}
        if (w,h)==(self.width,self.height) {return Ok(self);}
        if self.gpu {
            if let Some(frame)=&self.d3d11 {
                self.d3d11=Some(std::sync::Arc::new(crate::d3d11_frame::Frame::cropped_view(frame,visible)
                    .map_err(crate::CodecError::Backend)?));
            } else if !self.dma_buf.as_ref().is_some_and(|frame|frame.valid(visible)) {return Err(fail());}
        } else {
            let bpp=self.format.row_bytes(1);
            if self.stride < self.width*bpp {return Err(fail());}
            let (w,h,stride,old_h)=(w as usize,h as usize,self.stride as usize,self.height as usize);
            let mut data=vec![0; if self.format.packed() {w*h*bpp as usize}else{w*h*bpp as usize*3/2}];
            let mut rows=|base:usize,pitch:usize,dst:usize,row_bytes:usize,count:usize|->Result<(),crate::CodecError> {
                for row in 0..count {
                    let start=base+row*pitch;
                    let source=self.data.get(start..start+row_bytes).ok_or_else(fail)?;
                    data[dst+row*row_bytes..dst+(row+1)*row_bytes].copy_from_slice(source);
                }
                Ok(())
            };
            rows(0,stride,0,w*bpp as usize,h)?;
            match self.format {
                PixelFormat::Bgra8|PixelFormat::ScRgb16=>{},
                PixelFormat::Nv12|PixelFormat::P010=>rows(stride*old_h,stride,w*h*bpp as usize,w*bpp as usize,h/2)?,
                PixelFormat::I420=>{
                    let chroma=stride.div_ceil(2);
                    rows(stride*old_h,chroma,w*h,w/2,h/2)?;
                    rows(stride*old_h+chroma*old_h.div_ceil(2),chroma,w*h+w*h/4,w/2,h/2)?;
                }
            }
            self.data=data.into();self.stride=w as u32*bpp;
        }
        self.width=w;self.height=h;
        Ok(self)
    }
    /// A CPU-side I420 frame, the common currency of the encoder path.
    pub fn new_i420(width: u32, height: u32, data: Vec<u8>, pts: Timestamp) -> Self {
        Self {
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data: Bytes::from(data),
            pts,
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }
    }

    /// Tightly packed byte length for the frame's format.
    pub fn expected_len(&self) -> usize {
        let stride = self.stride as usize;
        let height = self.height as usize;
        match self.format {
            PixelFormat::Bgra8 | PixelFormat::ScRgb16 => stride * height,
            // Both 4:2:0 layouts are luma + two half-resolution chroma planes.
            PixelFormat::Nv12 | PixelFormat::I420 | PixelFormat::P010 => stride * height * 3 / 2,
        }
    }

    /// Validate that the buffer can actually hold a full frame.
    pub fn is_consistent(&self) -> bool {
        if self.gpu {
            self.dma_buf.as_ref().is_some_and(|dma|dma.valid([self.width,self.height]))
                || self.d3d11.as_ref().is_some_and(|frame|frame.valid([self.width,self.height]))
        } else {self.data.len() >= self.expected_len()}
    }
}

/// One encoded access unit (e.g. an H.264 NAL chain).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub pts: Timestamp,
    pub is_keyframe: bool,
}

impl EncodedFrame {
    /// H.264 start-code scan used to tag IDR frames.
    ///
    /// The controller uses `is_keyframe` to decide whether it can join
    /// immediately or must wait for the next IDR.
    pub fn detect_h264_keyframe(data: &[u8]) -> bool {
        let mut i = 0usize;
        while i + 3 < data.len() {
            let sc = if data[i..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[i..].starts_with(&[0, 0, 1]) {
                3
            } else {
                i += 1;
                continue;
            };
            let header_at = i + sc;
            if header_at < data.len() {
                let nal_type = data[header_at] & 0x1f;
                // SPS/PPS alone cannot repair a broken reference chain. Only
                // an IDR is a safe join point after packet loss or queue drops.
                if nal_type == 5 {
                    return true;
                }
            }
            i = header_at;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_keeps_chroma_at_original_plane_offsets_and_preserves_timestamp() {
        for format in [PixelFormat::Bgra8,PixelFormat::Nv12,PixelFormat::I420] {
            let stride=if format==PixelFormat::Bgra8 {32}else{8};
            let len=if format==PixelFormat::Bgra8 {stride*6}else{stride*9};
            let original=RawFrame {width:6,height:6,format,stride:stride as u32,
                data:Bytes::from((0..len).map(|n|n as u8).collect::<Vec<_>>()),
                pts:Timestamp(12345),gpu:false,dma_buf:None,d3d11:None};
            let cropped=original.clone().crop_decoded([4,4]).unwrap();
            assert_eq!((cropped.width,cropped.height,cropped.pts),(4,4,Timestamp(12345)));
            let mut expected=Vec::new();
            let row=if format==PixelFormat::Bgra8 {16}else{4};
            for y in 0..4 {expected.extend_from_slice(&original.data[y*stride..y*stride+row]);}
            match format {
                PixelFormat::Bgra8=>{},
                PixelFormat::Nv12=>expected.extend_from_slice(&[48,49,50,51,56,57,58,59]),
                PixelFormat::I420=>expected.extend_from_slice(&[48,49,52,53,60,61,64,65]),
                _=>unreachable!(),
            }
            assert_eq!(cropped.data.as_ref(),expected);
            assert_eq!(cropped.stride,row as u32);
            assert_eq!(original.data.len(),len);
            assert!(cropped.is_consistent());
            for invalid in [[0,4],[3,4],[8,4],[4,8]] {assert!(original.clone().crop_decoded(invalid).is_err());}
            let mut truncated=original;truncated.data=Bytes::from_static(&[0;8]);
            assert!(truncated.crop_decoded([4,4]).is_err());
        }
    }

    #[test]
    fn nv12_size_is_one_and_a_half_planes() {
        let f = RawFrame {
            width: 4,
            height: 4,
            format: PixelFormat::Nv12,
            stride: 4,
            data: Bytes::from(vec![0u8; 4 * 4 * 3 / 2]),
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        };
        assert_eq!(f.expected_len(), 24);
        assert!(f.is_consistent());
    }

    #[test]
    fn idr_detection_finds_nal_type_5() {
        // Annex-B: 00 00 00 01 then NAL header with type 5 (IDR).
        let data = [0u8, 0, 0, 1, 0x65, 0x88, 0x84];
        assert!(EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn idr_detection_rejects_non_idr() {
        // NAL type 1 = non-IDR slice.
        let data = [0u8, 0, 0, 1, 0x41, 0x9a, 0x11];
        assert!(!EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn three_byte_start_code_is_honoured() {
        let data = [0u8, 0, 1, 0x65, 0x42]; // type 5 = IDR
        assert!(EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn parameter_sets_are_not_reference_recovery_points() {
        assert!(!EncodedFrame::detect_h264_keyframe(&[0,0,1,0x67,0x42,0,0,1,0x68,0xce]));
    }
}

#[cfg(test)] mod hdr_layout_tests {
    use super::*;
    #[test] fn p010_crop_preserves_word_alignment_and_original_chroma_offset() {
        let original=RawFrame{width:6,height:6,format:PixelFormat::P010,stride:16,data:(0..144).map(|n|n as u8).collect::<Vec<_>>().into(),pts:Timestamp(123),gpu:false,dma_buf:None,d3d11:None};
        let cropped=original.clone().crop_decoded([4,4]).unwrap();
        let mut expected=Vec::new();for row in 0..4{expected.extend_from_slice(&original.data[row*16..row*16+8]);}
        for row in 0..2{expected.extend_from_slice(&original.data[96+row*16..96+row*16+8]);}
        assert_eq!(cropped.stride,8);assert_eq!(cropped.data.as_ref(),expected);assert_eq!(cropped.pts,Timestamp(123));assert!(cropped.is_consistent());
    }
}
