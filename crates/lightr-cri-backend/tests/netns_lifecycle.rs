//! WP-#83 — Linux-only, root+CNI-gated integration test that proves the CRI
//! backend's FULL network-namespace + CNI lifecycle through the public
//! `CriBackend` API only. This is NOT "CNI ADD assigned an IP" (already covered
//! by the lib unit test) — it proves the *lifecycle*: netns created+pinned, CNI
//! wired REAL connectivity (not just an allocation), the container actually
//! JOINED the netns, and teardown leaks NOTHING (the containerd#6143 class).
//!
//! ── Gating (this NEVER runs on the macOS gate or unprivileged CI) ────────────
//! The whole file is `#![cfg(target_os = "linux")]` — on macOS/windows it is an
//! empty (but valid) test crate, so the gate stays green and nothing here can
//! break a non-linux build. At runtime the test self-skips (prints SKIP +
//! returns) unless BOTH hold: `LIGHTR_NETNS_IT=1` is set AND the process is root
//! (real netns create + CNI plugins need privilege). `libc` is a *regular*
//! dependency of this crate, not a dev-dependency, so it is NOT reachable from an
//! integration-test crate — root is detected via `id -u` instead (no new dep).
//!
//! ── Why only the public API ──────────────────────────────────────────────────
//! `net::{setup,teardown,join_netns}` are `pub(crate)`; a `tests/` crate cannot
//! call them. The lifecycle is driven exclusively through the public trait:
//! `run_sandbox` → `sandbox_status` → `create_container`/`start_container` →
//! `stop_sandbox`/`remove_sandbox`. The public-API shape (temp_home,
//! SandboxConfig, container cfg) is copied from `tests/wired.rs`.
//!
//! ── Parallel-safety ──────────────────────────────────────────────────────────
//! The CI runs ONLY this test binary (`--test netns_lifecycle`), and this file
//! holds a single test, so there is no concurrent netns/veth churn to confuse
//! the host-veth-baseline leak check. The sandbox netns is pinned at a unique
//! id-derived path (`/run/netns/lightr-sb-<nanos>-<ctr>`), so even cross-process
//! runs cannot collide. `temp_home()` gives each backend its own state root.
//!
//! ── A NOTE on the container-join leg (assertion 4) ───────────────────────────
//! The CRI container plane spawns a REAL host process (no chroot/rootfs) with a
//! `setns(CLONE_NEWNET)` pre_exec, so the join probe uses host `/bin/sh`+`stat`
//! — no alpine rootfs needed. We therefore achieve the STRONG form: the
//! container writes its own net-ns inode and we assert it EQUALS the sandbox
//! netns inode (proves the pre_exec setns actually landed), on top of the weak
//! form (start + clean exit ⇒ the pre_exec did not fail-closed).

#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lightr_cri_backend::{
    ContainerConfig, ContainerState, CriBackend, LightrBackend, SandboxConfig,
};

/// Conflist subnet the CI installs (10-lightr-bridge.conflist): 10.88.0.0/16,
/// gateway 10.88.0.1 (bridge isGateway:true).
const GW: &str = "10.88.0.1";

// ── helpers (copied shape from tests/wired.rs) ───────────────────────────────

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-netns-it-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn container_cfg(name: &str, command: Vec<&str>) -> ContainerConfig {
    ContainerConfig {
        name: name.into(),
        attempt: 0,
        image_ref: "test-image".into(),
        command: command.into_iter().map(String::from).collect(),
        args: Vec::new(),
        working_dir: String::new(),
        envs: Vec::new(),
        mounts: Vec::new(),
        labels: BTreeMap::new(),
        annotations: BTreeMap::new(),
        log_path: String::new(),
        tty: false,
        stdin: false,
        security: None,
    }
}

fn sandbox_cfg() -> SandboxConfig {
    SandboxConfig {
        name: "netns-pod".into(),
        uid: "uid".into(),
        namespace: "ns".into(),
        attempt: 0,
        labels: BTreeMap::new(),
        annotations: BTreeMap::new(),
        log_directory: String::new(),
        hostname: String::new(),
        // The whole point: a pod-network sandbox → netns + CNI ADD on linux.
        host_network: false,
        dns: None,
        port_mappings: Vec::new(),
    }
}

/// Run a command to completion → (exit_code, stdout, stderr). Panics on spawn
/// failure (the tools used — ip/nsenter/mountpoint/stat — are core util-linux/
/// iproute2 binaries; a missing one means the test env is unfit and we want it
/// LOUD). `nsenter <inner>` never fails to spawn even when `<inner>` is missing
/// (it exits 127), so ping-availability is handled via the exit code, not here.
fn run(cmd: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn `{cmd} {args:?}`: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn is_root() -> bool {
    run("id", &["-u"]).1.trim() == "0"
}

/// Runtime gate. Returns true (and prints why) when the test must self-skip.
fn skip() -> bool {
    if std::env::var("LIGHTR_NETNS_IT").is_err() {
        println!("SKIP: set LIGHTR_NETNS_IT=1 (needs root+CNI)");
        return true;
    }
    if !is_root() {
        println!("SKIP: must run as root (needs root+CNI)");
        return true;
    }
    false
}

/// The set of host `veth*` interface names (the CNI bridge plugin creates a veth
/// pair per sandbox; the host-side peer must be GONE after teardown). Diffed
/// before/after (not an absolute count) so unrelated host veths (docker, etc.)
/// do not perturb the leak check.
fn host_veths() -> BTreeSet<String> {
    let (_code, out, _err) = run("ip", &["-o", "link", "show"]);
    out.lines()
        .filter_map(|l| {
            // "3: vethXXXX@if2: <BROADCAST,...>" → take field after the index.
            let after_idx = l.split(':').nth(1)?;
            let name = after_idx.trim().split('@').next()?.trim();
            name.starts_with("veth").then(|| name.to_string())
        })
        .collect()
}

// ── the lifecycle proof ──────────────────────────────────────────────────────

/// One sandbox, host_network:false, asserted through assertions 1–5 IN ORDER.
/// Every failure carries the actual observed value so a CI red is diagnosable
/// without a rerun.
#[test]
fn netns_cni_full_lifecycle_no_leak() {
    if skip() {
        return;
    }
    let b = LightrBackend::new(temp_home());

    // Host veth baseline BEFORE anything (assertion 5 diffs against this).
    let veth_before = host_veths();

    let id = b
        .run_sandbox(sandbox_cfg())
        .expect("run_sandbox(host_network:false) must succeed on a root+CNI host");

    // ── 1. netns created + pinned at the canonical path, and IS a mountpoint ──
    let status = b.sandbox_status(&id).expect("sandbox_status");
    let netns_path = status.netns_path.clone().unwrap_or_else(|| {
        panic!("assertion 1: netns_path is None — the netns/CNI path did not run (is CNI installed + root?). status={status:?}")
    });
    assert!(
        Path::new(&netns_path).exists(),
        "assertion 1: netns pin file {netns_path} does not exist"
    );
    let expected = format!("/run/netns/lightr-{}", &id.0[..id.0.len().min(24)]);
    assert_eq!(
        netns_path, expected,
        "assertion 1: netns is not pinned at the canonical /run/netns path"
    );
    let (mp_code, _o, _e) = run("mountpoint", &["-q", &netns_path]);
    assert_eq!(
        mp_code, 0,
        "assertion 1: {netns_path} is NOT a mountpoint — the bind-mount pin is missing (the kernel ns is not held)"
    );

    // ── 2. CNI assigned a real, parseable IP inside 10.88.0.0/16 ─────────────
    let ip_str = status.ip.clone().unwrap_or_else(|| {
        panic!("assertion 2: status.ip is None — CNI ADD did not assign an IP. status={status:?}")
    });
    let ip: std::net::IpAddr = ip_str
        .parse()
        .unwrap_or_else(|e| panic!("assertion 2: CNI ip {ip_str:?} is not a valid IpAddr: {e}"));
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            assert!(
                o[0] == 10 && o[1] == 88,
                "assertion 2: CNI ip {ip} is not in the conflist subnet 10.88.0.0/16"
            );
        }
        std::net::IpAddr::V6(_) => panic!("assertion 2: expected an IPv4 from the bridge IPAM, got {ip}"),
    }

    // ── 3. the netns has the WIRED interface + real connectivity ─────────────
    let netarg = format!("--net={netns_path}");

    // eth0 present with the assigned IP (the container-side veth named by CNI_IFNAME).
    let (a_code, addr_out, a_err) = run("nsenter", &[&netarg, "ip", "-o", "addr", "show"]);
    assert_eq!(a_code, 0, "assertion 3: `nsenter ip addr` failed: {a_err}");
    assert!(
        addr_out.contains("eth0"),
        "assertion 3: eth0 (CNI veth) not present in the netns:\n{addr_out}"
    );
    assert!(
        addr_out.contains(&ip_str),
        "assertion 3: assigned IP {ip_str} is not configured on any iface in the netns:\n{addr_out}"
    );

    // lo is UP inside the fresh netns (setup brought it up; in-ns 127.0.0.1 dials
    // would otherwise hang). UP flag lives in `ip link` output, not `ip addr`.
    let (l_code, lo_out, l_err) = run("nsenter", &[&netarg, "ip", "-o", "link", "show", "lo"]);
    assert_eq!(l_code, 0, "assertion 3: `nsenter ip link show lo` failed: {l_err}");
    assert!(
        lo_out.contains("UP"),
        "assertion 3: loopback `lo` is not UP inside the netns:\n{lo_out}"
    );

    // CONNECTIVITY — the proof CNI wired a real veth+bridge, not just an IP:
    // ping the bridge gateway from inside the netns. Prefer ping; if ping is
    // unavailable (nsenter exits 127) or fails, fall back to proving a default
    // route via the gateway exists.
    let (ping_code, ping_out, ping_err) =
        run("nsenter", &[&netarg, "ping", "-c1", "-W2", GW]);
    if ping_code != 0 {
        let (r_code, route_out, r_err) = run("nsenter", &[&netarg, "ip", "route"]);
        assert_eq!(r_code, 0, "assertion 3: `nsenter ip route` failed: {r_err}");
        assert!(
            route_out.contains("default") && route_out.contains(GW),
            "assertion 3: CNI did not wire connectivity — ping {GW} exit {ping_code} \
             (out={ping_out:?} err={ping_err:?}) AND no default route via {GW}:\n{route_out}"
        );
    }

    // ── 4. the container JOINS the sandbox netns (proves the setns pre_exec) ──
    // STRONG form: the container writes its own net-ns inode; we assert it equals
    // the sandbox netns inode. The CRI container plane runs a host process (no
    // rootfs), so `/bin/sh`+`stat` are host binaries; `stat -L` on /proc/self/
    // ns/net resolves the magic symlink to the nsfs inode — the SAME value
    // `stat -L` reports for the pinned netns file (this is how netns identity is
    // established). WEAK form (also asserted): start + clean exit ⇒ the pre_exec
    // setns did not fail-closed.
    let probe_file = std::env::temp_dir().join(format!("lightr-netns-probe-{}.txt", id.0));
    let _ = std::fs::remove_file(&probe_file);
    let probe_path = probe_file.to_string_lossy().into_owned();
    let probe_cmd = format!("stat -L -c %i /proc/self/ns/net > {probe_path}");
    let cid = b
        .create_container(&id, container_cfg("netns-probe", vec!["/bin/sh", "-c", &probe_cmd]))
        .expect("create_container on a Ready netns'd sandbox");
    b.start_container(&cid)
        .expect("assertion 4 (weak): start_container must succeed — the setns pre_exec must not fail-closed");

    let mut exited = false;
    for _ in 0..300 {
        if b.container_status(&cid).unwrap().state == ContainerState::Exited {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(exited, "assertion 4: netns-probe container never reached Exited");
    let cst = b.container_status(&cid).unwrap();
    assert_eq!(
        cst.exit_code, 0,
        "assertion 4 (weak): netns-probe exited non-zero ({}): reason={} message={}",
        cst.exit_code, cst.reason, cst.message
    );

    // STRONG: inode equality.
    let container_inode = std::fs::read_to_string(&probe_file)
        .unwrap_or_else(|e| panic!("assertion 4: probe output {probe_path} unreadable: {e}"))
        .trim()
        .to_string();
    let (s_code, sandbox_inode, s_err) = run("stat", &["-L", "-c", "%i", &netns_path]);
    assert_eq!(s_code, 0, "assertion 4: stat on netns pin failed: {s_err}");
    let sandbox_inode = sandbox_inode.trim();
    assert!(
        !container_inode.is_empty() && container_inode == sandbox_inode,
        "assertion 4 (strong): container did NOT join the sandbox netns — \
         container net-ns inode {container_inode:?} != sandbox netns inode {sandbox_inode:?}"
    );
    let _ = std::fs::remove_file(&probe_file);

    // ── 5. teardown leaves NO leak (containerd#6143 class — the heart of #83) ─
    b.stop_sandbox(&id).expect("stop_sandbox");
    b.remove_sandbox(&id).expect("remove_sandbox");

    // The netns pin file is gone …
    assert!(
        !Path::new(&netns_path).exists(),
        "assertion 5 LEAK: netns pin {netns_path} still exists after remove_sandbox"
    );
    // … and is no longer a mountpoint …
    let (mp2_code, _o, _e) = run("mountpoint", &["-q", &netns_path]);
    assert_ne!(
        mp2_code, 0,
        "assertion 5 LEAK: {netns_path} is still a mountpoint after teardown (umount2 did not run)"
    );
    // … and `ip netns list` no longer reports it …
    let ns_name = format!("lightr-{}", &id.0[..id.0.len().min(24)]);
    let (_c, netns_list, _e) = run("ip", &["netns", "list"]);
    assert!(
        !netns_list.contains(&ns_name),
        "assertion 5 LEAK: {ns_name} still present in `ip netns list`:\n{netns_list}"
    );
    // … and no host-side veth peer leaked (baseline restored).
    let veth_after = host_veths();
    let leaked: Vec<&String> = veth_after.difference(&veth_before).collect();
    assert!(
        leaked.is_empty(),
        "assertion 5 LEAK: host veth peer(s) leaked after teardown: {leaked:?} \
         (before={veth_before:?} after={veth_after:?})"
    );
}
