//! The ring's named file mapping and "new frame" event.
//!
//! Each user has their own ring ([`RingConfig`], names from
//! [`crate::naming`]). Both objects are created with a protected DACL granting
//! SYSTEM, Local Service, Administrators and the owning user only
//! ([`crate::naming::ring_sddl`]): the two ends run as different accounts in
//! different sessions (the app as the signed-in user, the media source inside
//! the Frame Server service as Local Service in session 0), and no other user
//! or process may read the live programme or inject frames. Whichever end
//! arrives first creates the objects;
//! a normal app lacks `SeCreateGlobalPrivilege`, so its create attempt on a
//! `Global\` section fails and it opens the section the source created.

use parking_lot::RwLock;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    LocalFree, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
    WaitForSingleObject,
};
use windows_core::{HSTRING, PCWSTR};

use crate::layout::RING_SIZE;
use crate::naming::{ring_prefix_for_sid, ring_sddl};
use crate::ring::Ring;

/// The kernel object names of one ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingNames {
    /// File mapping name.
    pub mapping: String,
    /// Auto-reset "new frame" event name.
    pub event: String,
}

impl RingNames {
    /// Names derived from `prefix` (`<prefix>-ring`, `<prefix>-frame`), for
    /// tests that must not touch the real camera's ring.
    #[must_use]
    pub fn with_prefix(prefix: &str) -> Self {
        Self {
            mapping: format!("{prefix}-ring"),
            event: format!("{prefix}-frame"),
        }
    }
}

/// One user's ring: its object names and the SID its DACL grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingConfig {
    /// The kernel object names.
    pub names: RingNames,
    /// The owning user's string SID.
    pub owner_sid: String,
}

impl RingConfig {
    /// The ring of user `sid` (`None` for anything that is not a string SID).
    #[must_use]
    pub fn for_sid(sid: &str) -> Option<Self> {
        Some(Self {
            names: RingNames::with_prefix(&ring_prefix_for_sid(sid)?),
            owner_sid: sid.to_string(),
        })
    }

    /// The ring of the user this process runs as.
    ///
    /// # Errors
    /// A failing token query.
    pub fn for_current_user() -> windows_core::Result<Self> {
        let sid = super::identity::current_user_sid()?;
        Self::for_sid(&sid)
            .ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_UNEXPECTED))
    }

    /// Custom names owned by the current user (tests use private `Local\`
    /// rings).
    ///
    /// # Errors
    /// A failing token query.
    pub fn with_names(names: RingNames) -> windows_core::Result<Self> {
        Ok(Self {
            names,
            owner_sid: super::identity::current_user_sid()?,
        })
    }
}

static OVERRIDE: RwLock<Option<RingNames>> = RwLock::new(None);

/// Point media sources created in this process without an owner attribute at
/// `names` (tests use a private `Local\` ring); `None` clears it. Production
/// sources get the owner from the virtual camera's attribute and never use
/// this.
pub fn set_process_names(names: Option<RingNames>) {
    *OVERRIDE.write() = names;
}

/// The test override set by [`set_process_names`], if any.
#[must_use]
pub fn process_names() -> Option<RingNames> {
    OVERRIDE.read().clone()
}

/// Which end of the ring this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The app: maps the ring writable.
    Producer,
    /// The media source: maps it read-only.
    Consumer,
}

/// A mapped ring plus its event. Unmapped and closed on drop.
#[derive(Debug)]
pub struct SharedRing {
    view: *mut u8,
    mapping: HANDLE,
    event: Option<HANDLE>,
    ring: Ring,
    created: bool,
}

// SAFETY: the view is process-shared memory accessed only through `Ring`'s
// atomic/seqlock protocol; the handles are process-wide kernel handles.
unsafe impl Send for SharedRing {}
// SAFETY: as above; no method mutates `self` through `&self` except via the
// kernel (SetEvent/WaitForSingleObject are thread-safe).
unsafe impl Sync for SharedRing {}

/// A security descriptor built from [`ring_sddl`], freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn new(owner_sid: &str) -> windows_core::Result<Self> {
        let sddl = ring_sddl(owner_sid)
            .ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_INVALIDARG))?;
        let sddl = HSTRING::from(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid NUL-terminated wide string; `sd` receives
        // a LocalAlloc'd descriptor that Drop frees.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )?;
        }
        Ok(Self(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was LocalAlloc'd by the conversion above.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

impl SharedRing {
    /// Map `config`'s ring. With `allow_create`, try to create the objects
    /// first (with the owner-only security descriptor); fall back to opening
    /// existing ones. The event is optional: without it the consumer
    /// paces by sleeping.
    ///
    /// # Errors
    /// The last Win32 error when the mapping can neither be created nor
    /// opened (typically `ERROR_FILE_NOT_FOUND`: nobody created it yet), or
    /// cannot be mapped.
    pub fn open(config: &RingConfig, role: Role, allow_create: bool) -> windows_core::Result<Self> {
        let names = &config.names;
        let descriptor = Descriptor::new(&config.owner_sid)?;
        let sa = descriptor.attributes();
        let mapping_name = HSTRING::from(names.mapping.as_str());
        let event_name = HSTRING::from(names.event.as_str());

        let mut created = false;
        let mut mapping = None;
        if allow_create {
            // SAFETY: `sa` and its descriptor outlive the call; the name is a
            // valid wide string; INVALID_HANDLE_VALUE backs the section with
            // the paging file.
            let res = unsafe {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    Some(&sa),
                    PAGE_READWRITE,
                    0,
                    RING_SIZE as u32,
                    &mapping_name,
                )
            };
            if let Ok(h) = res {
                // SAFETY: reads this thread's last-error value.
                created = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;
                mapping = Some(h);
            }
        }
        let access = match role {
            Role::Producer => FILE_MAP_READ | FILE_MAP_WRITE,
            Role::Consumer => FILE_MAP_READ,
        };
        let mapping = match mapping {
            Some(h) => h,
            // SAFETY: valid wide-string name.
            None => unsafe { OpenFileMappingW(access.0, false, &mapping_name)? },
        };
        // SAFETY: `mapping` is a valid section of at least RING_SIZE bytes
        // (both ends create it with that size).
        let view = unsafe { MapViewOfFile(mapping, access, 0, 0, RING_SIZE) };
        if view.Value.is_null() {
            let err = windows_core::Error::from_thread();
            // SAFETY: closing the handle we just obtained.
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(err);
        }
        // SAFETY: the view is RING_SIZE bytes, page aligned, and lives until
        // Drop unmaps it (after `ring` can no longer be used).
        let Some(ring) = (unsafe { Ring::from_raw(view.Value.cast(), RING_SIZE) }) else {
            // SAFETY: unmapping and closing what we just mapped/opened.
            unsafe {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(mapping);
            }
            return Err(windows_core::Error::from_hresult(windows_core::HRESULT(
                0x8007_0057_u32 as i32,
            )));
        };

        // SAFETY: as for the mapping: valid attributes and name.
        let event = unsafe { CreateEventW(Some(&sa), false, false, &event_name) }
            .or_else(|_| {
                // SAFETY: valid name; the rights are the two we use.
                unsafe {
                    OpenEventW(
                        EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                        false,
                        &event_name,
                    )
                }
            })
            .ok();

        Ok(Self {
            view: view.Value.cast(),
            mapping,
            event,
            ring,
            created,
        })
    }

    /// The ring in the mapping.
    #[must_use]
    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    /// Whether this call created the mapping (rather than opening one).
    #[must_use]
    pub fn created(&self) -> bool {
        self.created
    }

    /// Whether the "new frame" event is available.
    #[must_use]
    pub fn has_event(&self) -> bool {
        self.event.is_some()
    }

    /// Signal "new frame" to a waiting consumer.
    pub fn signal(&self) {
        if let Some(event) = self.event {
            // SAFETY: a valid event handle owned by `self`.
            unsafe {
                let _ = SetEvent(event);
            }
        }
    }

    /// Wait up to `timeout_ms` for "new frame". Returns `true` when signalled;
    /// without an event, sleeps and returns `false`.
    pub fn wait(&self, timeout_ms: u32) -> bool {
        match self.event {
            // SAFETY: a valid event handle owned by `self`.
            Some(event) => unsafe { WaitForSingleObject(event, timeout_ms) == WAIT_OBJECT_0 },
            None => {
                std::thread::sleep(std::time::Duration::from_millis(u64::from(timeout_ms)));
                false
            }
        }
    }
}

impl Drop for SharedRing {
    fn drop(&mut self) {
        // SAFETY: `view` came from MapViewOfFile and the handles from
        // Create*/Open*; nothing uses `ring` after this point.
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view.cast(),
            });
            let _ = CloseHandle(self.mapping);
            if let Some(event) = self.event {
                let _ = CloseHandle(event);
            }
        }
    }
}
