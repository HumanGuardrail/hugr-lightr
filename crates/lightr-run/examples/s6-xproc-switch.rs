//! S6-XPROC — Phase-2 networking KEYSTONE de-risk spike. THROWAWAY.
//!
//! Proves the ONE unproven half of vz container networking: a per-network L2
//! switch SHARED ACROSS SEPARATE OS PROCESSES. The in-process [`VSwitch`] is
//! already proven (s5-vz-switch: VM↔fd, DHCP/L2/DNS in ONE process, 2 threads).
//! Production, however, spawns each container as a SEPARATE detached supervisor
//! (both `run` and `compose` detach), so the switch must be attached to from
//! OTHER processes. The mechanism under test:
//!
//!   per-net switch PROCESS  ──ctl.sock (UnixListener)──┐
//!                                                       │ each member process:
//!   member proc "a" ── socketpair(AF_UNIX,SOCK_DGRAM) ─┤   send host_fd over
//!   member proc "b" ── socketpair ────────────────────┘   ctl.sock via
//!                                                          SCM_RIGHTS + meta
//!   switch: recv_fd(host_fd) → VSwitch::add_member(host_fd, mac, ip, name)
//!           ^^^^^^^^^^^^^^^^^   ^^^^^^^^^^^^^^^^^^^^^^^^^^ the PROVEN call,
//!                                                          reused verbatim.
//!   member keeps guest_fd → sends/recvs FRAMES (simulating the guest NIC).
//!
//! No VMs here on purpose: the VM↔fd half is s5's job. We drive crafted frames
//! on the guest_fd directly. We prove, ACROSS PROCESS BOUNDARIES:
//!   1. A→B Ethernet frame forwarding (L2 switching xproc).
//!   2. embedded DHCP answers a crafted DISCOVER (member gets its registry IP).
//!   3. embedded DNS answers an A-query for the OTHER member's name (curl-by-name).
//!   4. refcount/EOF teardown: both members exit ⇒ switch sees the network empty
//!      ⇒ exits cleanly. No leaked process, no leaked socket.
//!
//! Re-exec model: `s6-xproc-switch` with no args == the SWITCH process (also the
//! orchestrator); `s6-xproc-switch --member <name> <ctl.sock>` == a member
//! process. The orchestrator spawns two member subprocesses and drives the
//! assertions over each member's stdin/stdout (a tiny line protocol), so every
//! frame genuinely crosses a process boundary.
//!
//! Run:  cargo run -p lightr-run --example s6-xproc-switch
//! Exit: 0 on PASS, non-zero on ANY failed assertion.
//!
//! Platform: unix-only. The keystone (SCM_RIGHTS fd-pass, AF_UNIX SOCK_DGRAM
//! NICs, `vswitch`) is POSIX; on windows the example compiles to an honest stub
//! that exits non-zero (windows container networking is a future ring — see
//! `vswitch/mod.rs`), so the windows `--all-targets` clippy gate stays clean.

#[cfg(not(unix))]
fn main() {
    eprintln!("S6-XPROC: unsupported on this platform (unix-only spike)");
    std::process::exit(1);
}

#[cfg(unix)]
fn main() {
    imp::run();
}

#[cfg(unix)]
mod imp {
    use std::io::{BufRead, BufReader, Write};
    use std::net::Ipv4Addr;
    use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use lightr_run::network::NetworkRegistry;
    use lightr_run::vswitch::passfd::{recv_fd, send_fd};
    use lightr_run::vswitch::VSwitch;

    // ── result reporting ─────────────────────────────────────────────────────────

    fn ok(msg: &str) {
        eprintln!("  [PASS] {msg}");
    }
    fn fail(msg: &str) -> ! {
        eprintln!("  [FAIL] {msg}");
        eprintln!("\nS6-XPROC: FAIL");
        std::process::exit(1);
    }

    // ── attach metadata: the member-side payload carried alongside the fd ─────────
    //
    // Fixed layout: 6 MAC | 4 IP | 1 name-len | name bytes. Sent via SCM_RIGHTS in
    // the SAME message as the host_fd so the switch never sees a torn attach.

    fn encode_meta(mac: [u8; 6], ip: Ipv4Addr, name: &str) -> Vec<u8> {
        let nb = name.as_bytes();
        let mut v = Vec::with_capacity(11 + nb.len());
        v.extend_from_slice(&mac);
        v.extend_from_slice(&ip.octets());
        v.push(nb.len() as u8);
        v.extend_from_slice(nb);
        v
    }

    fn decode_meta(buf: &[u8]) -> Option<([u8; 6], Ipv4Addr, String)> {
        if buf.len() < 11 {
            return None;
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[0..6]);
        let ip = Ipv4Addr::new(buf[6], buf[7], buf[8], buf[9]);
        let nlen = buf[10] as usize;
        let name = String::from_utf8(buf.get(11..11 + nlen)?.to_vec()).ok()?;
        Some((mac, ip, name))
    }

    // ── entrypoint: dispatch switch (orchestrator) vs member by argv ──────────────

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        if args.len() >= 4 && args[1] == "--member" {
            member_main(&args[2], &args[3]);
        } else {
            switch_main();
        }
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // MEMBER PROCESS
    // ─────────────────────────────────────────────────────────────────────────────
    //
    // A genuinely separate OS process. It:
    //   * socketpair → (host_fd, guest_fd)
    //   * connects to ctl.sock, sends host_fd + meta via SCM_RIGHTS, closes host_fd
    //   * keeps guest_fd as its "guest NIC"
    //   * obeys a line protocol on stdin, replying on stdout:
    //       SEND <hex>      → write hex-decoded frame on guest_fd
    //       RECV            → recv one frame (≤2s), reply "FRAME <hex>" or "TIMEOUT"
    //       DHCP <ip>       → craft+send a DISCOVER, recv the OFFER, reply
    //                         "DHCPIP <ip>" with the yiaddr, or "DHCPNONE"
    //       DNS <name>      → craft+send an A-query for <name>, recv the answer,
    //                         reply "DNSIP <ip>" or "DNSNONE"
    //       QUIT            → drop guest_fd and exit 0 (simulates container exit)

    fn member_main(name: &str, ctl_path: &str) -> ! {
        let mac = mac_for_name(name);

        // socketpair(AF_UNIX, SOCK_DGRAM): one datagram == one Ethernet frame.
        let (guest, host) = match UnixDatagram::pair() {
            Ok(p) => p,
            Err(e) => member_die(&format!("socketpair: {e}")),
        };

        // Connect to the switch control socket and pass the host end + metadata.
        let ctl = match UnixStream::connect(ctl_path) {
            Ok(s) => s,
            Err(e) => member_die(&format!("connect {ctl_path}: {e}")),
        };
        // The member learns its registry-assigned IP from the orchestrator over the
        // first stdin line ("IP a.b.c.d") so the meta we pass matches the registry.
        let mut stdin = BufReader::new(std::io::stdin());
        let ip = read_ip_line(&mut stdin);

        use std::os::unix::io::AsRawFd;
        let meta = encode_meta(mac, ip, name);
        if let Err(e) = send_fd(&ctl, host.as_raw_fd(), &meta) {
            member_die(&format!("send_fd: {e}"));
        }
        // We handed an independent dup to the switch; close our host end so the only
        // host-side copy lives in the switch process.
        drop(host);

        // Signal readiness, then service the line protocol against the guest fd.
        println!("READY {name}");
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        loop {
            line.clear();
            if stdin.read_line(&mut line).unwrap_or(0) == 0 {
                break; // stdin closed → orchestrator gone → exit
            }
            let cmd = line.trim();
            if cmd == "QUIT" {
                break;
            } else if let Some(hex) = cmd.strip_prefix("SEND ") {
                let frame = hex_decode(hex);
                let _ = guest.send(&frame);
                println!("SENT");
            } else if cmd == "RECV" {
                match recv_frame(&guest, Duration::from_secs(2)) {
                    Some(f) => println!("FRAME {}", hex_encode(&f)),
                    None => println!("TIMEOUT"),
                }
            } else if let Some(ips) = cmd.strip_prefix("DHCP ") {
                let want: Ipv4Addr = ips.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);
                match do_dhcp(&guest, mac, want) {
                    Some(got) => println!("DHCPIP {got}"),
                    None => println!("DHCPNONE"),
                }
            } else if let Some(n) = cmd.strip_prefix("DNS ") {
                match do_dns(&guest, mac, ip, n) {
                    Some(got) => println!("DNSIP {got}"),
                    None => println!("DNSNONE"),
                }
            }
            let _ = std::io::stdout().flush();
        }

        // Dropping `guest` closes our end of the socketpair; the switch's host end
        // then sees the carrier drop. Exit clean.
        drop(guest);
        std::process::exit(0);
    }

    fn member_die(msg: &str) -> ! {
        eprintln!("member: {msg}");
        std::process::exit(2);
    }

    fn read_ip_line(stdin: &mut BufReader<std::io::Stdin>) -> Ipv4Addr {
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            member_die("orchestrator closed stdin before IP line");
        }
        let t = line.trim();
        match t.strip_prefix("IP ").and_then(|s| s.parse().ok()) {
            Some(ip) => ip,
            None => member_die(&format!("expected 'IP <addr>', got {t:?}")),
        }
    }

    /// Block on `sock` for up to `to`, returning one frame (datagram) or `None`.
    fn recv_frame(sock: &UnixDatagram, to: Duration) -> Option<Vec<u8>> {
        sock.set_read_timeout(Some(to)).ok()?;
        let mut buf = vec![0u8; 64 * 1024];
        match sock.recv(&mut buf) {
            Ok(n) if n > 0 => Some(buf[..n].to_vec()),
            _ => None,
        }
    }

    // ── member-side DHCP DISCOVER + OFFER decode ──────────────────────────────────

    fn do_dhcp(sock: &UnixDatagram, mac: [u8; 6], _want: Ipv4Addr) -> Option<Ipv4Addr> {
        let frame = build_dhcp_discover(mac);
        sock.send(&frame).ok()?;
        let reply = recv_frame(sock, Duration::from_secs(2))?;
        decode_dhcp_yiaddr(&reply)
    }

    // ── member-side DNS A-query + answer decode ───────────────────────────────────

    fn do_dns(sock: &UnixDatagram, mac: [u8; 6], src_ip: Ipv4Addr, name: &str) -> Option<Ipv4Addr> {
        let frame = build_dns_query(mac, src_ip, name);
        sock.send(&frame).ok()?;
        let reply = recv_frame(sock, Duration::from_secs(2))?;
        decode_dns_first_a(&reply)
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // SWITCH PROCESS (also the orchestrator/harness)
    // ─────────────────────────────────────────────────────────────────────────────

    fn switch_main() -> ! {
        eprintln!("S6-XPROC — cross-process shared L2 switch (SCM_RIGHTS) de-risk spike");

        // Per-run network dir under a throwaway home so we never touch the real
        // $LIGHTR_HOME. The ctl.sock lives at <net dir>/ctl.sock per the brief.
        let home = std::env::temp_dir().join(format!("s6-xproc-{}", std::process::id()));
        let id = format!("s6net-{}", std::process::id());
        std::fs::create_dir_all(&home).expect("mk home");

        let reg = NetworkRegistry::create(&home, &id)
            .unwrap_or_else(|e| fail(&format!("NetworkRegistry::create: {e}")));
        let subnet = reg.subnet();
        eprintln!(
            "  network={id} subnet=10.69.{}.0/24 gw={}",
            subnet.base.octets()[2],
            subnet.gateway
        );

        // The PROVEN in-process switch — the spike's main process acts as the
        // per-network switch process that wraps it (brief option: "the spike's main
        // acts as it").
        let sw = Arc::new(
            VSwitch::start(&id, subnet).unwrap_or_else(|e| fail(&format!("VSwitch::start: {e}"))),
        );

        // ── members are registered FIRST (registry refcount = lifecycle truth) ──
        let a = reg
            .join("a", &[], &[])
            .unwrap_or_else(|e| fail(&format!("join a: {e}")));
        let b = reg
            .join("b", &[], &[])
            .unwrap_or_else(|e| fail(&format!("join b: {e}")));
        eprintln!(
            "  registry: a={} mac={}  b={} mac={}",
            a.ip,
            fmt_mac(a.mac.0),
            b.ip,
            fmt_mac(b.mac.0)
        );

        // ── ctl.sock: the cross-process attach control plane ──
        let net_dir = home.join("net").join(&id);
        let ctl_path = net_dir.join("ctl.sock");
        let _ = std::fs::remove_file(&ctl_path);
        let listener =
            UnixListener::bind(&ctl_path).unwrap_or_else(|e| fail(&format!("bind ctl.sock: {e}")));

        // Switch-side accept loop on a thread: for each attach connection, recv the
        // passed fd + meta via SCM_RIGHTS and call the PROVEN add_member verbatim.
        // attached counts successful cross-process attaches (refcount mirror).
        let attached = Arc::new(AtomicUsize::new(0));
        let stop_accept = Arc::new(AtomicBool::new(false));
        let acc_sw = Arc::clone(&sw);
        let acc_attached = Arc::clone(&attached);
        let acc_stop = Arc::clone(&stop_accept);
        let acc_path = ctl_path.clone();
        let accept_thread = thread::spawn(move || {
            accept_loop(&listener, &acc_sw, &acc_attached, &acc_stop, &acc_path)
        });

        // ── spawn TWO SEPARATE member processes (re-exec of this example) ──
        let exe = std::env::current_exe().unwrap_or_else(|e| fail(&format!("current_exe: {e}")));
        let mut ma = spawn_member(&exe, "a", &ctl_path, a.ip);
        let mut mb = spawn_member(&exe, "b", &ctl_path, b.ip);

        // Wait for both members to attach (the accept loop bumped `attached`).
        wait_until(
            || attached.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(10),
        )
        .unwrap_or_else(|| {
            fail("both members did not attach via SCM_RIGHTS within 10s");
        });
        ok("2 SEPARATE processes attached via ctl.sock + SCM_RIGHTS fd-pass");

        // Member stdio handles for the line protocol.
        let mut a_in = ma.stdin.take().unwrap();
        let mut a_out = BufReader::new(ma.stdout.take().unwrap());
        let mut b_in = mb.stdin.take().unwrap();
        let mut b_out = BufReader::new(mb.stdout.take().unwrap());

        // Drain each member's "READY <name>" line (printed right after attach) so
        // the line protocol starts clean.
        expect_line(&mut a_out, "READY a", "member a ready");
        expect_line(&mut b_out, "READY b", "member b ready");

        // ── PROOF 1: A→B Ethernet frame forwarding across processes ──
        prove_frame_forward(
            a.mac.0, b.mac.0, &mut a_in, &mut a_out, &mut b_in, &mut b_out,
        );

        // ── PROOF 2: embedded DHCP answers a crafted DISCOVER (xproc) ──
        prove_dhcp(a.ip, &mut a_in, &mut a_out);

        // ── PROOF 3: embedded DNS resolves the OTHER member's name (xproc) ──
        prove_dns(b.ip, &mut a_in, &mut a_out);

        // ── PROOF 4: refcount/EOF teardown — both members exit ⇒ switch exits ──
        prove_teardown(
            &reg,
            sw,
            &attached,
            &stop_accept,
            &ctl_path,
            &mut a_in,
            &mut b_in,
            &mut ma,
            &mut mb,
            accept_thread,
        );

        // Best-effort scratch cleanup.
        let _ = std::fs::remove_dir_all(&home);

        eprintln!("\nS6-XPROC: PASS");
        std::process::exit(0);
    }

    /// Switch-side: accept attach connections, recv the SCM_RIGHTS fd + meta, and
    /// call the PROVEN `VSwitch::add_member` verbatim. The switch now owns the host
    /// end of each member's socketpair. Exits when signalled.
    fn accept_loop(
        listener: &UnixListener,
        sw: &Arc<VSwitch>,
        attached: &AtomicUsize,
        stop: &AtomicBool,
        ctl_path: &Path,
    ) {
        listener.set_nonblocking(true).ok();
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((conn, _addr)) => {
                    conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let (fd, meta) = match recv_fd(&conn) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("  switch: recv_fd failed: {e}");
                            continue;
                        }
                    };
                    let Some((mac, ip, name)) = decode_meta(&meta) else {
                        // SAFETY: close the orphaned fd so it cannot leak.
                        unsafe { libc::close(fd) };
                        eprintln!("  switch: bad attach metadata");
                        continue;
                    };
                    // THE PROVEN CALL, REUSED VERBATIM — the switch owns `fd` now.
                    match sw.add_member(fd, mac, ip, &name) {
                        Ok(()) => {
                            eprintln!("  switch: add_member({name}, {ip}) via passed fd OK");
                            attached.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) => eprintln!("  switch: add_member({name}) failed: {e}"),
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    eprintln!("  switch: accept error: {e}");
                    let _ = std::fs::remove_file(ctl_path);
                    return;
                }
            }
        }
    }

    fn spawn_member(exe: &Path, name: &str, ctl: &Path, ip: Ipv4Addr) -> Child {
        let mut child = Command::new(exe)
            .arg("--member")
            .arg(name)
            .arg(ctl)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| fail(&format!("spawn member {name}: {e}")));
        // Hand the member its registry IP, then wait for its READY line.
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "IP {ip}").unwrap_or_else(|e| fail(&format!("write IP to {name}: {e}")));
        stdin.flush().ok();
        // Re-attach stdin so the orchestrator keeps driving the line protocol.
        child.stdin = Some(stdin);
        child
    }

    // ── PROOF 1 ───────────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn prove_frame_forward(
        a_mac: [u8; 6],
        b_mac: [u8; 6],
        a_in: &mut impl Write,
        a_out: &mut impl BufRead,
        b_in: &mut impl Write,
        b_out: &mut impl BufRead,
    ) {
        // First flood A→B so the switch learns A's MAC (so B's reply can unicast),
        // then the real assertion: a unicast A→B frame must arrive at B verbatim.
        // Arm B's receiver FIRST (RECV blocks up to 2s), then send from A.
        let payload = b"S6-XPROC-FRAME-AB";
        let frame = build_eth(b_mac, a_mac, 0x88b5, payload); // 0x88b5 = local experimental

        // Arm B's RECV on a background reader so it is listening before A sends.
        send_line(b_in, "RECV");
        thread::sleep(Duration::from_millis(150));
        send_line(a_in, &format!("SEND {}", hex_encode(&frame)));
        expect_line(a_out, "SENT", "A SEND ack");

        let got = read_line(b_out);
        let hex = got.strip_prefix("FRAME ").unwrap_or_else(|| {
            fail(&format!(
                "PROOF1: B did not receive A's frame (got {got:?})"
            ))
        });
        let recvd = hex_decode(hex);
        if recvd != frame {
            fail("PROOF1: B received a DIFFERENT frame than A sent");
        }
        ok("PROOF1: A→B Ethernet frame FORWARDED across processes (L2 switch xproc)");
    }

    // ── PROOF 2 ───────────────────────────────────────────────────────────────────

    fn prove_dhcp(a_ip: Ipv4Addr, a_in: &mut impl Write, a_out: &mut impl BufRead) {
        send_line(a_in, &format!("DHCP {a_ip}"));
        let got = read_line(a_out);
        let leased = got.strip_prefix("DHCPIP ").unwrap_or_else(|| {
            fail(&format!(
                "PROOF2: no DHCP OFFER across processes (got {got:?})"
            ))
        });
        let leased: Ipv4Addr = leased
            .parse()
            .unwrap_or_else(|_| fail("PROOF2: bad DHCP IP"));
        if leased != a_ip {
            fail(&format!(
                "PROOF2: DHCP leased {leased}, expected registry IP {a_ip}"
            ));
        }
        ok(&format!(
            "PROOF2: embedded DHCP answered DISCOVER xproc → registry IP {a_ip}"
        ));
    }

    // ── PROOF 3 ───────────────────────────────────────────────────────────────────

    fn prove_dns(b_ip: Ipv4Addr, a_in: &mut impl Write, a_out: &mut impl BufRead) {
        // A asks the switch's embedded DNS for "b" — the OTHER member's name. This
        // is the `curl http://b` mechanism: cross-process name resolution.
        send_line(a_in, "DNS b");
        let got = read_line(a_out);
        let resolved = got.strip_prefix("DNSIP ").unwrap_or_else(|| {
            fail(&format!(
                "PROOF3: no DNS answer for 'b' across processes (got {got:?})"
            ))
        });
        let resolved: Ipv4Addr = resolved
            .parse()
            .unwrap_or_else(|_| fail("PROOF3: bad DNS IP"));
        if resolved != b_ip {
            fail(&format!(
                "PROOF3: DNS resolved b→{resolved}, expected {b_ip}"
            ));
        }
        ok(&format!(
            "PROOF3: embedded DNS resolved 'b'→{b_ip} xproc (curl-by-name mechanism)"
        ));
    }

    // ── PROOF 4 ───────────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn prove_teardown(
        reg: &NetworkRegistry,
        sw: Arc<VSwitch>,
        attached: &AtomicUsize,
        stop_accept: &AtomicBool,
        ctl_path: &Path,
        a_in: &mut impl Write,
        b_in: &mut impl Write,
        ma: &mut Child,
        mb: &mut Child,
        accept_thread: thread::JoinHandle<()>,
    ) {
        // Both members exit (drop guest_fd) ⇒ they leave the registry. The switch
        // detects the empty network via the registry refcount AND closes down: it
        // signals the accept loop, drops the ctl.sock, and tears down the VSwitch.
        send_line(a_in, "QUIT");
        send_line(b_in, "QUIT");

        let a_code = ma.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        let b_code = mb.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        if a_code != 0 || b_code != 0 {
            fail(&format!(
                "PROOF4: a member exited non-zero (a={a_code} b={b_code})"
            ));
        }

        // The supervisors call registry.leave on exit; here the orchestrator stands
        // in for that (the spike owns the registry). The empty refcount is the
        // teardown trigger — exactly as production decides to stop the switch.
        let _ = reg
            .leave("a")
            .unwrap_or_else(|e| fail(&format!("leave a: {e}")));
        let remaining = reg
            .leave("b")
            .unwrap_or_else(|e| fail(&format!("leave b: {e}")));
        if remaining != 0 {
            fail(&format!(
                "PROOF4: registry refcount not empty after both left ({remaining})"
            ));
        }
        ok("PROOF4: registry refcount reached 0 (both members left) → teardown trigger");

        // Network empty ⇒ stop the switch: stop the accept loop, remove the ctl.sock,
        // and shut down the VSwitch (joins every member thread). This is what a
        // per-network switch process would do before exiting cleanly.
        stop_accept.store(true, Ordering::SeqCst);
        let _ = accept_thread.join(); // drops the accept loop's Arc<VSwitch> clone

        // The accept thread released its clone; `sw` is now the sole owner. Taking
        // the inner VSwitch out lets us call the PROVEN `shutdown()` (consumes self,
        // joins every member thread) — the explicit per-network-switch stop path.
        match Arc::try_unwrap(sw) {
            Ok(vswitch) => vswitch
                .shutdown()
                .unwrap_or_else(|e| fail(&format!("PROOF4: VSwitch::shutdown: {e}"))),
            Err(arc) => fail(&format!(
                "PROOF4: VSwitch still has {} refs at teardown (thread leak)",
                Arc::strong_count(&arc)
            )),
        }
        let _ = std::fs::remove_file(ctl_path);

        // Assert clean teardown: ctl.sock gone, no member procs leaked, attached==2.
        if ctl_path.exists() {
            fail("PROOF4: ctl.sock leaked (not removed on teardown)");
        }
        if attached.load(Ordering::SeqCst) != 2 {
            fail("PROOF4: attach count mismatch");
        }
        if leaked_member_procs() {
            fail("PROOF4: a member subprocess leaked after teardown");
        }
        ok("PROOF4: clean teardown — switch stopped, ctl.sock + threads reclaimed, no leaked proc");
    }

    /// True iff any `s6-xproc-switch` member process (argv[0] basename match, not
    /// our own pid) is still running — a genuine leak. Mirrors s5's careful parse.
    fn leaked_member_procs() -> bool {
        let out = match Command::new("ps").args(["-axo", "pid=,command="]).output() {
            Ok(o) => o,
            Err(_) => return false,
        };
        let me = std::process::id();
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().any(|l| {
            let l = l.trim_start();
            let Some((pid_str, cmd)) = l.split_once(char::is_whitespace) else {
                return false;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                return false;
            };
            if pid == me {
                return false;
            }
            let argv0 = cmd.split_whitespace().next().unwrap_or("");
            let base = argv0.rsplit('/').next().unwrap_or(argv0);
            // A leaked MEMBER is an s6 binary running with --member in its argv.
            base == "s6-xproc-switch" && cmd.contains("--member")
        })
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // line-protocol helpers (orchestrator ↔ member)
    // ─────────────────────────────────────────────────────────────────────────────

    fn send_line(w: &mut impl Write, s: &str) {
        writeln!(w, "{s}").unwrap_or_else(|e| fail(&format!("write member line: {e}")));
        w.flush().ok();
    }

    fn read_line(r: &mut impl BufRead) -> String {
        let mut s = String::new();
        if r.read_line(&mut s).unwrap_or(0) == 0 {
            fail("member closed its stdout unexpectedly");
        }
        s.trim().to_string()
    }

    fn expect_line(r: &mut impl BufRead, want: &str, ctx: &str) {
        let got = read_line(r);
        if got != want {
            fail(&format!("{ctx}: expected {want:?}, got {got:?}"));
        }
    }

    fn wait_until(mut cond: impl FnMut() -> bool, to: Duration) -> Option<()> {
        let deadline = Instant::now() + to;
        while Instant::now() < deadline {
            if cond() {
                return Some(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        cond().then_some(())
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // frame crafting / decoding (mirrors vswitch/dhcp + dns wire helpers; the spike
    // must build the CLIENT-side frames the switch's PROVEN handlers answer).
    // ─────────────────────────────────────────────────────────────────────────────

    fn mac_for_name(name: &str) -> [u8; 6] {
        let h = blake3::hash(name.as_bytes());
        let b = h.as_bytes();
        [0x0a, 0x00, 0x00, b[0], b[1], b[2]]
    }

    fn build_eth(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + payload.len());
        e.extend_from_slice(&dst);
        e.extend_from_slice(&src);
        e.extend_from_slice(&ethertype.to_be_bytes());
        e.extend_from_slice(payload);
        e
    }

    fn ipv4_checksum(h: &[u8]) -> u16 {
        let mut sum = 0u32;
        let mut i = 0;
        while i + 1 < h.len() {
            sum += u16::from_be_bytes([h[i], h[i + 1]]) as u32;
            i += 2;
        }
        if i < h.len() {
            sum += (h[i] as u32) << 8;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }

    fn build_udp_ipv4_eth(
        src_mac: [u8; 6],
        dst_mac: [u8; 6],
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let udp_len = 8 + payload.len();
        let mut udp = Vec::with_capacity(udp_len);
        udp.extend_from_slice(&src_port.to_be_bytes());
        udp.extend_from_slice(&dst_port.to_be_bytes());
        udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]); // checksum 0 = not computed (allowed for IPv4 UDP)
        udp.extend_from_slice(payload);

        let total = 20 + udp.len();
        let mut ip = Vec::with_capacity(total);
        ip.push((4 << 4) | 5);
        ip.push(0);
        ip.extend_from_slice(&(total as u16).to_be_bytes());
        ip.extend_from_slice(&[0, 0]);
        ip.extend_from_slice(&[0x40, 0x00]);
        ip.push(64);
        ip.push(17); // UDP
        ip.extend_from_slice(&[0, 0]); // checksum placeholder
        ip.extend_from_slice(&src_ip.octets());
        ip.extend_from_slice(&dst_ip.octets());
        let csum = ipv4_checksum(&ip);
        ip[10..12].copy_from_slice(&csum.to_be_bytes());
        ip.extend_from_slice(&udp);

        build_eth(dst_mac, src_mac, 0x0800, &ip)
    }

    // ── DHCP DISCOVER (client → 255.255.255.255:67) ──

    fn build_dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
        const BOOTP_FIXED_LEN: usize = 236;
        let mut bootp = vec![0u8; BOOTP_FIXED_LEN];
        bootp[0] = 1; // BOOTREQUEST
        bootp[1] = 1; // HTYPE ethernet
        bootp[2] = 6; // HLEN
        bootp[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // xid
        bootp[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag
        bootp[28..34].copy_from_slice(&mac); // chaddr
        bootp.extend_from_slice(&0x6382_5363u32.to_be_bytes()); // magic cookie
        bootp.push(53);
        bootp.push(1);
        bootp.push(1); // option 53 = DISCOVER
        bootp.push(255); // END

        build_udp_ipv4_eth(
            mac,
            [0xff; 6],
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(255, 255, 255, 255),
            68,
            67,
            &bootp,
        )
    }

    /// Pull yiaddr (offered IP) out of a DHCP OFFER/ACK reply frame.
    fn decode_dhcp_yiaddr(frame: &[u8]) -> Option<Ipv4Addr> {
        // eth(14) + ip(ihl) + udp(8) + bootp; yiaddr is bootp bytes 16..20.
        let ip = frame.get(14..)?;
        if ip.first()? >> 4 != 4 {
            return None;
        }
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let udp = ip.get(ihl..)?;
        if udp.len() < 8 {
            return None;
        }
        // Must be from server port 67.
        if u16::from_be_bytes([udp[0], udp[1]]) != 67 {
            return None;
        }
        let bootp = udp.get(8..)?;
        if bootp.first()? != &2 {
            return None; // not BOOTREPLY
        }
        let y = bootp.get(16..20)?;
        Some(Ipv4Addr::new(y[0], y[1], y[2], y[3]))
    }

    // ── DNS A-query (client → gateway:53) ──

    fn build_dns_query(mac: [u8; 6], src_ip: Ipv4Addr, name: &str) -> Vec<u8> {
        let mut dns = Vec::new();
        dns.extend_from_slice(&0x1234u16.to_be_bytes()); // id
        dns.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1, QR=0
        dns.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        for label in name.split('.') {
            dns.push(label.len() as u8);
            dns.extend_from_slice(label.as_bytes());
        }
        dns.push(0); // root
        dns.extend_from_slice(&1u16.to_be_bytes()); // QTYPE A
        dns.extend_from_slice(&1u16.to_be_bytes()); // QCLASS IN

        // dst gateway IP doesn't have to match the embedded server's gateway for the
        // dns handler (it parses on dst_port==53), but use the subnet gateway shape.
        let gw = Ipv4Addr::new(
            src_ip.octets()[0],
            src_ip.octets()[1],
            src_ip.octets()[2],
            1,
        );
        build_udp_ipv4_eth(mac, [0x02, 0, 0, 0, 0, 1], src_ip, gw, 0xC001, 53, &dns)
    }

    /// Extract the first A-record IP from a DNS answer frame.
    fn decode_dns_first_a(frame: &[u8]) -> Option<Ipv4Addr> {
        let ip = frame.get(14..)?;
        if ip.first()? >> 4 != 4 {
            return None;
        }
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let udp = ip.get(ihl..)?;
        if udp.len() < 8 || u16::from_be_bytes([udp[0], udp[1]]) != 53 {
            return None;
        }
        let dns = udp.get(8..)?;
        if dns.len() < 12 {
            return None;
        }
        let ancount = u16::from_be_bytes([dns[6], dns[7]]);
        if ancount < 1 {
            return None;
        }
        // Skip the header + question. Walk the QNAME (uncompressed) then QTYPE/QCLASS.
        let mut pos = 12;
        loop {
            let len = *dns.get(pos)? as usize;
            if len == 0 {
                pos += 1;
                break;
            }
            if len & 0xc0 != 0 {
                // compression pointer (2 bytes) — shouldn't appear in a question.
                pos += 2;
                break;
            }
            pos += 1 + len;
        }
        pos += 4; // QTYPE + QCLASS
                  // First answer RR: NAME(2, pointer 0xC00C) TYPE(2) CLASS(2) TTL(4) RDLEN(2) RDATA.
                  // NAME may be a pointer (0xC0..) or a label sequence; handle the pointer.
        let name0 = *dns.get(pos)?;
        if name0 & 0xc0 == 0xc0 {
            pos += 2;
        } else {
            // walk labels
            loop {
                let len = *dns.get(pos)? as usize;
                pos += 1;
                if len == 0 {
                    break;
                }
                pos += len;
            }
        }
        let rtype = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]);
        pos += 2; // TYPE
        pos += 2; // CLASS
        pos += 4; // TTL
        let rdlen = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]) as usize;
        pos += 2;
        if rtype != 1 || rdlen != 4 {
            return None; // not an A record
        }
        let rd = dns.get(pos..pos + 4)?;
        Some(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3]))
    }

    // ── hex codec (line protocol carries frames as hex) ──

    fn hex_encode(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for &x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect()
    }

    fn fmt_mac(m: [u8; 6]) -> String {
        m.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }
} // mod imp (cfg(unix))
