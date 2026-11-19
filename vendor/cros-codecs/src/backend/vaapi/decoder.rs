// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context as AnyhowContext;
use libva::{
    Buffer, Context, Display, Picture, PictureEnd, PictureNew, PictureSync, Surface,
    SurfaceMemoryDescriptor, VaError,
};

use crate::decoder::stateless::StatelessBackendResult;
use crate::decoder::stateless::StatelessCodec;
use crate::decoder::stateless::StatelessDecoderBackend;
use crate::decoder::stateless::StatelessDecoderBackendPicture;
use crate::decoder::DecodedHandle as DecodedHandleTrait;
use crate::decoder::StreamInfo;
use crate::video_frame::VideoFrame;
use crate::DecodedFormat;
use crate::Rect;
use crate::Resolution;

/// A decoded frame handle.
pub(crate) type DecodedHandle<V> = Rc<RefCell<VaapiDecodedHandle<V>>>;

/// Gets the VASurfaceID for the given `picture`.
pub(crate) fn va_surface_id<V: VideoFrame>(
    handle: &Option<DecodedHandle<V>>,
) -> libva::VASurfaceID {
    match handle {
        None => libva::VA_INVALID_SURFACE,
        Some(handle) => handle.borrow().surface().id(),
    }
}

impl<V: VideoFrame> DecodedHandleTrait for DecodedHandle<V> {
    type Frame = V;

    fn video_frame(&self) -> Arc<Self::Frame> {
        self.borrow().backing_frame.clone()
    }

    fn coded_resolution(&self) -> Resolution {
        self.borrow().surface().size().into()
    }

    fn display_resolution(&self) -> Resolution {
        self.borrow().visible_rect.into()
    }

    fn timestamp(&self) -> u64 {
        self.borrow().timestamp()
    }

    fn is_ready(&self) -> bool {
        self.borrow().state.is_ready().unwrap_or(true)
    }

    fn sync(&self) -> anyhow::Result<()> {
        self.borrow_mut().sync().context("while syncing picture")?;

        Ok(())
    }
}

/// A trait for providing the basic information needed to setup libva for decoding.
pub(crate) trait VaStreamInfo {
    /// Returns the VA profile of the stream.
    fn va_profile(&self) -> anyhow::Result<i32>;
    /// Returns the RT format of the stream.
    fn rt_format(&self) -> anyhow::Result<u32>;
    /// Returns the minimum number of surfaces required to decode the stream.
    fn min_num_surfaces(&self) -> usize;
    /// Returns the coded size of the surfaces required to decode the stream.
    fn coded_size(&self) -> Resolution;
    /// Returns the visible rectangle within the coded size for the stream.
    fn visible_rect(&self) -> Rect;
}

/// Rendering state of a VA picture.
enum PictureState<M: SurfaceMemoryDescriptor> {
    Ready(Picture<PictureSync, Surface<M>>),
    Pending(Picture<PictureEnd, Surface<M>>),
    // Only set in sync when we take ownership of the VA picture.
    Invalid,
}

impl<M: SurfaceMemoryDescriptor> PictureState<M> {
    /// Make sure that all pending operations on the picture have completed.
    fn sync(&mut self) -> Result<(), VaError> {
        let res;

        (*self, res) = match std::mem::replace(self, PictureState::Invalid) {
            state @ PictureState::Ready(_) => (state, Ok(())),
            PictureState::Pending(picture) => match picture.sync() {
                Ok(picture) => (PictureState::Ready(picture), Ok(())),
                Err((e, picture)) => (PictureState::Pending(picture), Err(e)),
            },
            PictureState::Invalid => unreachable!(),
        };

        res
    }

    fn surface(&self) -> &Surface<M> {
        match self {
            PictureState::Ready(picture) => picture.surface(),
            PictureState::Pending(picture) => picture.surface(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn timestamp(&self) -> u64 {
        match self {
            PictureState::Ready(picture) => picture.timestamp(),
            PictureState::Pending(picture) => picture.timestamp(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn is_ready(&self) -> Result<bool, VaError> {
        match self {
            PictureState::Ready(_) => Ok(true),
            PictureState::Pending(picture) => picture
                .surface()
                .query_status()
                .map(|s| s == libva::VASurfaceStatus::VASurfaceReady),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn new_from_same_surface(&self, timestamp: u64) -> Picture<PictureNew, Surface<M>> {
        match &self {
            PictureState::Ready(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Pending(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Invalid => unreachable!(),
        }
    }
}

/// VA-API backend handle.
///
/// This includes the VA picture which can be pending rendering or complete, as well as useful
/// meta-information.
pub struct VaapiDecodedHandle<V: VideoFrame> {
    backing_frame: Arc<V>,
    state: PictureState<<V as VideoFrame>::MemDescriptor>,
    /// Crop frozen with this decoded picture, including its luma origin.
    visible_rect: Rect,
}

impl<V: VideoFrame> VaapiDecodedHandle<V> {
    /// Creates a new pending handle on `surface_id`.
    fn new(picture: VaapiPicture<V>, visible_rect: Rect) -> anyhow::Result<Self> {
        let backing_frame = picture.backing_frame;
        let picture = picture.picture.begin()?.render()?.end()?;
        Ok(Self {
            backing_frame: backing_frame,
            state: PictureState::Pending(picture),
            visible_rect,
        })
    }

    fn sync(&mut self) -> Result<(), VaError> {
        self.state.sync()
    }

    /// Creates a new picture from the surface backing the current one. Useful for interlaced
    /// decoding. TODO: Do we need this for other purposes? We don't intend to support interlaced.
    pub(crate) fn new_picture_from_same_surface(&self, timestamp: u64) -> VaapiPicture<V> {
        VaapiPicture {
            picture: self.state.new_from_same_surface(timestamp),
            backing_frame: self.backing_frame.clone(),
        }
    }

    pub fn visible_rect(&self) -> Rect {
        self.visible_rect
    }

    /// Borrow the actual decoded surface for readback/export without importing
    /// its DMA allocation into a second VA surface. The handle retains its
    /// backing frame and decoder reference lifetime throughout this borrow.
    pub fn surface(&self) -> &Surface<<V as VideoFrame>::MemDescriptor> {
        self.state.surface()
    }

    /// Returns the timestamp of this handle.
    fn timestamp(&self) -> u64 {
        self.state.timestamp()
    }
}

pub struct VaapiBackend<V: VideoFrame> {
    pub display: Rc<Display>,
    pub context: Rc<Context>,
    stream_info: StreamInfo,
    visible_rect: Rect,
    // TODO: We should try to support context reuse
    _supports_context_reuse: bool,
    // These never-submitted pictures own only the VA import and context, not
    // the backing-frame lease. They are used exclusively on the decode thread.
    surface_generation: std::sync::Weak<()>,
    surface_templates: HashMap<usize, Picture<PictureNew, Surface<V::MemDescriptor>>>,
}

impl<V: VideoFrame> VaapiBackend<V> {
    pub(crate) fn new(display: Rc<libva::Display>, supports_context_reuse: bool) -> Self {
        let init_stream_info = StreamInfo {
            format: DecodedFormat::NV12,
            coded_resolution: Resolution::from((16, 16)),
            display_resolution: Resolution::from((16, 16)),
            min_num_frames: 1,
        };
        let config = display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: libva::VA_RT_FORMAT_YUV420,
                }],
                libva::VAProfile::VAProfileH264Main,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .expect("Could not create initial VAConfig!");
        let context = display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                init_stream_info.coded_resolution.width,
                init_stream_info.coded_resolution.height,
                None,
                true,
            )
            .expect("Could not create initial VAContext!");
        Self {
            display: display,
            context: context,
            _supports_context_reuse: supports_context_reuse,
            visible_rect: init_stream_info.display_resolution.into(),
            stream_info: init_stream_info,
            surface_generation: std::sync::Weak::new(),
            surface_templates: HashMap::new(),
        }
    }

    pub(crate) fn new_sequence<StreamData>(
        &mut self,
        stream_params: &StreamData,
    ) -> StatelessBackendResult<()>
    where
        for<'a> &'a StreamData: VaStreamInfo,
    {
        let rt_format = stream_params.rt_format()?;
        let format = match rt_format {
            libva::VA_RT_FORMAT_YUV420 => DecodedFormat::NV12,
            libva::VA_RT_FORMAT_YUV420_10 => DecodedFormat::P010,
            other => return Err(anyhow!("Unsupported VA render target format {other:#x}").into()),
        };
        let visible = stream_params.visible_rect();
        let coded = stream_params.coded_size();
        if visible.x % 2 != 0 || visible.y % 2 != 0 ||
             visible.width <= visible.x || visible.height <= visible.y ||
             visible.width > coded.width || visible.height > coded.height
        {
            return Err(anyhow!("Unsupported decoded visible rectangle {visible:?} in {coded:?}").into());
        }
        self.visible_rect = visible;
        self.stream_info.format = format;
        self.stream_info.display_resolution = Resolution::from(visible);
        self.stream_info.coded_resolution = stream_params.coded_size().clone();
        self.stream_info.min_num_frames = stream_params.min_num_surfaces();

        // TODO: Handle context re-use
        let config = self
            .display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: rt_format,
                }],
                stream_params.va_profile()?,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .map_err(|_| anyhow!("Could not create VAConfig!"))?;
        let context = self
            .display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                self.stream_info.coded_resolution.width,
                self.stream_info.coded_resolution.height,
                None,
                true,
            )
            .map_err(|_| anyhow!("Could not create VAContext!"))?;
        // Pictures still referenced by the old DPB retain their own surface
        // and context; never submit them through the new sequence.
        self.surface_templates.clear();
        self.context = context;

        Ok(())
    }

    pub(crate) fn acquire_picture(
        &mut self,
        timestamp: u64,
        frame: V,
    ) -> anyhow::Result<VaapiPicture<V>> {
        let import = || {
            frame
                .to_native_handle(self.context.display())
                .map(Into::into)
                .map_err(|error| anyhow!("Failed to import video frame into VA: {error}"))
        };
        let picture = if let Some(slot) = frame.va_pool_slot() {
            if !self.surface_generation.ptr_eq(&slot.generation) {
                self.surface_templates.clear();
                self.surface_generation = slot.generation;
            }
            let template = match self.surface_templates.entry(slot.index) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let template = entry.into_mut();
                    // Normally blocking decoding and the EGL consumer have
                    // already completed before the pool returns this slot.
                    // Also handle discarded or asynchronously decoded pictures.
                    let surface = template.surface();
                    if !matches!(surface.query_status(), Ok(libva::VASurfaceStatus::VASurfaceReady)) {
                        // Some drivers implement synchronization but not the optional
                        // status query. Sync the same surface before the next write.
                        surface.sync().context("confirming VA surface is reusable")?;
                    }
                    template
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Picture::new(0, self.context.clone(), import()?))
                }
            };
            Picture::new_from_same_surface(timestamp, &*template)
        } else {
            Picture::new(timestamp, self.context.clone(), import()?)
        };
        Ok(VaapiPicture { picture, backing_frame: Arc::new(frame) })
    }

    pub(crate) fn process_picture<Codec: StatelessCodec>(
        &mut self,
        picture: VaapiPicture<V>,
    ) -> StatelessBackendResult<<Self as StatelessDecoderBackend>::Handle>
    where
        Self: StatelessDecoderBackendPicture<Codec>,
        for<'a> &'a Codec::FormatInfo: VaStreamInfo,
    {
        Ok(Rc::new(RefCell::new(VaapiDecodedHandle::new(
            picture,
            self.visible_rect,
        )?)))
    }
}

/// Shortcut for pictures used for the VAAPI backend.
pub struct VaapiPicture<V: VideoFrame> {
    picture: Picture<PictureNew, Surface<V::MemDescriptor>>,
    backing_frame: Arc<V>,
}

impl<V: VideoFrame> VaapiPicture<V> {
    pub fn new(timestamp: u64, context: Rc<Context>, backing_frame: V) -> Self {
        let display = context.display();
        let surface = backing_frame
            .to_native_handle(display)
            .expect("Failed to export video frame to vaapi picture!")
            .into();
        Self {
            backing_frame: Arc::new(backing_frame),
            picture: Picture::new(timestamp, context, surface),
        }
    }

    pub fn surface(&self) -> &Surface<V::MemDescriptor> {
        self.picture.surface()
    }

    pub fn add_buffer(&mut self, buffer: Buffer) {
        self.picture.add_buffer(buffer)
    }
}

impl<V: VideoFrame> StatelessDecoderBackend for VaapiBackend<V> {
    type Handle = DecodedHandle<V>;

    fn stream_info(&self) -> Option<&StreamInfo> {
        Some(&self.stream_info)
    }

    fn reset_backend(&mut self) -> anyhow::Result<()> {
        //TODO(bchoobineh): Implement VAAPI DRC
        Ok(())
    }
}
