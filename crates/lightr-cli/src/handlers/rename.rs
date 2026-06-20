//! `lightr rename` handler — remap a container name (docker rename).
//!
//! Docker-faithful: resolve the target (name-or-id) → validate the new name →
//! claim the new name → release the old → rewrite spec.json's `name`. The
//! ordering (claim BEFORE release) is load-bearing: a failed claim ("name
//! already in use") leaves the OLD name untouched, so the registry never ends
//! up with the container nameless.
//!
//! `SpecOnDisk` is `pub(super)` inside `lightr-run` and not re-exported, so the
//! spec.json rewrite goes through `serde_json::Value` — every other field is
//! preserved verbatim and only `name` is remapped (and written back in the same
//! compact form `write_spec_json` uses).

use crate::{exit::die_lightr, lightr_home};

pub fn run(target: &str, new_name: &str) -> i32 {
    let home = lightr_home();

    // 1. resolve target → id. Docker: "No such container" on miss (exit 1).
    let id = match lightr_run::resolve(&home, target) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("Error: No such container: {target}");
            return 1;
        }
    };

    // 2. validate the new name. InvalidRef maps to exit 2 via die_lightr.
    if let Err(e) = lightr_run::name_validate(new_name) {
        return die_lightr(&e);
    }

    // 3. read spec.json → the OLD name lives in its `name` field. Parse as a
    //    generic Value so the unexported SpecOnDisk shape is preserved verbatim.
    let spec_path = home.join("run").join(&id).join("spec.json");
    let raw = match std::fs::read(&spec_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("lightr: cannot read spec for {id}: {e}");
            return 1;
        }
    };
    let mut spec: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lightr: corrupt spec for {id}: {e}");
            return 1;
        }
    };
    // OLD name: the spec's `name` field (Option<String> → may be null/absent).
    let old_name = spec
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // No-op rename (same name): docker treats this as success, and re-claiming
    // the already-held name would spuriously fail ("name already in use").
    if old_name.as_deref() == Some(new_name) {
        return 0;
    }

    // 4. claim the new name FIRST. If it's taken, bail WITHOUT touching the old
    //    name — registry stays consistent. Docker: "name already in use" (1).
    if let Err(_e) = lightr_run::claim(&home, new_name, &id) {
        eprintln!("Error: Conflict. The container name \"/{new_name}\" is already in use.");
        return 1;
    }

    // 5. claim succeeded → release the old name (idempotent; absent = Ok), then
    //    rewrite spec.json's `name`. If the rewrite fails, roll the claim back
    //    so we don't leave the new name pointing at a spec that still says old.
    if let Some(ref old) = old_name {
        if let Err(e) = lightr_run::release(&home, old) {
            // Release failed: undo the claim to keep the registry consistent.
            let _ = lightr_run::release(&home, new_name);
            return die_lightr(&e);
        }
    }

    spec["name"] = serde_json::Value::String(new_name.to_string());
    let bytes = match serde_json::to_vec(&spec) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("lightr: cannot serialize spec for {id}: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&spec_path, &bytes) {
        eprintln!("lightr: cannot write spec for {id}: {e}");
        return 1;
    }

    // 6. docker rename is silent on success.
    0
}
