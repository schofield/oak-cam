//! The activation object the CLSID creates. The Frame Server (and any other
//! client) calls `ActivateObject` to get the media source; the attribute store
//! is a plain Media Foundation store the object delegates to.

use parking_lot::Mutex;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFMediaSource,
    MF_ATTRIBUTE_TYPE, MF_ATTRIBUTES_MATCH_TYPE, MFCreateAttributes,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{BOOL, GUID, IUnknown, Interface, PCWSTR, PWSTR, Ref, implement};

use super::super::shm::{RingConfig, process_names};
use super::super::{ATTR_OWNER_SID, Agile};
use super::source::create_source;
use windows::Win32::System::Com::CoTaskMemFree;

/// `IMFActivate` for the media source.
#[implement(IMFActivate)]
pub struct SourceActivate {
    _alive: super::super::Alive,
    attributes: Agile<IMFAttributes>,
    source: Mutex<Option<Agile<IMFMediaSource>>>,
}

impl SourceActivate {
    /// A fresh activation object with an empty attribute store.
    ///
    /// # Errors
    /// `MFCreateAttributes` failure.
    pub fn new() -> windows_core::Result<Self> {
        let mut attributes = None;
        // SAFETY: creating an attribute store into a local.
        unsafe { MFCreateAttributes(&mut attributes, 4)? };
        let attributes = attributes
            .ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
        Ok(Self {
            _alive: super::super::Alive::new(),
            attributes: Agile(attributes),
            source: Mutex::new(None),
        })
    }
}

impl SourceActivate {
    /// Whose ring the source reads: the owner SID the app set on the virtual
    /// camera (it arrives in this object's attribute store), else a test's
    /// process-wide override, else none (placeholder only). There is no
    /// shared default ring.
    fn ring_config(&self) -> Option<RingConfig> {
        let mut text = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: reading a string attribute from our own store; the
        // CoTaskMem string is freed after copying.
        let sid = unsafe {
            self.attributes
                .GetAllocatedString(&ATTR_OWNER_SID, &mut text, &mut len)
                .ok()
                .map(|()| {
                    let s = text.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(text.0 as _));
                    s
                })
        };
        if let Some(sid) = sid {
            return RingConfig::for_sid(&sid);
        }
        process_names().and_then(|names| RingConfig::with_names(names).ok())
    }
}

/// Create the activation object as `IMFActivate`.
///
/// # Errors
/// `MFCreateAttributes` failure.
pub fn create_activate() -> windows_core::Result<IMFActivate> {
    Ok(SourceActivate::new()?.into())
}

impl IMFActivate_Impl for SourceActivate_Impl {
    fn ActivateObject(
        &self,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        let source = {
            let mut slot = self.source.lock();
            match slot.as_ref() {
                Some(s) => s.0.clone(),
                None => {
                    let s: IMFMediaSource = create_source(self.ring_config())?.cast()?;
                    *slot = Some(Agile(s.clone()));
                    s
                }
            }
        };
        // SAFETY: `riid`/`ppv` are the caller's out-parameters, forwarded to
        // QueryInterface on a live object.
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> windows_core::Result<()> {
        if let Some(source) = self.source.lock().take() {
            // SAFETY: shutting down the source we created.
            unsafe {
                let _ = source.Shutdown();
            }
        }
        Ok(())
    }

    fn DetachObject(&self) -> windows_core::Result<()> {
        self.source.lock().take();
        Ok(())
    }
}

// Every method forwards to the inner store; the pointers are the caller's and
// are passed through unchanged, so each SAFETY argument is the same: the
// caller upholds the IMFAttributes contract for them.
impl IMFAttributes_Impl for SourceActivate_Impl {
    fn GetItem(&self, guidkey: *const GUID, pvalue: *mut PROPVARIANT) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers (see above).
        unsafe {
            self.attributes
                .GetItem(guidkey, (!pvalue.is_null()).then_some(pvalue))
        }
    }
    fn GetItemType(&self, guidkey: *const GUID) -> windows_core::Result<MF_ATTRIBUTE_TYPE> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetItemType(guidkey) }
    }
    fn CompareItem(
        &self,
        guidkey: *const GUID,
        value: *const PROPVARIANT,
    ) -> windows_core::Result<BOOL> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.CompareItem(guidkey, value) }
    }
    fn Compare(
        &self,
        ptheirs: Ref<IMFAttributes>,
        matchtype: MF_ATTRIBUTES_MATCH_TYPE,
    ) -> windows_core::Result<BOOL> {
        // SAFETY: forwarded caller interface.
        unsafe { self.attributes.Compare(ptheirs.ok()?, matchtype) }
    }
    fn GetUINT32(&self, guidkey: *const GUID) -> windows_core::Result<u32> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetUINT32(guidkey) }
    }
    fn GetUINT64(&self, guidkey: *const GUID) -> windows_core::Result<u64> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetUINT64(guidkey) }
    }
    fn GetDouble(&self, guidkey: *const GUID) -> windows_core::Result<f64> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetDouble(guidkey) }
    }
    fn GetGUID(&self, guidkey: *const GUID) -> windows_core::Result<GUID> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetGUID(guidkey) }
    }
    fn GetStringLength(&self, guidkey: *const GUID) -> windows_core::Result<u32> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetStringLength(guidkey) }
    }
    fn GetString(
        &self,
        guidkey: *const GUID,
        pwszvalue: PWSTR,
        cchbufsize: u32,
        pcchlength: *mut u32,
    ) -> windows_core::Result<()> {
        if pwszvalue.is_null() {
            return Err(windows::Win32::Foundation::E_POINTER.into());
        }
        // SAFETY: the caller supplies a buffer of `cchbufsize` UTF-16 units.
        let buf = unsafe { std::slice::from_raw_parts_mut(pwszvalue.0, cchbufsize as usize) };
        // SAFETY: forwarded caller pointers.
        unsafe {
            self.attributes
                .GetString(guidkey, buf, (!pcchlength.is_null()).then_some(pcchlength))
        }
    }
    fn GetAllocatedString(
        &self,
        guidkey: *const GUID,
        ppwszvalue: *mut PWSTR,
        pcchlength: *mut u32,
    ) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe {
            self.attributes
                .GetAllocatedString(guidkey, ppwszvalue, pcchlength)
        }
    }
    fn GetBlobSize(&self, guidkey: *const GUID) -> windows_core::Result<u32> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetBlobSize(guidkey) }
    }
    fn GetBlob(
        &self,
        guidkey: *const GUID,
        pbuf: *mut u8,
        cbbufsize: u32,
        pcbblobsize: *mut u32,
    ) -> windows_core::Result<()> {
        if pbuf.is_null() {
            return Err(windows::Win32::Foundation::E_POINTER.into());
        }
        // SAFETY: the caller supplies a buffer of `cbbufsize` bytes.
        let buf = unsafe { std::slice::from_raw_parts_mut(pbuf, cbbufsize as usize) };
        // SAFETY: forwarded caller pointers.
        unsafe {
            self.attributes.GetBlob(
                guidkey,
                buf,
                (!pcbblobsize.is_null()).then_some(pcbblobsize),
            )
        }
    }
    fn GetAllocatedBlob(
        &self,
        guidkey: *const GUID,
        ppbuf: *mut *mut u8,
        pcbsize: *mut u32,
    ) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.GetAllocatedBlob(guidkey, ppbuf, pcbsize) }
    }
    fn GetUnknown(
        &self,
        guidkey: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers; the stored object is queried for
        // the caller's interface.
        unsafe {
            let unknown: IUnknown = self.attributes.GetUnknown(guidkey)?;
            unknown.query(riid, ppv).ok()
        }
    }
    fn SetItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetItem(guidkey, value) }
    }
    fn DeleteItem(&self, guidkey: *const GUID) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.DeleteItem(guidkey) }
    }
    fn DeleteAllItems(&self) -> windows_core::Result<()> {
        // SAFETY: no pointers.
        unsafe { self.attributes.DeleteAllItems() }
    }
    fn SetUINT32(&self, guidkey: *const GUID, unvalue: u32) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetUINT32(guidkey, unvalue) }
    }
    fn SetUINT64(&self, guidkey: *const GUID, unvalue: u64) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetUINT64(guidkey, unvalue) }
    }
    fn SetDouble(&self, guidkey: *const GUID, fvalue: f64) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetDouble(guidkey, fvalue) }
    }
    fn SetGUID(&self, guidkey: *const GUID, guidvalue: *const GUID) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetGUID(guidkey, guidvalue) }
    }
    fn SetString(&self, guidkey: *const GUID, wszvalue: &PCWSTR) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetString(guidkey, *wszvalue) }
    }
    fn SetBlob(
        &self,
        guidkey: *const GUID,
        pbuf: *const u8,
        cbbufsize: u32,
    ) -> windows_core::Result<()> {
        let buf = if pbuf.is_null() {
            &[][..]
        } else {
            // SAFETY: the caller supplies `cbbufsize` readable bytes.
            unsafe { std::slice::from_raw_parts(pbuf, cbbufsize as usize) }
        };
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetBlob(guidkey, buf) }
    }
    fn SetUnknown(
        &self,
        guidkey: *const GUID,
        punknown: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe { self.attributes.SetUnknown(guidkey, punknown.as_ref()) }
    }
    fn LockStore(&self) -> windows_core::Result<()> {
        // SAFETY: no pointers.
        unsafe { self.attributes.LockStore() }
    }
    fn UnlockStore(&self) -> windows_core::Result<()> {
        // SAFETY: no pointers.
        unsafe { self.attributes.UnlockStore() }
    }
    fn GetCount(&self) -> windows_core::Result<u32> {
        // SAFETY: no pointers.
        unsafe { self.attributes.GetCount() }
    }
    fn GetItemByIndex(
        &self,
        unindex: u32,
        pguidkey: *mut GUID,
        pvalue: *mut PROPVARIANT,
    ) -> windows_core::Result<()> {
        // SAFETY: forwarded caller pointers.
        unsafe {
            self.attributes
                .GetItemByIndex(unindex, pguidkey, (!pvalue.is_null()).then_some(pvalue))
        }
    }
    fn CopyAllItems(&self, pdest: Ref<IMFAttributes>) -> windows_core::Result<()> {
        // SAFETY: forwarded caller interface.
        unsafe { self.attributes.CopyAllItems(pdest.ok()?) }
    }
}
