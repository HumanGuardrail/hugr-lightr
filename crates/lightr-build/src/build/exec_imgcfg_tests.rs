//! WP-DF-IMGCFG record-side end-to-end tests: a built image's config
//! instructions (ENTRYPOINT/CMD/WORKDIR/USER/ENV/EXPOSE + STOPSIGNAL/VOLUME)
//! land in the `.lightr-image.json` sidecar, read back via `ImageConfig::load`
//! from the hydrated result. Split out of `exec_tests.rs` to keep each file
//! under the 400-line godfile cap.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so every
//! build/hydrate here holds the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while it runs. Readers still parallelize among
//! themselves; each test uses a nanos-unique temp work dir.
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
    // build() + hydrate READ the process-global LIGHTR_HOME; hold the crate-wide
    // shared read lock across both so a concurrent setter (exec_tests/up_tests)
    // cannot flip the home mid-op. Poison-tolerant; many readers coexist.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
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
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
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

// ---- WP-DF-HEALTHCHECK-ONBUILD --------------------------------------------

#[test]
fn records_healthcheck_cmd_with_opts() {
    // HEALTHCHECK [opts] CMD <shell-form> → OCI shape: test = ["CMD-SHELL", cmd],
    // opts kept as raw token text. Was the fail-closed "unsupported" path.
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-hc-cmd",
        "FROM scratch\n\
         HEALTHCHECK --interval=30s --timeout=5s --start-period=2s --retries=3 \
         CMD curl -f http://localhost/\n",
    );
    let hc = cfg.healthcheck.expect("HEALTHCHECK must be recorded");
    assert_eq!(
        hc.test,
        vec![
            "CMD-SHELL".to_string(),
            "curl -f http://localhost/".to_string()
        ],
        "shell-form CMD ⇒ CMD-SHELL test"
    );
    assert_eq!(hc.interval.as_deref(), Some("30s"));
    assert_eq!(hc.timeout.as_deref(), Some("5s"));
    assert_eq!(hc.start_period.as_deref(), Some("2s"));
    assert_eq!(hc.retries.as_deref(), Some("3"));
}

#[test]
fn records_healthcheck_cmd_exec_form() {
    // Exec-form CMD → test = ["CMD", <argv>...]; absent opts stay None.
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-hc-exec",
        "FROM scratch\nHEALTHCHECK CMD [\"/bin/check\", \"--fast\"]\n",
    );
    let hc = cfg.healthcheck.expect("HEALTHCHECK must be recorded");
    assert_eq!(
        hc.test,
        vec![
            "CMD".to_string(),
            "/bin/check".to_string(),
            "--fast".to_string()
        ]
    );
    assert!(hc.interval.is_none() && hc.retries.is_none());
}

#[test]
fn records_healthcheck_none_as_disabled() {
    // HEALTHCHECK NONE → test = ["NONE"] (explicitly disabled / drop inherited).
    let f = fix();
    let cfg = build_and_load(&f, "imgcfg-hc-none", "FROM scratch\nHEALTHCHECK NONE\n");
    let hc = cfg.healthcheck.expect("HEALTHCHECK NONE must be recorded");
    assert_eq!(hc.test, vec!["NONE".to_string()], "NONE ⇒ disabled marker");
}

#[test]
fn records_onbuild_triggers_verbatim() {
    // ONBUILD triggers are recorded VERBATIM (keyword stripped), in order, and
    // accumulate. Trigger-execution on derived builds is a flagged follow-up.
    let f = fix();
    let cfg = build_and_load(
        &f,
        "imgcfg-onbuild",
        "FROM scratch\n\
         ONBUILD COPY . /app\n\
         ONBUILD RUN make build\n",
    );
    assert_eq!(
        cfg.onbuild,
        vec!["COPY . /app".to_string(), "RUN make build".to_string()],
        "ONBUILD triggers recorded verbatim, in order"
    );
}

#[test]
fn healthcheck_and_onbuild_no_longer_unsupported() {
    // Before this WP, HEALTHCHECK/ONBUILD routed to the "unsupported instruction"
    // error — a Dockerfile using them failed to build. Now they record + build.
    let f = fix();
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(
        &df_path,
        "FROM scratch\nHEALTHCHECK NONE\nONBUILD RUN echo deferred\n",
    )
    .unwrap();
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let report = build(
        &f.ctx_path,
        &df_path,
        "imgcfg-hc-ob-noerr",
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    );
    assert!(
        report.is_ok(),
        "HEALTHCHECK/ONBUILD must build (not 'unsupported'): {:?}",
        report.err()
    );
}

#[test]
fn config_less_image_has_no_healthcheck_or_onbuild() {
    // Behaviour-preserved: a Dockerfile WITHOUT HEALTHCHECK/ONBUILD records
    // neither (None / empty) — builds identically to before this WP.
    let f = fix();
    let cfg = build_and_load(&f, "imgcfg-no-hc-ob", "FROM scratch\nENV ONLY=env\n");
    assert!(cfg.healthcheck.is_none(), "no HEALTHCHECK ⇒ None");
    assert!(cfg.onbuild.is_empty(), "no ONBUILD ⇒ empty");
}
