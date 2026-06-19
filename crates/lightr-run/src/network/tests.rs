//! Tests for the network module — included via `#[cfg(test)] #[path="tests.rs"] mod tests;`
//! in registry.rs so that `use super::*` gives access to private helpers.

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
