//! `assemble-pack` — a thin CLI shim over [`lightr_engine::pack::assemble_pack`]
//! and [`lightr_engine::pack::verify_pack`], so `scripts/build-linux-pack.sh`
//! (and anyone) can produce a `kernel` + `initrd` + `pack.json` pack from a
//! kernel image and an `lightr-init` binary without hand-writing a cpio.
//!
//! Usage:
//!   cargo run -p lightr-engine --example assemble-pack -- \
//!       --kernel <kernel-image> \
//!       --init <lightr-init-binary> \
//!       --out <out-dir> \
//!       --arch <aarch64|x86_64> \
//!       [--kernel-version <ver>]
//!
//! On success it assembles the pack, then runs `verify_pack` over the output
//! and prints a one-line structural summary (the same facts the build script
//! reports). Exits non-zero with a clear message on any error.

use lightr_engine::pack::{assemble_pack, verify_pack};
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    kernel: PathBuf,
    init: PathBuf,
    out: PathBuf,
    arch: String,
    kernel_version: Option<String>,
}

fn usage() -> &'static str {
    "usage: assemble-pack --kernel <path> --init <path> --out <dir> \
     --arch <aarch64|x86_64> [--kernel-version <ver>]"
}

fn parse_args() -> Result<Args, String> {
    let mut kernel: Option<PathBuf> = None;
    let mut init: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut arch: Option<String> = None;
    let mut kernel_version: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |name: &str| -> Result<String, String> {
            it.next()
                .ok_or_else(|| format!("flag {name} requires a value\n{}", usage()))
        };
        match flag.as_str() {
            "--kernel" => kernel = Some(PathBuf::from(take("--kernel")?)),
            "--init" => init = Some(PathBuf::from(take("--init")?)),
            "--out" => out = Some(PathBuf::from(take("--out")?)),
            "--arch" => arch = Some(take("--arch")?),
            "--kernel-version" => kernel_version = Some(take("--kernel-version")?),
            "-h" | "--help" => return Err(usage().to_string()),
            other => return Err(format!("unknown flag: {other}\n{}", usage())),
        }
    }

    Ok(Args {
        kernel: kernel.ok_or_else(|| format!("--kernel is required\n{}", usage()))?,
        init: init.ok_or_else(|| format!("--init is required\n{}", usage()))?,
        out: out.ok_or_else(|| format!("--out is required\n{}", usage()))?,
        arch: arch.ok_or_else(|| format!("--arch is required\n{}", usage()))?,
        kernel_version,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("assemble-pack: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = assemble_pack(
        &args.kernel,
        &args.init,
        &args.out,
        &args.arch,
        args.kernel_version.as_deref(),
    ) {
        eprintln!("assemble-pack: assembly failed: {e}");
        return ExitCode::FAILURE;
    }

    match verify_pack(&args.out) {
        Ok(info) => {
            println!(
                "pack OK: arch={} kernel_present={} init_executable={} kernel_bytes={} ({})",
                info.arch,
                info.kernel_present,
                info.init_executable,
                info.kernel_bytes,
                args.out.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("assemble-pack: produced pack failed verify_pack: {e}");
            ExitCode::FAILURE
        }
    }
}
