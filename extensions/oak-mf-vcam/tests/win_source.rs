//! The media source in-process, without the Frame Server: the class factory
//! (through the rlib, through `LoadLibrary` of the built DLL, and through
//! `CoCreateInstance` with a per-user registration), the advertised media
//! types, and a Source Reader round trip that reads the ring's frames back in
//! every advertised pixel format.

#![cfg(target_os = "windows")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use oak_mf_vcam::formats::{MEDIA_FORMATS, MediaFormat, OutputFormat};
use oak_mf_vcam::layout::nv12_size;
use oak_mf_vcam::win::com::activate::create_activate;
use oak_mf_vcam::win::com::{media_type, parse_media_type};
use oak_mf_vcam::win::host::{MfSession, Producer, locate_dll, mf_startup};
use oak_mf_vcam::win::registry::{Scope, register, registered_dll, unregister};
use oak_mf_vcam::win::shm::{RingConfig, RingNames, set_process_names};
use parking_lot::Mutex;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFMediaSource, IMFSample, IMFSourceReader,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MFCreateSourceReaderFromMediaSource,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IClassFactory};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_core::{GUID, HRESULT, HSTRING, Interface, s};

use common::{LEFT_Y, MARKER, RIGHT_Y, near, paint_pattern};

/// The tests share the process-wide ring-name override: run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn mf() -> MfSession {
    mf_startup().expect("MFStartup")
}

fn activate_source(activate: &IMFActivate) -> IMFMediaSource {
    // SAFETY: activating the source for the IMFMediaSource interface.
    unsafe { activate.ActivateObject::<IMFMediaSource>() }.expect("ActivateObject")
}

/// The advertised types in order, read back through the presentation
/// descriptor.
fn advertised(source: &IMFMediaSource) -> Vec<MediaFormat> {
    // SAFETY: plain Media Foundation reads on live objects.
    unsafe {
        let pd = source.CreatePresentationDescriptor().expect("PD");
        assert_eq!(pd.GetStreamDescriptorCount().unwrap(), 1);
        let mut selected = windows_core::BOOL(0);
        let mut sd = None;
        pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)
            .unwrap();
        assert!(selected.as_bool(), "stream 0 selected by default");
        let handler = sd.unwrap().GetMediaTypeHandler().unwrap();
        let count = handler.GetMediaTypeCount().unwrap();
        let types: Vec<MediaFormat> = (0..count)
            .map(|i| {
                parse_media_type(&handler.GetMediaTypeByIndex(i).unwrap())
                    .expect("an advertised type parses")
            })
            .collect();
        let current = handler.GetCurrentMediaType().expect("a current type");
        assert_eq!(parse_media_type(&current), Some(MEDIA_FORMATS[0]));
        types
    }
}

fn shutdown(source: &IMFMediaSource) {
    // SAFETY: shutting down a live source.
    unsafe {
        let _ = source.Shutdown();
    }
}

/// The rlib's activation object yields a source advertising the six types.
#[test]
fn vcam_source_lists_media_types() {
    let _g = SERIAL.lock();
    let _mf = mf();
    let activate = create_activate().expect("activate");
    let source = activate_source(&activate);
    let types = advertised(&source);
    eprintln!("vcam advertised types: {types:?}");
    assert_eq!(types, MEDIA_FORMATS);
    // SAFETY: plain reads.
    unsafe {
        assert_eq!(
            source.GetCharacteristics().unwrap() & 1,
            1,
            "MFMEDIASOURCE_IS_LIVE"
        );
    }
    shutdown(&source);
}

/// `DllGetClassObject` from the built DLL (`LoadLibrary`) hands out a class
/// factory whose activation object yields the source.
#[test]
fn vcam_dll_get_class_object_yields_source() {
    let _g = SERIAL.lock();
    let _mf = mf();
    let Some(dll) = locate_dll() else {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap().parent().unwrap();
        let listing: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        panic!("vcam: oak_mf_vcam.dll not found near {exe:?}; {dir:?} holds {listing:?}");
    };
    eprintln!("vcam: loading {dll:?}");
    type GetClassObject =
        unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut core::ffi::c_void) -> HRESULT;
    // SAFETY: loading our own DLL and calling its documented export with
    // valid pointers; the factory pointer is taken over as an IClassFactory.
    unsafe {
        let module = LoadLibraryW(&HSTRING::from(dll.as_os_str())).expect("LoadLibraryW");
        let proc = GetProcAddress(module, s!("DllGetClassObject")).expect("export");
        let get: GetClassObject = std::mem::transmute(proc);
        let mut raw = std::ptr::null_mut();
        let clsid = oak_mf_vcam::win::clsid();
        get(&clsid, &IClassFactory::IID, &mut raw)
            .ok()
            .expect("DllGetClassObject");
        let factory = IClassFactory::from_raw(raw);
        let activate: IMFActivate = factory.CreateInstance(None).expect("CreateInstance");
        let source = activate_source(&activate);
        assert_eq!(advertised(&source), MEDIA_FORMATS);
        shutdown(&source);
        // A wrong CLSID is refused.
        let wrong = GUID::from_u128(1);
        let hr = get(&wrong, &IClassFactory::IID, &mut raw);
        assert_eq!(hr.0 as u32, 0x8004_0111, "CLASS_E_CLASSNOTAVAILABLE");
    }
}

/// Register the built DLL in `scope`, `CoCreateInstance` the class, and
/// restore the scope's previous registration.
fn cocreate_in(scope: Scope, dll: &std::path::Path) -> windows_core::Result<IMFActivate> {
    let before = registered_dll(scope);
    register(dll, scope)?;
    assert_eq!(registered_dll(scope).as_deref(), Some(dll));
    let clsid = oak_mf_vcam::win::clsid();
    // SAFETY: creating our registered class in-process.
    let result = unsafe { CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER) };
    match before {
        Some(prev) => register(&prev, scope).unwrap(),
        None => unregister(scope).unwrap(),
    }
    result
}

/// `CoCreateInstance` resolves the CLSID to the built DLL. A normal process
/// proves it with a per-user registration (no elevation); an elevated one
/// ignores `HKCU\Software\Classes` for COM activation, so there the machine
/// registration is used (and removed again).
#[test]
fn vcam_cocreate_with_registration() {
    let _g = SERIAL.lock();
    let _mf = mf();
    let dll = locate_dll().expect("oak_mf_vcam.dll next to the test binary");
    let activate = match cocreate_in(Scope::User, &dll) {
        Ok(a) => {
            eprintln!("vcam: CoCreateInstance via the per-user registration");
            a
        }
        Err(user_err) => {
            eprintln!(
                "vcam: per-user registration not used by COM here ({user_err}); \
                 trying the machine registration (elevated process)"
            );
            match cocreate_in(Scope::Machine, &dll) {
                Ok(a) => {
                    eprintln!("vcam: CoCreateInstance via the machine registration");
                    a
                }
                Err(e) if e.code().0 as u32 == 0x8007_0005 => {
                    // Not elevated either: the per-user failure is the result.
                    panic!("CoCreateInstance with the per-user registration: {user_err}");
                }
                Err(e) => panic!("CoCreateInstance with the machine registration: {e}"),
            }
        }
    };
    let source = activate_source(&activate);
    assert_eq!(advertised(&source), MEDIA_FORMATS);
    shutdown(&source);
}

struct Feed {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<u64>>,
}

impl Feed {
    /// Publish the pattern at 1920x1080 and 30 fps into `names`.
    fn start(names: &RingNames) -> Self {
        let mut producer = Producer::open(
            &RingConfig::with_names(names.clone()).expect("current user SID"),
            30.0,
        )
        .expect("producer opens a Local ring");
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let thread = std::thread::spawn(move || {
            let (w, h) = (1920u32, 1080u32);
            let mut buf = vec![0u8; nv12_size(w, h)];
            let mut n = 0;
            while !s2.load(Ordering::Relaxed) {
                n += 1;
                paint_pattern(&mut buf, w, h, n);
                producer.write_nv12(w, h, &buf).unwrap();
                std::thread::sleep(Duration::from_millis(33));
            }
            n
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let n = t.join().unwrap_or(0);
            eprintln!("vcam feed: published {n} frames");
        }
    }
}

/// Lock a sample's buffer and hand `(bytes, pitch)` to `f`.
fn with_pixels<R>(sample: &IMFSample, format: MediaFormat, f: impl FnOnce(&[u8], usize) -> R) -> R {
    // SAFETY: locking the sample's buffer for reading; unlocked before return.
    unsafe {
        let buffer = sample.GetBufferByIndex(0).expect("buffer");
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            two_d.Lock2D(&mut scan0, &mut pitch).expect("Lock2D");
            assert!(pitch > 0, "top-down buffer");
            let len =
                oak_mf_vcam::convert::required_len(format.format, format.height, pitch as usize);
            let r = f(std::slice::from_raw_parts(scan0, len), pitch as usize);
            two_d.Unlock2D().unwrap();
            r
        } else {
            let mut data = std::ptr::null_mut();
            let mut cur = 0u32;
            buffer.Lock(&mut data, None, Some(&mut cur)).expect("Lock");
            let r = f(
                std::slice::from_raw_parts(data, cur as usize),
                format.default_stride(),
            );
            buffer.Unlock().unwrap();
            r
        }
    }
}

/// Luma (or, for RGB32, green) at pixel (x, y) of a frame.
fn probe(bytes: &[u8], pitch: usize, format: OutputFormat, x: usize, y: usize) -> u8 {
    match format {
        OutputFormat::Nv12 => bytes[y * pitch + x],
        OutputFormat::Yuy2 => bytes[y * pitch + x * 2],
        OutputFormat::Rgb32 => bytes[y * pitch + x * 4 + 1],
    }
}

/// What a frame showed.
#[derive(Debug, Default)]
struct Tally {
    frames: usize,
    pattern: usize,
    placeholder: usize,
    markers: std::collections::BTreeSet<u8>,
}

/// Classify a frame: the pattern (left LEFT_Y, right RIGHT_Y at mid-height,
/// converted to RGB for RGB32) or the placeholder (grey top rows).
fn classify(tally: &mut Tally, bytes: &[u8], pitch: usize, m: MediaFormat) {
    let (w, h) = (m.width as usize, m.height as usize);
    let rgb = |y: u8| oak_mf_vcam::convert::bt709_to_rgb(y, 128, 128)[1];
    let (left, right, grey) = match m.format {
        OutputFormat::Rgb32 => (rgb(LEFT_Y), rgb(RIGHT_Y), rgb(32)),
        _ => (LEFT_Y, RIGHT_Y, 32),
    };
    let l = probe(bytes, pitch, m.format, w / 4, h / 2);
    let r = probe(bytes, pitch, m.format, w * 3 / 4, h / 2);
    let top = probe(bytes, pitch, m.format, w / 2, 2);
    tally.frames += 1;
    if near(l, left, 3) && near(r, right, 3) {
        tally.pattern += 1;
        // The marker is MARKER source pixels square; sample its middle.
        let mx = MARKER * w / 1920 / 2;
        tally.markers.insert(probe(bytes, pitch, m.format, mx, mx));
    } else if near(top, grey, 3) {
        tally.placeholder += 1;
    } else {
        eprintln!("vcam: unrecognised frame: left {l} right {r} top {top} ({m:?})");
    }
}

/// Read `count` samples from `reader` after selecting `m`.
fn read_frames(reader: &IMFSourceReader, m: MediaFormat, count: usize) -> Tally {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: Source Reader calls on a live reader with a type we built.
    unsafe {
        reader
            .SetCurrentMediaType(stream, None, &media_type(&m).unwrap())
            .expect("SetCurrentMediaType");
    }
    let mut tally = Tally::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    while tally.frames < count && Instant::now() < deadline {
        let mut flags = 0u32;
        let mut sample = None;
        // SAFETY: a synchronous read into locals.
        unsafe {
            reader
                .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .expect("ReadSample");
        }
        if let Some(sample) = sample {
            with_pixels(&sample, m, |bytes, pitch| {
                classify(&mut tally, bytes, pitch, m)
            });
        }
    }
    eprintln!("vcam read {m:?}: {tally:?}");
    tally
}

fn reader_for(source: &IMFMediaSource) -> IMFSourceReader {
    // SAFETY: wrapping a live source in a Source Reader.
    unsafe { MFCreateSourceReaderFromMediaSource(source, None) }.expect("Source Reader")
}

/// Source Reader round trip: with the app publishing, NV12 1080p reads back
/// the pattern exactly (and the marker changes), and YUY2/RGB32 at 720p read
/// back the converted, scaled pattern.
#[test]
fn vcam_source_reader_reads_ring_frames() {
    let _g = SERIAL.lock();
    let _mf = mf();
    let names = RingNames::with_prefix(&format!(r"Local\sc-vcam-reader-{}", std::process::id()));
    set_process_names(Some(names.clone()));
    let _feed = Feed::start(&names);
    // A fresh source and reader per format: no mid-stream type change.
    let read = |m: MediaFormat, count: usize| {
        let activate = create_activate().unwrap();
        let source = activate_source(&activate);
        let reader = reader_for(&source);
        let t = read_frames(&reader, m, count);
        drop(reader);
        shutdown(&source);
        t
    };
    let nv12 = read(MEDIA_FORMATS[0], 45);
    assert!(nv12.pattern >= 30, "NV12 1080p: {nv12:?}");
    assert!(nv12.markers.len() >= 10, "frames advance: {nv12:?}");
    for m in [MEDIA_FORMATS[3], MEDIA_FORMATS[5], MEDIA_FORMATS[1]] {
        let t = read(m, 15);
        assert!(t.pattern >= 10, "{m:?}: {t:?}");
    }
    set_process_names(None);
}

/// With nobody publishing, the source still delivers: the placeholder.
#[test]
fn vcam_source_reader_placeholder_without_app() {
    let _g = SERIAL.lock();
    let _mf = mf();
    let names = RingNames::with_prefix(&format!(r"Local\sc-vcam-idle-{}", std::process::id()));
    set_process_names(Some(names));
    let activate = create_activate().unwrap();
    let source = activate_source(&activate);
    let reader = reader_for(&source);
    let started = Instant::now();
    let t = read_frames(&reader, MEDIA_FORMATS[1], 20);
    let elapsed = started.elapsed();
    assert_eq!(t.placeholder, 20, "{t:?}");
    // Paced near 30 fps: 20 frames take at least ~0.5 s.
    assert!(elapsed >= Duration::from_millis(450), "paced: {elapsed:?}");
    drop(reader);
    shutdown(&source);
    set_process_names(None);
}
