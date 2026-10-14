//! Installing the macOS camera extension: the cameras are a CoreMediaIO system
//! extension embedded in `oak-cam.app`, and macOS only loads it after the app
//! submits an `OSSystemExtensionRequest` and the user approves it in System
//! Settings. This module owns that request ([`request_activation`]) and the
//! status readouts `oak-cam vcam status` prints ([`query_system_state`],
//! [`bundle_path`]).
//!
//! The request must come from the signed app bundle whose
//! `Contents/Library/SystemExtensions/` holds the extension, and that app must
//! carry the `com.apple.developer.system-extension.install` entitlement (which
//! needs a provisioning profile; see `scripts/vcam/build-macos.sh`). A bare
//! `cargo run` binary gets a clean failure from the delegate, not a crash.
//!
//! Ported from stream-concern's `src/app/vcam_install.rs`. The original
//! delivered callbacks on the main dispatch queue (serviced by the GUI run
//! loop); a CLI has no main run loop, so callbacks arrive on a private serial
//! queue and the caller polls the shared state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSError, NSString};

use super::{EXTENSION_BUNDLE_ID, SETTINGS_PATH};

/// Where an activation request stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallState {
    /// Nothing requested yet in this process.
    Idle,
    /// Submitted; waiting for macOS to answer.
    Requested,
    /// macOS asks the user to approve the extension in System Settings.
    NeedsApproval,
    /// Activated; the camera devices exist.
    Activated,
    /// Activated, but the previous version is still running until a reboot.
    AfterReboot,
    /// The request failed; the string is the user-facing reason.
    Failed(String),
}

impl InstallState {
    /// A short user-facing line for the state (empty for `Idle`).
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            InstallState::Idle => String::new(),
            InstallState::Requested => "Requesting the camera extension…".into(),
            InstallState::NeedsApproval => format!(
                "Approve the oak-cam camera extension under {SETTINGS_PATH} \
                 (on macOS 14: System Settings › Privacy & Security), then run \
                 `oak-cam vcam status`."
            ),
            InstallState::Activated => "Camera extension activated.".into(),
            InstallState::AfterReboot => {
                "Camera extension will finish activating after a reboot.".into()
            }
            InstallState::Failed(reason) => format!("Camera extension install failed: {reason}"),
        }
    }

    /// Whether the request is still waiting for macOS.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, InstallState::Requested)
    }
}

// Load the framework so the classes below resolve at run time; nothing is
// called through a linked symbol.
#[link(name = "SystemExtensions", kind = "framework")]
unsafe extern "C" {}

/// `OSSystemExtensionReplacementAction.replace`.
const REPLACEMENT_ACTION_REPLACE: isize = 1;
/// `OSSystemExtensionRequestResult.completed`.
const RESULT_COMPLETED: isize = 0;
/// `OSSystemExtensionRequestResult.willCompleteAfterReboot`.
const RESULT_AFTER_REBOOT: isize = 1;
/// `OSSystemExtensionError.missingEntitlement`.
const ERROR_MISSING_ENTITLEMENT: isize = 2;
/// `OSSystemExtensionError.unsupportedParentBundleLocation`.
const ERROR_UNSUPPORTED_LOCATION: isize = 3;
/// `OSSystemExtensionError.extensionNotFound`.
const ERROR_EXTENSION_NOT_FOUND: isize = 4;
/// `OSSystemExtensionError.codeSignatureInvalid`.
const ERROR_CODE_SIGNATURE_INVALID: isize = 8;

type Shared = Arc<Mutex<InstallState>>;

fn set(state: &Shared, value: InstallState) {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

fn get(state: &Shared) -> InstallState {
    state.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The user-facing reason for an `OSSystemExtensionErrorDomain` error code.
#[must_use]
pub fn error_reason(code: isize, localized: &str) -> String {
    let hint = match code {
        ERROR_MISSING_ENTITLEMENT => {
            " — the app lacks com.apple.developer.system-extension.install; build it with \
             scripts/vcam/build-macos.sh --provisionprofile <profile>"
        }
        ERROR_UNSUPPORTED_LOCATION => {
            " — the app must run from /Applications (or with `systemextensionsctl developer on`)"
        }
        ERROR_EXTENSION_NOT_FOUND => {
            " — run oak-cam from inside oak-cam.app (Contents/MacOS/oak-cam), whose \
             Contents/Library/SystemExtensions holds the extension"
        }
        ERROR_CODE_SIGNATURE_INVALID => {
            " — the app and extension must be Developer ID signed and notarised (or SIP off \
             with `systemextensionsctl developer on` for ad-hoc builds)"
        }
        _ => "",
    };
    format!("{localized} (OSSystemExtensionErrorDomain {code}){hint}")
}

struct DelegateIvars {
    state: Shared,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - The class does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "OakCamSystemExtensionRequestDelegate"]
    #[ivars = DelegateIvars]
    struct RequestDelegate;

    unsafe impl NSObjectProtocol for RequestDelegate {}

    // The four OSSystemExtensionRequestDelegate methods, looked up by
    // selector at run time (the protocol is not formally adopted).
    impl RequestDelegate {
        // SAFETY: matches the protocol signature; the returned value is an
        // OSSystemExtensionReplacementAction enum (NSInteger).
        #[unsafe(method(request:actionForReplacingExtension:withExtension:))]
        unsafe fn action_for_replacing(
            &self,
            _request: *mut AnyObject,
            _existing: *mut AnyObject,
            _extension: *mut AnyObject,
        ) -> isize {
            tracing::info!("camera extension: replacing the installed version");
            REPLACEMENT_ACTION_REPLACE
        }

        // SAFETY: matches the protocol signature; `error` is a live NSError
        // for the duration of the call.
        #[unsafe(method(request:didFailWithError:))]
        unsafe fn did_fail(&self, _request: *mut AnyObject, error: &NSError) {
            let reason = error_reason(error.code(), &error.localizedDescription().to_string());
            tracing::info!("camera extension: activation failed: {reason}");
            set(&self.ivars().state, InstallState::Failed(reason));
        }

        // SAFETY: matches the protocol signature; `result` is an
        // OSSystemExtensionRequestResult enum (NSInteger).
        #[unsafe(method(request:didFinishWithResult:))]
        unsafe fn did_finish(&self, _request: *mut AnyObject, result: isize) {
            let state = if result == RESULT_COMPLETED {
                InstallState::Activated
            } else if result == RESULT_AFTER_REBOOT {
                InstallState::AfterReboot
            } else {
                InstallState::Failed(format!("unexpected request result {result}"))
            };
            tracing::info!("camera extension: request finished: {state:?}");
            set(&self.ivars().state, state);
        }

        // SAFETY: matches the protocol signature.
        #[unsafe(method(requestNeedsUserApproval:))]
        unsafe fn needs_user_approval(&self, _request: *mut AnyObject) {
            tracing::info!("camera extension: waiting for the user's approval");
            set(&self.ivars().state, InstallState::NeedsApproval);
        }
    }
);

impl RequestDelegate {
    fn new(state: Shared) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { state });
        // SAFETY: init on a freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }
}

/// Submit an activation request for the bundled extension and wait until
/// macOS answers: `Activated`, `AfterReboot`, `Failed`, or `NeedsApproval`
/// (returned at once unless `wait_for_approval`; the approval then completes
/// in System Settings without this process). Gives up with `Failed` after
/// `timeout`.
#[must_use]
pub fn request_activation(timeout: Duration, wait_for_approval: bool) -> InstallState {
    let Some(request_class) = AnyClass::get(c"OSSystemExtensionRequest") else {
        return InstallState::Failed("SystemExtensions framework is unavailable".into());
    };
    let Some(manager_class) = AnyClass::get(c"OSSystemExtensionManager") else {
        return InstallState::Failed("SystemExtensions framework is unavailable".into());
    };
    let state: Shared = Arc::new(Mutex::new(InstallState::Requested));
    let delegate = RequestDelegate::new(Arc::clone(&state));
    let queue = dispatch2::DispatchQueue::new("com.surlysoft.oak-cam.sysext-request", None);
    let id = NSString::from_str(EXTENSION_BUNDLE_ID);
    // SAFETY: documented class methods with the documented argument types
    // (NSString identifier, dispatch queue, request object); the manager
    // retains the request while it runs and we keep our own retain in
    // `_request` until we return. The delegate outlives the request here too.
    let _request: Retained<AnyObject> = unsafe {
        let request: Retained<AnyObject> = msg_send![
            request_class,
            activationRequestForExtension: &*id,
            queue: &*queue
        ];
        let delegate_obj: &AnyObject = &delegate;
        let () = msg_send![&*request, setDelegate: delegate_obj];
        let manager: Retained<AnyObject> = msg_send![manager_class, sharedManager];
        let () = msg_send![&*manager, submitRequest: &*request];
        request
    };
    tracing::info!("camera extension: activation requested for {EXTENSION_BUNDLE_ID}");

    let deadline = Instant::now() + timeout;
    loop {
        let now = get(&state);
        let settled = match now {
            InstallState::Requested | InstallState::Idle => false,
            InstallState::NeedsApproval => !wait_for_approval,
            _ => true,
        };
        if settled {
            return now;
        }
        if Instant::now() >= deadline {
            return if now == InstallState::NeedsApproval {
                now
            } else {
                InstallState::Failed(format!(
                    "no answer from macOS within {} s",
                    timeout.as_secs()
                ))
            };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The extension's state as `systemextensionsctl list` reports it (readable
/// without privileges, from any process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemState {
    /// Not listed: never activated on this Mac (or uninstalled).
    NotInstalled,
    /// Listed and waiting for the user's approval in System Settings.
    AwaitingApproval {
        /// `short/bundle` version pair as listed.
        version: String,
    },
    /// Activated and enabled.
    Enabled {
        /// `short/bundle` version pair as listed.
        version: String,
    },
    /// Listed in another state (disabled, terminated, uninstalling…).
    Other {
        /// `short/bundle` version pair as listed.
        version: String,
        /// The bracketed state text.
        state: String,
    },
}

/// Parse `systemextensionsctl list` output for `bundle_id`. The active entry
/// (a `*` in the second column) wins over stale ones.
#[must_use]
pub fn parse_systemextensionsctl(output: &str, bundle_id: &str) -> SystemState {
    let mut best: Option<(bool, SystemState)> = None;
    for line in output.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        // enabled, active, teamID, "bundleID (version)", name, [state]
        let Some(ident) = cols.get(3) else { continue };
        let Some((id, rest)) = ident.split_once(" (") else {
            continue;
        };
        if id.trim() != bundle_id {
            continue;
        }
        let version = rest.trim_end_matches(')').to_string();
        let active = cols.get(1).is_some_and(|c| c.trim() == "*");
        let state_text = line
            .rsplit_once('[')
            .map(|(_, s)| s.trim_end().trim_end_matches(']').to_string())
            .unwrap_or_default();
        let state = if state_text.contains("waiting for user") {
            SystemState::AwaitingApproval { version }
        } else if state_text == "activated enabled" {
            SystemState::Enabled { version }
        } else {
            SystemState::Other {
                version,
                state: state_text,
            }
        };
        if best
            .as_ref()
            .is_none_or(|(was_active, _)| active && !was_active)
        {
            best = Some((active, state));
        }
    }
    best.map_or(SystemState::NotInstalled, |(_, s)| s)
}

/// Run `systemextensionsctl list` and report the extension's state.
///
/// # Errors
/// The tool could not be run.
pub fn query_system_state() -> std::io::Result<SystemState> {
    let out = Command::new("/usr/bin/systemextensionsctl")
        .arg("list")
        .output()?;
    Ok(parse_systemextensionsctl(
        &String::from_utf8_lossy(&out.stdout),
        EXTENSION_BUNDLE_ID,
    ))
}

/// The `.app` bundle containing `exe`, if it runs from `X.app/Contents/MacOS/`.
#[must_use]
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

/// The app bundle this process runs from, if any.
#[must_use]
pub fn bundle_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| bundle_of(&exe.canonicalize().unwrap_or(exe)))
}

/// The extension bundle inside `app`.
#[must_use]
pub fn embedded_extension(app: &Path) -> PathBuf {
    app.join("Contents/Library/SystemExtensions")
        .join(format!("{EXTENSION_BUNDLE_ID}.systemextension"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "7 extension(s)\n\
--- com.apple.system_extension.cmio (Go to 'System Settings > General > Login Items & Extensions > Camera Extensions' to modify these system extension(s))\n\
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]\n\
*\t*\t5V2V8BX632\tcom.surlysoft.stream-concern.camera-extension (0.1.0/0.1.508)\tstream-concern Virtual Camera\t[activated enabled]\n\
\t\t5V2V8BX632\tcom.surlysoft.oak-cam.camera-extension (0.1.0/0.1.3)\toak-cam Cameras\t[terminated waiting to uninstall on reboot]\n\
\t*\t5V2V8BX632\tcom.surlysoft.oak-cam.camera-extension (0.1.0/0.1.4)\toak-cam Cameras\t[activated waiting for user]\n";

    #[test]
    fn parses_systemextensionsctl() {
        assert_eq!(
            parse_systemextensionsctl(LIST, "com.surlysoft.oak-cam.camera-extension"),
            SystemState::AwaitingApproval {
                version: "0.1.0/0.1.4".into()
            }
        );
        assert_eq!(
            parse_systemextensionsctl(LIST, "com.surlysoft.stream-concern.camera-extension"),
            SystemState::Enabled {
                version: "0.1.0/0.1.508".into()
            }
        );
        assert_eq!(
            parse_systemextensionsctl(LIST, "com.example.none"),
            SystemState::NotInstalled
        );
        let stale_only = LIST.lines().take(4).collect::<Vec<_>>().join("\n");
        let stale_only = format!("{stale_only}\n{}", LIST.lines().nth(4).unwrap());
        assert!(matches!(
            parse_systemextensionsctl(&stale_only, "com.surlysoft.oak-cam.camera-extension"),
            SystemState::Other { .. }
        ));
    }

    #[test]
    fn describe_names_settings_pane() {
        assert!(InstallState::Idle.describe().is_empty());
        assert!(
            InstallState::NeedsApproval
                .describe()
                .contains("Camera Extensions")
        );
        assert!(
            InstallState::Failed("x".into())
                .describe()
                .ends_with("failed: x")
        );
        assert!(InstallState::Requested.is_pending());
        assert!(!InstallState::Activated.is_pending());
    }

    #[test]
    fn error_reasons_carry_hints() {
        assert!(error_reason(2, "Missing entitlement").contains("--provisionprofile"));
        assert!(error_reason(3, "x").contains("/Applications"));
        assert!(error_reason(4, "x").contains("Contents/MacOS/oak-cam"));
        assert_eq!(error_reason(99, "x"), "x (OSSystemExtensionErrorDomain 99)");
    }

    #[test]
    fn bundle_detection() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/oak-cam.app/Contents/MacOS/oak-cam"
            )),
            Some(PathBuf::from("/Applications/oak-cam.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/oak-cam")), None);
        assert_eq!(
            embedded_extension(Path::new("/Applications/oak-cam.app")),
            PathBuf::from(
                "/Applications/oak-cam.app/Contents/Library/SystemExtensions/\
                 com.surlysoft.oak-cam.camera-extension.systemextension"
            )
        );
    }

    /// From a test binary (not a bundled, entitled app) the request fails
    /// cleanly through the delegate instead of crashing or hanging. Ignored:
    /// it talks to sysextd, which a plain unit-test run should not do.
    #[test]
    #[ignore = "submits a real OSSystemExtensionRequest"]
    fn request_outside_bundle_fails_cleanly() {
        let state = request_activation(Duration::from_secs(10), false);
        eprintln!("{state:?}");
        assert!(matches!(state, InstallState::Failed(_)), "{state:?}");
    }
}
