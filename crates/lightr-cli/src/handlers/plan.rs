//! `lightr plan` handler — dry-run planning operations.

use lightr_index::{scan, Index};
use lightr_run::{predict, Mount, RunSpec};
use lightr_store::Store;

use crate::exit::die_lightr;
use crate::{lightr_home, PlanCmd};

pub fn run(subcmd: PlanCmd) -> i32 {
    match subcmd {
        PlanCmd::Snapshot { dir, name: _ } => plan_snapshot(&dir),
        PlanCmd::Hydrate { dest, name } => plan_hydrate(&dest, &name),
        PlanCmd::Run {
            dir,
            input,
            env,
            mount,
            command,
        } => plan_run(&dir, &input, &env, &mount, &command),
    }
}

fn plan_snapshot(dir: &str) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let dir_path = std::path::Path::new(dir);
    let mut index = match Index::load_for(dir_path) {
        Ok(i) => i,
        Err(e) => return die_lightr(&e),
    };

    let walk = match scan(dir_path, &mut index) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    let manifest = &walk.manifest;

    // Count files and total size
    let files = manifest
        .entries
        .iter()
        .filter(|e| matches!(e, lightr_core::Entry::File { .. }))
        .count() as u64;
    let bytes = manifest.total_size;

    // Count new objects: those not already in the store
    let new_objects = manifest
        .entries
        .iter()
        .filter(|e| {
            if let lightr_core::Entry::File { digest, .. } = e {
                !store.exists(digest)
            } else {
                false
            }
        })
        .count() as u64;

    println!("files={files} bytes={bytes} new-objects={new_objects}");
    0
}

fn plan_hydrate(dest: &str, name: &str) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let rec = match store.ref_get(name) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("lightr: ref not found: {name}");
            return 2;
        }
        Err(e) => return die_lightr(&e),
    };

    let manifest_bytes = match store.get_bytes(&rec.root) {
        Ok(b) => b,
        Err(e) => return die_lightr(&e),
    };

    let manifest = match lightr_core::Manifest::decode(&manifest_bytes) {
        Ok(m) => m,
        Err(e) => return die_lightr(&e),
    };

    let files = manifest
        .entries
        .iter()
        .filter(|e| matches!(e, lightr_core::Entry::File { .. }))
        .count() as u64;
    let bytes = manifest.total_size;

    println!("files={files} bytes={bytes} dest={dest}");
    0
}

fn plan_run(
    dir: &str,
    inputs: &[String],
    env_keys: &[String],
    mounts_raw: &[String],
    command: &[String],
) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // Parse mounts (same logic as run handler)
    let mut mounts: Vec<Mount> = Vec::new();
    for raw in mounts_raw {
        let colon = match raw.find(':') {
            Some(pos) => pos,
            None => {
                eprintln!("lightr: invalid --mount value (missing ':'): {raw}");
                return 2;
            }
        };
        let ref_name = &raw[..colon];
        let target = &raw[colon + 1..];

        if let Err(e) = lightr_core::validate_ref_name(ref_name) {
            eprintln!("lightr: invalid mount ref name: {e}");
            return 2;
        }

        if target.starts_with('/') {
            eprintln!("lightr: mount target must be relative, got: {target}");
            return 2;
        }

        mounts.push(Mount {
            ref_name: ref_name.to_string(),
            target: target.to_string(),
        });
    }

    let _ = lightr_home(); // used for consistency; store is already opened

    let cwd = std::path::PathBuf::from(dir);
    let input_paths: Vec<std::path::PathBuf> = if inputs.is_empty() {
        vec![cwd.clone()]
    } else {
        inputs.iter().map(std::path::PathBuf::from).collect()
    };

    let spec = RunSpec {
        cwd,
        inputs: input_paths,
        command: command.to_vec(),
        env_keys: env_keys.to_vec(),
        mounts,
        secrets: vec![],
        configs: vec![],
        ports: vec![],
        // WP-RC-1: `plan` doesn't take `-e`/`--env-file` yet → no explicit env.
        env_explicit: vec![],
        // WP-RC-WORKDIR: `plan` doesn't take `-w` yet, and workdir is RUNTIME (not
        // a memo-key input) → `None` (key/prediction unchanged).
        workdir: None,
        // WP-RC-USER (NON-OWNED site, set None): `plan` doesn't take `-u`, and
        // user is RUNTIME (not a memo-key input) → `None` (prediction unchanged).
        user: None,
        // WP-RC-RESTART (NON-OWNED site, set None): `plan` doesn't take `--restart`,
        // and restart is RUNTIME (not a memo-key input) → `None` (prediction unchanged).
        restart: None,
        // WP-RC-STOPSIGNAL (NON-OWNED site, set None): `plan` doesn't take
        // `--stop-signal`, and it is RUNTIME (not keyed) → `None` (prediction unchanged).
        stop_signal: None,
    };

    match predict(&spec, &store) {
        Ok((key, hit)) => {
            let hex = key.to_hex();
            let short = &hex[..16];
            let predict_str = if hit { "HIT" } else { "MISS" };
            println!("key={short} predict={predict_str}");
            0
        }
        Err(e) => die_lightr(&e),
    }
}
