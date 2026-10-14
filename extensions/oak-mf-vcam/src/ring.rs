//! Producer and consumer over the ring described in [`crate::layout`], on any
//! memory: a named file mapping on Windows (`win::shm`), a heap buffer in the
//! tests. The protocol is a per-slot seqlock, so the producer never waits for
//! a reader and a reader never observes a torn frame.

#![allow(unsafe_code)]

use std::ptr::{self, NonNull};
use std::sync::atomic::{Ordering, fence};

use crate::layout::{
    CLAIM_STALE_100NS, FOURCC_NV12, HEADER_SIZE, MAX_HEIGHT, MAX_WIDTH, RING_MAGIC, RING_SIZE,
    RING_VERSION, RingHeader, SLOT_CAPACITY, SLOT_COUNT, nv12_size, slot_for, slot_offset,
};

/// A view of ring memory. Cheap to copy around; it does not own the memory.
#[derive(Debug)]
pub struct Ring {
    base: NonNull<u8>,
}

// SAFETY: the ring is shared memory designed for concurrent access: every
// header field is an atomic and pixel data is only copied with raw pointer
// copies under the seqlock protocol, never borrowed as a Rust reference while
// another party may write it.
unsafe impl Send for Ring {}
// SAFETY: as above; `&Ring` exposes only atomic loads/stores and raw copies.
unsafe impl Sync for Ring {}

/// Why a frame could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// Width or height is zero or above [`MAX_WIDTH`] x [`MAX_HEIGHT`].
    BadSize,
    /// The data length is not the tight NV12 size for the geometry.
    BadLength,
    /// Another producer took the claim over (this one had gone stale); the
    /// writer must stop.
    Lost,
}

/// Why the producer role could not be claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// A live producer with a fresh heartbeat holds the ring.
    Busy {
        /// Its process id.
        pid: u32,
    },
}

/// Metadata of a frame read from the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameMeta {
    /// The frame number (1-based, increasing).
    pub frame: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel format four-cc (always [`FOURCC_NV12`] today).
    pub fourcc: u32,
    /// Bytes copied into the destination.
    pub len: usize,
    /// Producer wall clock (100 ns) when the frame was written.
    pub timestamp_100ns: u64,
}

impl Ring {
    /// View `len` bytes at `base` as a ring.
    ///
    /// Returns `None` when `base` is null, not 8-byte aligned, or `len` is
    /// smaller than [`RING_SIZE`].
    ///
    /// # Safety
    /// `base` must stay valid for reads and writes of [`RING_SIZE`] bytes for
    /// as long as the returned `Ring` (or anything built from it) is used, and
    /// other parties may modify it only through this module's protocol.
    #[must_use]
    pub unsafe fn from_raw(base: *mut u8, len: usize) -> Option<Self> {
        let base = NonNull::new(base)?;
        if len < RING_SIZE || base.as_ptr().align_offset(8) != 0 {
            return None;
        }
        Some(Self { base })
    }

    /// Another view of the same memory, under the same validity contract as
    /// the [`from_raw`](Self::from_raw) call that made `self`.
    #[must_use]
    pub fn view(&self) -> Ring {
        Ring { base: self.base }
    }

    fn header(&self) -> &RingHeader {
        // SAFETY: `from_raw` guarantees RING_SIZE valid, 8-aligned bytes; the
        // header is all atomics, so a shared reference is sound even while
        // another process writes it.
        unsafe { &*self.base.as_ptr().cast::<RingHeader>() }
    }

    fn slot_data(&self, slot: usize) -> *mut u8 {
        debug_assert!(slot < SLOT_COUNT);
        // SAFETY: slot_offset(slot) + SLOT_CAPACITY <= RING_SIZE for every
        // slot < SLOT_COUNT, inside the mapping.
        unsafe { self.base.as_ptr().add(slot_offset(slot)) }
    }

    /// Whether a producer has stamped the header with this layout.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        let h = self.header();
        h.magic.load(Ordering::Acquire) == RING_MAGIC
            && h.version.load(Ordering::Relaxed) == RING_VERSION
            && h.slot_count.load(Ordering::Relaxed) as usize == SLOT_COUNT
            && h.slot_capacity.load(Ordering::Relaxed) as usize == SLOT_CAPACITY
    }

    /// Number of the newest complete frame (0 = none).
    #[must_use]
    pub fn latest(&self) -> u64 {
        self.header().latest.load(Ordering::Acquire)
    }

    /// The producer's wall clock (100 ns) at its last write or heartbeat.
    #[must_use]
    pub fn heartbeat_100ns(&self) -> u64 {
        self.header().heartbeat_100ns.load(Ordering::Acquire)
    }

    /// The producer's frame rate x 1000 (0 = unknown).
    #[must_use]
    pub fn fps_milli(&self) -> u32 {
        self.header().fps_milli.load(Ordering::Relaxed)
    }

    /// The producer's process id (0 = none yet).
    #[must_use]
    pub fn producer_pid(&self) -> u32 {
        self.header().producer_pid.load(Ordering::Relaxed)
    }

    /// Copy the newest complete frame into `dst` if it is newer than `after`.
    /// Returns `None` when there is no such frame, the ring is not stamped by
    /// a compatible producer, the metadata is invalid, `dst` is too small, or
    /// the producer lapped the slot during the copy three times running.
    pub fn read_latest(&self, after: u64, dst: &mut [u8]) -> Option<FrameMeta> {
        if !self.is_compatible() {
            return None;
        }
        let h = self.header();
        for _ in 0..3 {
            let frame = h.latest.load(Ordering::Acquire);
            if frame == 0 || frame <= after {
                return None;
            }
            let slot = &h.slots[slot_for(frame)];
            let seq1 = slot.seq.load(Ordering::Acquire);
            if seq1 != frame.wrapping_mul(2) {
                // Already being overwritten by a newer frame: take that one.
                continue;
            }
            let width = slot.width.load(Ordering::Relaxed);
            let height = slot.height.load(Ordering::Relaxed);
            let fourcc = slot.fourcc.load(Ordering::Relaxed);
            let len = slot.data_len.load(Ordering::Relaxed) as usize;
            let timestamp_100ns = slot.timestamp_100ns.load(Ordering::Relaxed);
            if width == 0
                || height == 0
                || width > MAX_WIDTH
                || height > MAX_HEIGHT
                || fourcc != FOURCC_NV12
                || len != nv12_size(width, height)
                || len > SLOT_CAPACITY
                || len > dst.len()
            {
                return None;
            }
            // SAFETY: the source lies inside slot `slot_for(frame)` (len <=
            // SLOT_CAPACITY); `dst` holds at least `len` bytes and is our own
            // memory, so the ranges cannot overlap. A concurrent overwrite is
            // detected by the seq re-check below and the copy discarded.
            unsafe {
                ptr::copy_nonoverlapping(self.slot_data(slot_for(frame)), dst.as_mut_ptr(), len);
            }
            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Relaxed) != seq1 {
                continue;
            }
            return Some(FrameMeta {
                frame,
                width,
                height,
                fourcc,
                len,
                timestamp_100ns,
            });
        }
        None
    }
}

/// The producer side: stamps the header and writes frames.
#[derive(Debug)]
pub struct Writer {
    ring: Ring,
    next: u64,
    pid: u32,
}

impl Writer {
    /// Take the producer role on `ring` for process `pid`: compare-and-swap
    /// `producer_pid` from free (0), from `pid` itself, or from an owner that
    /// is gone (`is_alive(owner)` is false) or stale (no heartbeat for
    /// [`CLAIM_STALE_100NS`]); then stamp the header and continue numbering
    /// after whatever frame is there.
    ///
    /// # Errors
    /// [`ClaimError::Busy`] while another live, heartbeating producer holds it.
    pub fn claim(
        ring: Ring,
        pid: u32,
        now_100ns: u64,
        is_alive: impl Fn(u32) -> bool,
    ) -> Result<Self, ClaimError> {
        let h = ring.header();
        loop {
            let owner = h.producer_pid.load(Ordering::Acquire);
            let fresh = now_100ns.saturating_sub(h.heartbeat_100ns.load(Ordering::Acquire))
                < CLAIM_STALE_100NS;
            let takeable = owner == 0 || owner == pid || !fresh || !is_alive(owner);
            if !takeable {
                return Err(ClaimError::Busy { pid: owner });
            }
            if h.producer_pid
                .compare_exchange(owner, pid, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        h.version.store(RING_VERSION, Ordering::Relaxed);
        h.slot_count.store(SLOT_COUNT as u32, Ordering::Relaxed);
        h.slot_capacity
            .store(SLOT_CAPACITY as u32, Ordering::Relaxed);
        h.heartbeat_100ns.store(now_100ns, Ordering::Release);
        h.magic.store(RING_MAGIC, Ordering::Release);
        let next = h.latest.load(Ordering::Acquire).saturating_add(1);
        Ok(Self { ring, next, pid })
    }

    /// Whether this writer still holds the producer claim.
    #[must_use]
    pub fn holds_claim(&self) -> bool {
        self.ring.header().producer_pid.load(Ordering::Acquire) == self.pid
    }

    /// Record the producer's frame rate (the source paces to it).
    pub fn set_fps(&self, fps: f32) {
        let milli = if fps.is_finite() && fps > 0.0 {
            (fps * 1000.0).round().min(u32::MAX as f32) as u32
        } else {
            0
        };
        self.ring.header().fps_milli.store(milli, Ordering::Relaxed);
    }

    /// Refresh the heartbeat without writing a frame.
    pub fn heartbeat(&self, now_100ns: u64) {
        self.ring
            .header()
            .heartbeat_100ns
            .store(now_100ns, Ordering::Release);
    }

    /// The ring this writer publishes into.
    #[must_use]
    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    /// Write one tightly packed NV12 frame and publish it as the newest.
    /// Returns the frame number.
    ///
    /// # Errors
    /// [`WriteError`] when the geometry or the data length is out of range,
    /// or [`WriteError::Lost`] once another producer took the claim over.
    pub fn write_nv12(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
        now_100ns: u64,
    ) -> Result<u64, WriteError> {
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(WriteError::BadSize);
        }
        let len = nv12_size(width, height);
        if data.len() < len {
            return Err(WriteError::BadLength);
        }
        if !self.holds_claim() {
            return Err(WriteError::Lost);
        }
        let frame = self.next;
        let h = self.ring.header();
        let slot_index = slot_for(frame);
        let slot = &h.slots[slot_index];
        slot.seq
            .store(frame.wrapping_mul(2).wrapping_sub(1), Ordering::Relaxed);
        fence(Ordering::Release);
        // SAFETY: `len <= SLOT_CAPACITY` (geometry bounded above), the
        // destination is slot `slot_index` inside the mapping, and `data` is
        // our caller's buffer, so the ranges cannot overlap.
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), self.ring.slot_data(slot_index), len);
        }
        slot.width.store(width, Ordering::Relaxed);
        slot.height.store(height, Ordering::Relaxed);
        slot.fourcc.store(FOURCC_NV12, Ordering::Relaxed);
        slot.data_len.store(len as u32, Ordering::Relaxed);
        slot.timestamp_100ns.store(now_100ns, Ordering::Relaxed);
        slot.seq.store(frame.wrapping_mul(2), Ordering::Release);
        h.latest.store(frame, Ordering::Release);
        h.heartbeat_100ns.store(now_100ns, Ordering::Release);
        self.next = frame + 1;
        Ok(frame)
    }
}

impl Drop for Writer {
    /// Release the claim (only if it is still ours).
    fn drop(&mut self) {
        let _ = self.ring.header().producer_pid.compare_exchange(
            self.pid,
            0,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

/// The ring header must fit before the first slot.
const _: () = assert!(std::mem::size_of::<RingHeader>() <= HEADER_SIZE);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// Heap memory standing in for the mapping (u64 elements for alignment).
    struct HeapRing(Vec<u64>);

    impl HeapRing {
        fn new() -> Self {
            Self(vec![0u64; RING_SIZE.div_ceil(8)])
        }
        fn ring(&mut self) -> Ring {
            // SAFETY: the Vec outlives every Ring in these tests and is
            // RING_SIZE bytes, 8-aligned.
            unsafe { Ring::from_raw(self.0.as_mut_ptr().cast(), self.0.len() * 8) }.unwrap()
        }
    }

    fn pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut v = vec![0u8; nv12_size(width, height)];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        v
    }

    /// An empty ring reads nothing; a stamped ring returns the newest frame
    /// once, byte for byte, and bad geometry is refused.
    #[test]
    fn vcam_ring_write_then_read() {
        let mut heap = HeapRing::new();
        let reader = heap.ring();
        let mut dst = vec![0u8; SLOT_CAPACITY];
        assert!(
            reader.read_latest(0, &mut dst).is_none(),
            "zeroed ring is empty"
        );

        let mut writer = Writer::claim(heap.ring(), 42, 0, |_| true).unwrap();
        writer.set_fps(30.0);
        assert!(reader.is_compatible());
        assert_eq!(reader.producer_pid(), 42);
        assert_eq!(reader.fps_milli(), 30_000);

        let a = pattern(64, 36, 1);
        let b = pattern(64, 36, 2);
        assert_eq!(writer.write_nv12(64, 36, &a, 100), Ok(1));
        let meta = reader.read_latest(0, &mut dst).expect("frame 1");
        assert_eq!((meta.frame, meta.width, meta.height), (1, 64, 36));
        assert_eq!(&dst[..meta.len], &a[..]);
        assert!(reader.read_latest(1, &mut dst).is_none(), "nothing newer");

        assert_eq!(writer.write_nv12(64, 36, &b, 200), Ok(2));
        let meta = reader.read_latest(1, &mut dst).expect("frame 2");
        assert_eq!(meta.frame, 2);
        assert_eq!(meta.timestamp_100ns, 200);
        assert_eq!(&dst[..meta.len], &b[..]);
        assert_eq!(reader.heartbeat_100ns(), 200);

        assert_eq!(writer.write_nv12(0, 36, &b, 0), Err(WriteError::BadSize));
        assert_eq!(writer.write_nv12(4000, 36, &b, 0), Err(WriteError::BadSize));
        assert_eq!(
            writer.write_nv12(64, 36, &b[..10], 0),
            Err(WriteError::BadLength)
        );
        // A too-small destination is refused rather than truncated.
        assert!(reader.read_latest(0, &mut dst[..10]).is_none());

        // After the first producer releases, a new one continues the numbering.
        drop(writer);
        let mut again = Writer::claim(heap.ring(), 43, 300, |_| true).unwrap();
        assert_eq!(again.write_nv12(64, 36, &a, 300), Ok(3));
    }

    /// Corrupt metadata (a hostile or buggy producer) is rejected.
    #[test]
    fn vcam_ring_rejects_bad_metadata() {
        let mut heap = HeapRing::new();
        let reader = heap.ring();
        let mut writer = Writer::claim(heap.ring(), 1, 0, |_| true).unwrap();
        let a = pattern(16, 16, 0);
        writer.write_nv12(16, 16, &a, 1).unwrap();
        let h = reader.header();
        h.slots[slot_for(1)]
            .data_len
            .store(u32::MAX, Ordering::Relaxed);
        let mut dst = vec![0u8; SLOT_CAPACITY];
        assert!(reader.read_latest(0, &mut dst).is_none());
        h.version.store(99, Ordering::Relaxed);
        assert!(!reader.is_compatible());
    }

    /// Producer and consumer on two threads: every frame the consumer accepts
    /// is internally consistent (no torn frames), and it sees progress.
    #[test]
    fn vcam_ring_two_thread_round_trip() {
        let mut heap = HeapRing::new();
        let reader = heap.ring();
        let mut writer = Writer::claim(heap.ring(), 7, 0, |_| true).unwrap();
        let (w, h) = (320u32, 180u32);
        let len = nv12_size(w, h);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let producer = std::thread::spawn(move || {
            let mut buf = vec![0u8; len];
            let mut n = 0u32;
            while !stop_w.load(Ordering::Relaxed) {
                n = n.wrapping_add(1);
                // Every byte of frame n carries n's low byte: a torn copy
                // would mix two values.
                buf.fill(n as u8);
                writer.write_nv12(w, h, &buf, u64::from(n)).unwrap();
            }
        });
        let mut dst = vec![0u8; SLOT_CAPACITY];
        let mut last = 0;
        let mut accepted = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while accepted < 200 && std::time::Instant::now() < deadline {
            if let Some(meta) = reader.read_latest(last, &mut dst) {
                assert!(meta.frame > last);
                let first = dst[0];
                assert!(
                    dst[..meta.len].iter().all(|&b| b == first),
                    "torn frame {}",
                    meta.frame
                );
                last = meta.frame;
                accepted += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        producer.join().unwrap();
        assert!(accepted >= 20, "consumer accepted only {accepted} frames");
    }

    /// Single producer: a second writer is refused while the first is alive
    /// and heartbeating; it may take over once the first is dead or stale,
    /// after which the first gets `Lost` instead of interleaving; dropping a
    /// writer releases the claim.
    #[test]
    fn vcam_ring_single_producer_claim() {
        let mut heap = HeapRing::new();
        let frame = pattern(16, 16, 3);
        let mut first = Writer::claim(heap.ring(), 100, 1_000, |_| true).unwrap();
        first.write_nv12(16, 16, &frame, 2_000).unwrap();

        // Alive and fresh: refused, and the refusal names the owner.
        let alive = |pid: u32| pid == 100;
        assert_eq!(
            Writer::claim(heap.ring(), 200, 3_000, alive).unwrap_err(),
            ClaimError::Busy { pid: 100 }
        );
        // The same process may re-claim (e.g. a re-attach).
        assert!(Writer::claim(heap.ring(), 100, 3_000, alive).is_ok());
        let mut first = Writer::claim(heap.ring(), 100, 3_000, alive).unwrap();

        // Dead owner: taken over.
        let dead = |_pid: u32| false;
        let second = Writer::claim(heap.ring(), 200, 4_000, dead).unwrap();
        assert!(second.holds_claim());
        assert!(!first.holds_claim());
        assert_eq!(
            first.write_nv12(16, 16, &frame, 5_000),
            Err(WriteError::Lost)
        );
        // The loser's drop does not release the winner's claim.
        drop(first);
        assert!(second.holds_claim());
        assert_eq!(
            Writer::claim(heap.ring(), 300, 5_000, |_| true).unwrap_err(),
            ClaimError::Busy { pid: 200 }
        );

        // Stale owner (alive, but no heartbeat for CLAIM_STALE): taken over.
        let stale_now = 4_000 + CLAIM_STALE_100NS + 1;
        let third = Writer::claim(heap.ring(), 300, stale_now, |_| true).unwrap();
        assert!(third.holds_claim() && !second.holds_claim());

        // Dropping the holder frees the ring.
        drop(third);
        drop(second);
        let r = heap.ring();
        assert_eq!(r.producer_pid(), 0);
        assert!(Writer::claim(heap.ring(), 400, stale_now, |_| true).is_ok());
    }
}
