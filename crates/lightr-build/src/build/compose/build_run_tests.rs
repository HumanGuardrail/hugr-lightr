//! WP-E: tests for the up-path build step (`build_service_image`).
//!
//! Each builds a tiny `FROM scratch` image from an inline Dockerfile in its own
//! tempdir, then HYDRATES the produced store ref to prove the built filesystem
//! is reachable under exactly the ref the supervisor will hydrate. Parallel-safe:
//! own tempdir Store per test, absolute contexts, no process-global state.
use super::*;
use crate::build::compose::build_spec::ServiceBuild;
use crate::build::compose::model::empty_service;
use lightr_store::Store;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

static UNIQ: AtomicU64 = AtomicU64::new(0);

/// A unique service name so derived refs (`<project>_<service>`) never collide
/// across parallel tests sharing nothing else.
fn uniq_name(tag: &str) -> String {
    let n = UNIQ.fetch_add(1, Ordering::Relaxed);
    format!("{tag}{n}")
}

/// Stage a context dir with a Dockerfile and a file to COPY, returning the dir.
fn ctx_with_dockerfile(df: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hi-from-build").unwrap();
    std::fs::write(dir.path().join("Dockerfile"), df).unwrap();
    dir
}

/// A `Service` with a lowered `build:` over an absolute context dir.
fn svc_with_build(name: &str, image: Option<&str>, build: ServiceBuild) -> Service {
    let mut s = empty_service(name.to_string());
    if let Some(img) = image {
        s.image_ref = img.to_string();
    }
    s.build = Some(build);
    s
}

fn simple_build(ctx: &Path) -> ServiceBuild {
    ServiceBuild {
        context: ctx.to_string_lossy().into_owned(),
        dockerfile: "Dockerfile".to_string(),
        args: Vec::new(),
        target: None,
    }
}

#[test]
fn build_only_service_derives_project_service_ref_and_hydrates() {
    let dir = ctx_with_dockerfile("FROM scratch\nCOPY hello.txt /hello.txt\n");
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let name = uniq_name("app");

    let svc = svc_with_build(&name, None, simple_build(dir.path()));
    let resolved = build_service_image(&svc, &store, "proj").unwrap();

    assert_eq!(resolved, format!("proj_{name}"), "derived ref");
    // The ref hydrates the built filesystem (proves it is the snapshot ref).
    let dest = store_tmp.path().join("hyd");
    lightr_index::hydrate(&dest, &store, &resolved).unwrap();
    let got = std::fs::read(dest.join("hello.txt")).unwrap();
    assert_eq!(got, b"hi-from-build");
}

#[test]
fn build_plus_image_tags_under_image_ref() {
    // Compose "build then tag": the image: value is the ref the build registers.
    let dir = ctx_with_dockerfile("FROM scratch\nCOPY hello.txt /hello.txt\n");
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let name = uniq_name("svc");
    let img = uniq_name("myimg");

    let svc = svc_with_build(&name, Some(&img), simple_build(dir.path()));
    let resolved = build_service_image(&svc, &store, "proj").unwrap();

    assert_eq!(
        resolved, img,
        "built image is tagged under the `image:` ref"
    );
    let dest = store_tmp.path().join("hyd");
    lightr_index::hydrate(&dest, &store, &img).unwrap();
    assert!(dest.join("hello.txt").exists());
}

#[test]
fn build_args_reach_the_build() {
    // ARG substituted into a COPY destination proves the build-arg flowed through
    // `build_target`. The file lands at the arg-named path.
    let df = "FROM scratch\nARG DEST\nCOPY hello.txt /${DEST}\n";
    let dir = ctx_with_dockerfile(df);
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let name = uniq_name("argsvc");

    let build = ServiceBuild {
        context: dir.path().to_string_lossy().into_owned(),
        dockerfile: "Dockerfile".to_string(),
        args: vec![("DEST".to_string(), "out.txt".to_string())],
        target: None,
    };
    let svc = svc_with_build(&name, None, build);
    let resolved = build_service_image(&svc, &store, "proj").unwrap();

    let dest = store_tmp.path().join("hyd");
    lightr_index::hydrate(&dest, &store, &resolved).unwrap();
    assert!(
        dest.join("out.txt").exists(),
        "ARG-driven COPY dest proves build-args reached the build"
    );
}

#[test]
fn target_selects_the_named_stage() {
    // Two stages; target the FIRST. Its output has `early.txt`; the final stage's
    // `late.txt` must NOT be present (the build stopped at the target).
    std::fs::create_dir_all(std::env::temp_dir()).ok();
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let df = "FROM scratch AS early\nCOPY a.txt /early.txt\nFROM scratch AS final\nCOPY b.txt /late.txt\n";
    std::fs::write(dir.path().join("Dockerfile"), df).unwrap();
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let name = uniq_name("tgt");

    let build = ServiceBuild {
        context: dir.path().to_string_lossy().into_owned(),
        dockerfile: "Dockerfile".to_string(),
        args: Vec::new(),
        target: Some("early".to_string()),
    };
    let svc = svc_with_build(&name, None, build);
    let resolved = build_service_image(&svc, &store, "proj").unwrap();

    let dest = store_tmp.path().join("hyd");
    lightr_index::hydrate(&dest, &store, &resolved).unwrap();
    assert!(
        dest.join("early.txt").exists(),
        "target stage output present"
    );
    assert!(
        !dest.join("late.txt").exists(),
        "final-stage output absent when targeting an earlier stage"
    );
}

#[test]
fn dockerfile_path_is_relative_to_context() {
    // A non-default dockerfile name inside the context resolves correctly.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"x").unwrap();
    std::fs::write(
        dir.path().join("Build.df"),
        "FROM scratch\nCOPY hello.txt /hello.txt\n",
    )
    .unwrap();
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let name = uniq_name("dfsvc");

    let build = ServiceBuild {
        context: dir.path().to_string_lossy().into_owned(),
        dockerfile: "Build.df".to_string(),
        args: Vec::new(),
        target: None,
    };
    let svc = svc_with_build(&name, None, build);
    let resolved = build_service_image(&svc, &store, "proj").unwrap();
    let dest = store_tmp.path().join("hyd");
    lightr_index::hydrate(&dest, &store, &resolved).unwrap();
    assert!(dest.join("hello.txt").exists());
}
