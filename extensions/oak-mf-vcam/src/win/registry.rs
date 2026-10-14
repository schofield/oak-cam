//! COM registration of the media source.
//!
//! The Frame Server Monitor (Local System) and Frame Server (Local Service)
//! resolve the CLSID in their own registry view, so a working camera needs
//! the **machine** registration (`HKLM\Software\Classes\CLSID`), which needs an
//! administrator once. The per-user registration (`HKCU`) is visible only to
//! the user's own processes: the tests use it to exercise `CoCreateInstance`
//! without elevation.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows_core::{HSTRING, PCWSTR};

use crate::CLSID_STRING;

/// Where a registration lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `HKLM\Software\Classes`: what the Frame Server sees (needs admin).
    Machine,
    /// `HKCU\Software\Classes`: this user's processes only.
    User,
}

impl Scope {
    fn root(self) -> HKEY {
        match self {
            Scope::Machine => HKEY_LOCAL_MACHINE,
            Scope::User => HKEY_CURRENT_USER,
        }
    }
}

/// The class key under the scope's root.
#[must_use]
pub fn class_key() -> String {
    format!(r"Software\Classes\CLSID\{CLSID_STRING}")
}

fn check(err: WIN32_ERROR) -> windows_core::Result<()> {
    err.ok()
}

fn set_string(key: HKEY, name: Option<&str>, value: &str) -> windows_core::Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer; REG_SZ takes its bytes
    // including the terminator.
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    // SAFETY: `key` is open with KEY_WRITE; `name_ptr` is null or a valid
    // wide string; `bytes` outlives the call.
    check(unsafe { RegSetValueExW(key, name_ptr, None, REG_SZ, Some(bytes)) })
}

fn create_key(root: HKEY, path: &str) -> windows_core::Result<HKEY> {
    let mut key = HKEY::default();
    let path = HSTRING::from(path);
    // SAFETY: valid root and path; `key` receives an open handle we close.
    check(unsafe {
        RegCreateKeyExW(
            root,
            &path,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    })?;
    Ok(key)
}

/// Register `dll` as the in-process server of the media source class in
/// `scope` (`InprocServer32`, `ThreadingModel=Both`).
///
/// # Errors
/// The Win32 error, e.g. `E_ACCESSDENIED` for [`Scope::Machine`] without
/// elevation.
pub fn register(dll: &Path, scope: Scope) -> windows_core::Result<()> {
    let class = create_key(scope.root(), &class_key())?;
    let res = set_string(class, None, "oak-cam Virtual Camera media source");
    // SAFETY: closing the key we opened.
    unsafe {
        let _ = RegCloseKey(class);
    }
    res?;
    let server = create_key(scope.root(), &format!(r"{}\InprocServer32", class_key()))?;
    let res = set_string(server, None, &dll.to_string_lossy())
        .and_then(|()| set_string(server, Some("ThreadingModel"), "Both"));
    // SAFETY: closing the key we opened.
    unsafe {
        let _ = RegCloseKey(server);
    }
    res
}

/// Remove the class registration from `scope`. Removing an absent
/// registration succeeds.
///
/// # Errors
/// The Win32 error, e.g. `E_ACCESSDENIED` for [`Scope::Machine`] without
/// elevation.
pub fn unregister(scope: Scope) -> windows_core::Result<()> {
    let path = HSTRING::from(class_key());
    // SAFETY: valid root and path.
    let err = unsafe { RegDeleteTreeW(scope.root(), &path) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(err)
}

/// The DLL path registered for the class in `scope`, if any.
#[must_use]
pub fn registered_dll(scope: Scope) -> Option<PathBuf> {
    let path = HSTRING::from(format!(r"{}\InprocServer32", class_key()));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `len` bytes; RegGetValueW NUL-terminates REG_SZ.
    let err = unsafe {
        RegGetValueW(
            scope.root(),
            &path,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err.is_err() {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..chars]);
    let s = s.trim_end_matches('\0');
    (!s.is_empty()).then(|| PathBuf::from(s))
}
