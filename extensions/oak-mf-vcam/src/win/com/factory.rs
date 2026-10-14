//! The class factory `DllGetClassObject` returns for [`crate::CLSID_STRING`].

use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::Media::MediaFoundation::IMFActivate;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows_core::{BOOL, GUID, IUnknown, Interface, Ref, implement};

use super::activate::create_activate;

/// Creates [`super::activate::SourceActivate`] objects.
#[implement(IClassFactory)]
pub struct ClassFactory {
    _alive: super::super::Alive,
}

impl ClassFactory {
    /// A factory, counted as a live object.
    #[must_use]
    pub fn new() -> Self {
        Self {
            _alive: super::super::Alive::new(),
        }
    }
}

impl Default for ClassFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        if ppvobject.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `ppvobject` was checked non-null; clear it before any
        // failure path per the COM contract.
        unsafe { *ppvobject = std::ptr::null_mut() };
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activate: IMFActivate = create_activate()?;
        // SAFETY: forwarding the caller's IID and out-pointer to QueryInterface.
        unsafe { activate.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, flock: BOOL) -> windows_core::Result<()> {
        super::super::lock_server(flock.as_bool());
        Ok(())
    }
}
