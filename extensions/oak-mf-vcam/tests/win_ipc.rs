//! Windows ring IPC over real named kernel objects: producer and consumer on
//! two threads and in two processes (the test binary re-runs itself as the
//! child), the single-producer claim across processes, and the owner-only
//! DACL against a token without the owner's SID.

#![cfg(target_os = "windows")]

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use oak_mf_vcam::layout::{SLOT_CAPACITY, nv12_size};
use oak_mf_vcam::win::host::{Producer, ProducerError};
use oak_mf_vcam::win::shm::{RingConfig, RingNames, Role, SharedRing};

const CHILD_ENV: &str = "SC_VCAM_CHILD_RING";
const CLAIM_ENV: &str = "SC_VCAM_CLAIM_RING";
const CLAIM_MODE_ENV: &str = "SC_VCAM_CLAIM_MODE";
const W: u32 = 640;
const H: u32 = 360;

fn names(tag: &str) -> RingNames {
    RingNames::with_prefix(&format!(r"Local\sc-vcam-test-{tag}-{}", std::process::id()))
}

fn config(names: &RingNames) -> RingConfig {
    RingConfig::with_names(names.clone()).expect("current user SID")
}

/// Re-run this test binary filtered to `test`, with `envs`; returns stderr.
fn run_child(test: &str, envs: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the child process");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprintln!("vcam child ({test}) stderr:\n{stderr}");
    assert!(
        out.status.success(),
        "child {test} failed: {:?}",
        out.status
    );
    stderr
}

/// Read frames as the consumer until `want` distinct, untorn frames arrived.
/// Every byte of frame n is n's low byte, so a torn copy is detectable.
fn consume(names: &RingNames, want: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    let shared = loop {
        match SharedRing::open(&config(names), Role::Consumer, false) {
            Ok(s) => break s,
            Err(e) if Instant::now() < deadline => {
                eprintln!("vcam consumer: open failed ({e}), retrying");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("vcam consumer: could not open the ring: {e}"),
        }
    };
    assert!(shared.has_event(), "the new-frame event opened");
    let mut dst = vec![0u8; SLOT_CAPACITY];
    let mut last = 0;
    let mut seen = 0;
    while seen < want && Instant::now() < deadline {
        shared.wait(100);
        if let Some(meta) = shared.ring().read_latest(last, &mut dst) {
            assert_eq!((meta.width, meta.height), (W, H));
            let first = dst[0];
            assert!(
                dst[..meta.len].iter().all(|&b| b == first),
                "torn frame {}",
                meta.frame
            );
            assert_eq!(first, meta.frame as u8, "frame {} content", meta.frame);
            last = meta.frame;
            seen += 1;
        }
    }
    seen
}

fn produce_until(names: &RingNames, stop: &AtomicBool, max: Duration) -> Producer {
    let mut producer = Producer::open(&config(names), 30.0).expect("producer creates a Local ring");
    eprintln!("vcam producer: created={}", producer.created());
    let mut buf = vec![0u8; nv12_size(W, H)];
    let deadline = Instant::now() + max;
    let mut n = 0u64;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        n += 1;
        // Frame numbers continue from the ring's: fill with the number the
        // writer is about to assign.
        buf.fill((producer.latest() + 1) as u8);
        let got = producer.write_nv12(W, H, &buf).unwrap();
        assert_eq!(got as u8, buf[0]);
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("vcam producer: wrote {n} frames");
    producer
}

/// Producer and consumer threads over a named `Local\` mapping and event.
#[test]
fn vcam_ring_named_two_threads() {
    let names = names("threads");
    let stop = Arc::new(AtomicBool::new(false));
    let (n2, s2) = (names.clone(), stop.clone());
    let producer =
        std::thread::spawn(move || drop(produce_until(&n2, &s2, Duration::from_secs(20))));
    let seen = consume(&names, 30, Duration::from_secs(15));
    stop.store(true, Ordering::Relaxed);
    producer.join().unwrap();
    eprintln!("vcam two-thread: consumer saw {seen} frames");
    assert!(seen >= 30, "consumer saw only {seen} frames");
}

/// The consumer half of [`vcam_ring_cross_process`]; a no-op unless the
/// parent set the ring name.
#[test]
fn vcam_ring_cross_process_child() {
    let Ok(prefix) = std::env::var(CHILD_ENV) else {
        return;
    };
    let seen = consume(
        &RingNames::with_prefix(&prefix),
        30,
        Duration::from_secs(15),
    );
    eprintln!("vcam child: saw {seen} frames");
    assert!(seen >= 30, "child saw only {seen} frames");
}

/// Producer here, consumer in a child process (this binary re-run with a
/// filter), over the same named mapping and event.
#[test]
fn vcam_ring_cross_process() {
    let names = names("xproc");
    let prefix = names.mapping.trim_end_matches("-ring").to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let (n2, s2) = (names.clone(), stop.clone());
    let producer = std::thread::spawn(move || produce_until(&n2, &s2, Duration::from_secs(30)));
    // Let the producer create the objects before the child opens them.
    std::thread::sleep(Duration::from_millis(200));
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args([
            "vcam_ring_cross_process_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, &prefix)
        .output()
        .expect("spawn the consumer process");
    stop.store(true, Ordering::Relaxed);
    let _producer = producer.join().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!("vcam child stdout:\n{stdout}\nvcam child stderr:\n{stderr}");
    assert!(
        out.status.success(),
        "consumer process failed: {:?}",
        out.status
    );
    assert!(
        stderr.contains("vcam child: saw"),
        "the child ran the consumer"
    );
}

/// The producer-claim half of [`vcam_ring_claim_cross_process`]: tries to
/// claim the parent's ring and reports; in `die` mode it exits while still
/// holding the claim (no release), leaving a dead pid in the header.
#[test]
fn vcam_ring_claim_child() {
    let Ok(prefix) = std::env::var(CLAIM_ENV) else {
        return;
    };
    let die = std::env::var(CLAIM_MODE_ENV).as_deref() == Ok("die");
    let names = RingNames::with_prefix(&prefix);
    match Producer::open(&config(&names), 30.0) {
        Ok(p) => {
            eprintln!("vcam claim child: ok {}", std::process::id());
            if die {
                std::mem::forget(p);
                std::process::exit(0);
            }
        }
        Err(ProducerError::Busy { pid }) => eprintln!("vcam claim child: busy {pid}"),
        Err(ProducerError::Os(e)) => panic!("vcam claim child: open failed: {e}"),
    }
}

/// Single producer across processes: while this process holds the ring a
/// second process is refused (and told our pid); after we release, another
/// process may claim; a claim left by a process that died is taken over.
#[test]
fn vcam_ring_claim_cross_process() {
    let names = names("claim");
    let prefix = names.mapping.trim_end_matches("-ring").to_string();
    let me = std::process::id();
    let mut producer = Producer::open(&config(&names), 30.0).expect("claim the ring");
    producer
        .write_nv12(W, H, &vec![1u8; nv12_size(W, H)])
        .unwrap();

    let err = run_child("vcam_ring_claim_child", &[(CLAIM_ENV, &prefix)]);
    assert!(
        err.contains(&format!("vcam claim child: busy {me}")),
        "second producer refused"
    );

    // Keep the mapping alive without a claim, release ours, and let a child
    // claim and die holding it.
    let keep = SharedRing::open(&config(&names), Role::Consumer, false).unwrap();
    drop(producer);
    assert_eq!(keep.ring().producer_pid(), 0, "drop released the claim");
    let err = run_child(
        "vcam_ring_claim_child",
        &[(CLAIM_ENV, &prefix), (CLAIM_MODE_ENV, "die")],
    );
    assert!(
        err.contains("vcam claim child: ok"),
        "a free ring can be claimed"
    );
    let dead = keep.ring().producer_pid();
    assert!(
        dead != 0 && dead != me,
        "the dead child still holds the claim"
    );
    assert!(!oak_mf_vcam::win::identity::process_alive(dead));
    let again =
        Producer::open(&config(&names), 30.0).expect("a dead producer's claim is taken over");
    assert_eq!(keep.ring().producer_pid(), me);
    drop(again);
}

/// The ring's DACL admits the owner but not a token without the owner's SID
/// (and without Administrators): this process's own token, restricted with
/// those two SIDs deny-only, stands in for another user. As a control, the
/// same restricted token can open an object with the old Authenticated
/// Users DACL, so the denial comes from the DACL and not from the token.
#[test]
fn vcam_ring_dacl_denies_other_token() {
    use windows::Win32::Foundation::{
        CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateToken, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES, SecurityImpersonation, TOKEN_ALL_ACCESS,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, OpenFileMappingW, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};
    use windows_core::{HSTRING, PCWSTR};

    let names = names("dacl");
    let cfg = config(&names);
    let owner = SharedRing::open(&cfg, Role::Producer, true).expect("owner creates the ring");
    assert!(owner.created());
    let control_name = HSTRING::from(format!(
        r"Local\sc-vcam-test-dacl-au-{}",
        std::process::id()
    ));
    let ring_name = HSTRING::from(names.mapping.as_str());

    // SAFETY: token and descriptor plumbing on locals; every handle and
    // LocalAlloc'd pointer is released at the end; impersonation is undone
    // with SetThreadToken(None) on this thread.
    unsafe {
        // Control object with the old, too-wide DACL.
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from("D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;AU)"),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .unwrap();
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let control = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            Some(&sa),
            PAGE_READWRITE,
            0,
            4096,
            &control_name,
        )
        .unwrap();

        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut token).unwrap();
        let mut user = PSID::default();
        ConvertStringSidToSidW(
            PCWSTR(HSTRING::from(cfg.owner_sid.as_str()).as_ptr()),
            &mut user,
        )
        .unwrap();
        let mut admins = PSID::default();
        ConvertStringSidToSidW(PCWSTR(HSTRING::from("S-1-5-32-544").as_ptr()), &mut admins)
            .unwrap();
        let disable = [
            SID_AND_ATTRIBUTES {
                Sid: user,
                Attributes: 0,
            },
            SID_AND_ATTRIBUTES {
                Sid: admins,
                Attributes: 0,
            },
        ];
        let mut restricted = HANDLE::default();
        CreateRestrictedToken(
            token,
            DISABLE_MAX_PRIVILEGE,
            Some(&disable),
            None,
            None,
            &mut restricted,
        )
        .unwrap();
        let mut imp = HANDLE::default();
        DuplicateToken(restricted, SecurityImpersonation, &mut imp).unwrap();

        SetThreadToken(None, Some(imp)).unwrap();
        let ring_as_other = OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(ring_name.as_ptr()));
        let control_as_other =
            OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(control_name.as_ptr()));
        SetThreadToken(None, None).unwrap();
        let ring_as_owner = OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(ring_name.as_ptr()));

        eprintln!(
            "vcam dacl: other->ring {:?}; other->AU control {:?}; owner->ring {:?}",
            ring_as_other.as_ref().map(|_| ()).map_err(|e| e.code()),
            control_as_other.as_ref().map(|_| ()).map_err(|e| e.code()),
            ring_as_owner.as_ref().map(|_| ()).map_err(|e| e.code()),
        );
        for h in [&ring_as_other, &control_as_other, &ring_as_owner]
            .into_iter()
            .flatten()
        {
            let _ = CloseHandle(*h);
        }
        for h in [control, token, restricted, imp] {
            let _ = CloseHandle(h);
        }
        let _ = LocalFree(Some(HLOCAL(user.0)));
        let _ = LocalFree(Some(HLOCAL(admins.0)));
        let _ = LocalFree(Some(HLOCAL(sd.0)));

        assert!(ring_as_owner.is_ok(), "the owner opens its ring");
        assert!(
            control_as_other.is_ok(),
            "control: the restricted token can open an Authenticated Users object"
        );
        assert_eq!(
            ring_as_other.map(drop).unwrap_err().code().0 as u32,
            0x8007_0005,
            "a token without the owner SID is denied the ring"
        );
    }
    drop(owner);
}
