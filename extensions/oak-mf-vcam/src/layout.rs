//! The shared-memory ring's byte layout: a 4 KiB header followed by
//! [`SLOT_COUNT`] frame slots of [`SLOT_CAPACITY`] bytes each.
//!
//! ```text
//! offset  size   field
//! 0       4      magic            "OAKR" (little-endian u32), written by the producer
//! 4       4      version          RING_VERSION
//! 8       4      slot_count       SLOT_COUNT
//! 12      4      slot_capacity    SLOT_CAPACITY
//! 16      4      producer_pid     the process holding the producer claim (0 = none)
//! 20      4      fps_milli        producer frame rate x 1000
//! 24      8      latest           number of the newest complete frame (0 = none)
//! 32      8      heartbeat_100ns  producer wall clock (FILETIME units) at the last write
//! 40      24     reserved
//! 64      64xN   slots[N]         SlotHeader per slot (seqlock + frame metadata)
//! 4096    CxN    slot data        slot i at HEADER_SIZE + i * SLOT_CAPACITY
//! ```
//!
//! Frame `n` (1-based) lives in slot `n % SLOT_COUNT`. Each slot is a seqlock:
//! the producer stores `seq = 2n - 1` (odd: writing), copies the pixels and
//! metadata, stores `seq = 2n`, then publishes `latest = n`. A reader takes
//! `latest`, checks the slot's `seq == 2 * latest`, copies, and re-reads `seq`;
//! a change means the producer lapped it and the copy is discarded. Every
//! header field is an atomic, so reading memory another process writes is
//! defined behaviour; the reader validates every value before using it.
//!
//! Only one producer may write: [`crate::ring::Writer::claim`] takes
//! `producer_pid` with a compare-and-swap and refuses while another live,
//! heartbeating producer holds it; a writer whose claim was taken over stops.
//!
//! Zeroed memory is a valid empty ring (`latest == 0`), so whichever side
//! creates the mapping needs no initialisation step.

use std::sync::atomic::{AtomicU32, AtomicU64};

/// `"OAKR"` as a little-endian `u32`.
pub const RING_MAGIC: u32 = u32::from_le_bytes(*b"OAKR");
/// Layout version; a reader ignores a ring with any other value.
pub const RING_VERSION: u32 = 1;
/// Frame slots in the ring: the frame being written, the newest complete one,
/// and one spare so a slow reader is not lapped at once.
pub const SLOT_COUNT: usize = 3;
// M4c: slot capacity per camera index (RGB 4K NV12, mono 1280x800).
/// Largest frame the ring carries (4K, the RGB camera's largest mode).
pub const MAX_WIDTH: u32 = 3840;
/// Largest frame height the ring carries.
pub const MAX_HEIGHT: u32 = 2160;
/// Bytes per slot: one tightly packed NV12 frame at [`MAX_WIDTH`] x [`MAX_HEIGHT`].
pub const SLOT_CAPACITY: usize = nv12_size(MAX_WIDTH, MAX_HEIGHT);
/// Bytes reserved for the header (one page).
pub const HEADER_SIZE: usize = 4096;
/// Total bytes of the mapping.
pub const RING_SIZE: usize = HEADER_SIZE + SLOT_COUNT * SLOT_CAPACITY;

/// The only pixel format the ring carries today: NV12, little-endian four-cc
/// (the Media Foundation / V4L2 convention).
pub const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");

/// Frame duration the source paces to when the producer reports none: 30 fps
/// in 100 ns units.
pub const DEFAULT_FRAME_100NS: u64 = 333_333;

/// A producer whose heartbeat is older than this (100 ns units, 1 s) is treated
/// as gone and the source shows the placeholder.
pub const STALE_AFTER_100NS: u64 = 10_000_000;

/// A producer claim whose heartbeat is older than this (100 ns units, 5 s) may
/// be taken over by another producer even if its process still exists.
pub const CLAIM_STALE_100NS: u64 = 50_000_000;

/// Tightly packed NV12 bytes at `width` x `height` (odd sizes round the chroma
/// plane up).
#[must_use]
pub const fn nv12_size(width: u32, height: u32) -> usize {
    let w = width as usize;
    let h = height as usize;
    w * h + w.div_ceil(2) * 2 * h.div_ceil(2)
}

/// The slot that holds frame `frame` (1-based frame numbers).
#[must_use]
pub const fn slot_for(frame: u64) -> usize {
    (frame % SLOT_COUNT as u64) as usize
}

/// Byte offset of slot `slot`'s pixel data from the start of the mapping.
#[must_use]
pub const fn slot_offset(slot: usize) -> usize {
    HEADER_SIZE + slot * SLOT_CAPACITY
}

/// Per-slot seqlock and metadata (64 bytes).
#[repr(C)]
#[derive(Debug)]
pub struct SlotHeader {
    /// `2n - 1` while frame `n` is being written, `2n` once it is complete.
    pub seq: AtomicU64,
    /// Frame width in pixels.
    pub width: AtomicU32,
    /// Frame height in pixels.
    pub height: AtomicU32,
    /// Pixel format four-cc ([`FOURCC_NV12`]).
    pub fourcc: AtomicU32,
    /// Bytes of pixel data in the slot.
    pub data_len: AtomicU32,
    /// Producer wall clock (FILETIME 100 ns units) when the frame was written.
    pub timestamp_100ns: AtomicU64,
    /// Reserved, zero.
    pub reserved: [AtomicU64; 4],
}

/// The ring header at offset 0 of the mapping.
#[repr(C)]
#[derive(Debug)]
pub struct RingHeader {
    /// [`RING_MAGIC`] once a producer has attached.
    pub magic: AtomicU32,
    /// [`RING_VERSION`].
    pub version: AtomicU32,
    /// [`SLOT_COUNT`].
    pub slot_count: AtomicU32,
    /// [`SLOT_CAPACITY`].
    pub slot_capacity: AtomicU32,
    /// Process id of the producer.
    pub producer_pid: AtomicU32,
    /// Producer frame rate x 1000 (0 = unknown).
    pub fps_milli: AtomicU32,
    /// Number of the newest complete frame (0 = none yet).
    pub latest: AtomicU64,
    /// Producer wall clock (FILETIME 100 ns units) at its last write.
    pub heartbeat_100ns: AtomicU64,
    /// Reserved, zero.
    pub reserved: [AtomicU64; 3],
    /// One [`SlotHeader`] per slot.
    pub slots: [SlotHeader; SLOT_COUNT],
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    /// The header layout matches the documented offsets, fits in its page, and
    /// needs no more than 8-byte alignment (a mapping is page aligned).
    #[test]
    fn vcam_ring_header_layout() {
        assert_eq!(size_of::<SlotHeader>(), 64);
        assert_eq!(offset_of!(SlotHeader, seq), 0);
        assert_eq!(offset_of!(SlotHeader, width), 8);
        assert_eq!(offset_of!(SlotHeader, height), 12);
        assert_eq!(offset_of!(SlotHeader, fourcc), 16);
        assert_eq!(offset_of!(SlotHeader, data_len), 20);
        assert_eq!(offset_of!(SlotHeader, timestamp_100ns), 24);

        assert_eq!(offset_of!(RingHeader, magic), 0);
        assert_eq!(offset_of!(RingHeader, version), 4);
        assert_eq!(offset_of!(RingHeader, slot_count), 8);
        assert_eq!(offset_of!(RingHeader, slot_capacity), 12);
        assert_eq!(offset_of!(RingHeader, producer_pid), 16);
        assert_eq!(offset_of!(RingHeader, fps_milli), 20);
        assert_eq!(offset_of!(RingHeader, latest), 24);
        assert_eq!(offset_of!(RingHeader, heartbeat_100ns), 32);
        assert_eq!(offset_of!(RingHeader, slots), 64);
        assert_eq!(size_of::<RingHeader>(), 64 + 64 * SLOT_COUNT);
        assert!(size_of::<RingHeader>() <= HEADER_SIZE);
        assert!(align_of::<RingHeader>() <= 8);
        assert_eq!(RING_MAGIC, 0x524B_414F);
    }

    /// Slot maths: frames rotate through the slots, and each slot holds a 4K
    /// NV12 frame.
    #[test]
    fn vcam_ring_slot_math() {
        assert_eq!(SLOT_CAPACITY, 3840 * 2160 * 3 / 2);
        assert_eq!(slot_for(1), 1);
        assert_eq!(slot_for(2), 2);
        assert_eq!(slot_for(3), 0);
        assert_eq!(slot_for(4), 1);
        assert_eq!(slot_offset(0), HEADER_SIZE);
        assert_eq!(slot_offset(2), HEADER_SIZE + 2 * SLOT_CAPACITY);
        assert_eq!(RING_SIZE, slot_offset(SLOT_COUNT));
        assert!(
            u32::try_from(RING_SIZE).is_ok(),
            "one DWORD sizes the mapping"
        );
        assert_eq!(nv12_size(1920, 1080), 3_110_400);
        assert_eq!(nv12_size(641, 481), 641 * 481 + 642 * 241);
        assert_eq!(FOURCC_NV12, 0x3231_564e);
        // Slot data starts on a page boundary, and every slot on an 8-byte one.
        assert_eq!(HEADER_SIZE % 4096, 0);
        assert_eq!(SLOT_CAPACITY % 8, 0);
    }
}
