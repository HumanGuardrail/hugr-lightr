//! Small `lightr run` handler helpers, split out of `mod.rs` to keep that file
//! under the 400-LOC godfile cap (house convention). `claim_name_and_print`
//! (the detached-run name claim + id print) and `expose_port_maps` (the WP-B2
//! `-P/--publish-all` EXPOSE→PortMaps builder) live here; both are re-exported
//! through `run` so existing `super::claim_name_and_print` callers are unchanged.

use lightr_engine::EngineKind;
use lightr_run::PortMap;
use lightr_store::Store;

use crate::exit::die_lightr;

/// WP-B2: validate the `-P/--publish-all` invocation shape. `-P` reads the
/// image's EXPOSE list, so it is honored ONLY on the detached vz container path
/// (`-d` + `--engine vz --rootfs <img>`) — the only path with a hydrated rootfs
/// image. Returns `Some(exit_code)` with an honest stderr message for a bad shape
/// (fail closed — never a silent drop), or `None` when the invocation is valid.
pub(super) fn publish_all_policy_error(
    detach: bool,
    engine: EngineKind,
    rootfs_ref: Option<&str>,
) -> Option<i32> {
    if !detach {
        eprintln!("lightr: -P/--publish-all requires -d (a published service runs detached)");
        return Some(2);
    }
    if !(engine == EngineKind::Vz && rootfs_ref.is_some()) {
        eprintln!(
            "lightr: -P/--publish-all requires `--engine vz --rootfs <img>` \
             (the EXPOSE list comes from the rootfs image)"
        );
        return Some(2);
    }
    None
}

/// WP-NET3: the vz container-networking flags (`--network`/`--network-alias`/
/// `--dns`) are honored ONLY on the `--engine vz --rootfs <img>` path — that is
/// where a per-container mesh NIC + guest resolv.conf exist. The native engine
/// SHARES the host network (no per-container netns) and has no container rootfs to
/// write, so any of those flags on native (or vz without a rootfs) is an honest
/// exit 2, never a silent drop.
///
/// `--add-host` is the EXCEPTION: it now does REAL work on the `ns` engine (PID 1
/// appends `(ip, hostname)` lines to the container's /etc/hosts before pivot), so
/// it is split OUT of the vz-only set and ALLOWED on `--engine ns`. native still
/// rejects it (no container rootfs to write /etc/hosts into reliably). Guarded
/// BEFORE any provisioning, because only the handler has the engine + rootfs.
/// Returns `Some(2)` for a bad combo, `None` when valid.
pub(super) fn network_flags_policy_error(
    runflags: &super::runflags::RunFlags,
    engine: EngineKind,
    rootfs_ref: Option<&str>,
) -> Option<i32> {
    let vz_container = engine == EngineKind::Vz && rootfs_ref.is_some();
    // `--network`/`--network-alias`/`--dns` stay vz+rootfs-only.
    let vz_only_flag = runflags.network.is_some()
        || !runflags.network_alias.is_empty()
        || !runflags.dns.is_empty();
    if vz_only_flag && !vz_container {
        eprintln!(
            "lightr: --network/--network-alias/--dns require --engine vz \
             --rootfs <img> (the native engine shares the host network — no per-container \
             netns or rootfs)"
        );
        return Some(2);
    }
    // `--add-host` is honored on vz (guest /etc/hosts) AND on the ns engine (the
    // container rootfs /etc/hosts written in PID 1). It is rejected only where there
    // is no container rootfs to write into — native (and vz without a rootfs).
    if !runflags.add_host.is_empty() && !vz_container && engine != EngineKind::Ns {
        eprintln!(
            "lightr: --add-host requires --engine ns or --engine vz --rootfs <img> \
             (it writes the container's /etc/hosts; the native engine has no container \
             rootfs to write)"
        );
        return Some(2);
    }
    None
}

/// WP-RUNFLAGS: claim `--name` for a just-spawned detached run, then print its id
/// (the success line). On a duplicate name the run is rolled back (removed) and an
/// honest exit 1 returned — Docker refuses a duplicate name. `None` ⇒ no claim,
/// just print the id (byte-identical to before). Used by every detached path.
///
/// FIX #77: print the BARE id (Docker's `docker run -d` prints the container id
/// alone on stdout). The old `id=<id>` prefix broke `$(docker run -d …)` capture
/// and tool parity; the bare id is now the only success line.
pub(crate) fn claim_name_and_print(handle: &lightr_run::RunHandle, name: Option<&str>) -> i32 {
    if let Some(name) = name {
        let home = crate::lightr_home();
        if let Err(e) = lightr_run::claim(&home, name, &handle.id) {
            // Roll back the run we just spawned so a failed name claim leaves no
            // orphan (Docker creates nothing on a duplicate name). Best-effort.
            let _ = lightr_run::remove_run(&home, &handle.id, true);
            return die_lightr(&e);
        }
    }
    println!("{}", handle.id);
    0
}

/// WP-B2: build the `-P/--publish-all` PortMaps for a rootfs image — auto-publish
/// every TCP port the image EXPOSEs. Hydrates the `rootfs_ref` into a temp dir to
/// read its `.lightr-image.json` config sidecar (the EXPOSE list lives there),
/// then lowers it through `synth_publish_all`. A ref that cannot be hydrated, or
/// an image with no EXPOSE, yields an empty list — `-P` then contributes nothing
/// (honest no-op, never an error: Docker's `-P` on an image with no EXPOSE is a
/// no-op too). Each synthesized map binds the default interface (`0.0.0.0`).
pub(crate) fn expose_port_maps(rootfs_ref: &str, store: &Store) -> Vec<PortMap> {
    let tmp = match tempfile::TempDir::new() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    if lightr_index::hydrate(tmp.path(), store, rootfs_ref).is_err() {
        return Vec::new();
    }
    let cfg = lightr_build::ImageConfig::load(tmp.path());
    super::flags::publish::synth_publish_all(&cfg.expose)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_all_requires_detach() {
        // -P without -d ⇒ honest exit 2 (never a silent drop).
        assert_eq!(
            publish_all_policy_error(false, EngineKind::Vz, Some("img")),
            Some(2)
        );
    }

    #[test]
    fn publish_all_requires_vz_rootfs() {
        // -P -d but native (no rootfs image / no EXPOSE source) ⇒ exit 2.
        assert_eq!(
            publish_all_policy_error(true, EngineKind::Native, None),
            Some(2)
        );
        // -P -d --engine vz but NO rootfs ⇒ exit 2.
        assert_eq!(
            publish_all_policy_error(true, EngineKind::Vz, None),
            Some(2)
        );
    }

    #[test]
    fn publish_all_valid_on_detached_vz_container() {
        // -P -d --engine vz --rootfs img ⇒ valid (None = no error).
        assert_eq!(
            publish_all_policy_error(true, EngineKind::Vz, Some("img")),
            None
        );
    }
}
