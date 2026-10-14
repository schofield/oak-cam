//! The app side of the virtual camera: find `MFCreateVirtualCamera` at run
//! time (Windows 11 only; probed by name so Windows 10 gets a clean message
//! instead of a missing-import load failure), check and perform registration,
//! create/start/remove the camera, and feed the ring.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaSource, IMFVirtualCamera, MF_VERSION, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup,
    MFVirtualCameraAccess, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType, MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_core::{GUID, HRESULT, HSTRING, Interface, PCWSTR, s, w};

use super::registry::{Scope, registered_dll};
use super::shm::{RingConfig, Role, SharedRing};
use crate::ring::{ClaimError, WriteError, Writer};
use crate::{CLSID_STRING, DLL_NAME};

type CreateVirtualCameraFn = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut c_void,
) -> HRESULT;

/// `MFCreateVirtualCamera` from `mfsensorgroup.dll`, if this Windows has it
/// (Windows 11 build 22000 and later).
fn create_fn() -> Option<CreateVirtualCameraFn> {
    static FN: OnceLock<Option<usize>> = OnceLock::new();
    let addr = (*FN.get_or_init(|| {
        // SAFETY: loading a system DLL from System32 only, and looking up an
        // export by its documented name. The module stays loaded for the
        // process lifetime (never freed), so the address stays valid.
        unsafe {
            let module =
                LoadLibraryExW(w!("mfsensorgroup.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            GetProcAddress(module, s!("MFCreateVirtualCamera")).map(|f| f as usize)
        }
    }))?;
    // SAFETY: `addr` is the export's address, whose documented signature is
    // `CreateVirtualCameraFn`.
    Some(unsafe { std::mem::transmute::<usize, CreateVirtualCameraFn>(addr) })
}

/// Whether this Windows offers `MFCreateVirtualCamera` (Windows 11).
#[must_use]
pub fn virtual_camera_supported() -> bool {
    create_fn().is_some()
}

/// Where the camera stands on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Windows 10 or older: no `MFCreateVirtualCamera`.
    Unsupported,
    /// The media source is not registered for the machine; `dll` is the DLL
    /// found next to the executable, if any.
    NotRegistered {
        /// The DLL to register.
        dll: Option<PathBuf>,
    },
    /// Registered, but the registered file is missing.
    RegisteredFileMissing {
        /// The registered path.
        dll: PathBuf,
    },
    /// Registered and present: the camera can be created.
    Ready {
        /// The registered path.
        dll: PathBuf,
    },
}

/// The DLL next to `exe` (or next to its parent, where cargo puts test
/// binaries one level below the cdylib).
#[must_use]
pub fn locate_dll_near(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [dir.join(DLL_NAME), dir.parent()?.join(DLL_NAME)]
        .into_iter()
        .find(|p| p.is_file())
}

/// [`locate_dll_near`] the running executable.
#[must_use]
pub fn locate_dll() -> Option<PathBuf> {
    locate_dll_near(&std::env::current_exe().ok()?)
}

/// The camera's current [`Status`].
#[must_use]
pub fn status() -> Status {
    if !virtual_camera_supported() {
        return Status::Unsupported;
    }
    match registered_dll(Scope::Machine) {
        Some(dll) if dll.is_file() => Status::Ready { dll },
        Some(dll) => Status::RegisteredFileMissing { dll },
        None => Status::NotRegistered { dll: locate_dll() },
    }
}

/// Initialise COM (multithreaded; an existing apartment is kept) and Media
/// Foundation for this process. Media Foundation start-up is reference
/// counted; [`MfSession`] balances it.
///
/// # Errors
/// `MFStartup` failure.
pub fn mf_startup() -> windows_core::Result<MfSession> {
    // SAFETY: CoInitializeEx on the calling thread; RPC_E_CHANGED_MODE (an
    // STA already) is fine for the free-threaded objects used here, so the
    // result is deliberately ignored, and never balanced (the thread may be
    // reused by the engine).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
    }
    Ok(MfSession(()))
}

/// A Media Foundation start-up reference; `MFShutdown` on drop.
#[derive(Debug)]
pub struct MfSession(());

impl Drop for MfSession {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup in `mf_startup`.
        unsafe {
            let _ = MFShutdown();
        }
    }
}

/// A virtual camera registered with the Frame Server for this session.
/// Removed on drop.
#[derive(Debug)]
pub struct VirtualCamera {
    camera: IMFVirtualCamera,
    started: bool,
    _mf: MfSession,
}

// SAFETY: IMFVirtualCamera is a free-threaded Media Foundation object (the
// app creates it on one engine thread and may remove it from another).
unsafe impl Send for VirtualCamera {}

impl VirtualCamera {
    /// `MFCreateVirtualCamera(SoftwareCameraSource, Session, CurrentUser,
    /// friendly_name, CLSID)`, tagged with [`super::ATTR_OWNER_SID`] =
    /// `owner_sid` so the source reads that user's ring.
    ///
    /// # Errors
    /// `ERROR_NOT_SUPPORTED` (as an HRESULT) on Windows 10, or the call's
    /// HRESULT.
    pub fn create(friendly_name: &str, owner_sid: &str) -> windows_core::Result<Self> {
        let Some(create) = create_fn() else {
            return Err(windows_core::Error::from_hresult(
                windows::Win32::Foundation::ERROR_NOT_SUPPORTED.to_hresult(),
            ));
        };
        let mf = mf_startup()?;
        let name = HSTRING::from(friendly_name);
        let clsid = HSTRING::from(CLSID_STRING);
        let mut raw = std::ptr::null_mut();
        // SAFETY: calling the documented export with valid wide strings, no
        // categories, and an out-pointer we take ownership of on success.
        unsafe {
            create(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name.as_ptr()),
                PCWSTR(clsid.as_ptr()),
                std::ptr::null(),
                0,
                &mut raw,
            )
            .ok()?;
        }
        if raw.is_null() {
            return Err(windows_core::Error::from_hresult(
                windows::Win32::Foundation::E_POINTER,
            ));
        }
        // SAFETY: `raw` is the +1 IMFVirtualCamera the call returned.
        let camera = unsafe { IMFVirtualCamera::from_raw(raw) };
        // SAFETY: setting a string attribute on the live camera before Start.
        unsafe {
            camera.SetString(&super::ATTR_OWNER_SID, &HSTRING::from(owner_sid))?;
        }
        Ok(Self {
            camera,
            started: false,
            _mf: mf,
        })
    }

    /// Start the camera: other apps can now open it.
    ///
    /// # Errors
    /// The HRESULT of `IMFVirtualCamera::Start`.
    pub fn start(&mut self) -> windows_core::Result<()> {
        // SAFETY: starting a live camera without a completion callback.
        unsafe { self.camera.Start(None)? };
        self.started = true;
        Ok(())
    }

    /// The camera's media source as the Frame Server exposes it (for the
    /// round-trip test).
    ///
    /// # Errors
    /// The HRESULT of `IMFVirtualCamera::GetMediaSource`.
    pub fn media_source(&self) -> windows_core::Result<IMFMediaSource> {
        // SAFETY: plain getter on a live camera.
        unsafe { self.camera.GetMediaSource() }
    }

    /// The underlying interface (diagnostics: its attribute store carries the
    /// symbolic link once started).
    #[must_use]
    pub fn interface(&self) -> &IMFVirtualCamera {
        &self.camera
    }

    /// Stop and remove the camera. Also done on drop.
    pub fn remove(&mut self) {
        // SAFETY: stopping/removing/shutting down a live camera; errors are
        // ignored because this runs on teardown.
        unsafe {
            if self.started {
                let _ = self.camera.Stop();
                self.started = false;
            }
            let _ = self.camera.Remove();
            let _ = self.camera.Shutdown();
        }
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Stop the Frame Server and Frame Server Monitor services (administrator
/// only; they restart on demand) so the next camera loads the currently
/// registered DLL rather than one an earlier camera left loaded. Returns a
/// one-line summary for logs. Used by the install script's equivalent in
/// tests; the app never calls it.
#[must_use]
pub fn stop_frame_server_services() -> String {
    let mut out = Vec::new();
    for svc in ["FrameServer", "FrameServerMonitor"] {
        let stop = std::process::Command::new("sc.exe")
            .args(["stop", svc])
            .output();
        let mut state = String::from("?");
        for _ in 0..50 {
            let q = std::process::Command::new("sc.exe")
                .args(["query", svc])
                .output();
            let text = q
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            state = if text.contains("STOPPED") {
                "STOPPED".into()
            } else if text.contains("RUNNING") {
                "RUNNING".into()
            } else if text.contains("STOP_PENDING") {
                "STOP_PENDING".into()
            } else {
                text.lines()
                    .find(|l| l.contains("STATE"))
                    .unwrap_or("?")
                    .trim()
                    .to_string()
            };
            if state == "STOPPED" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let code = stop.map(|o| o.status.code().unwrap_or(-1)).unwrap_or(-1);
        out.push(format!("{svc}: stop exit {code}, now {state}"));
    }
    out.join("; ")
}

/// The media source's `ATTR_RING_STATE` (which ring and which DLL it uses),
/// read through `source`'s attributes, for diagnostics.
#[must_use]
pub fn source_ring_state(source: &IMFMediaSource) -> String {
    use windows::Win32::Media::MediaFoundation::IMFMediaSourceEx;
    // SAFETY: reading a string attribute through the source's interface; the
    // CoTaskMem string is freed after copying.
    let r = unsafe {
        source
            .cast::<IMFMediaSourceEx>()
            .and_then(|ex| ex.GetSourceAttributes())
            .and_then(|a| {
                let mut p = windows_core::PWSTR::null();
                let mut len = 0;
                a.GetAllocatedString(&super::ATTR_RING_STATE, &mut p, &mut len)?;
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                Ok(s)
            })
    };
    match r {
        Ok(s) => s,
        Err(e) => format!("unavailable (0x{:08X})", e.code().0 as u32),
    }
}

/// Why the app could not take its end of the ring.
#[derive(Debug)]
pub enum ProducerError {
    /// The ring does not exist yet or could not be mapped.
    Os(windows_core::Error),
    /// Another live producer (this pid) holds the ring.
    Busy {
        /// Its process id.
        pid: u32,
    },
}

impl std::fmt::Display for ProducerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProducerError::Os(e) => write!(f, "0x{:08X} {}", e.code().0 as u32, e.message()),
            ProducerError::Busy { pid } => write!(f, "process {pid} is already publishing"),
        }
    }
}

/// The app's end of the ring.
#[derive(Debug)]
pub struct Producer {
    // Field order matters: the writer (which releases the claim on drop)
    // must drop while the mapping is still alive.
    writer: Writer,
    shared: SharedRing,
}

impl Producer {
    /// Open (or, where this process may, create) `config`'s ring and claim
    /// the producer role (single producer; see [`Writer::claim`]).
    ///
    /// # Errors
    /// [`ProducerError::Os`], typically `ERROR_FILE_NOT_FOUND` until the
    /// camera's source has created the ring; [`ProducerError::Busy`] while
    /// another live producer holds it.
    pub fn open(config: &RingConfig, fps: f32) -> Result<Self, ProducerError> {
        let shared = SharedRing::open(config, Role::Producer, true).map_err(ProducerError::Os)?;
        // The writer's view stays valid while `shared` (held alongside it,
        // dropped after it) keeps the mapping alive.
        let writer = Writer::claim(
            shared.ring().view(),
            std::process::id(),
            super::now_100ns(),
            super::identity::process_alive,
        )
        .map_err(|ClaimError::Busy { pid }| ProducerError::Busy { pid })?;
        writer.set_fps(fps);
        Ok(Self { writer, shared })
    }

    /// Write one NV12 frame and wake the consumer.
    ///
    /// # Errors
    /// [`WriteError`] for bad geometry, or [`WriteError::Lost`] once another
    /// producer took the claim over.
    pub fn write_nv12(&mut self, width: u32, height: u32, data: &[u8]) -> Result<u64, WriteError> {
        let n = self
            .writer
            .write_nv12(width, height, data, super::now_100ns())?;
        self.shared.signal();
        Ok(n)
    }

    /// Number of the newest frame in the ring.
    #[must_use]
    pub fn latest(&self) -> u64 {
        self.writer.ring().latest()
    }

    /// Whether this process created the mapping.
    #[must_use]
    pub fn created(&self) -> bool {
        self.shared.created()
    }
}

/// One oak-cam virtual camera per user: an exclusively opened lock
/// file under `%LOCALAPPDATA%\oak-cam\` that the OS releases when the
/// holder exits (however it exits). A second app instance of the same user
/// learns the holder's pid instead of creating a second camera.
#[derive(Debug)]
pub struct InstanceLock {
    _file: std::fs::File,
}

impl InstanceLock {
    /// The lock file's path.
    #[must_use]
    pub fn path() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
        base.join("oak-cam").join("vcam.lock")
    }

    /// Take the lock.
    ///
    /// # Errors
    /// `Err(Some(pid))` when another live process holds it, `Err(None)` when
    /// the file cannot be opened for another reason.
    pub fn acquire() -> Result<Self, Option<u32>> {
        use std::io::{Read, Seek, Write};
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ)
            .open(&path);
        match opened {
            Ok(mut file) => {
                let _ = file.set_len(0);
                let _ = file.rewind();
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                Ok(Self { _file: file })
            }
            Err(_) => {
                let mut text = String::new();
                let pid = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .open(&path)
                    .ok()
                    .and_then(|mut f| f.read_to_string(&mut text).ok())
                    .and_then(|_| text.trim().parse().ok());
                Err(pid)
            }
        }
    }
}
