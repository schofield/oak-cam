//! The source's single video stream: answers `RequestSample` on a worker
//! thread that paces to the selected frame rate, reads the newest frame from
//! the ring (or paints the placeholder), converts it into a sample from the
//! (Frame Server-provided or own) sample allocator, and queues `MEMediaSample`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use windows::Win32::Foundation::{ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFMediaBuffer, IMFMediaEvent,
    IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource, IMFMediaStream_Impl,
    IMFMediaStream2, IMFMediaStream2_Impl, IMFMediaType, IMFSample, IMFStreamDescriptor,
    IMFVideoSampleAllocator, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEError, MEMediaSample,
    MEStreamStarted, MEStreamStopped, MF_E_INVALIDREQUEST, MF_E_SAMPLEALLOCATOR_EMPTY,
    MF_E_SHUTDOWN, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING, MF_STREAM_STATE_STOPPED,
    MF2DBuffer_LockFlags_Write, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateVideoSampleAllocatorEx, MFGetSystemTime, MFSampleExtension_Token,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{GUID, HRESULT, IUnknown, Interface, Ref, implement};

use super::super::Agile;
use super::super::shm::{RingConfig, Role, SharedRing};
use crate::convert::{Nv12Frame, paint_placeholder_nv12, render, required_len};
use crate::formats::{MEDIA_FORMATS, MediaFormat};
use crate::layout::{SLOT_CAPACITY, STALE_AFTER_100NS, nv12_size};

/// Samples the allocator pre-creates.
const ALLOCATOR_SAMPLES: u32 = 10;
/// How often a missing ring is looked for again.
const RING_RETRY: Duration = Duration::from_millis(500);

/// State shared between the COM object and its worker thread.
struct Shared {
    queue: Agile<IMFMediaEventQueue>,
    /// The ring this stream reads; `None` means placeholder only.
    ring: Option<RingConfig>,
    descriptor: Agile<IMFStreamDescriptor>,
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    stream_state: MF_STREAM_STATE,
    shutdown: bool,
    active: bool,
    pending: VecDeque<Option<Agile<IUnknown>>>,
    format: MediaFormat,
    media_type: Option<Agile<IMFMediaType>>,
    provided_allocator: Option<Agile<IMFVideoSampleAllocator>>,
    d3d_manager: Option<Agile<IUnknown>>,
    /// Bumped whenever the format, allocator or device changes, so the
    /// worker re-initialises its allocator.
    generation: u64,
    /// Samples delivered (diagnostics).
    delivered: u64,
    /// Of which carried a ring frame (not the placeholder).
    delivered_live: u64,
}

/// The media stream COM object.
#[implement(IMFMediaStream2, IKsControl)]
pub struct MediaStream {
    _alive: super::super::Alive,
    source: Mutex<Option<Agile<IMFMediaSource>>>,
    descriptor: Agile<IMFStreamDescriptor>,
    attributes: Agile<IMFAttributes>,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

fn ks_not_found() -> windows_core::Error {
    windows_core::Error::from_hresult(ERROR_SET_NOT_FOUND.to_hresult())
}

impl MediaStream {
    /// A stream for `source` described by `descriptor`.
    ///
    /// # Errors
    /// `MFCreateEventQueue` failure.
    pub fn new(
        source: IMFMediaSource,
        descriptor: IMFStreamDescriptor,
        attributes: IMFAttributes,
        ring: Option<RingConfig>,
    ) -> windows_core::Result<Self> {
        // SAFETY: creating a Media Foundation event queue has no preconditions
        // beyond MFStartup, which the host process has done.
        let queue = unsafe { windows::Win32::Media::MediaFoundation::MFCreateEventQueue()? };
        Ok(Self {
            _alive: super::super::Alive::new(),
            source: Mutex::new(Some(Agile(source))),
            descriptor: Agile(descriptor.clone()),
            attributes: Agile(attributes),
            shared: Arc::new(Shared {
                queue: Agile(queue),
                ring,
                descriptor: Agile(descriptor),
                state: Mutex::new(State {
                    stream_state: MF_STREAM_STATE_STOPPED,
                    shutdown: false,
                    active: false,
                    pending: VecDeque::new(),
                    format: MEDIA_FORMATS[0],
                    media_type: None,
                    provided_allocator: None,
                    d3d_manager: None,
                    generation: 0,
                    delivered: 0,
                    delivered_live: 0,
                }),
                wake: Condvar::new(),
            }),
            worker: Mutex::new(None),
        })
    }

    /// The stream attributes (`GetStreamAttributes`).
    #[must_use]
    pub fn attributes(&self) -> IMFAttributes {
        self.attributes.0.clone()
    }

    /// Whether the stream is selected and started.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.shared.state.lock().active
    }

    /// `(delivered, delivered_live)` sample counts.
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        let s = self.shared.state.lock();
        (s.delivered, s.delivered_live)
    }

    /// Adopt `t` as the output type (it must be one of [`MEDIA_FORMATS`]).
    ///
    /// # Errors
    /// `MF_E_INVALIDMEDIATYPE` for a type we do not offer.
    pub fn set_media_type(&self, t: &IMFMediaType) -> windows_core::Result<()> {
        let format = super::parse_media_type(t).ok_or_else(|| {
            windows_core::Error::from_hresult(
                windows::Win32::Media::MediaFoundation::MF_E_INVALIDMEDIATYPE,
            )
        })?;
        let mut s = self.shared.state.lock();
        s.format = format;
        s.media_type = Some(Agile(t.clone()));
        s.generation += 1;
        Ok(())
    }

    /// Use the Frame Server's allocator for samples.
    pub fn set_allocator(&self, allocator: Option<IMFVideoSampleAllocator>) {
        let mut s = self.shared.state.lock();
        s.provided_allocator = allocator.map(Agile);
        s.generation += 1;
    }

    /// Hand the D3D device manager to the allocator.
    pub fn set_d3d_manager(&self, manager: Option<IUnknown>) {
        let mut s = self.shared.state.lock();
        s.d3d_manager = manager.map(Agile);
        s.generation += 1;
    }

    fn ensure_worker(&self) {
        let mut worker = self.worker.lock();
        if worker.is_none() {
            let shared = self.shared.clone();
            *worker = std::thread::Builder::new()
                .name("oak-mf-vcam stream".into())
                .spawn(move || run_worker(&shared))
                .ok();
        }
    }

    /// Start (or restart) the stream at `time` with `media_type` (the
    /// consumer's current type, if it set one), queuing `MEStreamStarted`.
    ///
    /// # Errors
    /// `MF_E_SHUTDOWN`, an unsupported type, or a failing queue call.
    pub fn start(&self, media_type: Option<&IMFMediaType>, time: i64) -> windows_core::Result<()> {
        if let Some(t) = media_type {
            self.set_media_type(t)?;
        }
        {
            let mut s = self.shared.state.lock();
            if s.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            s.active = true;
            s.stream_state = MF_STREAM_STATE_RUNNING;
        }
        self.ensure_worker();
        self.shared.wake.notify_all();
        // SAFETY: queueing an event with a VT_I8 time on our live queue.
        unsafe {
            self.shared.queue.QueueEventParamVar(
                MEStreamStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &PROPVARIANT::from(time),
            )
        }
    }

    /// Stop the stream, dropping outstanding requests, and queue
    /// `MEStreamStopped`.
    ///
    /// # Errors
    /// A failing queue call.
    pub fn stop(&self) -> windows_core::Result<()> {
        let was_active = {
            let mut s = self.shared.state.lock();
            let was = s.active;
            s.active = false;
            s.stream_state = MF_STREAM_STATE_STOPPED;
            s.pending.clear();
            was
        };
        if was_active {
            // SAFETY: queueing a plain event on our live queue.
            unsafe {
                self.shared.queue.QueueEventParamVar(
                    MEStreamStopped.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &PROPVARIANT::default(),
                )?;
            }
        }
        Ok(())
    }

    /// Tear down: stop the worker, shut the queue, and drop the reference to
    /// the source (breaking the source <-> stream cycle).
    pub fn shutdown(&self) {
        {
            let mut s = self.shared.state.lock();
            s.shutdown = true;
            s.active = false;
            s.pending.clear();
            s.provided_allocator = None;
            s.d3d_manager = None;
        }
        self.shared.wake.notify_all();
        if let Some(handle) = self.worker.lock().take() {
            let _ = handle.join();
        }
        // SAFETY: shutting down our own queue.
        unsafe {
            let _ = self.shared.queue.Shutdown();
        }
        *self.source.lock() = None;
    }

    fn check_live(&self) -> windows_core::Result<()> {
        if self.shared.state.lock().shutdown {
            Err(MF_E_SHUTDOWN.into())
        } else {
            Ok(())
        }
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(
        &self,
        dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows_core::Result<IMFMediaEvent> {
        self.check_live()?;
        let queue = self.shared.queue.clone();
        // SAFETY: a blocking read on our own queue, without holding a lock.
        unsafe { queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(
        &self,
        pcallback: Ref<IMFAsyncCallback>,
        punkstate: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        self.check_live()?;
        // SAFETY: forwarding the caller's callback and state to our queue.
        unsafe {
            self.shared
                .queue
                .BeginGetEvent(pcallback.ok()?, punkstate.as_ref())
        }
    }

    fn EndGetEvent(&self, presult: Ref<IMFAsyncResult>) -> windows_core::Result<IMFMediaEvent> {
        self.check_live()?;
        // SAFETY: forwarding the async result to our queue.
        unsafe { self.shared.queue.EndGetEvent(presult.ok()?) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        pvvalue: *const PROPVARIANT,
    ) -> windows_core::Result<()> {
        self.check_live()?;
        // SAFETY: the caller's pointers are forwarded unchanged.
        unsafe {
            self.shared
                .queue
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> windows_core::Result<IMFMediaSource> {
        self.check_live()?;
        self.source
            .lock()
            .as_ref()
            .map(|s| s.0.clone())
            .ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> windows_core::Result<IMFStreamDescriptor> {
        self.check_live()?;
        Ok(self.descriptor.0.clone())
    }

    fn RequestSample(&self, ptoken: Ref<IUnknown>) -> windows_core::Result<()> {
        {
            let mut s = self.shared.state.lock();
            if s.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            if s.stream_state != MF_STREAM_STATE_RUNNING {
                return Err(MF_E_INVALIDREQUEST.into());
            }
            s.pending.push_back(ptoken.cloned().map(Agile));
        }
        self.shared.wake.notify_all();
        Ok(())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows_core::Result<()> {
        {
            let mut s = self.shared.state.lock();
            if s.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            s.stream_state = value;
            if value != MF_STREAM_STATE_RUNNING {
                s.pending.clear();
            }
        }
        if value == MF_STREAM_STATE_RUNNING {
            self.ensure_worker();
        }
        self.shared.wake.notify_all();
        Ok(())
    }

    fn GetStreamState(&self) -> windows_core::Result<MF_STREAM_STATE> {
        let s = self.shared.state.lock();
        if s.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(s.stream_state)
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(
        &self,
        _property: *const KSIDENTIFIER,
        _propertylength: u32,
        _propertydata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ks_not_found())
    }

    fn KsMethod(
        &self,
        _method: *const KSIDENTIFIER,
        _methodlength: u32,
        _methoddata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ks_not_found())
    }

    fn KsEvent(
        &self,
        _event: *const KSIDENTIFIER,
        _eventlength: u32,
        _eventdata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ks_not_found())
    }
}

/// Per-worker resources, allocated once (or once per format change), never
/// per frame.
struct Worker {
    ring: Option<SharedRing>,
    last_ring_attempt: Option<Instant>,
    /// The newest ring frame (NV12, up to 4K) and its geometry.
    staging: Vec<u8>,
    staging_frame: Option<(u32, u32)>,
    last_frame: u64,
    /// The placeholder at the current output size.
    placeholder: Vec<u8>,
    placeholder_size: (u32, u32),
    allocator: Option<Agile<IMFVideoSampleAllocator>>,
    generation: u64,
    next_due: Option<Instant>,
}

fn run_worker(shared: &Shared) {
    // Counted so the DLL is not unloaded under a running worker.
    let _alive = super::super::Alive::new();
    let mut w = Worker {
        ring: None,
        last_ring_attempt: None,
        staging: vec![0u8; SLOT_CAPACITY],
        staging_frame: None,
        last_frame: 0,
        placeholder: Vec::new(),
        placeholder_size: (0, 0),
        allocator: None,
        generation: u64::MAX,
        next_due: None,
    };
    loop {
        let (token, format, media_type, generation, provided, d3d) = {
            let mut s = shared.state.lock();
            loop {
                if s.shutdown {
                    return;
                }
                if s.stream_state == MF_STREAM_STATE_RUNNING && !s.pending.is_empty() {
                    break;
                }
                shared.wake.wait(&mut s);
            }
            (
                s.pending.pop_front().flatten(),
                s.format,
                s.media_type.clone(),
                s.generation,
                s.provided_allocator.clone(),
                s.d3d_manager.clone(),
            )
        };
        // Follow a type the consumer set on the descriptor's handler while
        // streaming (a Source Reader does this without restarting us).
        let (format, media_type, generation) = match current_type(&shared.descriptor) {
            Some((f, t)) if f != format => {
                let mut s = shared.state.lock();
                s.format = f;
                s.media_type = Some(Agile(t.clone()));
                s.generation += 1;
                (f, Some(Agile(t)), s.generation)
            }
            _ => (format, media_type, generation),
        };
        if generation != w.generation {
            w.allocator = init_allocator(format, media_type.as_ref(), provided, d3d);
            w.generation = generation;
        }
        let interval = Duration::from_nanos(format.frame_duration_100ns() * 100);
        let live = w.pace_and_fetch(interval, shared.ring.as_ref());
        match w.deliver(shared, format, token, live) {
            Ok(()) => {
                let mut s = shared.state.lock();
                s.delivered += 1;
                if live {
                    s.delivered_live += 1;
                }
            }
            Err(e) => {
                // SAFETY: queueing an error event on our live queue.
                unsafe {
                    let _ = shared.queue.QueueEventParamVar(
                        MEError.0 as u32,
                        &GUID::zeroed(),
                        e.code(),
                        &PROPVARIANT::default(),
                    );
                }
            }
        }
    }
}

/// The descriptor handler's current type, if it is one we advertise.
fn current_type(descriptor: &IMFStreamDescriptor) -> Option<(MediaFormat, IMFMediaType)> {
    // SAFETY: plain reads on a live stream descriptor.
    let t = unsafe {
        descriptor
            .GetMediaTypeHandler()
            .ok()?
            .GetCurrentMediaType()
            .ok()?
    };
    super::parse_media_type(&t).map(|f| (f, t))
}

/// Initialise the sample allocator for `format`: the Frame Server's when it
/// provided one, else our own. `None` falls back to plain memory samples.
fn init_allocator(
    format: MediaFormat,
    media_type: Option<&Agile<IMFMediaType>>,
    provided: Option<Agile<IMFVideoSampleAllocator>>,
    d3d: Option<Agile<IUnknown>>,
) -> Option<Agile<IMFVideoSampleAllocator>> {
    let media_type = match media_type {
        Some(t) => t.0.clone(),
        None => super::media_type(&format).ok()?,
    };
    let allocator = match provided {
        Some(a) => a.0,
        None => {
            let mut raw = std::ptr::null_mut();
            // SAFETY: requesting IMFVideoSampleAllocator into `raw`, which we
            // take ownership of on success.
            unsafe {
                MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocator::IID, &mut raw).ok()?;
                IMFVideoSampleAllocator::from_raw(raw)
            }
        }
    };
    // SAFETY: configuring a live allocator with a live device manager / type.
    unsafe {
        if let Some(manager) = d3d {
            let _ = allocator.SetDirectXManager(&manager.0);
        }
        allocator
            .InitializeSampleAllocator(ALLOCATOR_SAMPLES, &media_type)
            .ok()?;
    }
    Some(Agile(allocator))
}

impl Worker {
    /// Wait until the next frame is due, then pull the newest ring frame into
    /// `staging`. Returns whether a live (non-placeholder) frame is available.
    fn pace_and_fetch(&mut self, interval: Duration, config: Option<&RingConfig>) -> bool {
        let now = Instant::now();
        let due = self.next_due.unwrap_or(now);
        if due > now {
            std::thread::sleep(due - now);
        }
        let now = Instant::now();
        // Keep a steady cadence, but never try to catch up a backlog.
        self.next_due = Some(if due + interval < now {
            now + interval
        } else {
            due + interval
        });

        if let Some(config) = config
            && self.ring.is_none()
            && self
                .last_ring_attempt
                .is_none_or(|t| t.elapsed() >= RING_RETRY)
        {
            self.last_ring_attempt = Some(Instant::now());
            self.ring = SharedRing::open(config, Role::Consumer, true).ok();
        }
        let Some(ring) = self.ring.as_ref() else {
            return false;
        };
        let r = ring.ring();
        let fresh =
            super::super::now_100ns().saturating_sub(r.heartbeat_100ns()) < STALE_AFTER_100NS;
        if !fresh {
            return false;
        }
        if r.latest() <= self.last_frame {
            // Nothing new yet: give the producer half a frame to deliver.
            let half = (interval.as_millis() / 2) as u32;
            ring.wait(half);
        }
        if r.latest() > self.last_frame {
            match r.read_latest(self.last_frame, &mut self.staging) {
                Some(meta) => {
                    self.last_frame = meta.frame;
                    self.staging_frame = Some((meta.width, meta.height));
                }
                None => {
                    // A failed read may have left a partial copy behind.
                    self.staging_frame = None;
                }
            }
        }
        self.staging_frame.is_some()
    }

    fn source_frame(&mut self, format: MediaFormat, live: bool) -> Nv12Frame<'_> {
        if live && let Some((width, height)) = self.staging_frame {
            return Nv12Frame {
                data: &self.staging[..nv12_size(width, height)],
                width,
                height,
            };
        }
        let size = (format.width, format.height);
        if self.placeholder_size != size {
            self.placeholder = vec![0u8; nv12_size(size.0, size.1)];
            paint_placeholder_nv12(&mut self.placeholder, size.0, size.1);
            self.placeholder_size = size;
        }
        Nv12Frame {
            data: &self.placeholder,
            width: size.0,
            height: size.1,
        }
    }

    fn allocate(&self, format: MediaFormat) -> windows_core::Result<IMFSample> {
        if let Some(allocator) = self.allocator.as_ref() {
            for _ in 0..20 {
                // SAFETY: allocating from an initialised allocator.
                match unsafe { allocator.AllocateSample() } {
                    Ok(sample) => return Ok(sample),
                    Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        // Fallback: a system-memory sample (allocates; used only when no
        // allocator could be initialised or it stays exhausted).
        // SAFETY: creating and assembling a fresh sample.
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(format.sample_size() as u32)?;
            sample.AddBuffer(&buffer)?;
            Ok(sample)
        }
    }

    fn deliver(
        &mut self,
        shared: &Shared,
        format: MediaFormat,
        token: Option<Agile<IUnknown>>,
        live: bool,
    ) -> windows_core::Result<()> {
        let sample = self.allocate(format)?;
        // SAFETY: the sample has one buffer (allocator samples and our
        // fallback alike).
        let buffer: IMFMediaBuffer = unsafe { sample.GetBufferByIndex(0)? };
        let src = self.source_frame(format, live);
        fill_buffer(&buffer, &src, format)?;
        // SAFETY: stamping and queueing a live sample.
        unsafe {
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(format.frame_duration_100ns() as i64)?;
            if let Some(token) = token {
                sample.SetUnknown(&MFSampleExtension_Token, &token.0)?;
            }
            shared
                .queue
                .QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        }
    }
}

/// Render `src` into `buffer` as `format`, through the 2-D interface when the
/// buffer has one (it gives the real pitch), else as a contiguous buffer.
fn fill_buffer(
    buffer: &IMFMediaBuffer,
    src: &Nv12Frame<'_>,
    format: MediaFormat,
) -> windows_core::Result<()> {
    let invalid = || windows_core::Error::from_hresult(MF_E_INVALIDREQUEST);
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        let mut start = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: locking a live 2-D buffer for writing; unlocked below.
        unsafe {
            two_d.Lock2DSize(
                MF2DBuffer_LockFlags_Write,
                &mut scan0,
                &mut pitch,
                &mut start,
                &mut len,
            )?;
        }
        let ok = pitch > 0 && !scan0.is_null() && {
            let pitch = pitch as usize;
            let avail = (start as usize + len as usize).saturating_sub(scan0 as usize);
            let need = required_len(format.format, format.height, pitch);
            need <= avail && {
                // SAFETY: [scan0, scan0 + need) lies inside the locked buffer
                // (checked against its start and length above).
                let dst = unsafe { std::slice::from_raw_parts_mut(scan0, need) };
                render(src, format.format, format.width, format.height, dst, pitch)
            }
        };
        // SAFETY: balancing the Lock2DSize above.
        unsafe {
            two_d.Unlock2D()?;
        }
        return if ok { Ok(()) } else { Err(invalid()) };
    }
    let mut data = std::ptr::null_mut();
    let mut max = 0u32;
    // SAFETY: locking a live buffer; unlocked below.
    unsafe {
        buffer.Lock(&mut data, Some(&mut max), None)?;
    }
    let need = format.sample_size();
    let ok = !data.is_null() && need <= max as usize && {
        // SAFETY: `data` points at `max >= need` locked bytes.
        let dst = unsafe { std::slice::from_raw_parts_mut(data, need) };
        render(
            src,
            format.format,
            format.width,
            format.height,
            dst,
            format.default_stride(),
        )
    };
    // SAFETY: balancing the Lock above, then recording the payload length.
    unsafe {
        buffer.Unlock()?;
        if ok {
            buffer.SetCurrentLength(need as u32)?;
        }
    }
    if ok { Ok(()) } else { Err(invalid()) }
}
