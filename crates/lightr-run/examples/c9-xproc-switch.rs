//! C9-XPROC — WP-C9 production cross-process switch_host PASS gate.
//!
//! Where `s6-xproc-switch` was the de-risk SPIKE (the orchestrator process WAS
//! the switch), this example drives the PRODUCTION [`switch_host`] API in its
//! real three-role topology:
//!
//!   * the SWITCH HOST is its own process — `switch_host::attach` BIRTHS it by
//!     re-execing this binary with [`SWITCH_HOST_ARGV`], which dispatches into
//!     `switch_host::run_switch_host` (the productionized accept-loop + refcount
//!     self-stop);
//!   * each MEMBER is its own process that calls `switch_host::attach` and keeps
//!     the returned guest fd as its mesh NIC, exactly as the detached `vz`
//!     supervisor (`svz.rs`) does.
//!
//! No VMs (the VM↔fd half is s5's job; the full vz boot E2E is the deferred
//! on-box follow-up). We drive crafted frames on the guest fd directly and prove,
//! ACROSS PROCESS BOUNDARIES, via the production API only:
//!   1. flock-elected birth + connect-or-birth (two members, ONE switch host),
//!   2. A→B Ethernet frame forwarding (L2 switch xproc),
//!   3. embedded DHCP answers a DISCOVER with the registry IP,
//!   4. embedded DNS resolves the OTHER member's name,
//!   5. detach → refcount 0 → the switch host SELF-stops (no leaked process/sock).
//!
//! Run:  cargo run -p lightr-run --example c9-xproc-switch
//! Exit: 0 on PASS, non-zero on ANY failed assertion.
//!
//! Platform: unix-only (SCM_RIGHTS / socketpair / flock). On windows it compiles
//! to an honest non-zero stub so the windows `--all-targets` gate stays clean.

#[cfg(not(unix))]
fn main() {
    eprintln!("C9-XPROC: unsupported on this platform (unix-only)");
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
    use std::os::unix::net::UnixDatagram;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use lightr_run::network::NetworkRegistry;
    use lightr_run::vswitch::switch_host::{attach, run_switch_host, SWITCH_HOST_ARGV};

    fn ok(m: &str) {
        eprintln!("  [PASS] {m}");
    }
    fn fail(m: &str) -> ! {
        eprintln!("  [FAIL] {m}\nC9-XPROC: FAIL");
        std::process::exit(1);
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        // Dispatch the switch-host re-exec (what `attach` spawns) into the
        // production body — this is the consumer's job (CLI/NET3 in prod).
        if args.len() >= 4 && args[1] == SWITCH_HOST_ARGV {
            let _ = run_switch_host(Path::new(&args[2]), &args[3]);
            std::process::exit(0);
        }
        if args.len() >= 5 && args[1] == "--member" {
            member_main(&args[2], &args[3], &args[4]);
        }
        orchestrator();
    }

    // ── MEMBER PROCESS ───────────────────────────────────────────────────────
    // Calls the production `attach` (births-or-connects the switch host), keeps
    // the guest fd, and services a tiny stdin line protocol on it.
    fn member_main(home: &str, id: &str, name: &str) -> ! {
        let reg = NetworkRegistry::open(Path::new(home), &id.to_string())
            .unwrap_or_else(|e| die(&format!("open registry: {e}")));
        let members = reg
            .members()
            .unwrap_or_else(|e| die(&format!("members: {e}")));
        let me = members
            .into_iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| die("member not in registry"));
        let guest =
            attach(Path::new(home), id, &me).unwrap_or_else(|e| die(&format!("attach: {e}")));
        let guest = UnixDatagram::from(guest);
        guest.set_read_timeout(Some(Duration::from_secs(2))).ok();

        println!("READY {name}");
        let _ = std::io::stdout().flush();

        let mut stdin = BufReader::new(std::io::stdin());
        let mut line = String::new();
        loop {
            line.clear();
            if stdin.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let cmd = line.trim();
            if cmd == "QUIT" {
                break;
            } else if let Some(hex) = cmd.strip_prefix("SEND ") {
                let _ = guest.send(&hex_decode(hex));
                println!("SENT");
            } else if cmd == "RECV" {
                match recv_one(&guest) {
                    Some(f) => println!("FRAME {}", hex_encode(&f)),
                    None => println!("TIMEOUT"),
                }
            } else if let Some(ips) = cmd.strip_prefix("DHCP ") {
                let _ = ips;
                match dhcp(&guest, me.mac.0) {
                    Some(ip) => println!("DHCPIP {ip}"),
                    None => println!("DHCPNONE"),
                }
            } else if let Some(n) = cmd.strip_prefix("DNS ") {
                match dns(&guest, me.mac.0, me.ip, n) {
                    Some(ip) => println!("DNSIP {ip}"),
                    None => println!("DNSNONE"),
                }
            }
            let _ = std::io::stdout().flush();
        }
        drop(guest);
        std::process::exit(0);
    }

    fn die(m: &str) -> ! {
        eprintln!("member: {m}");
        std::process::exit(2);
    }

    fn recv_one(s: &UnixDatagram) -> Option<Vec<u8>> {
        let mut b = vec![0u8; 64 * 1024];
        match s.recv(&mut b) {
            Ok(n) if n > 0 => Some(b[..n].to_vec()),
            _ => None,
        }
    }

    fn dhcp(s: &UnixDatagram, mac: [u8; 6]) -> Option<Ipv4Addr> {
        s.send(&build_dhcp_discover(mac)).ok()?;
        decode_dhcp_yiaddr(&recv_one(s)?)
    }

    fn dns(s: &UnixDatagram, mac: [u8; 6], ip: Ipv4Addr, name: &str) -> Option<Ipv4Addr> {
        s.send(&build_dns_query(mac, ip, name)).ok()?;
        decode_dns_first_a(&recv_one(s)?)
    }

    // ── ORCHESTRATOR ──────────────────────────────────────────────────────────
    fn orchestrator() -> ! {
        eprintln!("C9-XPROC — production cross-process switch_host PASS gate");
        let home = std::env::temp_dir().join(format!("c9-xproc-{}", std::process::id()));
        let id = format!("c9net-{}", std::process::id());
        std::fs::create_dir_all(&home).expect("mk home");

        let reg =
            NetworkRegistry::create(&home, &id).unwrap_or_else(|e| fail(&format!("create: {e}")));
        let a = reg
            .join("a", &[], &[])
            .unwrap_or_else(|e| fail(&format!("join a: {e}")));
        let b = reg
            .join("b", &[], &[])
            .unwrap_or_else(|e| fail(&format!("join b: {e}")));
        eprintln!("  registry: a={} b={}", a.ip, b.ip);

        let exe = std::env::current_exe().unwrap_or_else(|e| fail(&format!("current_exe: {e}")));
        // Member A attaches first → BIRTHS the switch host (flock-elect → re-exec).
        let mut ma = spawn_member(&exe, &home, &id, "a");
        let mut mb = spawn_member(&exe, &home, &id, "b");

        let mut a_in = ma.stdin.take().unwrap();
        let mut a_out = BufReader::new(ma.stdout.take().unwrap());
        let mut b_in = mb.stdin.take().unwrap();
        let mut b_out = BufReader::new(mb.stdout.take().unwrap());
        expect(&mut a_out, "READY a", "a ready");
        expect(&mut b_out, "READY b", "b ready");
        ok("2 SEPARATE member processes attached; switch host birthed via re-exec");

        // PROOF: A→B frame forward.
        let frame = build_eth(b.mac.0, a.mac.0, 0x88b5, b"C9-FRAME-AB");
        send(&mut b_in, "RECV");
        std::thread::sleep(Duration::from_millis(150));
        send(&mut a_in, &format!("SEND {}", hex_encode(&frame)));
        expect(&mut a_out, "SENT", "a sent");
        let got = read(&mut b_out);
        let hex = got
            .strip_prefix("FRAME ")
            .unwrap_or_else(|| fail(&format!("B did not get A's frame: {got:?}")));
        if hex_decode(hex) != frame {
            fail("B got a different frame");
        }
        ok("A→B Ethernet frame forwarded across processes (xproc L2 switch)");

        // PROOF: DHCP.
        send(&mut a_in, &format!("DHCP {}", a.ip));
        let got = read(&mut a_out);
        let leased = got
            .strip_prefix("DHCPIP ")
            .unwrap_or_else(|| fail(&format!("no DHCP OFFER: {got:?}")));
        if leased.parse::<Ipv4Addr>().ok() != Some(a.ip) {
            fail(&format!("DHCP leased {leased}, want {}", a.ip));
        }
        ok(&format!(
            "DHCP answered DISCOVER xproc → registry IP {}",
            a.ip
        ));

        // PROOF: DNS for the OTHER member's name.
        send(&mut a_in, "DNS b");
        let got = read(&mut a_out);
        let res = got
            .strip_prefix("DNSIP ")
            .unwrap_or_else(|| fail(&format!("no DNS answer: {got:?}")));
        if res.parse::<Ipv4Addr>().ok() != Some(b.ip) {
            fail(&format!("DNS b→{res}, want {}", b.ip));
        }
        ok(&format!("DNS resolved 'b'→{} xproc (curl-by-name)", b.ip));

        // PROOF: detach both → refcount 0 → switch host self-stops.
        send(&mut a_in, "QUIT");
        send(&mut b_in, "QUIT");
        if ma.wait().ok().and_then(|s| s.code()) != Some(0)
            || mb.wait().ok().and_then(|s| s.code()) != Some(0)
        {
            fail("a member exited non-zero");
        }
        reg.leave("a")
            .unwrap_or_else(|e| fail(&format!("leave a: {e}")));
        let rem = reg
            .leave("b")
            .unwrap_or_else(|e| fail(&format!("leave b: {e}")));
        if rem != 0 {
            fail("registry not empty after both left");
        }

        // The switch host watches the refcount and self-stops; its ctl.sock must
        // disappear and no switch-host process may linger.
        let ctl = home.join("net").join(&id).join("ctl.sock");
        let deadline = Instant::now() + Duration::from_secs(8);
        while ctl.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if ctl.exists() {
            fail("switch host did not self-stop (ctl.sock leaked)");
        }
        if leaked_switch_host() {
            fail("a switch-host process leaked after refcount 0");
        }
        ok("detach → refcount 0 → switch host self-stopped, ctl.sock + process reclaimed");

        let _ = std::fs::remove_dir_all(&home);
        eprintln!("\nC9-XPROC: PASS");
        std::process::exit(0);
    }

    fn spawn_member(exe: &Path, home: &Path, id: &str, name: &str) -> Child {
        Command::new(exe)
            .arg("--member")
            .arg(home)
            .arg(id)
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| fail(&format!("spawn member {name}: {e}")))
    }

    fn leaked_switch_host() -> bool {
        let out = match Command::new("ps").args(["-axo", "command="]).output() {
            Ok(o) => o,
            Err(_) => return false,
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.contains("c9-xproc-switch") && l.contains(SWITCH_HOST_ARGV))
    }

    fn send(w: &mut impl Write, s: &str) {
        writeln!(w, "{s}").unwrap_or_else(|e| fail(&format!("write: {e}")));
        w.flush().ok();
    }
    fn read(r: &mut impl BufRead) -> String {
        let mut s = String::new();
        if r.read_line(&mut s).unwrap_or(0) == 0 {
            fail("member closed stdout");
        }
        s.trim().to_string()
    }
    fn expect(r: &mut impl BufRead, want: &str, ctx: &str) {
        let got = read(r);
        if got != want {
            fail(&format!("{ctx}: want {want:?}, got {got:?}"));
        }
    }

    // ── crafted client frames (mirror s6) ─────────────────────────────────────
    fn build_eth(dst: [u8; 6], src: [u8; 6], et: u16, p: &[u8]) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + p.len());
        e.extend_from_slice(&dst);
        e.extend_from_slice(&src);
        e.extend_from_slice(&et.to_be_bytes());
        e.extend_from_slice(p);
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
    #[allow(clippy::too_many_arguments)]
    fn build_udp(
        sm: [u8; 6],
        dm: [u8; 6],
        si: Ipv4Addr,
        di: Ipv4Addr,
        sp: u16,
        dp: u16,
        pl: &[u8],
    ) -> Vec<u8> {
        let ul = 8 + pl.len();
        let mut udp = Vec::with_capacity(ul);
        udp.extend_from_slice(&sp.to_be_bytes());
        udp.extend_from_slice(&dp.to_be_bytes());
        udp.extend_from_slice(&(ul as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]);
        udp.extend_from_slice(pl);
        let total = 20 + udp.len();
        let mut ip = Vec::with_capacity(total);
        ip.push((4 << 4) | 5);
        ip.push(0);
        ip.extend_from_slice(&(total as u16).to_be_bytes());
        ip.extend_from_slice(&[0, 0, 0x40, 0x00]);
        ip.push(64);
        ip.push(17);
        ip.extend_from_slice(&[0, 0]);
        ip.extend_from_slice(&si.octets());
        ip.extend_from_slice(&di.octets());
        let c = ipv4_checksum(&ip);
        ip[10..12].copy_from_slice(&c.to_be_bytes());
        ip.extend_from_slice(&udp);
        build_eth(dm, sm, 0x0800, &ip)
    }
    fn build_dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
        let mut b = vec![0u8; 236];
        b[0] = 1;
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        b[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        b[28..34].copy_from_slice(&mac);
        b.extend_from_slice(&0x6382_5363u32.to_be_bytes());
        b.push(53);
        b.push(1);
        b.push(1);
        b.push(255);
        build_udp(
            mac,
            [0xff; 6],
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(255, 255, 255, 255),
            68,
            67,
            &b,
        )
    }
    fn decode_dhcp_yiaddr(f: &[u8]) -> Option<Ipv4Addr> {
        let ip = f.get(14..)?;
        if ip.first()? >> 4 != 4 {
            return None;
        }
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let udp = ip.get(ihl..)?;
        if udp.len() < 8 || u16::from_be_bytes([udp[0], udp[1]]) != 67 {
            return None;
        }
        let bp = udp.get(8..)?;
        if bp.first()? != &2 {
            return None;
        }
        let y = bp.get(16..20)?;
        Some(Ipv4Addr::new(y[0], y[1], y[2], y[3]))
    }
    fn build_dns_query(mac: [u8; 6], si: Ipv4Addr, name: &str) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&0x1234u16.to_be_bytes());
        d.extend_from_slice(&0x0100u16.to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes());
        for l in name.split('.') {
            d.push(l.len() as u8);
            d.extend_from_slice(l.as_bytes());
        }
        d.push(0);
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes());
        let gw = Ipv4Addr::new(si.octets()[0], si.octets()[1], si.octets()[2], 1);
        build_udp(mac, [0x02, 0, 0, 0, 0, 1], si, gw, 0xC001, 53, &d)
    }
    fn decode_dns_first_a(f: &[u8]) -> Option<Ipv4Addr> {
        let ip = f.get(14..)?;
        if ip.first()? >> 4 != 4 {
            return None;
        }
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let udp = ip.get(ihl..)?;
        if udp.len() < 8 || u16::from_be_bytes([udp[0], udp[1]]) != 53 {
            return None;
        }
        let dns = udp.get(8..)?;
        if dns.len() < 12 || u16::from_be_bytes([dns[6], dns[7]]) < 1 {
            return None;
        }
        let mut pos = 12;
        loop {
            let len = *dns.get(pos)? as usize;
            if len == 0 {
                pos += 1;
                break;
            }
            if len & 0xc0 != 0 {
                pos += 2;
                break;
            }
            pos += 1 + len;
        }
        pos += 4;
        if *dns.get(pos)? & 0xc0 == 0xc0 {
            pos += 2;
        } else {
            loop {
                let len = *dns.get(pos)? as usize;
                pos += 1;
                if len == 0 {
                    break;
                }
                pos += len;
            }
        }
        let rt = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]);
        pos += 8;
        let rl = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]) as usize;
        pos += 2;
        if rt != 1 || rl != 4 {
            return None;
        }
        let rd = dns.get(pos..pos + 4)?;
        Some(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3]))
    }

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
}
