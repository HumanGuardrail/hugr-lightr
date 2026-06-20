//! WP-DF-IMGCFG record-side end-to-end tests: a built image's config
//! instructions (ENTRYPOINT/CMD/WORKDIR/USER/ENV/EXPOSE + STOPSIGNAL/VOLUME)
//! land in the `.lightr-image.json` sidecar, read back via `ImageConfig::load`
//! from the hydrated result. Split out of `exec_tests.rs` to keep each file
//! under the 400-line godfile cap.
//!
//! Parallel-safe by construction: each test owns its tempdirs + store and never
//! mutates process-global state — `build()` takes the store explicitly and uses
//! a nanos-unique temp work dir.
use super::*;
use crate::build::imgcfg::ImageConfig;
use tempfile::TempDir;

struct Fix {
    _ctx: TempDir,
    store_tmp: TempDir,
    store: Store,
    ctx_path: std::path::PathBuf,
}

fn fix() -> Fix {
    let _ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let ctx_path = _ctx.path().to_path_buf();
    Fix {
        _ctx,
        store_tmp,
        store,
        ctx_path,
    }
}

/// Build `df_body` under `name`, then hydrate the result + load its config.
fn build_and_load(f: &Fix, name: &str, df_body: &str) -> ImageConfig {
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, df_body).unwrap();
    build(
        &f.ctx_path,
        &df_path,
        name,
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    )
    .unwrap();
    let dest = f.store_tmp.path().join(format!("hydrated-{name}"));
    lightr_index::hydrate(&dest, &f.store, name).unwrap();
    ImageConfig::load(&dest)
}

#[test]
fn records_entrypoint_cmd_workdir_user_env_expose() {
    // The full vertical's record half: every config instruction must persist into
    // the image config sidecar (one Dockerfile, all fields asserted at once).
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-all",
        "FROM scratch\n\
         ENV APP_ENV=prod LANG=C\n\
         WORKDIR /srv/app\n\
         USER appuser:appgrp\n\
         EXPOSE 8080 9090/udp\n\
         ENTRYPOINT [\"/bin/tini\", \"--\"]\n\
         CMD [\"server\", \"--port\", \"8080\"]\n",
    );

    assert_eq!(
        cfg.entrypoint.as_deref(),
        Some(["/bin/tini".to_string(), "--".to_string()].as_slice()),
        "ENTRYPOINT must be recorded"
    );
    assert_eq!(
        cfg.cmd.as_deref(),
        Some(
            [
                "server".to_string(),
                "--port".to_string(),
                "8080".to_string()
            ]
            .as_slice()
        ),
        "CMD must be recorded"
    );
    assert_eq!(
        cfg.workdir.as_deref(),
        Some("/srv/app"),
        "WORKDIR must be recorded"
    );
    assert_eq!(
        cfg.user.as_deref(),
        Some("appuser:appgrp"),
        "USER must be recorded"
    );
    assert!(
        cfg.env
            .contains(&("APP_ENV".to_string(), "prod".to_string()))
            && cfg.env.contains(&("LANG".to_string(), "C".to_string())),
        "ENV pairs must be recorded: {:?}",
        cfg.env
    );
    assert_eq!(
        cfg.expose,
        vec!["8080".to_string(), "9090/udp".to_string()],
        "EXPOSE specs must be recorded verbatim (metadata)"
    );
}

#[test]
fn records_stopsignal_and_volume() {
    // STOPSIGNAL + VOLUME are cheap metadata records (consumed by stop / future
    // volume wiring): assert they persist too. Multiple VOLUME lines accumulate.
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-stop-vol",
        "FROM scratch\n\
         STOPSIGNAL SIGTERM\n\
         VOLUME /data\n\
         VOLUME /var/log /cache\n",
    );
    assert_eq!(cfg.stop_signal.as_deref(), Some("SIGTERM"));
    assert_eq!(
        cfg.volume,
        vec![
            "/data".to_string(),
            "/var/log".to_string(),
            "/cache".to_string()
        ],
        "VOLUME paths accumulate across instructions"
    );
}

#[test]
fn entrypoint_interpolates_build_vars() {
    // ENTRYPOINT tokens interpolate against the build scope (Docker), mirroring CMD.
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-ep-interp",
        "FROM scratch\nENV BIN=/opt/app\nENTRYPOINT [\"${BIN}/run\"]\n",
    );
    assert_eq!(
        cfg.entrypoint.as_deref(),
        Some(["/opt/app/run".to_string()].as_slice()),
        "ENTRYPOINT must interpolate ${{BIN}}"
    );
}

#[test]
fn config_less_image_has_default_config() {
    // Behaviour-preserved: an image with NO config instructions loads the default
    // ImageConfig (no entrypoint/cmd/workdir/user), so `run` behaves as before.
    let f = fix();
    let cfg = build_and_load(&f, "imgcfg-none", "FROM scratch\nENV ONLY=env\n");
    assert!(cfg.entrypoint.is_none(), "no ENTRYPOINT ⇒ None");
    assert!(cfg.cmd.is_none(), "no CMD ⇒ None");
    assert!(cfg.workdir.is_none(), "no WORKDIR ⇒ None");
    assert!(cfg.user.is_none(), "no USER ⇒ None");
    assert!(cfg.expose.is_empty(), "no EXPOSE ⇒ empty");
    // ENV still round-trips (the historically-recorded subset is preserved).
    assert_eq!(cfg.env, vec![("ONLY".to_string(), "env".to_string())]);
}

#[test]
fn build_with_config_instructions_does_not_error() {
    // Before this WP, ENTRYPOINT/USER/EXPOSE/STOPSIGNAL/VOLUME routed to the
    // "unsupported instruction" error — a Dockerfile using them failed to build.
    // Now they record + build cleanly. (The other tests assert the recorded
    // values; this pins the no-longer-unsupported behaviour explicitly.)
    let f = fix();
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(
        &df_path,
        "FROM scratch\nENTRYPOINT [\"/x\"]\nUSER u\nEXPOSE 1\nSTOPSIGNAL SIGINT\nVOLUME /v\n",
    )
    .unwrap();
    let report = build(
        &f.ctx_path,
        &df_path,
        "imgcfg-noerr",
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    );
    assert!(
        report.is_ok(),
        "config instructions must build (not 'unsupported'): {:?}",
        report.err()
    );
}
