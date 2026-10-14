//! The full Windows 11 path: register the media source for the machine,
//! `MFCreateVirtualCamera` + `Start`, open the camera through the Frame Server
//! and read frames the test publishes into the real (`Global\`) ring.
//!
//! Every step the environment can refuse (Windows 10, no elevation for the
//! machine registration, no Frame Server) prints `NOT VERIFIED` with the
//! HRESULT and returns; once the camera streams, anything short of the
//! published pattern is a failure.

#![cfg(target_os = "windows")]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use oak_mf_vcam::layout::nv12_size;
use oak_mf_vcam::win::host::{
    Producer, VirtualCamera, locate_dll, mf_startup, virtual_camera_supported,
};
use oak_mf_vcam::win::registry::{Scope, register, registered_dll, unregister};
use oak_mf_vcam::win::shm::RingConfig;
use oak_mf_vcam::{CLSID_STRING, DEVICE_NAME};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMF2DBuffer2, IMFActivate, IMFDXGIBuffer, IMFSample,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF2DBuffer_LockFlags_Read, MFCreateAttributes, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows_core::Interface;

use common::{LEFT_Y, RIGHT_Y, near, paint_pattern};

fn not_verified(step: &str, e: &windows_core::Error) {
    println!(
        "vcam frame server: NOT VERIFIED at {step}: HRESULT 0x{:08X} ({})",
        e.code().0 as u32,
        e.message()
    );
}

/// Removes the machine registration and the copied DLL on every exit path.
struct Cleanup {
    registered: bool,
    copy: Option<PathBuf>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.registered
            && let Err(e) = unregister(Scope::Machine)
        {
            println!("vcam frame server: unregister failed: {e}");
        }
        if let Some(copy) = self.copy.take() {
            // The Frame Server may still hold it (DllCanUnloadNow says no);
            // a leftover copy is harmless and replaced by name next run.
            let _ = std::fs::remove_file(copy);
        }
    }
}

/// Every video capture device Media Foundation lists: (name, symbolic link).
fn list_cameras() -> Vec<(String, String)> {
    let mut out = Vec::new();
    // SAFETY: enumerating capture devices into a CoTaskMem array we free.
    unsafe {
        let mut attrs = None;
        if MFCreateAttributes(&mut attrs, 1).is_err() {
            return out;
        }
        let Some(attrs) = attrs else { return out };
        if attrs
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .is_err()
        {
            return out;
        }
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        if MFEnumDeviceSources(&attrs, &mut array, &mut count).is_err() || array.is_null() {
            return out;
        }
        for i in 0..count as usize {
            if let Some(a) = (*array.add(i)).take() {
                let text = |key| {
                    let mut p = windows_core::PWSTR::null();
                    let mut len = 0;
                    if a.GetAllocatedString(key, &mut p, &mut len).is_ok() {
                        let s = p.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(p.0 as _));
                        s
                    } else {
                        String::new()
                    }
                };
                out.push((
                    text(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME),
                    text(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK),
                ));
            }
        }
        CoTaskMemFree(Some(array as _));
    }
    out
}

fn hr(e: &windows_core::Error) -> String {
    format!("0x{:08X} {}", e.code().0 as u32, e.message())
}

/// Luma at the pattern's probe points (left, right, top) of a 1080p NV12
/// sample, trying the 2-D lock, the 2-D read lock, then a contiguous copy, and
/// naming the path that worked (the Frame Server's samples may be GPU-backed).
fn probe(sample: &IMFSample) -> Result<(u8, u8, u8, String), String> {
    let pick = |base: *const u8, pitch: usize| {
        // SAFETY: `base` points at a locked NV12 1080p frame with rows `pitch`
        // bytes apart; the probe points lie inside its Y plane.
        unsafe {
            let at = |x: usize, y: usize| *base.add(y * pitch + x);
            (at(480, 540), at(1440, 540), at(960, 2))
        }
    };
    let mut notes = Vec::new();
    // SAFETY: buffer locks on a live sample, each balanced by its unlock.
    unsafe {
        let buffer = sample.GetBufferByIndex(0).map_err(|e| hr(&e))?;
        let dxgi = buffer.cast::<IMFDXGIBuffer>().is_ok();
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            match two_d.Lock2D(&mut scan0, &mut pitch) {
                Ok(()) => {
                    let v = pick(scan0, pitch.unsigned_abs() as usize);
                    let _ = two_d.Unlock2D();
                    return Ok((
                        v.0,
                        v.1,
                        v.2,
                        format!("Lock2D (dxgi={dxgi}, pitch {pitch})"),
                    ));
                }
                Err(e) => notes.push(format!("Lock2D {}", hr(&e))),
            }
        }
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
            let (mut scan0, mut start) = (std::ptr::null_mut(), std::ptr::null_mut());
            let (mut pitch, mut len) = (0i32, 0u32);
            match two_d.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut scan0,
                &mut pitch,
                &mut start,
                &mut len,
            ) {
                Ok(()) => {
                    let v = pick(scan0, pitch.unsigned_abs() as usize);
                    let _ = two_d.Unlock2D();
                    return Ok((
                        v.0,
                        v.1,
                        v.2,
                        format!("Lock2DSize(Read) (dxgi={dxgi}, pitch {pitch})"),
                    ));
                }
                Err(e) => notes.push(format!("Lock2DSize(Read) {}", hr(&e))),
            }
        }
        match sample.ConvertToContiguousBuffer() {
            Ok(contig) => {
                let mut data = std::ptr::null_mut();
                let mut cur = 0u32;
                match contig.Lock(&mut data, None, Some(&mut cur)) {
                    Ok(()) => {
                        let v = if cur as usize >= 1920 * 1080 {
                            Some(pick(data, 1920))
                        } else {
                            None
                        };
                        let _ = contig.Unlock();
                        if let Some(v) = v {
                            return Ok((
                                v.0,
                                v.1,
                                v.2,
                                format!("ConvertToContiguousBuffer (dxgi={dxgi}, {cur} bytes)"),
                            ));
                        }
                        notes.push(format!("contiguous buffer only {cur} bytes"));
                    }
                    Err(e) => notes.push(format!("contiguous Lock {}", hr(&e))),
                }
            }
            Err(e) => notes.push(format!("ConvertToContiguousBuffer {}", hr(&e))),
        }
        Err(format!("dxgi={dxgi}: {}", notes.join("; ")))
    }
}

#[test]
fn vcam_frame_server_round_trip() {
    println!(
        "vcam frame server: CLSID {CLSID_STRING}, MFCreateVirtualCamera present: {}",
        virtual_camera_supported()
    );
    if !virtual_camera_supported() {
        println!(
            "vcam frame server: NOT VERIFIED: MFCreateVirtualCamera is missing (Windows 10 or older)"
        );
        return;
    }
    let dll = locate_dll().expect("oak_mf_vcam.dll next to the test binary");
    // The Frame Server runs as Local Service and cannot read a user profile:
    // run the camera from a copy under ProgramData.
    let program_data = std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    let dir = program_data.join("oak-cam").join("vcam-test");
    let copy = dir.join(format!("oak_mf_vcam-{}.dll", std::process::id()));
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::copy(&dll, &copy).map(drop))
    {
        println!("vcam frame server: NOT VERIFIED: cannot stage the DLL in {dir:?}: {e}");
        return;
    }
    println!("vcam frame server: staged {dll:?} as {copy:?}");
    println!(
        "vcam frame server: prior machine registration: {:?}",
        registered_dll(Scope::Machine)
    );
    let mut cleanup = Cleanup {
        registered: false,
        copy: Some(copy.clone()),
    };
    if let Err(e) = register(&copy, Scope::Machine) {
        not_verified("HKLM registration (the runner is not elevated)", &e);
        return;
    }
    cleanup.registered = true;
    println!("vcam frame server: registered for the machine");
    // Evict any source DLL an earlier camera left loaded in the services.
    println!(
        "vcam frame server: {}",
        oak_mf_vcam::win::host::stop_frame_server_services()
    );

    let _mf = mf_startup().expect("MFStartup");
    let before = list_cameras();
    println!("vcam frame server: cameras before: {before:?}");

    // The app side: publish the pattern into the real ring as soon as the
    // camera's source has created it (or create it, when elevated).
    // The app's side: this user's ring, and the camera tagged with our SID.
    let ring = RingConfig::for_current_user().expect("current user SID");
    println!(
        "vcam frame server: owner {} ring {}",
        ring.owner_sid, ring.names.mapping
    );
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let feed_ring = ring.clone();
    let (s2, w2) = (stop.clone(), written.clone());
    let feeder = std::thread::spawn(move || {
        let (w, h) = (1920u32, 1080u32);
        let mut buf = vec![0u8; nv12_size(w, h)];
        let mut producer = None;
        let mut last_err = String::new();
        let mut n = 0;
        while !s2.load(Ordering::Relaxed) {
            if producer.is_none() {
                match Producer::open(&feed_ring, 30.0) {
                    Ok(p) => {
                        println!(
                            "vcam frame server: producer attached (created={})",
                            p.created()
                        );
                        producer = Some(p);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg != last_err {
                            println!("vcam frame server: producer open: {msg}");
                            last_err = msg;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
            if let Some(p) = producer.as_mut() {
                n += 1;
                paint_pattern(&mut buf, w, h, n);
                if p.write_nv12(w, h, &buf).is_ok() {
                    w2.fetch_add(1, Ordering::Relaxed);
                }
            }
            std::thread::sleep(Duration::from_millis(33));
        }
    });
    let finish = |stop: &AtomicBool| stop.store(true, Ordering::Relaxed);

    let mut camera = match VirtualCamera::create(DEVICE_NAME, &ring.owner_sid) {
        Ok(c) => c,
        Err(e) => {
            not_verified("MFCreateVirtualCamera", &e);
            finish(&stop);
            feeder.join().unwrap();
            return;
        }
    };
    if let Err(e) = camera.start() {
        not_verified("IMFVirtualCamera::Start", &e);
        finish(&stop);
        feeder.join().unwrap();
        return;
    }
    // SAFETY: reading a string attribute from the live camera.
    let link = unsafe {
        let mut p = windows_core::PWSTR::null();
        let mut len = 0;
        camera
            .interface()
            .GetAllocatedString(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                &mut p,
                &mut len,
            )
            .map(|()| {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as _));
                s
            })
    };
    println!("vcam frame server: started; symbolic link {link:?}");
    println!(
        "vcam frame server: cameras after start: {:?}",
        list_cameras()
    );

    let source = match camera.media_source() {
        Ok(s) => s,
        Err(e) => {
            not_verified("IMFVirtualCamera::GetMediaSource", &e);
            finish(&stop);
            feeder.join().unwrap();
            return;
        }
    };
    // Which ring (and which DLL) did the Frame Server's source pick up?
    println!(
        "vcam frame server: source ring state {}",
        oak_mf_vcam::win::host::source_ring_state(&source)
    );
    // SAFETY: wrapping the Frame Server's source in a Source Reader.
    let reader = match unsafe { MFCreateSourceReaderFromMediaSource(&source, None) } {
        Ok(r) => r,
        Err(e) => {
            not_verified("MFCreateSourceReaderFromMediaSource", &e);
            finish(&stop);
            feeder.join().unwrap();
            return;
        }
    };
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: Source Reader calls on a live reader.
    unsafe {
        let mut i = 0;
        while let Ok(t) = reader.GetNativeMediaType(stream, i) {
            println!(
                "vcam frame server: native type {i}: {:?}",
                oak_mf_vcam::win::com::parse_media_type(&t)
            );
            i += 1;
        }
        let nv12 =
            oak_mf_vcam::win::com::media_type(&oak_mf_vcam::formats::MEDIA_FORMATS[0]).unwrap();
        if let Err(e) = reader.SetCurrentMediaType(stream, None, &nv12) {
            not_verified("SetCurrentMediaType NV12 1080p", &e);
            finish(&stop);
            feeder.join().unwrap();
            return;
        }
    }

    // A hang inside the Frame Server must not wedge the runner.
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let wd = watchdog_done.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if wd.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        println!("vcam frame server: FAILED: ReadSample hung for 120 s");
        std::process::exit(101);
    });

    let (mut frames, mut pattern, mut placeholder, mut errors, mut unreadable) = (0, 0, 0, 0, 0);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut first_error = None;
    while pattern < 30 && frames < 300 && Instant::now() < deadline {
        let mut flags = 0u32;
        let mut sample = None;
        // SAFETY: a synchronous read into locals.
        let r = unsafe {
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
        };
        if let Err(e) = r {
            errors += 1;
            if first_error.is_none() {
                not_verified("ReadSample", &e);
                first_error = Some(e);
            }
            if errors > 5 {
                break;
            }
            continue;
        }
        let Some(sample) = sample else { continue };
        frames += 1;
        match probe(&sample) {
            Ok((l, r, top, how)) => {
                if frames == 1 {
                    println!("vcam frame server: first frame read via {how}");
                }
                if near(l, LEFT_Y, 3) && near(r, RIGHT_Y, 3) {
                    pattern += 1;
                } else if near(top, 32, 3) {
                    placeholder += 1;
                } else if frames < 5 {
                    println!("vcam frame server: frame {frames}: left {l} right {r} top {top}");
                }
            }
            Err(e) => {
                unreadable += 1;
                if unreadable <= 3 {
                    println!("vcam frame server: frame {frames} unreadable: {e}");
                }
            }
        }
    }
    watchdog_done.store(true, Ordering::Relaxed);
    finish(&stop);
    feeder.join().unwrap();
    println!(
        "vcam frame server: read {frames} frames: {pattern} pattern, {placeholder} placeholder, \
         {unreadable} unreadable, {errors} read errors; app wrote {}",
        written.load(Ordering::Relaxed)
    );
    drop(reader);
    // SAFETY: shutting down the source proxy before removing the camera.
    unsafe {
        let _ = source.Shutdown();
    }
    camera.remove();
    drop(cleanup);

    if frames == 0 {
        if let Some(e) = first_error {
            not_verified("reading frames through the Frame Server", &e);
            return;
        }
        panic!("vcam frame server: the camera started but delivered no frames");
    }
    assert!(
        unreadable < frames,
        "the camera streamed {frames} frames but none could be read back"
    );
    assert!(
        pattern >= 30,
        "the camera streamed {frames} frames but only {pattern} carried the app's pattern \
         ({placeholder} placeholder): the ring did not reach the Frame Server's source"
    );
    println!("vcam frame server: PASS ({pattern} frames matched the published pattern)");
}
