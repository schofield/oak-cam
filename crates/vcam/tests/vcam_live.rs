//! Live publish through the platform virtual camera: start each camera at its
//! kind's default size, push two seconds of the test pattern and check frames
//! flow. Needs the OS side installed (macOS extension approved, v4l2loopback
//! loaded, Windows media source registered), so it is `#[ignore]`d; run with
//! `cargo test -p vcam --test vcam_live -- --ignored --nocapture`. It skips
//! loudly (and passes) when the OS side is missing.
//!
//! Ported in spirit from stream-concern's `vcam_macos_live.rs`,
//! `vcam_linux_loopback.rs` and `vcam_windows_live.rs`; the consumer half (open
//! the camera and read frames back) needs a capture stack this crate does not
//! have, so it is checked by hand (Photo Booth, `ffplay /dev/video10`, the
//! Windows Camera app).

use std::time::{Duration, Instant};

use vcam::test_pattern::{GrayPattern, Nv12Pattern};
use vcam::{CameraId, CameraKind, DropReason, Publish, VcamError, platform_camera};

const PUBLISH_FOR: Duration = Duration::from_secs(2);
const FPS: f32 = 30.0;

#[test]
#[ignore = "needs the OS virtual camera installed"]
fn publishes_test_pattern_to_every_camera() {
    // M4: on Linux/Windows the backends are still single-device, so only RGB.
    let cameras: &[CameraId] = if cfg!(target_os = "macos") {
        &CameraId::ALL
    } else {
        &[CameraId::Rgb]
    };
    for &id in cameras {
        let res = id.kind().default_resolution();
        let mut cam = platform_camera(id);
        match cam.start(res.width, res.height, FPS) {
            Ok(()) => {}
            Err(VcamError::NotAvailable(msg)) => {
                eprintln!("*** SKIP {id}: {msg}");
                continue;
            }
            Err(e) => panic!("{id}: start failed: {e}"),
        }
        let mut nv12 = Nv12Pattern::new(res.width, res.height);
        let mut gray = GrayPattern::new(res.width, res.height);
        let start = Instant::now();
        let mut hard_drops = 0;
        let mut i = 0i64;
        while start.elapsed() < PUBLISH_FOR {
            let frame = match id.kind() {
                CameraKind::Rgb => nv12.next_frame(i),
                CameraKind::Mono => gray.next_frame(i),
            };
            if let Publish::Dropped(
                reason
                @ (DropReason::Backend | DropReason::FormatMismatch | DropReason::NotStarted),
            ) = cam.publish(&frame)
            {
                eprintln!("{id}: frame {i} dropped: {reason:?}");
                hard_drops += 1;
            }
            i += 1;
            std::thread::sleep(Duration::from_secs_f32(1.0 / FPS));
        }
        let stats = cam.stats();
        eprintln!("{id}: {stats:?}");
        assert_eq!(hard_drops, 0, "{id}: no backend/format drops");
        if cfg!(not(target_os = "windows")) {
            // Windows only creates the ring once a consumer opens the camera.
            assert!(stats.published > 0, "{id}: frames were published");
        }
        cam.stop();
    }
}
