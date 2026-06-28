//! WP-RUNFLAGS / WP-NET3 — parsing + lowering for the core docker `run` flags
//! wired here: `-v/--volume`, `--tmpfs`, `--name`, `--rm`, `--entrypoint`, and
//! the vz container-networking flags (`--network`/`--network-alias`/`--add-host`/
//! `--dns`).
//!
//! The raw clap values arrive bundled in [`RawRunFlags`]; [`RawRunFlags::resolve`]
//! validates + lowers them to a [`RunFlags`] the handler carries into `RunSpec`.
//! Fail-closed: a bad value prints to stderr + returns `Err(exit_code)` (mirrors
//! the other run-flag parsers). An all-default bundle resolves to all-default ⇒
//! the no-flag run is byte-identical to before.
//!
//! WP-NET3: the networking flags are now WIRED (off the honest Phase-2 stub).
//! `resolve` only does VALUE validation here — the engine/rootfs guardrails
//! (`--network`/`--add-host`/`--dns` require `--engine vz --rootfs <img>`; the
//! native engine shares the host network with no per-container netns) need the
//! engine + rootfs context, so they live in the handler ([`super::run`]), not
//! here. `--network` is a clap `Option<String>` so "single network per
//! container" is structurally enforced (a 2nd `--network` is last-wins at the
//! clap layer, exactly like docker).

use lightr_run::{parse_v, MountKind, VolumeBind};

/// The WP-RUNFLAGS run flags as RAW clap values, bundled to keep `run()`'s arity
/// flat. RUNTIME-ONLY — none of these enters the memo key.
#[derive(Clone, Debug, Default)]
pub struct RawRunFlags {
    pub volume: Vec<String>,
    pub tmpfs: Vec<String>,
    /// `--ulimit TYPE=SOFT[:HARD]` (raw clap strings; parsed in the handler via
    /// `parse_ulimits`, mirroring how `tmpfs` is parsed via `parse_tmpfs`).
    pub ulimit: Vec<String>,
    pub name: Option<String>,
    pub rm: bool,
    pub entrypoint: Option<String>,
    pub network: Option<String>,
    pub network_alias: Vec<String>,
    pub add_host: Vec<String>,
    pub dns: Vec<String>,
}

/// The resolved WP-RUNFLAGS config the handler lowers into `RunSpec`. `-v` parsed
/// to host binds, `--entrypoint` split to argv tokens; `--name`/`--rm`/`--tmpfs`
/// pass through. RUNTIME-ONLY.
#[derive(Clone, Debug, Default)]
pub struct RunFlags {
    pub volumes: Vec<VolumeBind>,
    pub tmpfs: Vec<String>,
    /// `--ulimit` raw strings, carried through (parsed in the handler via
    /// `parse_ulimits`, mirroring `tmpfs`).
    pub ulimit: Vec<String>,
    pub name: Option<String>,
    pub rm: bool,
    pub entrypoint: Option<Vec<String>>,
    /// WP-NET3: `--network <name>` — the vz user network this run joins. `None` ⇒
    /// the single-NAT-NIC vz path (byte-identical). The engine/rootfs guardrail
    /// lives in the handler; here it is only carried through (value-validated).
    pub network: Option<String>,
    /// WP-NET3: `--network-alias` — extra DNS names this member answers to.
    pub network_alias: Vec<String>,
    /// WP-NET3: `--add-host HOST:IP` — extra `/etc/hosts` entries, carried as raw
    /// `"host:ip"` strings (svz parses them to `(host, ip)` at the vz wiring site).
    pub add_host: Vec<String>,
    /// WP-NET3: `--dns` — resolver addresses for the guest's `/etc/resolv.conf`.
    pub dns: Vec<String>,
}

impl RawRunFlags {
    /// Validate + lower. Fail-closed: bad input ⇒ printed error + `Err(exit_code)`.
    ///
    /// WP-NET3: the networking flags (`--network`/`--network-alias`/`--add-host`/
    /// `--dns`) are now WIRED (off the honest Phase-2 stub) — they thread to
    /// `RunSpec` and the vz supervisor joins the per-network L2 switch. This fn
    /// VALUE-validates `--add-host` (each entry must be `HOST:IP`, so a typo is an
    /// honest exit 2 rather than a silently dropped host); the ENGINE/ROOTFS
    /// guardrail (these are vz-only — the native engine shares the host network
    /// with no per-container netns) needs the engine + rootfs context and so lives
    /// in the handler ([`super::run`]), not here.
    pub fn resolve(self) -> Result<RunFlags, i32> {
        // `--add-host HOST:IP`: validate the shape so a malformed entry is an
        // honest error here (svz drops un-parsable entries; NET3 owns surfacing
        // the parse error to the user). Carried as raw `"host:ip"` strings —
        // svz re-splits to `(host, ip)` at the wiring site.
        for raw in &self.add_host {
            match raw.split_once(':') {
                Some((h, ip)) if !h.is_empty() && !ip.is_empty() => {}
                _ => {
                    eprintln!("lightr: --add-host {raw}: expected HOST:IP");
                    return Err(2);
                }
            }
        }

        // `-v/--volume`: parse via the frozen `parse_v` grammar, then require a
        // HostBind (the docker `-v /host:/ctr` form WP-RUNFLAGS wires on native).
        // Named/anon volumes + CAS refs are the WP-VOL ring's job — an honest
        // exit 2 here, never a silent drop.
        let mut volumes: Vec<VolumeBind> = Vec::new();
        for raw in &self.volume {
            let spec = match parse_v(raw) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("lightr: {e}");
                    return Err(2);
                }
            };
            match spec.kind {
                MountKind::HostBind => {
                    let source = spec.source.unwrap_or_default();
                    volumes.push(VolumeBind {
                        source,
                        target: spec.target,
                        readonly: spec.readonly,
                    });
                }
                MountKind::NamedVolume | MountKind::AnonVolume => {
                    eprintln!(
                        "lightr: -v {raw}: named/anonymous volumes are Phase 2 (WP-VOL); use a \
                         host path SRC:DST[:ro] (a bind) on the native engine"
                    );
                    return Err(2);
                }
                MountKind::CasRef => {
                    eprintln!(
                        "lightr: -v {raw}: a CAS-ref mount uses `--mount @ref:target`, not `-v`"
                    );
                    return Err(2);
                }
                MountKind::Tmpfs => {
                    // parse_v never yields Tmpfs, but stay exhaustive + fail-closed.
                    eprintln!("lightr: -v {raw}: use `--tmpfs DST` for a tmpfs mount");
                    return Err(2);
                }
            }
        }

        // `--entrypoint`: docker's `--entrypoint` is a single executable (shell
        // splitting is the image's job); we take it as one argv token, prepended
        // to the CLI command. Empty string ⇒ honest exit 2 (docker rejects it too).
        let entrypoint = match self.entrypoint.as_deref() {
            None => None,
            Some(ep) if ep.trim().is_empty() => {
                eprintln!("lightr: --entrypoint must not be empty");
                return Err(2);
            }
            Some(ep) => Some(vec![ep.to_string()]),
        };

        Ok(RunFlags {
            volumes,
            tmpfs: self.tmpfs,
            ulimit: self.ulimit,
            name: self.name,
            rm: self.rm,
            entrypoint,
            // WP-NET3: carry the vz networking flags through to the handler, which
            // applies the engine/rootfs guardrail then threads them into RunSpec.
            network: self.network,
            network_alias: self.network_alias,
            add_host: self.add_host,
            dns: self.dns,
        })
    }
}

#[cfg(test)]
#[path = "runflags_tests.rs"]
mod tests;
