//! Who is running: the current user's string SID (the ring's owner) and
//! whether a process is still alive (the producer claim's liveness check).

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, STILL_ACTIVE};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_core::PWSTR;

/// The string SID (`S-1-5-21-…`) of the user this process runs as.
///
/// # Errors
/// A failing token query.
pub fn current_user_sid() -> windows_core::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: querying our own process token into a local handle closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let result = token_user_sid(token);
    // SAFETY: closing the handle we opened.
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

/// The user SID of `token` as a string.
///
/// # Errors
/// A failing token query or conversion.
pub fn token_user_sid(token: HANDLE) -> windows_core::Result<String> {
    // TOKEN_USER plus its SID fits comfortably in 256 bytes; u64 storage
    // keeps it pointer-aligned.
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for its full size; TOKEN_USER is written at
    // its start, followed by the SID it points to.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            &mut len,
        )?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
        let s = text.to_string();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        s.map_err(|_| windows_core::Error::from(windows::Win32::Foundation::E_UNEXPECTED))
    }
}

/// Whether process `pid` is still running. A process we may not query is
/// reported alive (the claim then falls back to the heartbeat age).
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: opening a process for limited query and reading its exit code;
    // the handle is closed before returning.
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut code = 0u32;
                let alive =
                    GetExitCodeProcess(h, &mut code).is_err() || code == STILL_ACTIVE.0 as u32;
                let _ = CloseHandle(h);
                alive
            }
            // ERROR_INVALID_PARAMETER: no such process.
            Err(e) => e.code() != windows::Win32::Foundation::E_INVALIDARG,
        }
    }
}
