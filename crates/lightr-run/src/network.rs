//! Network registry for vz container networking (ADR-0018, F-304 Phase-2).
//!
//! Per-network membership + deterministic addressing, persisted under
//! `$LIGHTR_HOME/net/<id>/` and `flock`-guarded (mirroring the gc lock law).
//! The registry is the source of truth the userspace L2 switch reads for its
//! MAC-learning seed, DHCP leases, and DNS name table.
//!
//! CONTRACT STUB (ADR-0018, WP-C1): the signatures + types below are frozen.
//! WP-C1 fills the bodies and REMOVES the `#![allow]` line.
//!
//! ## Locking discipline (mirrors `lightr-store`'s gc lock law)
//!
//! Each network dir owns a `<id>/.lock` file. Mutators (`create` persist,
//! `join`, `leave`) take an EXCLUSIVE advisory `flock` (`LOCK_EX`) for the
//! whole read-modify-write; readers (`members`) take a SHARED lock
//! (`LOCK_SH`). This is a real advisory `flock` (this module is `#[cfg(unix)]`
//! and `libc` is a dep), so concurrent supervisors joining/leaving the same
//! network serialize their `members.json` updates and never tear it. Writes
//! are atomic (temp + fsync + rename + parent fsync), so a crash mid-write
//! leaves the previous `members.json` intact. Corrupt JSON fails closed
//! (an `io::Error`), never a silent empty-membership.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// A network's user-facing name (e.g. `"web-net"`); also its on-disk dir name.
pub type NetworkId = String;

/// A 6-byte Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacAddr(pub [u8; 6]);

/// One member of a network (a single container).
#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub aliases: Vec<String>,
    pub mac: MacAddr,
    pub ip: Ipv4Addr,
    /// `(host, container)` published ports for this member.
    pub ports: Vec<(u16, u16)>,
}

/// The IPv4 subnet a network leases addresses from (e.g. `10.69.0.0/24`,
/// gateway `.1` = the switch's virtual IP, which is also the DNS server).
#[derive(Clone, Copy, Debug)]
pub struct Subnet {
    pub base: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
}

// ─────────────────────── on-disk serde shapes ──────────────────────────────
//
// The public `Member`/`Subnet`/`MacAddr` types are frozen (ADR-0018) and do
// NOT derive serde, so we persist via small mirror records. This also keeps
// the on-disk format explicit and stable independent of the in-memory types.

#[derive(Serialize, Deserialize)]
struct SubnetOnDisk {
    base: [u8; 4],
    prefix: u8,
    gateway: [u8; 4],
}

impl From<Subnet> for SubnetOnDisk {
    fn from(s: Subnet) -> Self {
        SubnetOnDisk {
            base: s.base.octets(),
            prefix: s.prefix,
            gateway: s.gateway.octets(),
        }
    }
}

impl From<SubnetOnDisk> for Subnet {
    fn from(s: SubnetOnDisk) -> Self {
        Subnet {
            base: Ipv4Addr::from(s.base),
            prefix: s.prefix,
            gateway: Ipv4Addr::from(s.gateway),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct MemberOnDisk {
    name: String,
    aliases: Vec<String>,
    mac: [u8; 6],
    ip: [u8; 4],
    ports: Vec<(u16, u16)>,
}

impl From<&Member> for MemberOnDisk {
    fn from(m: &Member) -> Self {
        MemberOnDisk {
            name: m.name.clone(),
            aliases: m.aliases.clone(),
            mac: m.mac.0,
            ip: m.ip.octets(),
            ports: m.ports.clone(),
        }
    }
}

impl From<MemberOnDisk> for Member {
    fn from(m: MemberOnDisk) -> Self {
        Member {
            name: m.name,
            aliases: m.aliases,
            mac: MacAddr(m.mac),
            ip: Ipv4Addr::from(m.ip),
            ports: m.ports,
        }
    }
}

// ───────────────────────── flock guard (RAII) ──────────────────────────────

/// RAII advisory-lock guard over a network's `.lock` file. Mirrors
/// `lightr-store`'s `WriteGuard`/`GcGuard`: the held `File` keeps the lock; the
/// `Drop` releases it (closing the fd would also release, we call `LOCK_UN`
/// explicitly for clarity).
struct FlockGuard {
    _file: File,
}

impl FlockGuard {
    /// Acquire an advisory lock (`LOCK_SH` or `LOCK_EX`) on `lock_path`.
    fn acquire(lock_path: &Path, exclusive: bool) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        let ret = unsafe { libc::flock(file.as_raw_fd(), op) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FlockGuard { _file: file })
    }
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        let fd = self._file.as_raw_fd();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}

// ─────────────────────────── fs helpers ────────────────────────────────────

/// fsync the parent directory so a rename (directory-entry change) is durable.
fn fsync_dir(dir: &Path) -> io::Result<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

/// Atomic write: temp file in `parent`, fsync the file, rename to `dest`, then
/// fsync `parent` so the rename is crash-durable (mirrors lightr-store).
fn atomic_write(parent: &Path, dest: &Path, data: &[u8]) -> io::Result<()> {
    fs::create_dir_all(parent)?;
    let nonce = format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(nonce);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dest)?;
    fsync_dir(parent)?;
    Ok(())
}

// ─────────────────── deterministic allocation helpers ──────────────────────

/// Stable subnet third-octet `k` from the network id: `10.69.<k>.0/24`.
/// `k` is `blake3(id)[0]`, so distinct ids land on distinct /24s with high
/// probability and the same id is always the same subnet across processes.
fn subnet_for(id: &NetworkId) -> Subnet {
    let h = blake3::hash(id.as_bytes());
    let k = h.as_bytes()[0];
    Subnet {
        base: Ipv4Addr::new(10, 69, k, 0),
        prefix: 24,
        gateway: Ipv4Addr::new(10, 69, k, 1),
    }
}

/// Deterministic locally-administered unicast MAC for `name`:
/// `0a:00:00` + 3 bytes of `blake3(name)`. `0x0a` has the locally-administered
/// bit set and the unicast (group) bit clear, so it never collides with a
/// real vendor OUI and is a valid source/dest MAC.
fn mac_for(name: &str) -> MacAddr {
    let h = blake3::hash(name.as_bytes());
    let b = h.as_bytes();
    MacAddr([0x0a, 0x00, 0x00, b[0], b[1], b[2]])
}

/// On-disk, `flock`-guarded network registry under `$LIGHTR_HOME/net/<id>/`.
pub struct NetworkRegistry {
    /// The network dir: `<home>/net/<id>/` (its last component is the id).
    dir: PathBuf,
    /// The allocated subnet (loaded/persisted in `subnet.json`).
    subnet: Subnet,
}

impl NetworkRegistry {
    /// `<home>/net`
    fn net_root(home: &Path) -> PathBuf {
        home.join("net")
    }

    /// `<home>/net/<id>`
    fn dir_for(home: &Path, id: &NetworkId) -> PathBuf {
        Self::net_root(home).join(id)
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(".lock")
    }

    fn subnet_path(&self) -> PathBuf {
        self.dir.join("subnet.json")
    }

    fn members_path(&self) -> PathBuf {
        self.dir.join("members.json")
    }

    /// Read the members file under an already-held lock. Absent ⇒ empty.
    /// Corrupt JSON ⇒ fail closed (`InvalidData`), never a silent reset.
    fn read_members(&self) -> io::Result<Vec<Member>> {
        let path = self.members_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&path)?;
        let recs: Vec<MemberOnDisk> = serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt members.json at {}: {e}", path.display()),
            )
        })?;
        Ok(recs.into_iter().map(Member::from).collect())
    }

    /// Persist the members file atomically (caller holds the lock).
    fn write_members(&self, members: &[Member]) -> io::Result<()> {
        let recs: Vec<MemberOnDisk> = members.iter().map(MemberOnDisk::from).collect();
        let bytes = serde_json::to_vec_pretty(&recs).map_err(io::Error::other)?;
        atomic_write(&self.dir, &self.members_path(), &bytes)
    }

    /// Next free host IP in the subnet, skipping `.1` (gateway) and any IP
    /// already taken. `/24` only (the deterministic allocator only ever mints
    /// `10.69.<k>.0/24`). Host range is `.2 ..= .254`.
    fn next_free_ip(&self, members: &[Member]) -> io::Result<Ipv4Addr> {
        let base = self.subnet.base.octets();
        for host in 2u8..=254 {
            let candidate = Ipv4Addr::new(base[0], base[1], base[2], host);
            if candidate == self.subnet.gateway {
                continue;
            }
            if members.iter().all(|m| m.ip != candidate) {
                return Ok(candidate);
            }
        }
        Err(io::Error::other(format!(
            "subnet {}/24 exhausted (>252 members)",
            self.subnet.base
        )))
    }

    /// Create (or open if present) the named network, allocating its subnet.
    /// Idempotent: if `<home>/net/<id>/` already exists, behaves like [`open`].
    ///
    /// [`open`]: NetworkRegistry::open
    pub fn create(home: &Path, id: &NetworkId) -> io::Result<Self> {
        let dir = Self::dir_for(home, id);
        let subnet_path = dir.join("subnet.json");

        // Idempotent: a fully-initialised dir ⇒ open it.
        if subnet_path.exists() {
            return Self::open(home, id);
        }

        fs::create_dir_all(&dir)?;

        // Serialise create/init against any concurrent creator via the lock.
        let reg = NetworkRegistry {
            dir: dir.clone(),
            subnet: subnet_for(id),
        };
        let _guard = FlockGuard::acquire(&reg.lock_path(), true)?;

        // Re-check under the lock (another process may have won the race).
        if subnet_path.exists() {
            drop(_guard);
            return Self::open(home, id);
        }

        // Persist subnet.json and an empty members.json.
        let sub: SubnetOnDisk = reg.subnet.into();
        let sub_bytes = serde_json::to_vec_pretty(&sub).map_err(io::Error::other)?;
        atomic_write(&reg.dir, &reg.subnet_path(), &sub_bytes)?;
        reg.write_members(&[])?;

        Ok(reg)
    }

    /// Open an existing network; error if absent.
    pub fn open(home: &Path, id: &NetworkId) -> io::Result<Self> {
        let dir = Self::dir_for(home, id);
        let subnet_path = dir.join("subnet.json");
        if !subnet_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "network {id} not found under {}",
                    Self::net_root(home).display()
                ),
            ));
        }
        let bytes = fs::read(&subnet_path)?;
        let sub: SubnetOnDisk = serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt subnet.json at {}: {e}", subnet_path.display()),
            )
        })?;
        Ok(NetworkRegistry {
            dir,
            subnet: sub.into(),
        })
    }

    /// Join: allocate a deterministic MAC + IP for `name`, persist the member,
    /// bump the refcount, and return the assigned `Member`.
    ///
    /// Deterministic: the MAC is a stable hash of `name`; re-joining an
    /// existing `name` returns its already-persisted `Member` unchanged (same
    /// IP + MAC across calls), refreshing nothing — idempotent. A new `name`
    /// takes the next free host IP (skipping the `.1` gateway).
    pub fn join(&self, name: &str, aliases: &[String], ports: &[(u16, u16)]) -> io::Result<Member> {
        let _guard = FlockGuard::acquire(&self.lock_path(), true)?;
        let mut members = self.read_members()?;

        // Idempotent re-join: same (id, name) ⇒ same record (IP + MAC stable).
        if let Some(existing) = members.iter().find(|m| m.name == name) {
            return Ok(existing.clone());
        }

        let member = Member {
            name: name.to_string(),
            aliases: aliases.to_vec(),
            mac: mac_for(name),
            ip: self.next_free_ip(&members)?,
            ports: ports.to_vec(),
        };
        members.push(member.clone());
        self.write_members(&members)?;
        Ok(member)
    }

    /// Leave: remove `name`, decrement the refcount, return remaining count.
    /// Removing an absent member is a no-op that returns the current count.
    pub fn leave(&self, name: &str) -> io::Result<usize> {
        let _guard = FlockGuard::acquire(&self.lock_path(), true)?;
        let mut members = self.read_members()?;
        members.retain(|m| m.name != name);
        let remaining = members.len();
        self.write_members(&members)?;
        Ok(remaining)
    }

    /// All current members (the switch's flooding set + DNS/lease seed).
    pub fn members(&self) -> io::Result<Vec<Member>> {
        let _guard = FlockGuard::acquire(&self.lock_path(), false)?;
        self.read_members()
    }

    /// This network's subnet.
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// List every network registered under `home`. A `net/<id>/` dir counts
    /// only if it carries a `subnet.json` (a fully-initialised network); a
    /// half-created dir is skipped. Returns sorted ascending.
    pub fn list(home: &Path) -> io::Result<Vec<NetworkId>> {
        let root = Self::net_root(home);
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if !path.join("subnet.json").exists() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn home() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn create_then_join_two_members_distinct_ip_mac() {
        let h = home();
        let reg = NetworkRegistry::create(h.path(), &"web".to_string()).unwrap();

        let a = reg.join("api", &[], &[]).unwrap();
        let b = reg.join("db", &[], &[]).unwrap();

        // Both present in members().
        let members = reg.members().unwrap();
        assert_eq!(members.len(), 2);
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db"));

        // Distinct, deterministic IPs and MACs.
        assert_ne!(a.ip, b.ip, "members must get distinct IPs");
        assert_ne!(a.mac, b.mac, "members must get distinct MACs");

        // IPs are in-subnet and never the gateway.
        let sub = reg.subnet();
        for m in [&a, &b] {
            assert_ne!(m.ip, sub.gateway, "must skip the .1 gateway");
            assert_eq!(m.ip.octets()[0], 10);
            assert_eq!(m.ip.octets()[1], 69);
            assert_eq!(m.ip.octets()[2], sub.base.octets()[2]);
        }

        // MACs are locally-administered unicast (0x0a prefix).
        assert_eq!(a.mac.0[0], 0x0a);
        assert_eq!(b.mac.0[0], 0x0a);
    }

    #[test]
    fn join_is_deterministic_same_name_same_ip_mac() {
        let h = home();
        let reg = NetworkRegistry::create(h.path(), &"net1".to_string()).unwrap();

        let first = reg
            .join("svc", &["alias".to_string()], &[(8080, 80)])
            .unwrap();
        // Re-join the same name: must return the identical record, no churn.
        let again = reg.join("svc", &[], &[]).unwrap();

        assert_eq!(first.ip, again.ip, "same name ⇒ same IP");
        assert_eq!(first.mac, again.mac, "same name ⇒ same MAC");
        // Idempotent: still exactly one member, original aliases/ports kept.
        let members = reg.members().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].aliases, vec!["alias".to_string()]);
        assert_eq!(members[0].ports, vec![(8080, 80)]);
    }

    #[test]
    fn mac_ip_stable_across_reopen() {
        let h = home();
        let id = "persist".to_string();
        let assigned = {
            let reg = NetworkRegistry::create(h.path(), &id).unwrap();
            reg.join("one", &[], &[]).unwrap()
        };
        // Reopen from disk: the persisted member must round-trip exactly.
        let reg2 = NetworkRegistry::open(h.path(), &id).unwrap();
        let members = reg2.members().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].ip, assigned.ip);
        assert_eq!(members[0].mac, assigned.mac);
        // The MAC is the pure hash of the name — verify the scheme directly.
        assert_eq!(assigned.mac, mac_for("one"));
    }

    #[test]
    fn leave_returns_remaining_count() {
        let h = home();
        let reg = NetworkRegistry::create(h.path(), &"net2".to_string()).unwrap();
        reg.join("a", &[], &[]).unwrap();
        reg.join("b", &[], &[]).unwrap();
        reg.join("c", &[], &[]).unwrap();

        assert_eq!(reg.leave("b").unwrap(), 2, "3 − 1 = 2 remaining");
        assert_eq!(reg.leave("a").unwrap(), 1);
        // Removing an absent member is a no-op returning the current count.
        assert_eq!(reg.leave("ghost").unwrap(), 1);
        assert_eq!(reg.leave("c").unwrap(), 0);

        assert!(reg.members().unwrap().is_empty());
    }

    #[test]
    fn list_enumerates_networks_sorted() {
        let h = home();
        NetworkRegistry::create(h.path(), &"zeta".to_string()).unwrap();
        NetworkRegistry::create(h.path(), &"alpha".to_string()).unwrap();
        NetworkRegistry::create(h.path(), &"mid".to_string()).unwrap();

        let ids = NetworkRegistry::list(h.path()).unwrap();
        assert_eq!(ids, vec!["alpha", "mid", "zeta"]);

        // A home with no net/ dir lists empty (no error).
        let empty = home();
        assert!(NetworkRegistry::list(empty.path()).unwrap().is_empty());
    }

    #[test]
    fn create_is_idempotent_and_preserves_members() {
        let h = home();
        let id = "idem".to_string();
        let reg = NetworkRegistry::create(h.path(), &id).unwrap();
        let m = reg.join("keepme", &[], &[]).unwrap();
        let sub_before = reg.subnet();

        // A second create() must NOT wipe the dir — it opens the existing net.
        let reg2 = NetworkRegistry::create(h.path(), &id).unwrap();
        let members = reg2.members().unwrap();
        assert_eq!(members.len(), 1, "create must not clobber existing members");
        assert_eq!(members[0].name, "keepme");
        assert_eq!(members[0].ip, m.ip);
        // Same subnet across the idempotent create.
        assert_eq!(reg2.subnet().base, sub_before.base);
        assert_eq!(reg2.subnet().gateway, sub_before.gateway);

        // Only one network on disk.
        assert_eq!(NetworkRegistry::list(h.path()).unwrap(), vec!["idem"]);
    }

    #[test]
    fn distinct_ids_get_distinct_subnets() {
        // The deterministic allocator should separate distinct networks.
        let a = subnet_for(&"web".to_string());
        let b = subnet_for(&"data".to_string());
        assert_ne!(
            a.base.octets()[2],
            b.base.octets()[2],
            "distinct ids should map to distinct /24s"
        );
        // Gateway is always .1 of the allocated /24.
        assert_eq!(a.gateway.octets()[3], 1);
        assert_eq!(a.gateway.octets()[2], a.base.octets()[2]);
    }

    #[test]
    fn corrupt_members_fails_closed() {
        let h = home();
        let id = "corrupt".to_string();
        let reg = NetworkRegistry::create(h.path(), &id).unwrap();
        // Stomp members.json with garbage.
        fs::write(reg.members_path(), b"{not valid json").unwrap();
        let err = reg.members().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "must fail closed");
    }
}
