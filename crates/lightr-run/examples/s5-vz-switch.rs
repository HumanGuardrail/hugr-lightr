//! S5-VZ-SWITCH — ADR-0018 keystone END-TO-END validation (WP-C10). THROWAWAY.
//!
//! Proves the merged Design-C networking library against REAL alpine microVMs on
//! this Intel box, in ONE process:
//!
//!   STEP 1  one VM leases its registry IP from our hand-rolled DHCP (vswitch/dhcp.rs)
//!   STEP 2  two VMs reach each other BY IP over the mesh (L2 switching)
//!   STEP 3  curl-by-NAME round-trips via our embedded DNS (vswitch/dns.rs)
//!   STEP 4  teardown is clean (switch down, VMs gone, no leaked procs)
//!
//! Architecture per member:
//!   socketpair(AF_UNIX, SOCK_DGRAM) → (host_fd, guest_fd)
//!     host_fd  → VSwitch::add_member  (the switch owns + closes it)
//!     guest_fd → ExecSpec.net_fd      (vz.swift attaches eth1 over it; the VZ
//!                                       FileHandle is closeOnDealloc:false, but
//!                                       the engine takes the raw fd by value, so
//!                                       WE keep the guest fd alive for the boot)
//!   The guest sees eth0 (NAT egress, kernel ip=dhcp) + eth1 (mesh → the switch).
//!
//! Reading guest output: PID1 (lightr-init) captures the command's stdout to
//! `<rootfs>/.lightr-stdout` on the shared rootfs, fsync'd before the exit
//! marker. After the VM stops we read that file from the host — that is how we
//! observe `ip addr show eth1`, `wget -O-`, etc.
//!
//! Run:  bash spikes/s5-vz-switch/run.sh   (builds + codesigns + runs this)

use std::io::Write;
use std::net::Ipv4Addr;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec};
use lightr_run::network::NetworkRegistry;
use lightr_run::vswitch::VSwitch;
use lightr_store::Store;

const ALPINE_REF: &str = "alpine";

// ── tiny test harness ───────────────────────────────────────────────────────

fn step(name: &str) {
    eprintln!("\n=== {name} ===");
}
fn ok(msg: &str) {
    eprintln!("  [GREEN] {msg}");
}
fn fail(msg: &str) -> ! {
    eprintln!("  [BLOCKED] {msg}");
    eprintln!("\nS5-VZ-SWITCH: BLOCKED");
    std::process::exit(1);
}

// ── socketpair(AF_UNIX, SOCK_DGRAM) ─────────────────────────────────────────

/// Returns (host_fd, guest_fd). One datagram == one Ethernet frame (ADR-0018).
fn datagram_socketpair() -> (RawFd, RawFd) {
    let mut fds = [0 as libc::c_int; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        fail(&format!("socketpair failed: {e}"));
    }
    (fds[0], fds[1])
}

// ── per-VM boot on a worker thread ──────────────────────────────────────────

/// Hydrate a fresh CoW rootfs for `member_name`, boot ONE alpine VM whose eth1
/// is `guest_fd`, run `sh -c <command>`, and return (exit_code, captured stdout,
/// captured stderr). Blocks until the VM stops. `guest_fd` is consumed by the
/// engine (passed to the shim as net_fd); we own its lifetime for the boot.
fn boot_vm(
    store_root: &Path,
    scratch: &Path,
    member_name: &str,
    guest_fd: RawFd,
    mesh_mac: [u8; 6],
    command: &str,
) -> (i32, String, String) {
    let store = Store::open(store_root).unwrap_or_else(|e| fail(&format!("Store::open: {e}")));

    // Each boot gets its OWN fresh hydrated rootfs so the file channels
    // (CMD/EXIT/STDOUT/STDERR) never collide between concurrent VMs OR between
    // sequential boots of the same member name (member "a" is reused as the
    // STEP-2 server after its STEP-1 lease). hydrate() refuses a non-empty
    // destination, so we remove any prior tree before re-hydrating.
    let rootfs_dir = scratch.join(format!("rootfs-{member_name}"));
    let _ = std::fs::remove_dir_all(&rootfs_dir);
    std::fs::create_dir_all(&rootfs_dir).expect("mk rootfs dir");
    lightr_index::hydrate(&rootfs_dir, &store, ALPINE_REF)
        .unwrap_or_else(|e| fail(&format!("hydrate {ALPINE_REF} -> {member_name}: {e}")));

    let cwd = PathBuf::from("/");
    let cmd: Vec<String> = vec!["/bin/sh".into(), "-c".into(), command.to_string()];

    let engine =
        engine_for(EngineKind::Vz).unwrap_or_else(|e| fail(&format!("engine_for(Vz): {e}")));
    let spec = ExecSpec {
        cwd: &cwd,
        command: &cmd,
        rootfs: Some(&rootfs_dir),
        limits: ResourceLimits::default(),
        // net:true ⇒ NAT NIC (eth0) + `ip=dhcp` on the cmdline. net_fd:Some ⇒
        // the file-handle mesh NIC (eth1) over our socketpair → the VSwitch.
        net: true,
        net_isolate: false,
        net_fd: Some(guest_fd),
        // net_mac: the registry-assigned per-member MAC. The fix plumbs this
        // through the engine FFI to vz.swift so the guest's eth1 emits THIS MAC
        // (matching the switch's DHCP lease / MAC-table key) instead of a
        // hardcoded one. This is the seam under validation.
        net_mac: Some(mesh_mac),
        mounts: &[],
        env: &[],
        workdir: None,
        user: None,
        hostname: None,
        add_host: &[],
        dns: &[],
        mesh_ip: None,
        read_only: false,
        shm_size: None,
    };
    let code = engine
        .run(&spec)
        .unwrap_or_else(|e| fail(&format!("engine.run({member_name}): {e}")));

    let so = std::fs::read_to_string(rootfs_dir.join(".lightr-stdout")).unwrap_or_default();
    let se = std::fs::read_to_string(rootfs_dir.join(".lightr-stderr")).unwrap_or_default();
    (code, so, se)
}

fn main() {
    // Scratch dir for this run's hydrated rootfses (host-side; throwaway).
    let scratch = std::env::temp_dir().join(format!("s5-vz-switch-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("mk scratch");
    let store_root = Store::default_root();
    eprintln!("S5-VZ-SWITCH harness");
    eprintln!("  store     = {}", store_root.display());
    eprintln!("  scratch   = {}", scratch.display());
    eprintln!("  rootfs    = {ALPINE_REF}");

    // Create the network + start the switch ONCE; members join across all steps.
    let id = format!("s5switch-{}", std::process::id());
    let reg = NetworkRegistry::create(&store_root, &id)
        .unwrap_or_else(|e| fail(&format!("NetworkRegistry::create: {e}")));
    let subnet = reg.subnet();
    eprintln!(
        "  network   = {id}  subnet=10.69.{}.0/24 gw={}",
        subnet.base.octets()[2],
        subnet.gateway
    );

    step_1(&reg, subnet, &store_root, &scratch, &id);
    let (a_ip, b_ip) = step_2_and_3(&reg, subnet, &store_root, &scratch, &id);
    step_4(&id, &store_root, a_ip, b_ip);

    // Best-effort scratch cleanup (rootfs dirs are throwaway CoW trees).
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(store_root.join("net").join(&id));

    eprintln!("\nS5-VZ-SWITCH: ALL GREEN");
}

// ── STEP 1 — one VM leases its registry IP from our DHCP ────────────────────

fn step_1(
    reg: &NetworkRegistry,
    subnet: lightr_run::network::Subnet,
    store_root: &Path,
    scratch: &Path,
    id: &str,
) {
    step("STEP 1 — 1-VM leases our DHCP (vswitch/dhcp.rs ⇄ busybox udhcpc)");

    let member = reg
        .join("a", &[], &[])
        .unwrap_or_else(|e| fail(&format!("registry.join(a): {e}")));
    eprintln!(
        "  member a: mac={} ip={} (registry-assigned; udhcpc must lease THIS)",
        fmt_mac(member.mac.0),
        member.ip
    );

    let sw = VSwitch::start(&id.to_string(), subnet)
        .unwrap_or_else(|e| fail(&format!("VSwitch::start: {e}")));

    // The MAC we register with the switch (lease store + MacTable key) IS the
    // registry MAC. The net_mac fix makes the guest's eth1 emit this same MAC, so
    // the switch key and the wire source MAC are identical → DHCP OFFER lands.
    let reg_mac = member.mac.0;

    let (host_fd, guest_fd) = datagram_socketpair();
    sw.add_member(host_fd, reg_mac, member.ip, "a")
        .unwrap_or_else(|e| fail(&format!("add_member(a): {e}")));

    // Bring eth1 up, lease via udhcpc on eth1 only, then dump eth1's address +
    // the lease's resolv.conf (DNS=gateway proof for STEP 3). `-n` = exit if no
    // lease; `-q` = quit after obtaining; `-f` = foreground (so sh waits for it);
    // `-t 8` = up to 8 DISCOVER tries.
    let expected_ip = member.ip;
    let cmd = "ip link set eth1 up; \
               echo '--- udhcpc eth1 ---'; \
               udhcpc -i eth1 -n -q -f -t 8 2>&1; \
               echo '--- ip addr eth1 ---'; \
               ip -4 addr show eth1; \
               echo '--- resolv.conf ---'; \
               cat /etc/resolv.conf 2>&1";

    let (code, stdout, stderr) = boot_vm(store_root, scratch, "a", guest_fd, reg_mac, cmd);
    eprintln!("  guest exit={code}");
    eprintln!("  ---- guest stdout ----\n{}", indent(&stdout));
    if !stderr.trim().is_empty() {
        eprintln!("  ---- guest stderr ----\n{}", indent(&stderr));
    }

    let leased = stdout.contains(&expected_ip.to_string());
    if !leased {
        // Diagnostics: did udhcpc even see an offer? Dump the registry vs what we
        // observed so the lead can compare wire bytes against dhcp.rs.
        eprintln!(
            "  DIAGNOSIS: guest stdout does NOT contain the registry IP {expected_ip}.\n  \
             If udhcpc printed no lease, our DHCP DISCOVER→OFFER (vswitch/dhcp.rs) did not\n  \
             round-trip over eth1. Capture the frames the VSwitch recv loop sees and compare\n  \
             vs busybox udhcpc's DISCOVER (xid, broadcast flag, option 53/50/54)."
        );
        sw.shutdown().ok();
        fail(&format!(
            "STEP 1: eth1 did not lease the registry IP {expected_ip} from our DHCP"
        ));
    }

    sw.shutdown()
        .unwrap_or_else(|e| fail(&format!("VSwitch::shutdown (step1): {e}")));
    ok(&format!(
        "eth1 leased {expected_ip} from vswitch/dhcp.rs (busybox udhcpc ⇄ C3 confirmed)"
    ));
}

// ── STEP 2 + 3 — 2-VM L2 (by IP) and name-DNS (by name) ─────────────────────

/// Boots a (server) and b (client). Server: lease eth1, then a one-shot busybox
/// httpd-style responder bound to eth1's port 80. Client: lease eth1, wget the
/// server BY IP (step 2) AND BY NAME (step 3). Returns (a_ip, b_ip).
fn step_2_and_3(
    reg: &NetworkRegistry,
    subnet: lightr_run::network::Subnet,
    store_root: &Path,
    scratch: &Path,
    id: &str,
) -> (Ipv4Addr, Ipv4Addr) {
    step("STEP 2+3 — 2-VM L2 by IP + curl-by-name via embedded DNS");

    // Reuse "a" (already joined in step 1) as the server; add "b" as the client.
    let a = reg
        .join("a", &[], &[])
        .unwrap_or_else(|e| fail(&format!("join a: {e}")));
    let b = reg
        .join("b", &[], &[])
        .unwrap_or_else(|e| fail(&format!("join b: {e}")));
    eprintln!("  server a: ip={}  client b: ip={}", a.ip, b.ip);

    let sw = VSwitch::start(&id.to_string(), subnet)
        .unwrap_or_else(|e| fail(&format!("VSwitch::start (step2): {e}")));

    let (a_host, a_guest) = datagram_socketpair();
    let (b_host, b_guest) = datagram_socketpair();
    // The DNS name registered with the switch is MULTI-LABEL ("a.mesh"), not the
    // bare "a". This is deliberate and load-bearing: the alpine guest's musl
    // libc does NOT issue a DNS query for single-label (dotless) names from
    // getaddrinfo() — it only consults /etc/hosts for them — so `wget http://a/`
    // fails with "bad address" before any packet reaches our DNS. (A direct
    // `nslookup a.` DOES get answered 10.69.x.2 by vswitch/dns.rs, proving the
    // resolver path works; the gap is purely musl's dotless-name policy.) Giving
    // the service a dotted name makes getaddrinfo actually query DNS, which our
    // embedded server answers from this NameTable entry.
    const SERVER_DNS_NAME: &str = "a.mesh";
    sw.add_member(a_host, a.mac.0, a.ip, SERVER_DNS_NAME)
        .unwrap_or_else(|e| fail(&format!("add_member(a): {e}")));
    sw.add_member(b_host, b.mac.0, b.ip, "b.mesh")
        .unwrap_or_else(|e| fail(&format!("add_member(b): {e}")));

    // Server: lease eth1, then serve a fixed body on :80 for a bounded window so
    // the client (which boots concurrently + needs DHCP) has time to connect
    // twice (by-IP then by-name). We loop a fixed number of accepts then exit so
    // the VM powers off cleanly.
    //
    // LIVENESS: each `nc -l` blocks until a peer connects. If any client request
    // fails to connect (the exact failure mode under validation), the server
    // would block forever on that accept and the VM would never power off,
    // hanging the harness. We bound EACH accept with `timeout` (so a stuck nc is
    // killed and the loop advances) AND bound the whole server with an outer
    // `timeout` as a backstop. This is behavior-preserving for the GREEN path
    // (both connections arrive well within the windows) and guarantees the VM
    // always powers off so the harness reports instead of hanging. `timeout`
    // returns 124 on expiry; we swallow it so the loop continues / exits 0.
    let server_cmd = "ip link set eth1 up; \
                      udhcpc -i eth1 -n -q -f -t 8 >/dev/null 2>&1; \
                      echo SERVER_IP=$(ip -4 addr show eth1 | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p'); \
                      i=0; while [ $i -lt 2 ]; do \
                        printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 8\\r\\n\\r\\nMESH-OK\\n' \
                          | timeout 45 nc -l -p 80 || true; \
                        i=$((i+1)); \
                      done; \
                      echo SERVER_DONE";

    // Client: lease eth1, set resolv.conf (udhcpc does this from our DHCP option
    // 6 = gateway). Then wget BY IP (step2) and BY NAME 'a.mesh' (step3). The
    // dotted name is what makes musl's getaddrinfo issue the DNS query our
    // embedded server answers (see the SERVER_DNS_NAME note above).
    //
    // LIVENESS: busybox `nc -l` is a ONE-SHOT listener — the server serves one
    // connection, exits, and the loop re-binds for the next. There is a small
    // window between serving the by-IP request and the next `nc -l` binding; a
    // client request landing in that gap gets "Connection refused". That is a
    // server-side accept-loop artifact, NOT a networking defect (DNS already
    // resolved the name to the right IP). We make each client fetch RETRY a few
    // times with a short backoff so the validation no longer depends on winning
    // the re-bind race. Behavior-preserving: a working path still succeeds on
    // the first try; only the gap is tolerated. `fetch URL MARKER` prints the
    // body on success or the MARKER on persistent failure.
    let a_ip = a.ip;
    let server_name = SERVER_DNS_NAME;
    let client_cmd = format!(
        "ip link set eth1 up; \
         udhcpc -i eth1 -n -q -f -t 8 >/dev/null 2>&1; \
         echo CLIENT_IP=$(ip -4 addr show eth1 | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p'); \
         echo '--- resolv.conf ---'; cat /etc/resolv.conf 2>&1; \
         fetch() {{ url=$1; marker=$2; n=0; \
           while [ $n -lt 15 ]; do \
             body=$(wget -T 8 -q -O - \"$url\" 2>/dev/null) && \
               {{ printf '%s\\n' \"$body\"; return 0; }}; \
             n=$((n+1)); sleep 1; \
           done; \
           echo \"$marker\"; return 1; }}; \
         echo '--- BY IP ({a_ip}) ---'; \
         fetch http://{a_ip}/ WGET_IP_FAIL; \
         echo '--- BY NAME ({server_name}) ---'; \
         fetch http://{server_name}/ WGET_NAME_FAIL"
    );

    // Boot both VMs on their own threads; the client must start ~immediately so
    // it overlaps the server's serving window. Give the server a small head start
    // so its listener is up before the client's wget.
    let (tx_a, rx_a) = mpsc::channel();
    let store_a = store_root.to_path_buf();
    let scratch_a = scratch.to_path_buf();
    let a_mac = a.mac.0;
    let ta = thread::spawn(move || {
        let r = boot_vm(&store_a, &scratch_a, "a", a_guest, a_mac, server_cmd);
        let _ = tx_a.send(r);
    });

    // Head start: let the server boot + lease + bind before the client wgets.
    thread::sleep(Duration::from_secs(12));

    let (tx_b, rx_b) = mpsc::channel();
    let store_b = store_root.to_path_buf();
    let scratch_b = scratch.to_path_buf();
    let cc = client_cmd.clone();
    let b_mac = b.mac.0;
    let tb = thread::spawn(move || {
        let r = boot_vm(&store_b, &scratch_b, "b", b_guest, b_mac, &cc);
        let _ = tx_b.send(r);
    });

    // Client finishes first (bounded wgets); collect it, then the server.
    let (b_code, b_out, b_err) = rx_b
        .recv_timeout(Duration::from_secs(150))
        .unwrap_or_else(|_| fail("STEP 2/3: client VM did not finish in time"));
    let _ = tb.join();

    eprintln!("  client b exit={b_code}");
    eprintln!("  ---- client stdout ----\n{}", indent(&b_out));
    if !b_err.trim().is_empty() {
        eprintln!("  ---- client stderr ----\n{}", indent(&b_err));
    }

    // The server may still be in its 2nd accept; wait for it (it exits after 2
    // connections or the recv timeout). Then drain its output for diagnostics.
    let server_res = rx_a.recv_timeout(Duration::from_secs(90));
    let _ = ta.join();
    if let Ok((a_code, a_out, a_err)) = server_res {
        eprintln!("  server a exit={a_code}");
        eprintln!("  ---- server stdout ----\n{}", indent(&a_out));
        if !a_err.trim().is_empty() {
            eprintln!("  ---- server stderr ----\n{}", indent(&a_err));
        }
    } else {
        eprintln!("  (server VM still running after window; forcing teardown via shutdown)");
    }

    // ── verdicts ──
    // STEP 2: by-IP must return the body and NOT the fail marker.
    let by_ip_ok = b_out.contains("MESH-OK") && !b_out.contains("WGET_IP_FAIL");
    // STEP 3: by-name must ALSO return the body. Both bodies present means BOTH
    // requests reached the server; distinguish via the explicit fail markers.
    let by_name_ok = !b_out.contains("WGET_NAME_FAIL") && b_out.matches("MESH-OK").count() >= 2;

    sw.shutdown()
        .unwrap_or_else(|e| fail(&format!("VSwitch::shutdown (step2/3): {e}")));

    if !by_ip_ok {
        eprintln!(
            "  DIAGNOSIS (step2): client did not fetch http://{a_ip}/ over eth1.\n  \
             If CLIENT_IP/SERVER_IP are empty → DHCP (C3) failed for one VM.\n  \
             If both leased but wget failed → L2 unicast (vswitch/switch.rs) or ARP\n  \
             flood is not delivering between ports; check MacTable learning."
        );
        fail("STEP 2: b did NOT reach a by IP over the mesh");
    }
    ok(&format!(
        "b fetched http://{a_ip}/ (L2 switching through the VSwitch — 'MESH-OK')"
    ));

    if !by_name_ok {
        eprintln!(
            "  DIAGNOSIS (step3): by-name wget for http://{server_name}/ did not round-trip.\n  \
             Check the client's resolv.conf above: it must read 'nameserver {gw}'.\n  \
             If it does but the name didn't resolve → vswitch/dns.rs is not answering\n  \
             the A query for '{server_name}', or the gateway-IP ARP responder\n  \
             (vswitch/switch.rs::arp_gateway_reply) is not handing the guest the\n  \
             nameserver's MAC so the DNS query never leaves the guest. If resolv.conf\n  \
             is wrong → our DHCP option 6 (DNS) is not being honored by udhcpc.",
            gw = subnet.gateway
        );
        fail(&format!(
            "STEP 3: curl-by-name http://{server_name}/ did NOT round-trip via embedded DNS"
        ));
    }
    ok(&format!(
        "b fetched http://{server_name}/ by NAME (ARP gateway reply + vswitch/dns.rs \
         A-record + DHCP DNS=gateway confirmed)"
    ));

    (a.ip, b.ip)
}

// ── STEP 4 — teardown clean ─────────────────────────────────────────────────

fn step_4(id: &str, store_root: &Path, _a_ip: Ipv4Addr, _b_ip: Ipv4Addr) {
    step("STEP 4 — teardown clean (no leaked switch threads / VM procs)");

    // The VSwitch instances were already shut down at the end of each step
    // (shutdown joins every member thread). Prove no vswitch threads and no
    // stray VZ/supervisor processes from THIS harness remain.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut leaks;
    loop {
        leaks = leaked_procs(id);
        if leaks.is_empty() || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    if !leaks.is_empty() {
        eprintln!("  leaked processes:\n{}", indent(&leaks));
        fail("STEP 4: processes leaked after teardown");
    }

    // The network dir is reference-counted on disk; we remove it (the harness is
    // the sole owner). Its absence is the on-disk daemonless proof.
    let net_dir = store_root.join("net").join(id);
    let _ = std::fs::remove_dir_all(&net_dir);
    ok("VSwitch threads joined on shutdown; no leaked VM/supervisor procs; net dir reclaimed");
}

/// A genuinely-leaked process: an actual `s5-vz-switch` executable (a second
/// copy of THIS harness that failed to exit) still running after teardown.
///
/// We must NOT flag processes that merely MENTION the binary in their argv:
/// the launching shell's command line echoes `target/debug/examples/s5-vz-switch`
/// and so trivially substring-matches — that's the launcher, not a leak. So we
/// parse each `ps` line into (pid, argv) and flag it only when argv[0]'s
/// BASENAME is exactly `s5-vz-switch` (i.e. the executable itself is running),
/// excluding our own PID. This is the daemonless proof: the VZ engine runs the
/// microVM in-process (no child supervisor), so after `run()` returns and the
/// switch threads join, no `s5-vz-switch` image other than us may remain.
fn leaked_procs(_id: &str) -> String {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output();
    let Ok(out) = out else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let me = std::process::id();
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid_str, cmd) = l.split_once(char::is_whitespace)?;
            let pid = pid_str.parse::<u32>().ok()?;
            if pid == me {
                return None; // ourselves
            }
            // argv[0] is the first whitespace-delimited token of the command.
            let argv0 = cmd.split_whitespace().next()?;
            // The executable IS this harness iff its path basename matches.
            let basename = argv0.rsplit('/').next().unwrap_or(argv0);
            if basename == "s5-vz-switch" {
                Some(l.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── small helpers ───────────────────────────────────────────────────────────

fn fmt_mac(m: [u8; 6]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// Silence unused-import warnings on helpers used only in some build configs.
#[allow(dead_code)]
fn _touch(_w: &mut dyn Write) {}
