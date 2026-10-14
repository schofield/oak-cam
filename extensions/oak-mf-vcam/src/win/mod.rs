//! Windows side of the virtual camera: the named shared memory ([`shm`]), the
//! COM server the Frame Server loads ([`com`], exported by `exports`),
//! registration ([`registry`]) and the app-side host that creates the camera
//! and feeds the ring ([`host`]).

#![allow(unsafe_code)]

pub mod com;
pub mod exports;
pub mod host;
pub mod identity;
pub mod registry;
pub mod shm;

use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_core::GUID;

/// Attribute (string) the app sets on the `IMFVirtualCamera` with the owning
/// user's SID. Attributes set on the virtual camera reach the activation
/// object's store before `ActivateObject` (the mechanism Microsoft's
/// VirtualCamera sample uses for its `VCAM_KIND`), so the source learns whose
/// ring to read without any shared, guessable default.
pub const ATTR_OWNER_SID: GUID = GUID::from_u128(0x01414E1B_358C_44AC_8A7D_66DF31F83FB3);

/// Attribute (string) the source publishes in its source attributes naming
/// the ring it reads (`sid:<SID>`, `override:<prefix>` in tests, or `none`),
/// for diagnostics.
pub const ATTR_RING_STATE: GUID = GUID::from_u128(0x31D9BBF6_8695_4980_B591_9FDE7EB92156);

/// The media source's CLSID as a `GUID`.
#[must_use]
pub const fn clsid() -> GUID {
    GUID::from_u128(crate::CLSID_U128)
}

/// Wall clock in FILETIME units (100 ns since 1601), comparable across
/// processes and sessions; the ring's heartbeat and timestamps use it.
#[must_use]
pub fn now_100ns() -> u64 {
    // SAFETY: GetSystemTimeAsFileTime has no preconditions.
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Live COM objects, server locks and stream worker threads of this module;
/// `DllCanUnloadNow` allows unloading only at zero, so a Frame Server that
/// has released every camera can load a newer DLL.
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One unit of [`LIVE`], held by every COM object and worker thread.
#[derive(Debug)]
pub struct Alive(());

impl Alive {
    /// Count one more live object.
    #[must_use]
    pub fn new() -> Self {
        LIVE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(())
    }
}

impl Default for Alive {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Whether nothing of this module is alive (see [`Alive`]).
#[must_use]
pub fn nothing_alive() -> bool {
    LIVE.load(std::sync::atomic::Ordering::Acquire) == 0
}

/// Adjust [`LIVE`] for `IClassFactory::LockServer`.
pub fn lock_server(lock: bool) {
    if lock {
        LIVE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    } else {
        let _ = LIVE.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |n| n.checked_sub(1),
        );
    }
}

/// A Media Foundation interface pointer that may cross threads.
///
/// The `windows` crate marks COM interfaces `!Send`/`!Sync` because apartment
/// rules differ per object. Every object wrapped here is a Media Foundation
/// object documented as free-threaded (attribute stores, media types, stream
/// and presentation descriptors, event queues, samples, sample allocators) or
/// one of this crate's own objects (all state behind locks), and the DLL is
/// registered `ThreadingModel=Both`.
#[derive(Clone, Debug)]
pub struct Agile<T>(pub T);

// SAFETY: see the type docs: only free-threaded Media Foundation objects and
// this crate's lock-protected objects are wrapped.
unsafe impl<T> Send for Agile<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for Agile<T> {}

impl<T> std::ops::Deref for Agile<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
