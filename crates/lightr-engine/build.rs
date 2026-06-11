// build.rs — lightr-engine
//
// ONLY active when feature "vz" is enabled (default OFF).
// Default builds stay pure Rust and never invoke swiftc.
//
// When vz IS enabled, this compiles shim/vz.swift into a static lib and
// links Virtualization + Foundation frameworks.
//
// Gate: `cargo build --features vz` (macOS only; build will fail on Linux/
// non-mac as swiftc is unavailable there — which is expected and correct
// because probe(Vz) is already cfg-gated to macos+feature).

fn main() {
    // Do nothing unless the vz feature is active.
    if std::env::var("CARGO_FEATURE_VZ").is_err() {
        return;
    }

    // Only meaningful on macOS; Cargo.toml cfg already gates the extern "C"
    // block, but be defensive here too.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        // vz on non-macos: nothing to compile; the Rust cfg will exclude it.
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let shim_src = std::path::Path::new(&manifest_dir)
        .join("shim")
        .join("vz.swift");
    let lib_out = std::path::Path::new(&out_dir).join("liblightr_vz_shim.a");

    // Compile Swift shim to a static library.
    // S5 BOOT NOTE: swiftc invocation validated only when a real kernel pack
    // is present on the machine; the compile step itself can be checked with
    // `cargo check --features vz` provided swiftc is in PATH.
    let status = std::process::Command::new("swiftc")
        .args([
            "-parse-as-library",
            "-module-name",
            "LightrVzShim",
            "-emit-library",
            "-static",
            shim_src.to_str().expect("shim path UTF-8"),
            "-o",
            lib_out.to_str().expect("out path UTF-8"),
        ])
        .status()
        .expect("failed to launch swiftc — ensure Xcode is installed for `--features vz` builds");

    if !status.success() {
        panic!(
            "swiftc failed compiling shim/vz.swift (exit {:?})",
            status.code()
        );
    }

    // Tell Cargo where to find the static lib and which frameworks to link.
    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static=lightr_vz_shim");
    println!("cargo:rustc-link-lib=framework=Virtualization");
    println!("cargo:rustc-link-lib=framework=Foundation");

    // Re-run if the Swift source changes.
    println!("cargo:rerun-if-changed=shim/vz.swift");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_VZ");
}
