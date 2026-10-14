//! The media source: one live video stream, the presentation descriptor
//! advertising [`MEDIA_FORMATS`], and the interfaces the Frame Server queries
//! (`IMFMediaSource2`, `IMFGetService`, `IKsControl`,
//! `IMFSampleAllocatorControl`).

use parking_lot::Mutex;
use windows::Win32::Foundation::{ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSCAMERAPROFILE_Legacy, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFGetService, IMFGetService_Impl,
    IMFMediaEvent, IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource,
    IMFMediaSource_Impl, IMFMediaSource2, IMFMediaSource2_Impl, IMFMediaSourceEx_Impl,
    IMFMediaType, IMFPresentationDescriptor, IMFSampleAllocatorControl,
    IMFSampleAllocatorControl_Impl, IMFSensorProfile, IMFSensorProfileCollection,
    IMFStreamDescriptor, IMFVideoSampleAllocator, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    MENewStream, MESourceStarted, MESourceStopped, MEUpdatedStream,
    MF_DEVICEMFT_SENSORPROFILE_COLLECTION, MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
    MF_DEVICESTREAM_FRAMESERVER_SHARED, MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID,
    MF_E_INVALID_STATE_TRANSITION, MF_E_INVALIDMEDIATYPE, MF_E_INVALIDSTREAMNUMBER, MF_E_SHUTDOWN,
    MF_E_UNSUPPORTED_SERVICE, MF_E_UNSUPPORTED_TIME_FORMAT, MFCreateAttributes, MFCreateEventQueue,
    MFCreatePresentationDescriptor, MFCreateStreamDescriptor, MFFrameSourceTypes_Color,
    MFGetSystemTime, MFMEDIASOURCE_IS_LIVE, MFSampleAllocatorUsage,
    MFSampleAllocatorUsage_UsesProvidedAllocator,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_core::{
    BOOL, ComObject, GUID, HRESULT, HSTRING, IUnknown, Interface, Ref, implement, s, w,
};

use super::super::shm::RingConfig;
use super::super::{ATTR_RING_STATE, Agile};
use super::stream::MediaStream;
use crate::formats::MEDIA_FORMATS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceState {
    Stopped,
    Started,
    Shutdown,
}

/// The media source COM object.
#[implement(IMFMediaSource2, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct MediaSource {
    _alive: super::super::Alive,
    queue: Agile<IMFMediaEventQueue>,
    attributes: Agile<IMFAttributes>,
    descriptor: Agile<IMFPresentationDescriptor>,
    stream: Mutex<Option<ComObject<MediaStream>>>,
    state: Mutex<SourceState>,
}

fn ks_not_found() -> windows_core::Error {
    windows_core::Error::from_hresult(ERROR_SET_NOT_FOUND.to_hresult())
}

/// Set the Frame Server's device-stream attributes on `attrs`.
fn set_stream_attributes(attrs: &IMFAttributes) -> windows_core::Result<()> {
    // SAFETY: attribute writes on a live store with constant GUIDs.
    unsafe {
        attrs.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        attrs.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
        attrs.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        attrs.SetUINT32(
            &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
            MFFrameSourceTypes_Color.0 as u32,
        )?;
    }
    Ok(())
}

/// Attach a sensor-profile collection with the Legacy profile (the Windows
/// camera samples call it mandatory so profile-unaware apps keep working).
/// Best effort: the two factory functions are resolved at run time from
/// `mfsensorgroup.dll`, so nothing here adds a static import to the app.
fn add_sensor_profiles(attrs: &IMFAttributes) -> windows_core::Result<()> {
    type CreateCollection = unsafe extern "system" fn(*mut *mut core::ffi::c_void) -> HRESULT;
    type CreateProfile = unsafe extern "system" fn(
        *const GUID,
        u32,
        windows_core::PCWSTR,
        *mut *mut core::ffi::c_void,
    ) -> HRESULT;
    let not_found = || windows_core::Error::from(windows::Win32::Foundation::E_NOTIMPL);
    // SAFETY: loading a System32 DLL, resolving two documented exports and
    // calling them with valid out-pointers; each returned +1 pointer is taken
    // over by `from_raw`.
    unsafe {
        let module = LoadLibraryExW(w!("mfsensorgroup.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let collection_fn =
            GetProcAddress(module, s!("MFCreateSensorProfileCollection")).ok_or_else(not_found)?;
        let profile_fn =
            GetProcAddress(module, s!("MFCreateSensorProfile")).ok_or_else(not_found)?;
        let create_collection: CreateCollection = std::mem::transmute(collection_fn);
        let create_profile: CreateProfile = std::mem::transmute(profile_fn);
        let mut raw = std::ptr::null_mut();
        create_collection(&mut raw).ok()?;
        let collection = IMFSensorProfileCollection::from_raw(raw);
        let mut raw = std::ptr::null_mut();
        create_profile(
            &KSCAMERAPROFILE_Legacy,
            0,
            windows_core::PCWSTR::null(),
            &mut raw,
        )
        .ok()?;
        let profile = IMFSensorProfile::from_raw(raw);
        profile.AddProfileFilter(0, w!("((RES==;FRT<=30,1;SUT==))"))?;
        collection.AddProfile(&profile)?;
        attrs.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &collection)?;
    }
    Ok(())
}

/// Build a media source reading `ring` (`None`: the placeholder only, for a
/// camera whose owner is unknown): the stream descriptor with every advertised type
/// (NV12 1080p30 current), the presentation descriptor (stream selected), the
/// event queue and the stream object.
///
/// # Errors
/// Any failing Media Foundation call.
pub fn create_source(ring: Option<RingConfig>) -> windows_core::Result<ComObject<MediaSource>> {
    let types = MEDIA_FORMATS
        .iter()
        .map(|m| super::media_type(m).map(Some))
        .collect::<windows_core::Result<Vec<Option<IMFMediaType>>>>()?;
    // SAFETY: plain Media Foundation object construction; every pointer
    // passed is to a live local.
    let (queue, attributes, stream_desc, stream_attrs, descriptor) = unsafe {
        let queue = MFCreateEventQueue()?;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 4)?;
        let attributes = attributes.ok_or_else(|| windows_core::Error::from(MF_E_SHUTDOWN))?;
        let state = match ring.as_ref() {
            Some(r) => format!("sid:{} ring:{}", r.owner_sid, r.names.mapping),
            None => "none".to_string(),
        };
        let state = format!(
            "{state} dll:{}",
            super::super::exports::module_path()
                .map_or_else(|| "?".to_string(), |p| p.display().to_string())
        );
        attributes.SetString(&ATTR_RING_STATE, &HSTRING::from(state))?;
        let stream_desc: IMFStreamDescriptor = MFCreateStreamDescriptor(0, &types)?;
        let handler = stream_desc.GetMediaTypeHandler()?;
        handler.SetCurrentMediaType(types[0].as_ref())?;
        set_stream_attributes(&stream_desc.cast::<IMFAttributes>()?)?;
        let _ = add_sensor_profiles(&attributes);
        let mut stream_attrs = None;
        MFCreateAttributes(&mut stream_attrs, 4)?;
        let stream_attrs = stream_attrs.ok_or_else(|| windows_core::Error::from(MF_E_SHUTDOWN))?;
        set_stream_attributes(&stream_attrs)?;
        let descriptor = MFCreatePresentationDescriptor(Some(&[Some(stream_desc.clone())]))?;
        descriptor.SelectStream(0)?;
        (queue, attributes, stream_desc, stream_attrs, descriptor)
    };
    let source = ComObject::new(MediaSource {
        _alive: super::super::Alive::new(),
        queue: Agile(queue),
        attributes: Agile(attributes),
        descriptor: Agile(descriptor),
        stream: Mutex::new(None),
        state: Mutex::new(SourceState::Stopped),
    });
    let stream = ComObject::new(MediaStream::new(
        source.cast::<IMFMediaSource>()?,
        stream_desc,
        stream_attrs,
        ring,
    )?);
    *source.stream.lock() = Some(stream);
    Ok(source)
}

impl MediaSource {
    fn stream(&self) -> windows_core::Result<ComObject<MediaStream>> {
        if *self.state.lock() == SourceState::Shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        self.stream
            .lock()
            .clone()
            .ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn check_live(&self) -> windows_core::Result<()> {
        if *self.state.lock() == SourceState::Shutdown {
            Err(MF_E_SHUTDOWN.into())
        } else {
            Ok(())
        }
    }

    /// `(delivered, delivered_live)` sample counts of the stream (tests).
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        self.stream.lock().as_ref().map_or((0, 0), |s| s.counters())
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(
        &self,
        dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows_core::Result<IMFMediaEvent> {
        self.check_live()?;
        let queue = self.queue.clone();
        // SAFETY: a (possibly blocking) read on our queue, no lock held.
        unsafe { queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(
        &self,
        pcallback: Ref<IMFAsyncCallback>,
        punkstate: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        self.check_live()?;
        // SAFETY: forwarding the caller's callback and state.
        unsafe {
            self.queue
                .BeginGetEvent(pcallback.ok()?, punkstate.as_ref())
        }
    }

    fn EndGetEvent(&self, presult: Ref<IMFAsyncResult>) -> windows_core::Result<IMFMediaEvent> {
        self.check_live()?;
        // SAFETY: forwarding the async result.
        unsafe { self.queue.EndGetEvent(presult.ok()?) }
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
            self.queue
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> windows_core::Result<u32> {
        self.check_live()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows_core::Result<IMFPresentationDescriptor> {
        self.check_live()?;
        // SAFETY: cloning our live descriptor.
        unsafe { self.descriptor.Clone() }
    }

    fn Start(
        &self,
        ppresentationdescriptor: Ref<IMFPresentationDescriptor>,
        pguidtimeformat: *const GUID,
        _pvarstartposition: *const PROPVARIANT,
    ) -> windows_core::Result<()> {
        let stream = self.stream()?;
        let pd = ppresentationdescriptor.ok()?;
        // SAFETY: a non-null time-format pointer comes from the caller and is
        // valid for the call.
        if !pguidtimeformat.is_null() && unsafe { *pguidtimeformat } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        let mut selected = BOOL(0);
        let mut sd: Option<IMFStreamDescriptor> = None;
        // SAFETY: reading stream 0 of the caller's descriptor.
        unsafe { pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)? };
        let sd = sd.ok_or_else(|| windows_core::Error::from(MF_E_INVALIDSTREAMNUMBER))?;
        // SAFETY: plain reads on a live stream descriptor.
        let (id, current) = unsafe {
            (
                sd.GetStreamIdentifier()?,
                sd.GetMediaTypeHandler()?.GetCurrentMediaType().ok(),
            )
        };
        if id != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        // SAFETY: the system clock read has no preconditions.
        let time = unsafe { MFGetSystemTime() };
        if selected.as_bool() {
            let was_active = stream.is_active();
            let unk: IUnknown = stream.to_interface();
            // SAFETY: event queue calls with live arguments.
            unsafe {
                self.descriptor.SelectStream(0)?;
                self.queue.QueueEventParamUnk(
                    if was_active {
                        MEUpdatedStream.0
                    } else {
                        MENewStream.0
                    } as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &unk,
                )?;
            }
            stream.start(current.as_ref(), time)?;
        } else {
            // SAFETY: deselecting on our own descriptor.
            unsafe { self.descriptor.DeselectStream(0)? };
            stream.stop()?;
        }
        *self.state.lock() = SourceState::Started;
        // SAFETY: queueing the start event with its VT_I8 time.
        unsafe {
            self.queue.QueueEventParamVar(
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &PROPVARIANT::from(time),
            )
        }
    }

    fn Stop(&self) -> windows_core::Result<()> {
        let stream = self.stream()?;
        stream.stop()?;
        *self.state.lock() = SourceState::Stopped;
        // SAFETY: the system clock read and a queue call with a live value.
        unsafe {
            let time = MFGetSystemTime();
            self.queue.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &PROPVARIANT::from(time),
            )
        }
    }

    fn Pause(&self) -> windows_core::Result<()> {
        self.check_live()?;
        // A live camera cannot pause.
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> windows_core::Result<()> {
        let stream = {
            let mut state = self.state.lock();
            if *state == SourceState::Shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            *state = SourceState::Shutdown;
            self.stream.lock().take()
        };
        if let Some(stream) = stream {
            stream.shutdown();
        }
        // SAFETY: shutting down our own queue.
        unsafe {
            let _ = self.queue.Shutdown();
        }
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> windows_core::Result<IMFAttributes> {
        self.check_live()?;
        // The Frame Server expects the live store, not a copy.
        Ok(self.attributes.0.clone())
    }

    fn GetStreamAttributes(&self, dwstreamidentifier: u32) -> windows_core::Result<IMFAttributes> {
        let stream = self.stream()?;
        if dwstreamidentifier != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        Ok(stream.attributes())
    }

    fn SetD3DManager(&self, pmanager: Ref<IUnknown>) -> windows_core::Result<()> {
        let stream = self.stream()?;
        stream.set_d3d_manager(pmanager.cloned());
        Ok(())
    }
}

impl IMFMediaSource2_Impl for MediaSource_Impl {
    fn SetMediaType(
        &self,
        dwstreamid: u32,
        pmediatype: Ref<IMFMediaType>,
    ) -> windows_core::Result<()> {
        let stream = self.stream()?;
        if dwstreamid != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        let t = pmediatype.ok()?;
        if super::parse_media_type(t).is_none() {
            return Err(MF_E_INVALIDMEDIATYPE.into());
        }
        // Keep the descriptor's current type in step with the stream.
        // SAFETY: reading our own stream descriptor and setting a type we
        // just validated.
        unsafe {
            let mut selected = BOOL(0);
            let mut sd: Option<IMFStreamDescriptor> = None;
            self.descriptor
                .GetStreamDescriptorByIndex(0, &mut selected, &mut sd)?;
            if let Some(sd) = sd {
                let _ = sd.GetMediaTypeHandler()?.SetCurrentMediaType(t);
            }
        }
        stream.set_media_type(t)
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(
        &self,
        _guidservice: *const GUID,
        _riid: *const GUID,
        _ppvobject: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
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

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(
        &self,
        dwoutputstreamid: u32,
        pallocator: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        let stream = self.stream()?;
        if dwoutputstreamid != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        let allocator = pallocator
            .as_ref()
            .and_then(|a| a.cast::<IMFVideoSampleAllocator>().ok());
        stream.set_allocator(allocator);
        Ok(())
    }

    fn GetAllocatorUsage(
        &self,
        dwoutputstreamid: u32,
        pdwinputstreamid: *mut u32,
        peusage: *mut MFSampleAllocatorUsage,
    ) -> windows_core::Result<()> {
        self.check_live()?;
        if dwoutputstreamid != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        if pdwinputstreamid.is_null() || peusage.is_null() {
            return Err(windows_core::Error::from_hresult(HRESULT(
                0x8000_4003_u32 as i32, // E_POINTER
            )));
        }
        // SAFETY: both out-pointers were checked non-null and point at the
        // caller's storage.
        unsafe {
            *pdwinputstreamid = dwoutputstreamid;
            *peusage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}
