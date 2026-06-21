//! WP-RUNFLAGS — parsing + lowering for the core docker `run` flags wired by
//! this WP: `-v/--volume`, `--tmpfs`, `--name`, `--rm`, `--entrypoint`, and the
//! honest Phase-2 networking flags (`--network`/`--network-alias`/`--add-host`/
//! `--dns`).
//!
//! The raw clap values arrive bundled in [`RawRunFlags`]; [`RawRunFlags::resolve`]
//! validates + lowers them to a [`RunFlags`] the handler carries into `RunSpec`.
//! Fail-closed: a bad value prints to stderr + returns `Err(exit_code)` (mirrors
//! the other run-flag parsers). An all-default bundle resolves to all-default ⇒
//! the no-flag run is byte-identical to before.

use lightr_run::{parse_v, MountKind, VolumeBind};

/// The WP-RUNFLAGS run flags as RAW clap values, bundled to keep `run()`'s arity
/// flat. RUNTIME-ONLY — none of these enters the memo key.
#[derive(Clone, Debug, Default)]
pub struct RawRunFlags {
    pub volume: Vec<String>,
    pub tmpfs: Vec<String>,
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
    pub name: Option<String>,
    pub rm: bool,
    pub entrypoint: Option<Vec<String>>,
}

impl RawRunFlags {
    /// Validate + lower. Fail-closed: bad input ⇒ printed error + `Err(exit_code)`.
    ///
    /// The networking flags (`--network`/`--network-alias`/`--add-host`/`--dns`)
    /// are NOT applicable on the native engine (no per-container network
    /// namespace) — they get a SPECIFIC honest error here (exit 2), distinct from
    /// the generic WP-RUNFLAGS stub. `--add-host` could in principle write the run
    /// rootfs's `/etc/hosts`, but the native engine has NO rootfs (it shares the
    /// host fs/cwd), so it too is honest Phase-2. The vz engine is where these
    /// gain teeth (consistent with the DNS-in-compose Phase-2 boundary).
    pub fn resolve(self) -> Result<RunFlags, i32> {
        if let Some(net) = self.network.as_deref() {
            eprintln!(
                "lightr: --network ({net}) is wired for the vz engine / Phase 2; native runs \
                 share the host network (no per-container netns)"
            );
            return Err(2);
        }
        if !self.network_alias.is_empty() {
            eprintln!(
                "lightr: --network-alias is wired for the vz engine / Phase 2; native runs \
                 share the host network (no per-container netns)"
            );
            return Err(2);
        }
        if !self.add_host.is_empty() {
            eprintln!(
                "lightr: --add-host is wired for the vz engine / Phase 2; the native engine has \
                 no container rootfs /etc/hosts to write (it shares the host fs)"
            );
            return Err(2);
        }
        if !self.dns.is_empty() {
            eprintln!(
                "lightr: --dns is wired for the vz engine / Phase 2; native runs share the host \
                 resolver (no per-container netns)"
            );
            return Err(2);
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
            name: self.name,
            rm: self.rm,
            entrypoint,
        })
    }
}

#[cfg(test)]
#[path = "runflags_tests.rs"]
mod tests;
