//! depthai's host logger writes to fd 1; `oak-cam` must keep it off stdout.

use std::process::Command;

/// `DEPTHAI_PROTOCOL=bogus` makes depthai log a host warning during
/// discovery. stdout must still be exactly the JSON document.
#[test]
fn list_json_stdout_is_pure_json() {
    let out = Command::new(env!("CARGO_BIN_EXE_oak-cam"))
        .args(["--list", "--json"])
        .env("DEPTHAI_PROTOCOL", "bogus")
        .output()
        .expect("run oak-cam");
    assert!(
        out.status.success(),
        "exit {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not pure JSON ({e}): {stdout:?}"));
    assert!(parsed.is_array(), "stdout: {stdout:?}");
    // The warning (depthai v3.10: "Unsupported protocol specified") went to stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Unsupported protocol"),
        "expected depthai's warning on stderr, got: {stderr:?}"
    );
}
