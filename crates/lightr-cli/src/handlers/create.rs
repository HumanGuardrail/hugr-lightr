//! `lightr create` handler — prepare a container WITHOUT starting it (docker create).
//!
//! Docker `create` materializes a container in the "Created" state: its config is
//! persisted but no process runs. Lightr's daemonless analog: write the run dir +
//! `spec.json` (the prepared state) WITHOUT launching the supervisor. The run then
//! has a dir + spec.json, NO `status` file, NO live control endpoint — exactly what
//! `ps` reports as not-running and what `lightr start <id>` later launches in place
//! (`respawn_run` re-reads the same spec.json).
//!
//! Scope (HONEST): `create` prepares the NATIVE container path — the same
//! spec-construction the native detached `run -d` uses, minus the supervisor
//! launch. The engine/vz + `--rootfs` + `-p/--publish` paths boot a microVM / wire
//! a NAT NIC at SPAWN time, not at prepare time, so they have no meaningful
//! "Created" state to stop at; using them with `create` is an honest exit 2 (never
//! a silent drop), pointing the user at `lightr run`. The wired native run flags
//! (`--name`/`-w`/`-u`/`--entrypoint`/`-e`/`--env-file`/`-v`/`--tmpfs`/`--restart`/
//! `--stop-signal`/`-l`/the 11 RC flags) all persist into spec.json so the later
//! `start` honors them exactly as a directly-spawned run would.

use lightr_engine::EngineKind;
use lightr_run::{create_run_prepared, RunSpec};
use lightr_store::Store;

use crate::cli::cmd::RunArgs;
use crate::exit::die_lightr;
use crate::handlers::run::{HealthFlags, RawRcFlags, RawRunFlags};
use crate::lightr_home;

pub fn run(a: RunArgs, json: bool) -> i32 {
    // ── Honest scope guards (fail-closed: never silently drop a flag) ──────────
    // `create` prepares the NATIVE path only. The engine/vz/rootfs/publish paths
    // provision live resources (a microVM, a NAT NIC) at SPAWN, so there is no
    // "Created" state to stop at — direct the user at `lightr run` instead.
    let engine_kind = match a.engine.parse::<EngineKind>() {
        Ok(k) => k,
        Err(e) => return die_lightr(&e),
    };
    if engine_kind != EngineKind::Native {
        eprintln!(
            "lightr: create prepares the native container path only; \
             `--engine {}` boots its backend at spawn — use `lightr run` instead",
            a.engine
        );
        return 2;
    }
    if a.rootfs.is_some() {
        eprintln!(
            "lightr: create does not support --rootfs (a hydrated rootfs is provisioned \
             at spawn, not prepare) — use `lightr run`"
        );
        return 2;
    }
    if !a.publish.is_empty() || a.publish_all {
        eprintln!(
            "lightr: create does not support -p/-P (port publishing is wired by the \
             supervisor at spawn) — use `lightr run -d`"
        );
        return 2;
    }

    // ── Validate the policies `run` validates up-front (fail-closed exit 2) ─────
    if let Some(ref p) = a.restart {
        if let Err(e) = lightr_run::restart::RestartPolicy::parse(p) {
            eprintln!("lightr: {e}");
            return 2;
        }
    }
    if let Some(ref s) = a.stop_signal {
        if crate::handlers::kill::parse_signal(s).is_none() {
            eprintln!("lightr: invalid signal: {s}");
            return 2;
        }
    }

    // ── Resolve the wired run/rc flag bundles (same parsers as `run`) ──────────
    let rc = match (RawRcFlags {
        hostname: a.hostname,
        label: a.label,
        cap_add: a.cap_add,
        cap_drop: a.cap_drop,
        privileged: a.privileged,
        tty: a.tty,
        init: a.init,
        read_only: a.read_only,
        oom_score_adj: a.oom_score_adj,
        pids_limit: a.pids_limit,
        shm_size: a.shm_size,
        apparmor: a.apparmor,
        seccomp: a.seccomp,
    })
    .resolve()
    {
        Ok(c) => c,
        Err(code) => return code,
    };

    // WP-#106: AppArmor (aa_change_onexec) is an `ns`-engine feature; `create` is
    // native-only (the supervisor it later starts is a host process). Honest-error
    // (exit 2) rather than silently record a security flag that won't be enforced.
    if rc.apparmor.is_some() {
        eprintln!(
            "lightr: --apparmor is enforced only on the rootless ns engine \
             (`lightr run --engine ns --apparmor <profile>`); create is native-only \
             — refusing to run rather than give false security"
        );
        return 2;
    }

    // WP-#108: seccomp (cBPF filter install) is an `ns`-engine feature; `create` is
    // native-only (the supervisor it later starts is a host process). Honest-error
    // (exit 2) rather than silently record a security flag that won't be enforced.
    if rc.seccomp.is_some() {
        eprintln!(
            "lightr: --seccomp is enforced only on the rootless ns engine \
             (`lightr run --engine ns --seccomp <path>`); create is native-only \
             — refusing to run rather than give false security"
        );
        return 2;
    }

    let runflags = match (RawRunFlags {
        volume: a.volume,
        tmpfs: a.tmpfs,
        // --ulimit: carried through; create has no native exec yet (recorded slot).
        ulimit: a.ulimit,
        name: a.name,
        rm: a.rm,
        entrypoint: a.entrypoint,
        network: a.network,
        network_alias: a.network_alias,
        add_host: a.add_host,
        dns: a.dns,
    })
    .resolve()
    {
        Ok(f) => f,
        Err(code) => return code,
    };

    // The native engine shares the host network — the vz networking flags have no
    // per-container netns to apply to. Mirror `run`'s honest exit 2 (never silent).
    if runflags.network.is_some()
        || !runflags.network_alias.is_empty()
        || !runflags.add_host.is_empty()
        || !runflags.dns.is_empty()
    {
        eprintln!(
            "lightr: --network/--network-alias/--add-host/--dns require \
             `--engine vz --rootfs <img>` (not available on create's native path)"
        );
        return 2;
    }

    // ── Resource caps + env (same lowering `run` performs) ─────────────────────
    // WP-#90: fold in `--pids-limit`. `create` is native-only (guarded above), and
    // the native supervisor records-but-does-not-enforce pids (cgroup-only); the
    // `pids_limit` carry-field persists to spec.json + `inspect` for honesty.
    let limits =
        match lightr_core::ResourceLimits::parse(a.memory.as_deref(), a.cpus.as_deref()) {
            Ok(l) => l.with_pids(a.pids_limit),
            Err(e) => return die_lightr(&e),
        };
    let env_explicit = match resolve_env_explicit(&a.env_set, a.env_file.as_deref()) {
        Ok(pairs) => pairs,
        Err(code) => return code,
    };

    // ── Healthcheck (persisted for the supervisor the later `start` launches) ──
    let health = HealthFlags {
        cmd: a.health_cmd,
        interval: a.health_interval,
        timeout: a.health_timeout,
        start_period: a.health_start_period,
        retries: a.health_retries,
        no_healthcheck: a.no_healthcheck,
    };
    let healthcheck = health.build();

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let spec = RunSpec {
        cwd: std::path::PathBuf::from(a.dir),
        command: a.command,
        env_keys: a.env,
        env_explicit,
        limits,
        workdir: a.workdir,
        user: a.user,
        restart: a.restart,
        stop_signal: a.stop_signal,
        hostname: rc.hostname,
        labels: rc.labels,
        cap_add: rc.cap_add,
        cap_drop: rc.cap_drop,
        privileged: rc.privileged,
        tty: rc.tty,
        init: rc.init,
        read_only: rc.read_only,
        oom_score_adj: rc.oom_score_adj,
        pids_limit: rc.pids_limit,
        shm_size: rc.shm_size,
        volumes: runflags.volumes,
        tmpfs: runflags.tmpfs,
        entrypoint: runflags.entrypoint,
        name: runflags.name.clone(),
        rm: runflags.rm,
        ..Default::default()
    };

    // Prepare the run dir + spec.json WITHOUT a supervisor (the "Created" state).
    let handle = match create_run_prepared(
        &spec,
        &store,
        healthcheck.as_ref(),
        EngineKind::Native,
        None,
        &[],
    ) {
        Ok(h) => h,
        Err(e) => return die_lightr(&e),
    };

    // Claim the name AFTER prepare (Docker refuses a duplicate name; roll back the
    // prepared dir on a clash so create leaves no orphan).
    if let Some(name) = runflags.name.as_deref() {
        let home = lightr_home();
        if let Err(e) = lightr_run::claim(&home, name, &handle.id) {
            let _ = lightr_run::remove_run(&home, &handle.id, true);
            return die_lightr(&e);
        }
    }

    if json {
        println!("{{\"id\":\"{}\",\"status\":\"created\"}}", handle.id);
    } else {
        // Docker `create` prints the full container id on its own line.
        println!("{}", handle.id);
    }
    0
}

/// Resolve `-e KEY=VAL` + `--env-file` into explicit `(KEY, VALUE)` pairs (the
/// KEYED env that enters the run key). File first, then `-e` overrides; a bare
/// `KEY` (no `=`) inherits the value from the current process env. Mirrors the
/// `run` handler's `env::resolve_env_explicit_from_process` contract; kept here
/// (a small, self-contained transcription) so `create` does not reach into the
/// `run` module's `pub(super)` internals. Fail-closed: a missing env-file ⇒
/// honest exit 1 (never a silent skip).
fn resolve_env_explicit(
    env_set: &[String],
    env_file: Option<&str>,
) -> Result<Vec<(String, String)>, i32> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    // Append one `KEY=VAL` (or bare `KEY`) entry, last-write-wins on the key.
    fn push(raw: &str, pairs: &mut Vec<(String, String)>) {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            return;
        }
        let (key, val) = match raw.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            // Bare KEY ⇒ inherit from the process env (docker `-e KEY`).
            None => (raw.to_string(), std::env::var(raw).unwrap_or_default()),
        };
        // Last write wins (file then -e): drop any prior entry for this key.
        pairs.retain(|(k, _)| k != &key);
        pairs.push((key, val));
    }

    if let Some(path) = env_file {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lightr: --env-file {path}: {e}");
                return Err(1);
            }
        };
        for line in contents.lines() {
            push(line, &mut pairs);
        }
    }
    for raw in env_set {
        push(raw, &mut pairs);
    }
    Ok(pairs)
}
