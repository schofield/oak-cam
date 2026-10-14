//! Pipeline tests against a real OAK. Run with
//! `cargo test -p depthai -- --ignored` (a device must be attached and free).
//!
//! Only one process / pipeline can hold the device, so the tests serialise
//! on a lock.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use depthai::{
    ControlParams, Device, ImgType, OutputQueue, OutputSpec, PipelineBuilder, ResizeMode, Socket,
    UsbSpeed,
};

static DEVICE_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    DEVICE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens the first device, retrying while a previous test's device reboots.
fn open() -> Device {
    // As oak-cam's main: depthai 3.10.0 crashes saving a crash dump on Windows.
    #[cfg(windows)]
    depthai::env_default("DEPTHAI_CRASHDUMP", "0").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match Device::open("", UsbSpeed::SuperPlus) {
            Ok(d) => return d,
            Err(e) if Instant::now() < deadline => {
                eprintln!("open failed, retrying: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => panic!("cannot open device: {e}"),
        }
    }
}

fn spec(width: u32, height: u32, img_type: ImgType) -> OutputSpec {
    OutputSpec {
        width,
        height,
        img_type,
        resize: ResizeMode::Crop,
        fps: Some(30.0),
    }
}

#[allow(clippy::many_single_char_names)]
/// Receives `n` frames and checks size, type, monotonic seq and timestamps.
fn receive(queue: &OutputQueue, n: usize, w: u32, h: u32, ty: ImgType) {
    let mut last_seq = None;
    let mut last_ts = 0i64;
    let start = Instant::now();
    for i in 0..n {
        let frame = queue
            .get(Duration::from_secs(5))
            .expect("get")
            .unwrap_or_else(|| panic!("timeout waiting for frame {i}"));
        let v = frame.view();
        assert_eq!((v.width(), v.height()), (w, h), "frame {i}: {v:?}");
        assert_eq!(v.img_type(), ty, "frame {i}");
        assert!(v.stride() >= w, "stride {}", v.stride());
        match ty {
            ImgType::Nv12 => {
                let (y, uv) = v.nv12_planes().expect("nv12 planes");
                assert!(y.len() >= (w * h) as usize);
                assert!(uv.len() >= (w * h / 2) as usize);
            }
            ImgType::Gray8 => {
                assert!(v.gray8().expect("gray8").len() >= (w * h) as usize);
            }
            _ => {}
        }
        if let Some(prev) = last_seq {
            assert!(v.seq() > prev, "seq {} after {prev}", v.seq());
        }
        assert!(v.ts_host_ns() >= last_ts, "ts_host went backwards");
        let lat_ms = (depthai::steady_now_ns() - v.ts_host_ns()) as f64 / 1e6;
        assert!(
            (0.0..1000.0).contains(&lat_ms),
            "implausible latency {lat_ms} ms"
        );
        last_seq = Some(v.seq());
        last_ts = v.ts_host_ns();
        if i == 0 {
            eprintln!("first frame: {v:?}");
        }
    }
    let secs = start.elapsed().as_secs_f64();
    eprintln!("{n} frames in {secs:.2}s ({:.1} fps)", n as f64 / secs);
}

#[test]
#[ignore = "needs an OAK device"]
fn rgb_nv12_1080p30() {
    let _g = lock();
    let dev = open();
    let b = PipelineBuilder::new(&dev).unwrap();
    let cam = b.camera(Socket::CamA, None).unwrap();
    let out = cam.request_output(spec(1920, 1080, ImgType::Nv12)).unwrap();
    let q = out.create_queue(4, false).unwrap();
    let running = b.start().unwrap();
    assert!(running.is_running().unwrap());
    receive(&q, 30, 1920, 1080, ImgType::Nv12);
    running.stop().unwrap();
    assert!(!running.is_running().unwrap());
    assert!(q.is_closed().unwrap(), "queue should close on stop");
    assert!(q.get(Duration::from_millis(100)).is_err());
}

#[test]
#[ignore = "needs an OAK device"]
fn mono_gray8_800p30() {
    let _g = lock();
    let dev = open();
    let b = PipelineBuilder::new(&dev).unwrap();
    let cam = b.camera(Socket::CamB, None).unwrap();
    let q = cam
        .request_output(spec(1280, 800, ImgType::Gray8))
        .unwrap()
        .create_queue(4, false)
        .unwrap();
    let running = b.start().unwrap();
    receive(&q, 30, 1280, 800, ImgType::Gray8);
    // Queue dropped after the pipeline: Arc keeps the pipeline alive.
    drop(running);
    assert!(q.is_closed().unwrap());
    drop(q);
}

/// Waits for a frame satisfying `pred` within `within`; returns the
/// exposures seen.
fn wait_for(
    q: &OutputQueue,
    within: Duration,
    pred: impl Fn(u32, u32) -> bool,
) -> (bool, Vec<u32>) {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(f) = q.get(Duration::from_millis(500)).expect("get") {
            let v = f.view();
            seen.push(v.exposure_us());
            if pred(v.exposure_us(), v.iso()) {
                return (true, seen);
            }
        }
    }
    (false, seen)
}

#[test]
#[ignore = "needs an OAK device"]
fn manual_exposure_control_applies() {
    let _g = lock();
    let dev = open();
    let b = PipelineBuilder::new(&dev).unwrap();
    let cam = b.camera(Socket::CamA, None).unwrap();
    let q = cam
        .request_output(spec(1920, 1080, ImgType::Nv12))
        .unwrap()
        .create_queue(4, false)
        .unwrap();
    let ctl = cam.control_queue().unwrap();
    let _running = b.start().unwrap();

    // Let AE settle, then force two distinct manual exposures.
    receive(&q, 10, 1920, 1080, ImgType::Nv12);
    for (us, iso) in [(5_000u32, 400u32), (20_000, 200)] {
        ctl.send(&ControlParams::new().manual_exposure(us, iso))
            .unwrap();
        let (ok, seen) = wait_for(&q, Duration::from_secs(3), |e, i| {
            e.abs_diff(us) <= us / 20 && i.abs_diff(iso) <= iso / 5
        });
        assert!(
            ok,
            "exposure never reached {us} us / ISO {iso}; saw {seen:?}"
        );
        eprintln!("manual {us} us reached after {} frames", seen.len());
    }
    ctl.send(&ControlParams::new().auto_exposure()).unwrap();
}

#[test]
#[ignore = "needs an OAK device"]
fn initial_control_applies() {
    let _g = lock();
    let dev = open();
    let b = PipelineBuilder::new(&dev).unwrap();
    let cam = b.camera(Socket::CamC, None).unwrap();
    cam.set_initial_control(&ControlParams::new().manual_exposure(3_000, 800))
        .unwrap();
    let q = cam
        .request_output(spec(1280, 800, ImgType::Gray8))
        .unwrap()
        .create_queue(4, false)
        .unwrap();
    let _running = b.start().unwrap();
    let (ok, seen) = wait_for(&q, Duration::from_secs(3), |e, _| e.abs_diff(3_000) <= 150);
    assert!(ok, "initial exposure not applied; saw {seen:?}");
}

/// The documented contract: queues may outlive the running pipeline and the
/// `Device` handle; they then report closed, and nothing crashes.
#[test]
#[ignore = "needs an OAK device"]
fn queues_outlive_device() {
    let _g = lock();
    let dev = open();
    let b = PipelineBuilder::new(&dev).unwrap();
    let cam = b.camera(Socket::CamB, None).unwrap();
    let q = cam
        .request_output(spec(1280, 800, ImgType::Gray8))
        .unwrap()
        .create_queue(4, false)
        .unwrap();
    let ctl = cam.control_queue().unwrap();
    let running = b.start().unwrap();
    receive(&q, 5, 1280, 800, ImgType::Gray8);
    drop(running);
    drop(dev);
    assert!(q.is_closed().unwrap());
    assert!(q.get(Duration::from_millis(100)).is_err());
    let sent = ctl.send(&ControlParams::new().brightness(1));
    assert!(sent.is_err(), "control send after close: {sent:?}");
    drop(ctl);
    drop(q);
}
