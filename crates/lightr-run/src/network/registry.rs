//! On-disk, flock-guarded network registry under `$LIGHTR_HOME/net/<id>/`.

use super::alloc::{mac_for, subnet_for};
use super::fsutil::{FlockGuard, atomic_write};
use super::types::{Member, MemberOnDisk, NetworkId, Subnet, SubnetOnDisk};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

    pub(super) fn members_path(&self) -> PathBuf {
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
    fn next_free_ip(&self, members: &[Member]) -> io::Result<std::net::Ipv4Addr> {
        let base = self.subnet.base.octets();
        for host in 2u8..=254 {
            let candidate = std::net::Ipv4Addr::new(base[0], base[1], base[2], host);
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
