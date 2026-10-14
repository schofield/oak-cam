//! [`NullCamera`]: a virtual camera that publishes nowhere.

use crate::{CameraId, DropReason, FrameSrc, Publish, VcamError, VcamStats, VirtualCamera};

/// Counts frames and never fails: the backend for `--no-vcam` and for tests.
/// It still checks each frame against the started size, so a caller that
/// feeds the wrong geometry sees the same `FormatMismatch` drop a real
/// backend would report.
#[derive(Debug)]
pub struct NullCamera {
    id: CameraId,
    size: Option<(u32, u32)>,
    stats: VcamStats,
}

impl NullCamera {
    /// A null camera standing in for `id`.
    #[must_use]
    pub fn new(id: CameraId) -> Self {
        Self {
            id,
            size: None,
            stats: VcamStats::default(),
        }
    }

    /// The camera this stands in for.
    #[must_use]
    pub fn id(&self) -> CameraId {
        self.id
    }
}

impl VirtualCamera for NullCamera {
    fn start(&mut self, width: u32, height: u32, _fps: f32) -> Result<(), VcamError> {
        if width == 0 || height == 0 {
            return Err(VcamError::InvalidSize {
                width,
                height,
                reason: "empty frame",
            });
        }
        self.size = Some((width, height));
        self.stats = VcamStats::default();
        Ok(())
    }

    fn publish(&mut self, src: &FrameSrc<'_>) -> Publish {
        let outcome = match self.size {
            None => Publish::Dropped(DropReason::NotStarted),
            Some(size) if size != (src.width(), src.height()) => {
                Publish::Dropped(DropReason::FormatMismatch)
            }
            Some(_) => Publish::Published,
        };
        self.stats.record(outcome, crate::monotonic_ns());
        outcome
    }

    fn stats(&self) -> VcamStats {
        self.stats
    }

    fn stop(&mut self) {
        self.size = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_checks_size() {
        let mut cam = NullCamera::new(CameraId::Left);
        let y = vec![0u8; 16];
        let frame = FrameSrc::Gray8 {
            y: &y,
            stride: 4,
            w: 4,
            h: 4,
            ts_ns: 0,
        };
        assert_eq!(
            cam.publish(&frame),
            Publish::Dropped(DropReason::NotStarted)
        );
        cam.start(4, 4, 30.0).unwrap();
        assert_eq!(cam.publish(&frame), Publish::Published);
        assert_eq!(cam.publish(&frame), Publish::Published);
        cam.start(8, 4, 30.0).unwrap();
        assert_eq!(
            cam.publish(&frame),
            Publish::Dropped(DropReason::FormatMismatch)
        );
        assert_eq!(cam.stats().published, 0);
        assert_eq!(cam.stats().dropped, 1);
        cam.stop();
        cam.stop();
        assert!(cam.start(0, 4, 30.0).is_err());
        assert_eq!(cam.id(), CameraId::Left);
    }

    /// The trait object is `Send`, so a capture thread can own it.
    #[test]
    fn boxed_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let cam: Box<dyn VirtualCamera> = Box::new(NullCamera::new(CameraId::Rgb));
        assert_send(&cam);
    }
}
