// Copyright 2026 Eclipse contributors.
// Use of this source code is governed by the BSD-style license in LICENSE.
#![cfg(all(feature = "vaapi", target_os = "linux"))]

use std::ffi::CString;
use std::fs::File;
use std::sync::{mpsc, Arc};
use std::thread;

use cros_codecs::decoder::StreamInfo;
use cros_codecs::video_frame::frame_pool::FramePool;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::{DecodedFormat, Fourcc, FrameLayout, PlaneLayout};
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};

#[test]
fn held_backing_does_not_return_to_a_resized_pool() {
    let info = StreamInfo {
        format: DecodedFormat::NV12,
        coded_resolution: (64, 64).into(),
        display_resolution: (64, 64).into(),
        min_num_frames: 2,
    };
    let mut allocation = 0;
    let mut pool = FramePool::new(move |stream| {
        allocation += 1;
        let size = stream.coded_resolution;
        let stride = size.width as usize;
        let luma_len = stride * size.height as usize;
        // Distinct padding identifies the backing storage even if fd numbers are reused.
        let fd = File::from(
            memfd_create(&CString::new("frame-pool-test").unwrap(), MemFdCreateFlag::MFD_CLOEXEC)
                .unwrap(),
        );
        fd.set_len((luma_len + luma_len / 2 + allocation) as u64).unwrap();
        let layout = FrameLayout {
            format: (Fourcc::from(b"NV12"), 0),
            size,
            planes: vec![
                PlaneLayout { buffer_index: 0, offset: 0, stride },
                PlaneLayout { buffer_index: 0, offset: luma_len, stride },
            ],
        };
        GenericDmaVideoFrame::new(vec![fd], layout).unwrap()
    });
    pool.resize(&info);
    assert!(pool.alloc().unwrap().va_pool_slot().is_none());

    pool.resize_for_va_surface_reuse(&info);
    let retained = Arc::new(pool.alloc().unwrap());
    assert!(retained.va_pool_slot().is_some());
    let retained_len = retained.get_plane_size()[1];
    let (release, wait) = mpsc::channel();
    let consumer = thread::spawn(move || {
        wait.recv().unwrap();
        assert_eq!(retained.get_plane_size()[1], retained_len);
    });
    let spare_len = pool.alloc().unwrap().get_plane_size()[1];
    assert_ne!(spare_len, retained_len);
    for _ in 0..64 {
        let spare = pool.alloc().unwrap();
        assert_eq!(spare.get_plane_size()[1], spare_len);
        assert!(pool.alloc().is_none());
    }

    pool.resize_for_va_surface_reuse(&info);
    let first = pool.alloc().unwrap();
    let second = pool.alloc().unwrap();
    let first_len = first.get_plane_size()[1];
    for len in [first_len, second.get_plane_size()[1]] {
        assert!(len > retained_len.max(spare_len));
    }
    release.send(()).unwrap();
    consumer.join().unwrap();
    assert!(pool.alloc().is_none());
    drop(first);
    assert_eq!(pool.alloc().unwrap().get_plane_size()[1], first_len);
    pool.resize(&info);
    assert!(pool.alloc().unwrap().va_pool_slot().is_none());
}
