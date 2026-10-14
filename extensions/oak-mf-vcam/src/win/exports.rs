//! The four COM server exports. `DllRegisterServer` writes the **machine**
//! registration (what the Frame Server reads), so `regsvr32` must run
//! elevated; see [`super::registry`].

use std::ffi::c_void;
use std::path::PathBuf;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_POINTER, E_UNEXPECTED, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_core::{GUID, HRESULT, Interface, PCWSTR};

use super::com::factory::ClassFactory;
use super::registry::{Scope, register, unregister};

/// The path of the module (DLL or executable) this code is linked into.
#[must_use]
pub fn module_path() -> Option<PathBuf> {
    let mut module = HMODULE::default();
    let anchor = module_path as fn() -> Option<PathBuf>;
    // SAFETY: FROM_ADDRESS interprets the "name" as an address inside the
    // module; UNCHANGED_REFCOUNT means no FreeLibrary is owed.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(anchor as *const u16),
            &mut module,
        )
        .ok()?;
    }
    let mut buf = [0u16; 1024];
    // SAFETY: `buf` is a writable UTF-16 buffer.
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// Hand out the class factory for the media source's CLSID.
///
/// # Safety
/// COM calls this with valid `rclsid`/`riid` pointers and a writable `ppv`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null above; COM passes valid pointers.
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != super::clsid() {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory::new().into();
        factory.query(riid, ppv)
    }
}

/// The DLL may unload once no COM object, server lock or stream worker thread
/// of it is alive (so an updated DLL is picked up once cameras are released).
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if super::nothing_alive() {
        S_OK
    } else {
        S_FALSE
    }
}

/// Register this DLL for the media source class under `HKLM` (elevation
/// required; the Frame Server reads the machine registration).
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let Some(path) = module_path() else {
        return E_UNEXPECTED;
    };
    match register(&path, Scope::Machine) {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

/// Remove the machine registration.
#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match unregister(Scope::Machine) {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}
