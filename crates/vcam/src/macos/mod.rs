//! CoreMediaIO camera-extension virtual cameras (macOS).
//!
//! The cameras are a Swift system extension embedded in the app bundle
//! (`extensions/oak-camera-extension`, built by `scripts/vcam/build-macos.sh`).
//! Once the user activates it, macOS lists one device per [`CameraId`]
//! ("OAK RGB", "OAK Left", "OAK Right"), each with a **source** stream
//! consumers read and a **sink** stream this module writes. The handoff is the
//! CoreMediaIO client API: [`MacCamera::start`] finds the device by that
//! camera's pinned device UUID ([`CameraId::uuids`]), copies the sink stream's
//! `CMSimpleQueue` and starts the stream; each [`publish`](MacCamera::publish)
//! writes the frame straight into a locked IOSurface-backed `CVPixelBuffer`
//! from a per-camera pool of six, wraps it in a `CMSampleBuffer` stamped with
//! host time and enqueues it. CoreMediaIO ships the IOSurface to the extension
//! process without copying pixels, and the extension relays it to every
//! consumer. When the pool or the sink queue is full the frame is dropped
//! (latest wins, never blocks).
//!
//! Nothing here activates the extension: that is [`install`] (an
//! `OSSystemExtensionRequest` from the signed app bundle) plus the user's
//! approval in System Settings. [`status`] reports which camera devices exist.
//!
//! Ported from stream-concern's `sc-interchange/src/vcam/macos.rs`; the
//! compositor frame types and the UYVY/BGRA pool negotiation were dropped
//! (oak-cam always publishes NV12).

#![allow(unsafe_code)]

pub mod install;

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMClock, CMSampleBuffer, CMSampleTimingInfo, CMSimpleQueue, CMTime, CMTimeFlags,
    CMVideoFormatDescription, CMVideoFormatDescriptionCreateForImageBuffer,
    kCMSimpleQueueError_QueueIsFull,
};
use objc2_core_video::{
    CVAttachmentMode, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferColorPrimariesKey,
    kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferTransferFunctionKey,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVImageBufferYCbCrMatrixKey, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferPoolAllocationThresholdKey, kCVPixelBufferWidthKey,
    kCVReturnPoolAllocationFailed, kCVReturnWouldExceedAllocationThreshold,
};

use crate::{
    CameraFormat, CameraId, DropReason, FrameSrc, Nv12Dest, Publish, VcamError, VcamStats,
    VirtualCamera, write_nv12,
};

/// The camera extension's bundle identifier: the id the app activates through
/// `OSSystemExtensionRequest` and the bundle under
/// `Contents/Library/SystemExtensions/` in the app.
pub const EXTENSION_BUNDLE_ID: &str = "com.surlysoft.oak-cam.camera-extension";

/// The app's bundle identifier (the extension's container).
pub const APP_BUNDLE_ID: &str = "com.surlysoft.oak-cam";

/// Where `scripts/vcam/build-macos.sh` installs the app. System extensions
/// only activate from an app in `/Applications` (unless developer mode is on).
pub const APP_INSTALL_PATH: &str = "/Applications/oak-cam.app";

/// Where the user approves the extension (macOS 15 and later).
pub const SETTINGS_PATH: &str =
    "System Settings › General › Login Items & Extensions › Camera Extensions";

/// IOSurface-backed pixel buffers each camera's pool may hold before a publish
/// is dropped rather than allocating another: the sink queue depth (3) plus
/// the frame being filled plus the extension's held reference, with slack.
pub const POOL_DEPTH: i32 = 6;

/// Thin bindings over the CoreMediaIO client (DAL) API. Only the handful of
/// calls the sink handoff needs; the property selectors are the documented
/// four-character codes from `CMIOHardware*.h`.
mod cmio {
    use std::ffi::c_void;

    use objc2_core_media::CMSimpleQueue;

    pub type ObjectId = u32;

    #[repr(C)]
    pub struct PropertyAddress {
        pub selector: u32,
        pub scope: u32,
        pub element: u32,
    }

    pub const SYSTEM_OBJECT: ObjectId = 1;
    pub const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    pub const ELEMENT_MAIN: u32 = 0;
    pub const HARDWARE_PROPERTY_DEVICES: u32 = u32::from_be_bytes(*b"dev#");
    pub const OBJECT_PROPERTY_NAME: u32 = u32::from_be_bytes(*b"lnam");
    pub const DEVICE_PROPERTY_UID: u32 = u32::from_be_bytes(*b"uid ");
    pub const DEVICE_PROPERTY_STREAMS: u32 = u32::from_be_bytes(*b"stm#");
    pub const STREAM_PROPERTY_DIRECTION: u32 = u32::from_be_bytes(*b"sdir");
    /// `kCMIOStreamPropertyDirection`: 0 is an output stream (the client writes
    /// into the device: the sink), 1 an input stream (the client reads: the
    /// camera picture).
    pub const STREAM_DIRECTION_OUTPUT: u32 = 0;

    pub type QueueAlteredProc = Option<
        unsafe extern "C-unwind" fn(stream: ObjectId, token: *mut c_void, refcon: *mut c_void),
    >;

    #[link(name = "CoreMediaIO", kind = "framework")]
    unsafe extern "C-unwind" {
        pub fn CMIOObjectGetPropertyDataSize(
            object: ObjectId,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        pub fn CMIOObjectGetPropertyData(
            object: ObjectId,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            used: *mut u32,
            data: *mut c_void,
        ) -> i32;
        pub fn CMIOStreamCopyBufferQueue(
            stream: ObjectId,
            altered: QueueAlteredProc,
            refcon: *mut c_void,
            queue: *mut *mut CMSimpleQueue,
        ) -> i32;
        pub fn CMIODeviceStartStream(device: ObjectId, stream: ObjectId) -> i32;
        pub fn CMIODeviceStopStream(device: ObjectId, stream: ObjectId) -> i32;
    }

    pub const fn global(selector: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        }
    }
}

/// Read a property whose value is an array of `u32` object ids.
fn property_ids(object: cmio::ObjectId, selector: u32) -> Vec<u32> {
    let address = cmio::global(selector);
    let mut size = 0u32;
    // SAFETY: `address` and `size` are valid for the duration of the call; the
    // qualifier is unused (null, zero length).
    let rc = unsafe {
        cmio::CMIOObjectGetPropertyDataSize(
            object,
            &raw const address,
            0,
            ptr::null(),
            &raw mut size,
        )
    };
    if rc != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0u32; (size as usize).div_ceil(4)];
    let mut used = 0u32;
    // SAFETY: `ids` holds at least `size` bytes; CoreMediaIO writes `used` of them.
    let rc = unsafe {
        cmio::CMIOObjectGetPropertyData(
            object,
            &raw const address,
            0,
            ptr::null(),
            size,
            &raw mut used,
            ids.as_mut_ptr().cast(),
        )
    };
    if rc != 0 {
        return Vec::new();
    }
    ids.truncate((used as usize) / 4);
    ids
}

/// Read a `u32` property.
fn property_u32(object: cmio::ObjectId, selector: u32) -> Option<u32> {
    let address = cmio::global(selector);
    let mut value = 0u32;
    let mut used = 0u32;
    // SAFETY: a 4-byte destination for a UInt32 property.
    let rc = unsafe {
        cmio::CMIOObjectGetPropertyData(
            object,
            &raw const address,
            0,
            ptr::null(),
            4,
            &raw mut used,
            (&raw mut value).cast(),
        )
    };
    (rc == 0 && used == 4).then_some(value)
}

/// Read a `CFString` property (the caller owns the returned string, so it is
/// released here after conversion).
fn property_string(object: cmio::ObjectId, selector: u32) -> Option<String> {
    let address = cmio::global(selector);
    let mut raw: *const CFString = ptr::null();
    let mut used = 0u32;
    // SAFETY: the destination is one CFStringRef, which the property returns
    // retained (+1) per the CoreMediaIO ownership rule for CF-typed properties.
    let rc = unsafe {
        cmio::CMIOObjectGetPropertyData(
            object,
            &raw const address,
            0,
            ptr::null(),
            size_of::<*const CFString>() as u32,
            &raw mut used,
            (&raw mut raw).cast(),
        )
    };
    if rc != 0 {
        return None;
    }
    let ptr = NonNull::new(raw.cast_mut())?;
    // SAFETY: `ptr` is the +1 CFString the call handed us; taking ownership
    // releases it when `s` drops.
    let s = unsafe { CFRetained::from_raw(ptr) };
    Some(s.to_string())
}

/// One CoreMediaIO device as the client API lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmioDevice {
    /// The CMIO object id (valid for this process while the device exists).
    pub id: u32,
    /// The device UID (`kCMIODevicePropertyDeviceUID`): a camera extension's
    /// device UUID string.
    pub uid: String,
    /// The localized name.
    pub name: String,
}

/// Every CoreMediaIO device visible to this process. Public so the tests and
/// `oak-cam vcam status` can list what the system sees.
#[must_use]
pub fn devices() -> Vec<CmioDevice> {
    property_ids(cmio::SYSTEM_OBJECT, cmio::HARDWARE_PROPERTY_DEVICES)
        .into_iter()
        .map(|id| CmioDevice {
            id,
            uid: property_string(id, cmio::DEVICE_PROPERTY_UID).unwrap_or_default(),
            name: property_string(id, cmio::OBJECT_PROPERTY_NAME).unwrap_or_default(),
        })
        .collect()
}

/// `camera`'s device, if the extension is activated and running.
fn find_device(camera: CameraId) -> Option<u32> {
    let uuid = camera.uuids().device;
    devices()
        .into_iter()
        .find(|d| d.uid.eq_ignore_ascii_case(uuid))
        .map(|d| d.id)
}

/// The streams of `device` with their `kCMIOStreamPropertyDirection` (0 =
/// output/sink, 1 = input/capture) where readable.
fn streams(device: u32) -> Vec<(u32, Option<u32>)> {
    property_ids(device, cmio::DEVICE_PROPERTY_STREAMS)
        .into_iter()
        .map(|s| (s, property_u32(s, cmio::STREAM_PROPERTY_DIRECTION)))
        .collect()
}

/// The sink stream of `device`: the stream whose direction is *output* (the
/// client writes into it). The extension adds its source stream first and its
/// sink second, so the second stream is the fallback when the direction
/// property cannot be read.
fn find_sink_stream(device: u32) -> Option<u32> {
    let streams = streams(device);
    streams
        .iter()
        .find(|(_, dir)| *dir == Some(cmio::STREAM_DIRECTION_OUTPUT))
        .map(|(s, _)| *s)
        .or_else(|| streams.get(1).map(|(s, _)| *s))
}

/// Whether one camera's device exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The extension is running and lists this camera; `device` is its
    /// CoreMediaIO object id.
    Active {
        /// The device's CMIO object id.
        device: u32,
    },
    /// No device with this camera's UUID exists: the extension is not
    /// installed, not yet approved, disabled, or an older build without it.
    NotInstalled,
}

/// Query `camera`'s state through the CoreMediaIO device list.
#[must_use]
pub fn status(camera: CameraId) -> Status {
    match find_device(camera) {
        Some(device) => Status::Active { device },
        None => Status::NotInstalled,
    }
}

/// Whether `camera` can be started right now.
#[must_use]
pub fn available(camera: CameraId) -> bool {
    matches!(status(camera), Status::Active { .. })
}

/// The user-facing text for a start attempt while `camera`'s device is absent.
#[must_use]
pub fn missing_message(camera: CameraId) -> String {
    format!(
        "The oak-cam camera extension is not active ({} is not listed). Install {APP_INSTALL_PATH} \
         (scripts/vcam/build-macos.sh), run `{APP_INSTALL_PATH}/Contents/MacOS/oak-cam vcam \
         install`, approve it under {SETTINGS_PATH}, then start again. `oak-cam vcam status` \
         shows where it stands.",
        camera.name()
    )
}

/// Why one publish could not produce a sample buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The frame's geometry does not match the pool, or its planes are short.
    FormatMismatch,
    /// Every pooled buffer is still in flight (the consumer is behind).
    PoolExhausted,
    /// A CoreVideo call failed with this `CVReturn`.
    CoreVideo(i32),
    /// `CMSampleBufferCreateForImageBuffer` failed with this `OSStatus`.
    SampleBuffer(i32),
}

impl PublishError {
    fn drop_reason(self) -> DropReason {
        match self {
            PublishError::FormatMismatch => DropReason::FormatMismatch,
            PublishError::PoolExhausted => DropReason::PoolExhausted,
            PublishError::CoreVideo(_) | PublishError::SampleBuffer(_) => DropReason::Backend,
        }
    }
}

/// Tag a pixel buffer BT.709 (what oak-cam publishes): the three attachments
/// `CMSampleBufferCreateForImageBuffer` checks against the format description,
/// set with `ShouldPropagate` so they travel with the buffer.
fn tag_bt709(pb: &CVPixelBuffer) {
    // SAFETY: reading immutable CoreVideo key/value CFString constants; setting
    // attachments on a live buffer we own.
    unsafe {
        let pairs: [(&CFString, &CFString); 3] = [
            (
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            ),
            (
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            ),
            (
                kCVImageBufferYCbCrMatrixKey,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            ),
        ];
        for (key, value) in pairs {
            pb.set_attachment(key, value, CVAttachmentMode::ShouldPropagate);
        }
    }
}

/// Process-local attachment marking a pooled buffer whose UV plane already
/// holds neutral chroma (0x80) from a Gray8 publish, so the next Gray8 publish
/// into the same buffer writes only the Y plane. An NV12 publish removes it. A
/// buffer the pool creates afresh has no attachment and is filled.
const UV_NEUTRAL_KEY: &str = "com.surlysoft.oak-cam.uv-neutral";

/// A pool of IOSurface-backed NV12 `CVPixelBuffer`s at one size, plus the
/// video format description (BT.709) every sample buffer built from it
/// carries.
pub struct FramePool {
    pool: CFRetained<CVPixelBufferPool>,
    /// Always `Some` after [`new`](Self::new) returns.
    description: Option<CFRetained<CMVideoFormatDescription>>,
    aux: CFRetained<CFDictionary<CFString, CFType>>,
    uv_neutral_key: CFRetained<CFString>,
    uv_neutral_value: CFRetained<CFNumber>,
    width: u32,
    height: u32,
}

impl FramePool {
    /// Create an NV12 pool at `width` x `height`.
    ///
    /// # Errors
    /// [`VcamError::Backend`] when CoreVideo rejects the size or the format
    /// description cannot be created.
    pub fn new(width: u32, height: u32) -> Result<Self, VcamError> {
        if width == 0 || height == 0 {
            return Err(VcamError::InvalidSize {
                width,
                height,
                reason: "empty frame",
            });
        }
        let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
            return Err(VcamError::InvalidSize {
                width,
                height,
                reason: "too large",
            });
        };
        let w = CFNumber::new_i32(w);
        let h = CFNumber::new_i32(h);
        // Range check pending (M4a): this pool is '420v' (video range, Y 16..235)
        // tagged BT.709, but OV9282 GRAY8 and possibly depthai's NV12 may be
        // full range (0..255), which would crush blacks and clip whites.
        // Verify against real frames: compare the Y histogram extremes of
        // `--dump-frames` output under a dark/bright scene with what Photo Booth
        // shows; if the device is full range, switch to '420f'
        // (kCVPixelFormatType_420YpCbCr8BiPlanarFullRange) here and in the
        // extension's FormatTable, or scale Y in write_nv12.
        let pf = CFNumber::new_i32(CameraFormat::Nv12.cv_pixel_format().cast_signed());
        let io_surface = CFDictionary::<CFString, CFType>::empty();
        // SAFETY: reading immutable CoreVideo key CFString constants.
        let keys: [&CFString; 4] = unsafe {
            [
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferIOSurfacePropertiesKey,
            ]
        };
        let vals: [&CFType; 4] = [&w, &h, &pf, &io_surface];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &vals);

        let mut raw_pool: *mut CVPixelBufferPool = ptr::null_mut();
        // SAFETY: documented pixel-buffer attribute keys with correctly typed
        // values; `raw_pool` is a valid out-pointer.
        let rc = unsafe {
            CVPixelBufferPool::create(
                None,
                None,
                Some(attrs.as_ref()),
                NonNull::from(&mut raw_pool),
            )
        };
        let Some(pool_ptr) = NonNull::new(raw_pool).filter(|_| rc == 0) else {
            return Err(VcamError::Backend(format!(
                "virtual camera: CVPixelBufferPoolCreate NV12 {width}x{height} failed ({rc})"
            )));
        };
        // SAFETY: a successful create hands back a +1 pool.
        let pool = unsafe { CFRetained::from_raw(pool_ptr) };

        let threshold = CFNumber::new_i32(POOL_DEPTH);
        // SAFETY: reading an immutable CoreVideo key CFString constant.
        let aux_key: [&CFString; 1] = unsafe { [kCVPixelBufferPoolAllocationThresholdKey] };
        let aux_val: [&CFType; 1] = [&threshold];
        let aux = CFDictionary::<CFString, CFType>::from_slices(&aux_key, &aux_val);

        // The format description is derived from a tagged pool buffer, so it
        // matches every buffer the pool hands out (dimensions, format and the
        // BT.709 attachments `CMSampleBufferCreateForImageBuffer` compares).
        let mut this = Self {
            pool,
            description: None,
            aux,
            uv_neutral_key: CFString::from_static_str(UV_NEUTRAL_KEY),
            uv_neutral_value: CFNumber::new_i32(1),
            width,
            height,
        };
        let probe = this.acquire().map_err(|e| {
            VcamError::Backend(format!(
                "virtual camera: NV12 {width}x{height} pool yields no buffer ({e:?})"
            ))
        })?;
        let mut raw_desc: *const CMVideoFormatDescription = ptr::null();
        // SAFETY: `probe` is a live, tagged pixel buffer; `raw_desc` is a valid
        // out-pointer.
        let st = unsafe {
            CMVideoFormatDescriptionCreateForImageBuffer(None, &probe, NonNull::from(&mut raw_desc))
        };
        let Some(desc_ptr) = NonNull::new(raw_desc.cast_mut()).filter(|_| st == 0) else {
            return Err(VcamError::Backend(format!(
                "virtual camera: CMVideoFormatDescriptionCreateForImageBuffer failed ({st})"
            )));
        };
        // SAFETY: a successful create hands back a +1 description.
        this.description = Some(unsafe { CFRetained::from_raw(desc_ptr) });
        Ok(this)
    }

    /// The pool's frame size.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Take a buffer from the pool (allocating up to the threshold), tagged
    /// BT.709.
    fn acquire(&self) -> Result<CFRetained<CVPixelBuffer>, PublishError> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: `aux` is the documented allocation-threshold dictionary and
        // `raw` a valid out-pointer.
        let rc = unsafe {
            CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
                None,
                &self.pool,
                Some(self.aux.as_ref()),
                NonNull::from(&mut raw),
            )
        };
        if rc == kCVReturnPoolAllocationFailed || rc == kCVReturnWouldExceedAllocationThreshold {
            return Err(PublishError::PoolExhausted);
        }
        let Some(ptr) = NonNull::new(raw).filter(|_| rc == 0) else {
            return Err(PublishError::CoreVideo(rc));
        };
        // SAFETY: a successful create hands back a +1 pixel buffer.
        let pixel = unsafe { CFRetained::from_raw(ptr) };
        tag_bt709(&pixel);
        Ok(pixel)
    }

    /// Write `src` straight into the planes of a locked pooled pixel buffer
    /// (Gray8: Y plane, plus UV = 0x80 unless this buffer already carries
    /// neutral chroma) and wrap it in a `CMSampleBuffer` presented at
    /// `host_time_ns` (host clock nanoseconds) with `duration`.
    ///
    /// # Errors
    /// [`PublishError`] when the frame does not match the pool, the pool is
    /// exhausted, or CoreVideo/CoreMedia refuse.
    pub fn sample_from_frame(
        &self,
        src: &FrameSrc<'_>,
        host_time_ns: u64,
        duration: CMTime,
    ) -> Result<CFRetained<CMSampleBuffer>, PublishError> {
        if (src.width(), src.height()) != (self.width, self.height) {
            return Err(PublishError::FormatMismatch);
        }
        let pixel = self.acquire()?;
        let pb: &CVPixelBuffer = &pixel;
        if CVPixelBufferGetPixelFormatType(pb) != CameraFormat::Nv12.cv_pixel_format()
            || CVPixelBufferGetPlaneCount(pb) != 2
        {
            return Err(PublishError::FormatMismatch);
        }
        let uv_neutral = pb.has_attachment(&self.uv_neutral_key);
        // SAFETY: lock for writing before touching the planes.
        let lock = unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::empty()) };
        if lock != 0 {
            return Err(PublishError::CoreVideo(lock));
        }
        // SAFETY: the base address is locked until the unlock below; each
        // plane's slice spans exactly bytes-per-row x rows of that plane, which
        // CoreVideo allocated; the slices do not overlap and do not outlive the
        // lock.
        let written = unsafe {
            let y_ptr = CVPixelBufferGetBaseAddressOfPlane(pb, 0).cast::<u8>();
            let uv_ptr = CVPixelBufferGetBaseAddressOfPlane(pb, 1).cast::<u8>();
            if y_ptr.is_null() || uv_ptr.is_null() {
                Err(PublishError::CoreVideo(-1))
            } else {
                let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
                let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
                let y_rows = CVPixelBufferGetHeightOfPlane(pb, 0);
                let uv_rows = CVPixelBufferGetHeightOfPlane(pb, 1);
                let mut dst = Nv12Dest {
                    width: self.width,
                    height: self.height,
                    y: std::slice::from_raw_parts_mut(y_ptr, y_stride * y_rows),
                    y_stride,
                    uv: std::slice::from_raw_parts_mut(uv_ptr, uv_stride * uv_rows),
                    uv_stride,
                };
                write_nv12(src, &mut dst, !uv_neutral).map_err(|_| PublishError::FormatMismatch)
            }
        };
        // SAFETY: balances the lock above.
        unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::empty()) };
        written?;
        if src.has_chroma() {
            if uv_neutral {
                pb.remove_attachment(&self.uv_neutral_key);
            }
        } else if !uv_neutral {
            // SAFETY: a CFString key and CFNumber value on a live buffer.
            unsafe {
                pb.set_attachment(
                    &self.uv_neutral_key,
                    &self.uv_neutral_value,
                    CVAttachmentMode::ShouldNotPropagate,
                );
            }
        }

        let mut timing = CMSampleTimingInfo {
            duration,
            presentationTimeStamp: CMTime {
                value: i64::try_from(host_time_ns).unwrap_or(i64::MAX),
                timescale: 1_000_000_000,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            },
            decodeTimeStamp: CMTime {
                value: 0,
                timescale: 0,
                flags: CMTimeFlags::empty(),
                epoch: 0,
            },
        };
        let Some(description) = self.description.as_ref() else {
            return Err(PublishError::FormatMismatch);
        };
        let mut raw: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY: `pb` is a ready image buffer matching `description`; no
        // make-data-ready callback; `timing` and `raw` are valid pointers.
        let st = unsafe {
            CMSampleBuffer::create_for_image_buffer(
                None,
                pb,
                true,
                None,
                ptr::null_mut(),
                description,
                NonNull::from(&mut timing),
                NonNull::from(&mut raw),
            )
        };
        let Some(ptr) = NonNull::new(raw).filter(|_| st == 0) else {
            return Err(PublishError::SampleBuffer(st));
        };
        // SAFETY: a successful create hands back a +1 sample buffer (which
        // retains the pixel buffer, so `pixel` may drop).
        Ok(unsafe { CFRetained::from_raw(ptr) })
    }
}

/// The queue-altered callback CoreMediaIO invokes on the sink's `CMSimpleQueue`
/// whenever the extension side dequeues a buffer. We detect a full queue from
/// [`MacCamera::publish`] instead, so this is a no-op — but it must be a real C
/// function, not null: `CMIOStreamCopyBufferQueue` returns `noErr` with a
/// **NULL** queue when the proc is null.
///
/// # Safety
/// Called by CoreMediaIO on its own thread with the stream id, an opaque token,
/// and the refcon we registered. It touches none of them.
unsafe extern "C-unwind" fn queue_altered_noop(
    _stream: cmio::ObjectId,
    _token: *mut c_void,
    _refcon: *mut c_void,
) {
}

/// Host-clock time in nanoseconds (the clock camera consumers timestamp with).
#[must_use]
pub fn host_time_ns() -> u64 {
    // SAFETY: the host time clock is a process-wide singleton.
    let t = unsafe { CMClock::host_time_clock().time() };
    if t.timescale <= 0 || !t.flags.contains(CMTimeFlags::Valid) {
        return 0;
    }
    let secs = t.value as f64 / f64::from(t.timescale);
    (secs * 1e9) as u64
}

/// One macOS virtual camera: publishes NV12 (or Gray8 widened to NV12) frames
/// into the sink stream of the extension device for its [`CameraId`].
pub struct MacCamera {
    camera: CameraId,
    device: u32,
    stream: u32,
    queue: Option<CFRetained<CMSimpleQueue>>,
    pool: Option<FramePool>,
    duration: CMTime,
    stats: VcamStats,
}

// SAFETY: a MacCamera is used from one thread at a time (the capture thread
// that owns it; `VirtualCamera` takes `&mut self`). The pool, format
// description and attribute dictionaries are CF objects safe to use from any
// thread, and the CMSimpleQueue is CoreMedia's single-producer/single-consumer
// queue: this camera is its only producer and the DAL assistant its only
// consumer.
unsafe impl Send for MacCamera {}

impl MacCamera {
    /// A camera that attaches to `camera`'s extension device on
    /// [`start`](VirtualCamera::start).
    #[must_use]
    pub fn new(camera: CameraId) -> Self {
        Self {
            camera,
            device: 0,
            stream: 0,
            queue: None,
            pool: None,
            duration: CMTime {
                value: 0,
                timescale: 0,
                flags: CMTimeFlags::empty(),
                epoch: 0,
            },
            stats: VcamStats::default(),
        }
    }

    /// Which camera this publishes.
    #[must_use]
    pub fn camera(&self) -> CameraId {
        self.camera
    }

    /// Whether the sink queue has room for another sample.
    fn queue_has_room(queue: &CMSimpleQueue) -> bool {
        // SAFETY: plain getters on a live queue.
        let (count, capacity) = unsafe { (queue.count(), queue.capacity()) };
        capacity <= 0 || count < capacity
    }
}

impl VirtualCamera for MacCamera {
    fn start(&mut self, width: u32, height: u32, fps: f32) -> Result<(), VcamError> {
        if self.queue.is_some() {
            self.stop();
        }
        let Status::Active { device } = status(self.camera) else {
            return Err(VcamError::NotAvailable(missing_message(self.camera)));
        };
        let stream = find_sink_stream(device).ok_or_else(|| {
            VcamError::Backend(format!(
                "virtual camera: {} exposes no sink stream",
                self.camera
            ))
        })?;
        let pool = FramePool::new(width, height)?;

        let mut raw_queue: *mut CMSimpleQueue = ptr::null_mut();
        // SAFETY: `queue_altered_noop` is a real C callback of the required
        // signature (a null proc makes CoreMediaIO return noErr with a NULL
        // queue); no refcon is needed; `raw_queue` is a valid out-pointer that
        // receives a +1 queue.
        let rc = unsafe {
            cmio::CMIOStreamCopyBufferQueue(
                stream,
                Some(queue_altered_noop),
                ptr::null_mut(),
                &raw mut raw_queue,
            )
        };
        if rc != 0 {
            return Err(VcamError::Backend(format!(
                "virtual camera: CMIOStreamCopyBufferQueue failed ({rc}) on {} sink stream {stream}",
                self.camera
            )));
        }
        let Some(queue_ptr) = NonNull::new(raw_queue) else {
            // noErr but a NULL queue: in practice the sink is not writable yet,
            // most often because the calling process lacks Camera access.
            return Err(VcamError::Backend(format!(
                "virtual camera: CMIOStreamCopyBufferQueue returned no queue for {} sink stream \
                 {stream} (rc 0). Grant Camera access to the terminal or app running oak-cam in \
                 System Settings › Privacy & Security › Camera and confirm the oak-cam camera \
                 extension is enabled, then start again.",
                self.camera
            )));
        };
        // SAFETY: the copy hands back a +1 queue.
        let queue = unsafe { CFRetained::from_raw(queue_ptr) };

        // SAFETY: plain C call on ids the device list just handed us.
        let rc = unsafe { cmio::CMIODeviceStartStream(device, stream) };
        if rc != 0 {
            return Err(VcamError::Backend(format!(
                "virtual camera: CMIODeviceStartStream failed ({rc}) for {}",
                self.camera
            )));
        }

        self.device = device;
        self.stream = stream;
        self.queue = Some(queue);
        if fps > 0.0 {
            self.duration = CMTime {
                value: 1,
                timescale: fps.round().max(1.0) as i32,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            };
        }
        self.pool = Some(pool);
        self.stats = VcamStats::default();
        tracing::info!(
            "virtual camera: attached to {} (device {device}, sink stream {stream}) as NV12 {width}x{height}",
            self.camera
        );
        Ok(())
    }

    fn publish(&mut self, src: &FrameSrc<'_>) -> Publish {
        let outcome = match (self.queue.as_ref(), self.pool.as_ref()) {
            (Some(queue), Some(pool)) => {
                if Self::queue_has_room(queue) {
                    publish_into(queue, pool, src, self.duration)
                } else {
                    // Latest wins: skip the copy entirely while the consumer
                    // side is behind.
                    Publish::Dropped(DropReason::QueueFull)
                }
            }
            _ => Publish::Dropped(DropReason::NotStarted),
        };
        self.stats.record(outcome, crate::monotonic_ns());
        outcome
    }

    fn stats(&self) -> VcamStats {
        self.stats
    }

    fn stop(&mut self) {
        if self.queue.take().is_some() {
            // SAFETY: plain C call on the ids we started.
            let rc = unsafe { cmio::CMIODeviceStopStream(self.device, self.stream) };
            if rc != 0 {
                tracing::warn!(
                    "virtual camera: CMIODeviceStopStream failed ({rc}) for {}",
                    self.camera
                );
            }
        }
        self.pool = None;
        self.device = 0;
        self.stream = 0;
    }
}

/// Build a sample from `src` and enqueue it on `queue`.
fn publish_into(
    queue: &CMSimpleQueue,
    pool: &FramePool,
    src: &FrameSrc<'_>,
    duration: CMTime,
) -> Publish {
    let sample = match pool.sample_from_frame(src, host_time_ns(), duration) {
        Ok(s) => s,
        Err(e) => {
            if !matches!(e, PublishError::PoolExhausted) {
                tracing::warn!("virtual camera: publish skipped: {e:?}");
            }
            return Publish::Dropped(e.drop_reason());
        }
    };
    // The queue does not retain its elements: hand it our +1 and let the DAL
    // assistant release the sample once relayed. On a full queue the element
    // was not taken, so release it here.
    let raw = CFRetained::into_raw(sample);
    // SAFETY: `raw` is a live +1 CMSampleBuffer; the queue is the one
    // CoreMediaIO handed us for this stream.
    let rc = unsafe { queue.enqueue(raw.cast::<c_void>()) };
    if rc == 0 {
        Publish::Published
    } else {
        // SAFETY: the enqueue rejected the element, so ownership is still ours;
        // dropping releases it.
        drop(unsafe { CFRetained::from_raw(raw) });
        if rc == kCMSimpleQueueError_QueueIsFull {
            Publish::Dropped(DropReason::QueueFull)
        } else {
            tracing::warn!("virtual camera: CMSimpleQueueEnqueue failed ({rc})");
            Publish::Dropped(DropReason::Backend)
        }
    }
}

impl Drop for MacCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_video::CVPixelBufferGetIOSurface;

    fn d30() -> CMTime {
        CMTime {
            value: 1,
            timescale: 30,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    /// The CoreMediaIO device list is readable in any process (no camera
    /// permission needed) and every listed device has a UID. Prints what the
    /// machine sees; skips loudly on a Mac with no camera at all.
    #[test]
    fn cmio_device_list_is_readable() {
        let list = devices();
        if list.is_empty() {
            eprintln!("SKIP cmio_device_list_is_readable: no CoreMediaIO devices");
            return;
        }
        for d in &list {
            eprintln!(
                "cmio device {} uid={} name={} streams={:?}",
                d.id,
                d.uid,
                d.name,
                streams(d.id)
            );
            assert!(!d.uid.is_empty(), "device {} has no UID", d.id);
            assert!(d.id != 0);
        }
    }

    /// Without the extension, each camera is `NotInstalled` and the message
    /// says where to approve it; with it, the device exposes a sink stream.
    #[test]
    fn status_per_camera() {
        for cam in CameraId::ALL {
            match status(cam) {
                Status::NotInstalled => {
                    assert!(!available(cam));
                    let msg = missing_message(cam);
                    assert!(msg.contains(cam.name()), "{msg}");
                    assert!(msg.contains("Camera Extensions"), "{msg}");
                    assert!(msg.contains("vcam install"), "{msg}");
                }
                Status::Active { device } => {
                    eprintln!("{cam} is active (device {device})");
                    assert!(
                        find_sink_stream(device).is_some(),
                        "{cam} has a sink stream"
                    );
                }
            }
        }
    }

    /// Lock, read, unlock one pixel buffer's planes.
    fn read_planes(pb: &CVPixelBuffer) -> (Vec<u8>, Vec<u8>, usize, usize) {
        // SAFETY: read-only inspection of a live pixel buffer.
        unsafe {
            assert_eq!(
                CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly),
                0
            );
            let ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            let uvs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
            let yr = CVPixelBufferGetHeightOfPlane(pb, 0);
            let uvr = CVPixelBufferGetHeightOfPlane(pb, 1);
            let y = std::slice::from_raw_parts(
                CVPixelBufferGetBaseAddressOfPlane(pb, 0).cast::<u8>(),
                ys * yr,
            )
            .to_vec();
            let uv = std::slice::from_raw_parts(
                CVPixelBufferGetBaseAddressOfPlane(pb, 1).cast::<u8>(),
                uvs * uvr,
            )
            .to_vec();
            CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
            (y, uv, ys, uvs)
        }
    }

    /// An NV12 pool at 1280x720 hands out IOSurface-backed buffers, an NV12
    /// frame copies in plane by plane, and the sample carries the requested
    /// timestamp and a 720p NV12 format description.
    #[test]
    fn frame_pool_builds_nv12_sample_buffers() {
        use objc2_core_media::CMVideoFormatDescriptionGetDimensions;
        let pool = FramePool::new(1280, 720).expect("NV12 pool");
        assert_eq!(pool.size(), (1280, 720));
        let y = vec![0x40u8; 1280 * 720];
        let uv = vec![0x90u8; 1280 * 360];
        let frame = FrameSrc::Nv12 {
            y: &y,
            y_stride: 1280,
            uv: &uv,
            uv_stride: 1280,
            w: 1280,
            h: 720,
            ts_ns: 0,
        };
        let sample = pool
            .sample_from_frame(&frame, 123_456_789, d30())
            .expect("sample buffer");

        // SAFETY: getters on a live sample buffer we own.
        let (pts, desc, image) = unsafe {
            (
                sample.presentation_time_stamp(),
                sample.format_description(),
                sample.image_buffer(),
            )
        };
        assert_eq!((pts.value, pts.timescale), (123_456_789, 1_000_000_000));
        let desc = desc.expect("format description");
        // SAFETY: getters on a live video format description.
        let (subtype, dims) = unsafe {
            (
                desc.media_sub_type(),
                CMVideoFormatDescriptionGetDimensions(&desc),
            )
        };
        assert_eq!(subtype, CameraFormat::Nv12.cv_pixel_format());
        assert_eq!((dims.width, dims.height), (1280, 720));

        let image = image.expect("image buffer");
        let pb: &CVPixelBuffer = &image;
        assert!(
            CVPixelBufferGetIOSurface(Some(pb)).is_some(),
            "pool buffers are IOSurface-backed"
        );
        let (py, puv, ys, uvs) = read_planes(pb);
        assert_eq!(py[0], 0x40);
        assert_eq!(py[ys * 719 + 1279], 0x40);
        assert_eq!(puv[0], 0x90);
        assert_eq!(puv[uvs * 359 + 1279], 0x90);

        // A frame of the wrong size is refused, never copied.
        let small = FrameSrc::Gray8 {
            y: &y[..640 * 360],
            stride: 640,
            w: 640,
            h: 360,
            ts_ns: 0,
        };
        assert_eq!(
            pool.sample_from_frame(&small, 0, d30()).err(),
            Some(PublishError::FormatMismatch)
        );
        assert!(FramePool::new(0, 1080).is_err());
    }

    /// Gray8 frames land as Y + neutral chroma, including in a recycled buffer
    /// an NV12 frame wrote coloured chroma into first.
    #[test]
    fn gray8_publishes_neutral_chroma_through_recycled_buffers() {
        let pool = FramePool::new(640, 400).expect("pool");
        let y = vec![0x55u8; 640 * 400];
        let uv = vec![0x10u8; 640 * 200];
        let colour = FrameSrc::Nv12 {
            y: &y,
            y_stride: 640,
            uv: &uv,
            uv_stride: 640,
            w: 640,
            h: 400,
            ts_ns: 0,
        };
        let gray = FrameSrc::Gray8 {
            y: &y,
            stride: 640,
            w: 640,
            h: 400,
            ts_ns: 0,
        };
        // Cycle through more samples than the pool holds, alternating formats,
        // releasing each so buffers are recycled.
        for i in 0..(POOL_DEPTH * 3) {
            let src = if i % 3 == 0 { &colour } else { &gray };
            let sample = pool.sample_from_frame(src, 0, d30()).expect("sample");
            // SAFETY: getter on a live sample buffer.
            let image = unsafe { sample.image_buffer() }.expect("image");
            let (py, puv, _, uvs) = read_planes(&image);
            assert_eq!(py[0], 0x55);
            let want = if src.has_chroma() { 0x10 } else { 0x80 };
            for row in 0..200 {
                assert!(
                    puv[row * uvs..row * uvs + 640].iter().all(|&b| b == want),
                    "iteration {i} row {row}"
                );
            }
        }
    }

    /// The pool's allocation threshold bounds in-flight buffers: holding
    /// `POOL_DEPTH` samples makes the next publish report `PoolExhausted`
    /// instead of allocating, and releasing one frees a slot.
    #[test]
    fn frame_pool_is_bounded() {
        let pool = FramePool::new(320, 180).expect("pool");
        let y = vec![0u8; 320 * 180];
        let frame = FrameSrc::Gray8 {
            y: &y,
            stride: 320,
            w: 320,
            h: 180,
            ts_ns: 0,
        };
        let mut held = Vec::new();
        for _ in 0..POOL_DEPTH {
            held.push(
                pool.sample_from_frame(&frame, 0, d30())
                    .expect("within threshold"),
            );
        }
        assert_eq!(
            pool.sample_from_frame(&frame, 0, d30()).err(),
            Some(PublishError::PoolExhausted)
        );
        held.pop();
        assert!(pool.sample_from_frame(&frame, 0, d30()).is_ok());
    }

    /// Host time is monotonic and in nanoseconds.
    #[test]
    fn host_time_is_monotonic_nanoseconds() {
        let a = host_time_ns();
        let b = host_time_ns();
        assert!(a > 1_000_000_000, "uptime is well over a second: {a}");
        assert!(b >= a);
    }

    /// Without the extension a start fails cleanly with the install text,
    /// publish drops as NotStarted, and stop is idempotent. With the extension
    /// active, start attaches (the live test publishes).
    #[test]
    fn start_without_extension_is_clean() {
        let mut cam = MacCamera::new(CameraId::Rgb);
        assert_eq!(cam.camera(), CameraId::Rgb);
        match cam.start(1280, 720, 30.0) {
            Err(VcamError::NotAvailable(msg)) => {
                assert_eq!(msg, missing_message(CameraId::Rgb));
                let y = vec![0u8; 1280 * 720];
                let frame = FrameSrc::Gray8 {
                    y: &y,
                    stride: 1280,
                    w: 1280,
                    h: 720,
                    ts_ns: 0,
                };
                assert_eq!(
                    cam.publish(&frame),
                    Publish::Dropped(DropReason::NotStarted)
                );
                assert_eq!(cam.stats().dropped, 1);
                assert_eq!(cam.stats().published, 0);
            }
            Ok(()) => {
                eprintln!("camera extension is active; attached");
                assert!(cam.queue.is_some() && cam.pool.is_some());
            }
            Err(e) => eprintln!("start failed with the extension present: {e}"),
        }
        cam.stop();
        cam.stop();
        assert!(cam.queue.is_none() && cam.pool.is_none());
        assert_eq!((cam.device, cam.stream), (0, 0));
    }
}
