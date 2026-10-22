use super::FrameSlot;
use crate::pixels::{bgra_rows, Rotation};
use pipewire::{self as pw, properties::properties, spa};
use spa::{
    param::{
        format::FormatProperties as Prop,
        video::{VideoFormat, VideoInfoRaw},
    },
    pod::Pod,
};
use std::{
    os::fd::OwnedFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use uu_media::{
    frame::{PixelFormat, RawFrame},
    Timestamp,
};

struct State {
    format: VideoInfoRaw,
    frames: Arc<FrameSlot>,
}

/// All PipeWire resources and callbacks are created and destroyed on this thread.
pub(super) fn run(
    fd: OwnedFd,
    node: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<FrameSlot>,
) -> Result<(), String> {
    run_inner(fd, node, fps, stop, frames).map_err(|e| format!("PipeWire: {e}"))
}
fn run_inner(
    fd: OwnedFd,
    node: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<FrameSlot>,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "Eclipse screen sharing",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(State {
            format: VideoInfoRaw::default(),
            frames,
        })
        .state_changed(|_, state, _, next| {
            if let pw::stream::StreamState::Error(message) = next {
                state.frames.fail(message);
            }
        })
        .param_changed(|stream, state, id, pod| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(pod) = pod {
                if state.format.parse(pod).is_err() {
                    state.frames.fail("无法解析共享画面格式");
                    return;
                }
                // This backend produces CPU pixels. Do not let a compositor
                // negotiate tiled DMA-BUF storage and mistake it for rows.
                let memory = (1u32 << spa::buffer::DataType::MemFd.as_raw())
                    | (1u32 << spa::buffer::DataType::MemPtr.as_raw());
                let buffers = spa::pod::object!(
                    spa::utils::SpaTypes::ObjectParamBuffers,
                    spa::param::ParamType::Buffers,
                    spa::pod::Property::new(
                        spa::sys::SPA_PARAM_BUFFERS_dataType,
                        spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                            spa::utils::ChoiceFlags::empty(),
                            spa::utils::ChoiceEnum::Flags {
                                default: memory as i32,
                                flags: vec![]
                            }
                        )))
                    )
                );
                let serialized = spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &spa::pod::Value::Object(buffers),
                );
                match serialized {
                    Ok((bytes, _)) => {
                        let bytes = bytes.into_inner();
                        if let Some(pod) = Pod::from_bytes(&bytes) {
                            if stream.update_params(&mut [pod]).is_err() {
                                state.frames.fail("无法协商共享画面的内存格式");
                            }
                        }
                    }
                    Err(_) => state.frames.fail("无法描述共享画面的内存格式"),
                }
            }
        })
        .process(|stream, state| {
            // Bound the drain by the server's finite buffer pool. Return older
            // buffers before copying so bursts cannot turn into display delay.
            let mut newest = stream.dequeue_buffer();
            for _ in 0..32 {
                let Some(next) = stream.dequeue_buffer() else {
                    break;
                };
                newest = Some(next);
            }
            let Some(mut buffer) = newest else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            if ![spa::buffer::DataType::MemFd, spa::buffer::DataType::MemPtr]
                .contains(&data.type_())
            {
                state.frames.fail("系统返回了未经协商的共享画面内存格式");
                return;
            }
            let (offset, size, stride) = (
                data.chunk().offset() as usize,
                data.chunk().size() as usize,
                data.chunk().stride(),
            );
            if size == 0 {
                return;
            } // No damage on a static desktop.
            let Some(bytes) = data.data() else {
                state.frames.fail("系统未提供可映射的共享画面");
                return;
            };
            let result = copy_frame(
                bytes,
                offset,
                size,
                stride,
                state.format.size().width,
                state.format.size().height,
                state.format.format(),
            );
            match result {
                Ok(frame) => state.frames.publish(frame),
                Err(e) => state.frames.fail(e),
            }
        })
        .register()?;
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(Prop::MediaType, Id, spa::param::format::MediaType::Video),
        spa::pod::property!(
            Prop::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            Prop::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::RGB,
            VideoFormat::BGR
        ),
        spa::pod::property!(
            Prop::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: fps, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: fps, denom: 1 }
        )
    );
    let serialized = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&serialized).ok_or("Invalid PipeWire format specification")?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    while !stop.load(Ordering::Acquire) {
        if mainloop.loop_().iterate(Duration::from_millis(30)) < 0 {
            return Err("共享连接已关闭".into());
        }
    }
    stream.disconnect()?;
    Ok(())
}

fn copy_frame(
    bytes: &[u8],
    offset: usize,
    length: usize,
    stride: i32,
    width: u32,
    height: u32,
    format: VideoFormat,
) -> Result<RawFrame, String> {
    let packed_rgb = matches!(format, VideoFormat::RGB | VideoFormat::BGR);
    if stride <= 0
        || ![
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::RGB,
            VideoFormat::BGR,
        ]
        .contains(&format)
    {
        return Err("共享画面的像素格式或行跨度不受支持".into());
    }
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or("共享画面缓冲区越界")?;
    let mut data = if packed_rgb {
        let row = (width as usize).checked_mul(3).ok_or("共享画面尺寸溢出")?;
        let total = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .filter(|v| *v <= 256 * 1024 * 1024)
            .ok_or("共享画面尺寸超限")?;
        let needed = (height as usize)
            .checked_sub(1)
            .and_then(|v| v.checked_mul(stride as usize))
            .and_then(|v| v.checked_add(row))
            .ok_or("共享画面尺寸无效")?;
        if width == 0 || (stride as usize) < row || end - offset < needed {
            return Err("共享画面缓冲区不完整".into());
        }
        let mut data = vec![255; total];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let source = offset + y * stride as usize + x * 3;
                let dest = (y * width as usize + x) * 4;
                data[dest..dest + 3].copy_from_slice(&bytes[source..source + 3]);
            }
        }
        data
    } else {
        bgra_rows(
            &bytes[offset..end],
            width as usize,
            height as usize,
            stride as usize,
            Rotation::None,
        )
        .map_err(|_| "共享画面缓冲区不完整")?
        .2
    };
    if matches!(
        format,
        VideoFormat::RGBx | VideoFormat::RGBA | VideoFormat::RGB
    ) {
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    Ok(RawFrame {
        width,
        height,
        format: PixelFormat::Bgra8,
        stride: width * 4,
        data: data.into(),
        pts: Timestamp(0),
        gpu: false,
        dma_buf: None,
        d3d11: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_pixels_validate_chunk_bounds_padding_and_color_order() {
        let bytes = [
            99, 99, 1, 2, 3, 0, 4, 5, 6, 0, 88, 88, 7, 8, 9, 0, 10, 11, 12, 0,
        ];
        let bgr = copy_frame(&bytes, 2, 18, 10, 2, 2, VideoFormat::BGRx).unwrap();
        assert_eq!(
            &bgr.data[..],
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        let rgb = copy_frame(&bytes, 2, 18, 10, 2, 2, VideoFormat::RGBA).unwrap();
        assert_eq!(&rgb.data[..4], &[3, 2, 1, 255]);
        let rgb24 = copy_frame(
            &[1, 2, 3, 4, 5, 6, 99, 99, 7, 8, 9, 10, 11, 12],
            0,
            14,
            8,
            2,
            2,
            VideoFormat::RGB,
        )
        .unwrap();
        assert_eq!(
            &rgb24.data[..],
            &[3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
        assert!(copy_frame(&bytes, 3, 18, 10, 2, 2, VideoFormat::BGRx).is_err());
        assert!(copy_frame(&bytes, 2, 17, 10, 2, 2, VideoFormat::BGRx).is_err());
        assert!(copy_frame(&bytes, 2, 18, -10, 2, 2, VideoFormat::BGRx).is_err());
        assert!(copy_frame(&bytes, 2, 18, 10, u32::MAX, 2, VideoFormat::BGRx).is_err());
    }
}
