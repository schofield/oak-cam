//! Process environment as depthai sees it.

use std::ffi::CString;

use depthai_sys as sys;

use crate::{Error, Result, check};

/// Sets environment variable `key` to `value` unless it is already set.
///
/// Goes through the C runtime the shim and depthai share, so on Windows
/// depthai's `getenv` sees it (Rust's `std::env::set_var` updates only the
/// process block there). Must be called before other threads use the
/// environment, and before the first depthai call that reads `key`.
pub fn env_default(key: &str, value: &str) -> Result<()> {
    let nul = |what: &str, s: &str| Error::InvalidArgument(format!("{what} {s:?} contains NUL"));
    let k = CString::new(key).map_err(|_| nul("environment key", key))?;
    let v = CString::new(value).map_err(|_| nul("environment value", value))?;
    // SAFETY: both strings are NUL-terminated and outlive the call.
    check(unsafe { sys::oak_env_default(k.as_ptr(), v.as_ptr()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_only_when_unset_and_rejects_bad_keys() {
        let key = "OAK_CAM_TEST_ENV_DEFAULT";
        env_default(key, "first").unwrap();
        assert_eq!(std::env::var(key).as_deref(), Ok("first"));
        env_default(key, "second").unwrap();
        assert_eq!(
            std::env::var(key).as_deref(),
            Ok("first"),
            "existing value kept"
        );
        assert!(env_default("", "x").is_err());
        assert!(env_default("A=B", "x").is_err());
        assert!(env_default("A\0B", "x").is_err());
    }
}
