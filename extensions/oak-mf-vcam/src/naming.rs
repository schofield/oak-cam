//! Per-user names and access control for the ring (pure, testable anywhere).
//!
//! Every Windows user gets their own ring, named after their SID
//! (`Global\oak-cam-vcam-<SID>-ring` / `-frame`), and the kernel objects
//! grant access to SYSTEM, Local Service (the Frame Server), Administrators and
//! that user only. The SID travels from the app to the media source as an
//! attribute on the virtual camera ([`crate::win`]'s `ATTR_OWNER_SID`), so the
//! source running for user A's camera never opens user B's ring.

/// Prefix of every ring's kernel object names.
// M4c: one ring per camera index, `Global\oak-cam-vcam-<SID>-<idx>-{ring,frame}`.
pub const RING_NAME_PREFIX: &str = r"Global\oak-cam-vcam-";

/// Whether `sid` is a string SID (`S-1-` followed by digits and dashes). Only
/// such strings are ever placed in object names or SDDL, so nothing a caller
/// passes can add ACEs or path separators.
#[must_use]
pub fn is_valid_sid(sid: &str) -> bool {
    let Some(rest) = sid.strip_prefix("S-1-") else {
        return false;
    };
    !rest.is_empty()
        && sid.len() <= 184
        && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        && !rest.starts_with('-')
        && !rest.ends_with('-')
        && !rest.contains("--")
}

/// The object-name prefix of `sid`'s ring (`<prefix>-ring`, `<prefix>-frame`).
#[must_use]
pub fn ring_prefix_for_sid(sid: &str) -> Option<String> {
    is_valid_sid(sid).then(|| format!("{RING_NAME_PREFIX}{sid}"))
}

/// The security descriptor (SDDL) for `owner_sid`'s ring objects: a protected
/// DACL with full access for SYSTEM, LOCAL SERVICE, Administrators and the
/// owner, and nothing for anyone else (no Authenticated Users, no Everyone).
#[must_use]
pub fn ring_sddl(owner_sid: &str) -> Option<String> {
    is_valid_sid(owner_sid)
        .then(|| format!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;{owner_sid})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1004336348-1177238915-682003330-512";

    /// The descriptor for a SID grants exactly SYSTEM, Local Service,
    /// Administrators and that SID, and never Authenticated Users or Everyone.
    #[test]
    fn vcam_sddl_for_sid() {
        let sddl = ring_sddl(SID).unwrap();
        assert_eq!(
            sddl,
            "D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)\
             (A;;GA;;;S-1-5-21-1004336348-1177238915-682003330-512)"
        );
        assert!(!sddl.contains(";AU)") && !sddl.contains(";WD)") && !sddl.contains(";IU)"));
        assert_eq!(sddl.matches("(A;").count(), 4);
    }

    /// Names are per user.
    #[test]
    fn vcam_ring_names_per_sid() {
        assert_eq!(
            ring_prefix_for_sid(SID).unwrap(),
            format!(r"Global\oak-cam-vcam-{SID}")
        );
        assert_ne!(
            ring_prefix_for_sid(SID),
            ring_prefix_for_sid("S-1-5-21-1-2-3-1001")
        );
    }

    /// Anything that is not a plain string SID is refused, so it can never
    /// inject ACEs or path components.
    #[test]
    fn vcam_sid_validation() {
        assert!(is_valid_sid(SID));
        assert!(is_valid_sid("S-1-5-18"));
        for bad in [
            "",
            "S-1-",
            "S-1-5)(A;;GA;;;WD",
            "S-1-5-18\\x",
            "AU",
            "WD",
            "S-1-5--18",
            "S-1-5-18-",
            "s-1-5-18",
        ] {
            assert!(!is_valid_sid(bad), "{bad:?}");
            assert!(ring_sddl(bad).is_none(), "{bad:?}");
            assert!(ring_prefix_for_sid(bad).is_none(), "{bad:?}");
        }
    }
}
