//! oak-cam's Windows 11 virtual camera, ported from stream-concern's
//! `sc-mf-vcam` (MIT, same author) with new CLSID, attribute GUIDs, ring
//! names and ring magic so both apps coexist.
//!
//! `MFCreateVirtualCamera` (Windows 11 build 22000 and later) registers a
//! camera whose frames come from a **custom Media Foundation media source**: a
//! COM class, identified by [`CLSID_STRING`], implemented in this crate's
//! `oak_mf_vcam.dll`. The Frame Server Monitor and Frame Server services load
//! the DLL in their own processes (session 0, Local System / Local Service),
//! so the app cannot hand frames over in-process. Instead the app writes its
//! frames into a **per-user shared-memory ring** ([`layout`], [`ring`],
//! [`naming`]) in the machine-wide `Global\` namespace and the media source reads the newest
//! frame from it, converting and scaling to whichever of the advertised
//! [`formats`] the consumer picked. When no frame is fresh the source paints
//! the [`convert::paint_placeholder_nv12`] card (dark grey with a band of luma
//! steps, the macOS extension's look) so consumers never stall.
//!
//! Module map:
//! - [`layout`], [`ring`], [`naming`], [`formats`], [`convert`]: pure, compiled and tested
//!   on every OS.
//! - `win` (Windows only): the named shared memory and event, the COM server
//!   (activation object, media source, media stream, class factory), the DLL
//!   exports, registration, and the app-side virtual-camera host.

pub mod convert;
pub mod formats;
pub mod layout;
pub mod naming;
pub mod ring;

#[cfg(target_os = "windows")]
pub mod win;

/// The media source's COM class id, as registered under
/// `HKLM\Software\Classes\CLSID` and passed to `MFCreateVirtualCamera`.
pub const CLSID_STRING: &str = "{38EE9B00-B255-4538-A39A-9DC8B3D0C64D}";

/// [`CLSID_STRING`] as a 128-bit value (the `GUID::from_u128` form).
pub const CLSID_U128: u128 = 0x38EE9B00_B255_4538_A39A_9DC8B3D0C64D;

/// The camera name other apps show. M4c: one camera per `CameraId` ("OAK RGB",
/// "OAK Left", "OAK Right") via a camera-index attribute; single camera for now.
pub const DEVICE_NAME: &str = "OAK RGB";

/// The file name cargo gives the cdylib, which the app looks for next to its
/// executable.
pub const DLL_NAME: &str = "oak_mf_vcam.dll";

#[cfg(test)]
mod tests {
    use super::*;

    /// The string and numeric CLSID forms agree.
    #[test]
    fn vcam_clsid_forms_agree() {
        let hex: String = CLSID_STRING
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect();
        assert_eq!(hex.len(), 32);
        assert_eq!(u128::from_str_radix(&hex, 16).unwrap(), CLSID_U128);
        assert!(CLSID_STRING.starts_with('{') && CLSID_STRING.ends_with('}'));
    }

    /// The registration script registers this crate's CLSID (not the
    /// stream-concern one it was ported from).
    #[test]
    fn vcam_build_script_uses_this_clsid() {
        let ps1 = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/vcam/build-windows.ps1"
        ));
        assert!(
            ps1.contains(&format!("$Clsid = \"{CLSID_STRING}\"")),
            "build-windows.ps1 must register {CLSID_STRING}"
        );
        assert!(ps1.contains(&format!("$DllName = \"{DLL_NAME}\"")));
    }
}
