//! Pipelines, as a typestate: [`PipelineBuilder`] (add cameras, request
//! outputs, create queues) → [`PipelineBuilder::start`] → [`RunningPipeline`].
//!
//! Camera and output handles borrow the builder, so none can be used after
//! `start` consumes it: depthai v3 requires every output to be requested
//! before the pipeline starts. Queues ([`OutputQueue`], [`ControlQueue`]) own
//! an `Arc` to the pipeline, so the C++ pipeline is freed only after the last
//! queue is gone, whatever order the owner drops things in. The C++ pipeline
//! in turn holds a `shared_ptr` to the device, so a queue that outlives the
//! [`Device`] handle is still memory-safe (it just reports closed once the
//! pipeline stops).

use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use depthai_sys as sys;

use crate::control::{ControlParams, ControlQueue};
use crate::frame::Frame;
use crate::{Device, Error, ImgType, Result, Socket, check};

/// Owns the `oak_pipeline`; freed when the last holder (running pipeline or
/// queue) drops.
pub(crate) struct PipelineInner {
    raw: NonNull<sys::oak_pipeline>,
    /// Serialises pipeline-level calls (the shim forbids concurrent use of
    /// one handle).
    lock: Mutex<()>,
}

// SAFETY: every call on `raw` after construction goes through `lock`, or
// happens in Drop when no other reference exists.
unsafe impl Send for PipelineInner {}
// SAFETY: as above.
unsafe impl Sync for PipelineInner {}

impl PipelineInner {
    fn call<T>(&self, f: impl FnOnce(*mut sys::oak_pipeline) -> T) -> T {
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        f(self.raw.as_ptr())
    }
}

impl Drop for PipelineInner {
    fn drop(&mut self) {
        // SAFETY: last reference; every queue handle created on this
        // pipeline held an Arc and has been freed already. Stops if running.
        unsafe { sys::oak_pipeline_free(self.raw.as_ptr()) };
    }
}

/// Sensor mode for [`PipelineBuilder::camera`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorSpec {
    /// Sensor width in pixels.
    pub width: u32,
    /// Sensor height in pixels.
    pub height: u32,
    /// Sensor frame rate.
    pub fps: f32,
}

/// How to fit the sensor image into an output of a different aspect ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ResizeMode {
    /// Crop to the output aspect ratio (depthai's default).
    #[default]
    Crop,
    /// Stretch to fill.
    Stretch,
    /// Scale to fit, pad the rest.
    Letterbox,
}

impl ResizeMode {
    /// The `OAK_RESIZE_*` constant.
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Crop => sys::OAK_RESIZE_CROP,
            Self::Stretch => sys::OAK_RESIZE_STRETCH,
            Self::Letterbox => sys::OAK_RESIZE_LETTERBOX,
        }
    }
}

/// What to request from a camera with [`CameraHandle::request_output`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpec {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Pixel format ([`ImgType::None`] lets depthai choose).
    pub img_type: ImgType,
    /// Aspect-ratio handling.
    pub resize: ResizeMode,
    /// Output frame rate (`None`: the sensor's).
    pub fps: Option<f32>,
}

impl OutputSpec {
    fn to_raw(self) -> sys::oak_output_spec {
        sys::oak_output_spec {
            width: self.width,
            height: self.height,
            type_: self.img_type.to_raw(),
            resize_mode: self.resize.to_raw(),
            fps: self.fps.unwrap_or(0.0),
            _pad: 0,
        }
    }
}

/// Builds a pipeline on a device. Not `Sync`; handles borrow it, and
/// [`Self::start`] consumes it.
pub struct PipelineBuilder<'d> {
    inner: Arc<PipelineInner>,
    /// Borrow of the `Device` (as `&()` so the pipeline stays `Send`).
    _device: PhantomData<&'d ()>,
    /// Handles call into the shim through `&self`; keep the builder on one
    /// thread at a time.
    _not_sync: PhantomData<Cell<()>>,
}

impl<'d> PipelineBuilder<'d> {
    /// A new, empty pipeline on `device`.
    ///
    /// Fails if the device is closed: depthai v3's `Pipeline::stop` closes
    /// the device, so each open device runs at most one pipeline, and
    /// building on a closed one would crash inside depthai.
    pub fn new(device: &'d Device) -> Result<Self> {
        if device.is_closed()? {
            return Err(Error::Depthai(
                "device is closed (a stopped pipeline closes its device; reopen it)".into(),
            ));
        }
        let mut raw = std::ptr::null_mut();
        // SAFETY: device handle is live; raw is a valid out-param.
        check(unsafe { sys::oak_pipeline_new(device.as_raw(), &raw mut raw) })?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_pipeline_new returned NULL".into()))?;
        Ok(Self {
            inner: Arc::new(PipelineInner {
                raw,
                lock: Mutex::new(()),
            }),
            _device: PhantomData,
            _not_sync: PhantomData,
        })
    }

    /// Adds a Camera node on `socket`. `sensor: None` lets depthai pick the
    /// sensor mode from the requested outputs.
    pub fn camera(&self, socket: Socket, sensor: Option<SensorSpec>) -> Result<CameraHandle<'_>> {
        let (w, h, fps) = sensor.map_or((0, 0, 0.0), |s| (s.width, s.height, s.fps));
        let mut raw = std::ptr::null_mut();
        // SAFETY: pipeline handle is live; raw is a valid out-param.
        check(self.inner.call(|p| unsafe {
            sys::oak_pipeline_add_camera(p, socket.to_raw(), w, h, fps, &raw mut raw)
        }))?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_pipeline_add_camera returned NULL".into()))?;
        Ok(CameraHandle { raw, builder: self })
    }

    /// Starts the pipeline. Every output and queue must exist by now.
    pub fn start(self) -> Result<RunningPipeline<'d>> {
        // SAFETY: pipeline handle is live.
        check(self.inner.call(|p| unsafe { sys::oak_pipeline_start(p) }))?;
        Ok(RunningPipeline {
            inner: self.inner,
            _device: PhantomData,
        })
    }
}

impl std::fmt::Debug for PipelineBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineBuilder")
            .field("raw", &self.inner.raw)
            .finish_non_exhaustive()
    }
}

/// A Camera node being configured. Owned by the pipeline; borrows the
/// builder.
#[derive(Debug)]
pub struct CameraHandle<'b> {
    raw: NonNull<sys::oak_camera>,
    builder: &'b PipelineBuilder<'b>,
}

impl<'b> CameraHandle<'b> {
    fn call<T>(&self, f: impl FnOnce(*mut sys::oak_camera) -> T) -> T {
        // Camera calls touch the C++ pipeline: take the pipeline lock.
        self.builder.inner.call(|_| f(self.raw.as_ptr()))
    }

    /// Requests an output stream (`Camera::requestOutput`).
    pub fn request_output(&self, spec: OutputSpec) -> Result<OutputHandle<'b>> {
        let raw_spec = spec.to_raw();
        let mut raw = std::ptr::null_mut();
        // SAFETY: camera handle is live (owned by the pipeline, which the
        // builder keeps alive); spec and raw are valid pointers.
        check(self.call(|c| unsafe {
            sys::oak_camera_request_output(c, &raw const raw_spec, &raw mut raw)
        }))?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_camera_request_output returned NULL".into()))?;
        Ok(OutputHandle {
            raw,
            builder: self.builder,
        })
    }

    /// Controls applied when the pipeline starts (`Camera::initialControl`).
    pub fn set_initial_control(&self, params: &ControlParams) -> Result<()> {
        // SAFETY: camera handle is live; params points at a valid struct.
        check(self.call(|c| unsafe { sys::oak_camera_set_initial_control(c, params.as_raw()) }))
    }

    /// The camera's live control input (`inputControl.createInputQueue()`).
    pub fn control_queue(&self) -> Result<ControlQueue> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: camera handle is live; raw is a valid out-param.
        check(self.call(|c| unsafe { sys::oak_camera_control_queue(c, &raw mut raw) }))?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_camera_control_queue returned NULL".into()))?;
        Ok(ControlQueue::new(raw, Arc::clone(&self.builder.inner)))
    }

    /// The sensor's maximum resolution.
    pub fn max_size(&self) -> Result<(u32, u32)> {
        let (mut w, mut h) = (0u32, 0u32);
        // SAFETY: camera handle is live; w/h are valid out-params.
        check(self.call(|c| unsafe { sys::oak_camera_max_size(c, &raw mut w, &raw mut h) }))?;
        Ok((w, h))
    }
}

/// A requested camera output. Owned by the pipeline; borrows the builder.
#[derive(Debug)]
pub struct OutputHandle<'b> {
    raw: NonNull<sys::oak_output>,
    builder: &'b PipelineBuilder<'b>,
}

impl OutputHandle<'_> {
    /// A host queue on this output. `max_size` frames are buffered; when
    /// full, a non-blocking queue drops the oldest (latest wins), a blocking
    /// one stalls the device side.
    pub fn create_queue(&self, max_size: u32, blocking: bool) -> Result<OutputQueue> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: output handle is live; raw is a valid out-param.
        check(self.builder.inner.call(|_| unsafe {
            sys::oak_output_create_queue(
                self.raw.as_ptr(),
                max_size,
                i32::from(blocking),
                &raw mut raw,
            )
        }))?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("oak_output_create_queue returned NULL".into()))?;
        Ok(OutputQueue {
            raw,
            _pipeline: Arc::clone(&self.builder.inner),
        })
    }
}

/// A started pipeline. Stops on drop; the C++ pipeline is freed once every
/// queue created on it has been dropped too. Cannot outlive its [`Device`].
pub struct RunningPipeline<'d> {
    inner: Arc<PipelineInner>,
    /// Borrow of the `Device` (as `&()` so the pipeline stays `Send`).
    _device: PhantomData<&'d ()>,
}

impl RunningPipeline<'_> {
    /// Whether the pipeline is running (false after [`Self::stop`] or a
    /// device error).
    pub fn is_running(&self) -> Result<bool> {
        let mut v = 0i32;
        // SAFETY: pipeline handle is live; v is a valid out-param.
        check(
            self.inner
                .call(|p| unsafe { sys::oak_pipeline_is_running(p, &raw mut v) }),
        )?;
        Ok(v != 0)
    }

    /// Stops the pipeline; its output queues close. Idempotent. depthai v3
    /// also closes the device here: reopen it to start another pipeline.
    pub fn stop(&self) -> Result<()> {
        // SAFETY: pipeline handle is live.
        check(self.inner.call(|p| unsafe { sys::oak_pipeline_stop(p) }))
    }
}

impl Drop for RunningPipeline<'_> {
    fn drop(&mut self) {
        // Stop now so capture threads see their queues close; freeing waits
        // for the last queue (Arc).
        // This crate has no logging dependency; report on stderr.
        if let Err(e) = self.stop() {
            eprintln!("depthai: pipeline stop failed: {e}");
        }
    }
}

impl std::fmt::Debug for RunningPipeline<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningPipeline")
            .field("raw", &self.inner.raw)
            .finish_non_exhaustive()
    }
}

/// A host-side output queue. One consumer: `Send`, not `Sync`.
///
/// Holds the pipeline alive through an `Arc`, and the C++ pipeline holds
/// the device connection, so the queue may outlive both the
/// [`RunningPipeline`] and the [`Device`] handle. Once the pipeline has
/// stopped (explicitly or by dropping the `RunningPipeline`), [`Self::get`]
/// returns an error and [`Self::is_closed`] is `true`.
pub struct OutputQueue {
    raw: NonNull<sys::oak_queue>,
    _pipeline: Arc<PipelineInner>,
}

// SAFETY: oak_queue_* may be used from any thread by one consumer at a time
// (oak_dai.h, "Threading"); `!Sync` keeps it to one.
unsafe impl Send for OutputQueue {}

impl OutputQueue {
    /// Waits up to `timeout` for a frame. `Ok(None)` on timeout; an error
    /// once the queue is closed (see [`Self::is_closed`]).
    pub fn get(&self, timeout: Duration) -> Result<Option<Frame>> {
        let ns = i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX);
        let mut raw = std::ptr::null_mut();
        // SAFETY: queue handle is live; raw is a valid out-param.
        let rc = unsafe { sys::oak_queue_get(self.raw.as_ptr(), ns, &raw mut raw) };
        self.wrap(rc, raw)
    }

    /// Returns a frame if one is queued, without waiting.
    pub fn try_get(&self) -> Result<Option<Frame>> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: queue handle is live; raw is a valid out-param.
        let rc = unsafe { sys::oak_queue_try_get(self.raw.as_ptr(), &raw mut raw) };
        self.wrap(rc, raw)
    }

    #[allow(clippy::unused_self)]
    fn wrap(&self, rc: i32, raw: *mut sys::oak_frame) -> Result<Option<Frame>> {
        check(rc)?;
        if rc == 0 {
            return Ok(None);
        }
        let raw = NonNull::new(raw)
            .ok_or_else(|| Error::Depthai("queue returned a NULL frame".into()))?;
        Frame::from_raw(raw).map(Some)
    }

    /// Whether the queue has closed (pipeline stopped or device lost).
    pub fn is_closed(&self) -> Result<bool> {
        let mut v = 0i32;
        // SAFETY: queue handle is live; v is a valid out-param.
        check(unsafe { sys::oak_queue_is_closed(self.raw.as_ptr(), &raw mut v) })?;
        Ok(v != 0)
    }
}

impl Drop for OutputQueue {
    fn drop(&mut self) {
        // SAFETY: we own the handle; freed once, before the pipeline (Arc).
        unsafe { sys::oak_queue_free(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for OutputQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputQueue")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send<T: Send>() {}
    const _: () = assert_send::<OutputQueue>();
    const _: () = assert_send::<RunningPipeline<'static>>();

    #[test]
    fn output_spec_to_raw() {
        let raw = OutputSpec {
            width: 1920,
            height: 1080,
            img_type: ImgType::Nv12,
            resize: ResizeMode::Letterbox,
            fps: Some(30.0),
        }
        .to_raw();
        assert_eq!((raw.width, raw.height), (1920, 1080));
        assert_eq!(raw.type_, sys::OAK_IMG_NV12);
        assert_eq!(raw.resize_mode, sys::OAK_RESIZE_LETTERBOX);
        assert!((raw.fps - 30.0).abs() < f32::EPSILON);
        let raw = OutputSpec {
            width: 1,
            height: 1,
            img_type: ImgType::None,
            resize: ResizeMode::default(),
            fps: None,
        }
        .to_raw();
        assert!(raw.fps.abs() < f32::EPSILON);
        assert_eq!(raw.resize_mode, sys::OAK_RESIZE_CROP);
    }
}
