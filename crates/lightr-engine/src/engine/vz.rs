//! VzEngine — macOS Virtualization.framework microVM engine (feature "vz").

use super::spec::ExecSpec;
use super::Engine;
use lightr_core::{LightrError, Result};

// ── VzEngine (macOS + feature "vz") ─────────────────────────────────────────

#[cfg(all(target_os = "macos", feature = "vz"))]
mod vz_impl {
    use super::{Engine, ExecSpec};
    use crate::engine::probe::pack_dir;
    use lightr_core::{LightrError, Result};
    use lightr_init::{InitSpec, CMD_FILE, EXIT_FILE, GUEST_PATH};
    use std::ffi::CString;

    /// Exit code returned when the VM booted (and stopped) but the guest never
    /// wrote a readable exit file — i.e. PID1 crashed before reporting. NOT a
    /// success: we surface a real, non-zero failure rather than fabricate 0.
    const GUEST_NO_REPORT_CODE: i32 = 255;

    extern "C" {
        /// C ABI exposed by shim/vz.swift (compiled to static lib by build.rs).
        ///
        /// VALIDATED end-to-end on Intel x86_64 (i7-9750H, macOS 15.3.2,
        /// 2026-06-12): boots a bzImage microVM, runs the command, returns its
        /// real exit code. The kernel MUST be an x86_64 bzImage — VZ boots via
        /// the x86 setup-header / real-mode protocol; a raw `vmlinux` ELF (even a
        /// PVH one) is rejected with "Internal Virtualization error".
        ///
        /// RETURN CONTRACT: this is a VM-LIFECYCLE status, NOT the guest's exit
        /// code. `0` = the VM booted and stopped cleanly; a negative value =
        /// boot/config failure. The guest's REAL exit code arrives via the file
        /// channel on the shared rootfs (`EXIT_FILE`), read back by `run` below —
        /// never from this return value. The shim never fabricates a `0`.
        ///
        /// F-203: `memory_mb`/`cpu_count` carry the resource caps (build-spec-
        /// parity.md §2.4). `0` means "use the shim default" (unlimited). When
        /// `memory_mb` is below the VZ memory floor the shim returns a config
        /// failure (< 0) rather than silently clamping — an honest boundary.
        ///
        /// ADR-0018 (WP-C6/C7): `net_fd` is the GUEST-side fd of a
        /// `socketpair(AF_UNIX, SOCK_DGRAM)` (host end owned by the L2 switch).
        /// `>= 0` ⇒ the shim attaches a SECOND virtio-net NIC
        /// (`VZFileHandleNetworkDeviceAttachment` over a non-owning `FileHandle`
        /// on the fd, with SO_*BUF tuning) ALONGSIDE the NAT NIC — the dual-NIC
        /// mesh path. `-1` ⇒ no file-handle NIC (today's single-NAT-NIC path,
        /// byte-for-byte). The fd's lifetime is the caller's (the switch); the
        /// shim wraps it `closeOnDealloc:false`.
        fn lightr_vz_run(
            kernel: *const libc::c_char,
            initrd: *const libc::c_char,
            rootfs: *const libc::c_char,
            store: *const libc::c_char,
            memory_mb: u64,
            cpu_count: u64,
            net_fd: libc::c_int,
            net_mac: *const libc::c_char,
            argc: libc::c_int,
            argv: *const *const libc::c_char,
        ) -> libc::c_int;
    }

    pub struct VzEngine;

    impl Engine for VzEngine {
        /// Run the guest and return its REAL exit code.
        ///
        /// Sequence (file channel — macOS has NO host AF_VSOCK):
        ///   1. Write the command spec (InitSpec JSON) to CMD_FILE on the
        ///      writable rootfs share, and clear any stale EXIT_FILE.
        ///   2. Boot the VM via the Swift shim and block until it stops.
        ///   3. Read the guest's REAL exit code back from EXIT_FILE.
        ///
        /// Exit-code law:
        ///   - boot/config failure (shim < 0)             ⇒ `Err(LightrError)`
        ///   - VM stopped but no EXIT_FILE (guest crash)  ⇒ 255, NOT 0
        ///   - otherwise                                  ⇒ the guest's code
        ///     parsed from EXIT_FILE.
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            let dir = pack_dir();
            let kernel = dir.join("kernel");
            let initrd = dir.join("initrd");
            let rootfs = spec.rootfs.ok_or_else(|| {
                LightrError::InvalidRef("vz engine requires a rootfs".to_string())
            })?;

            // ── 1. Write the command spec onto the rootfs share BEFORE boot ──
            // macOS has NO host AF_VSOCK, so the host↔guest channel is two files
            // on the shared (writable) rootfs virtiofs share (decisions-log
            // 2026-06-12): the host writes the command to CMD_FILE here; the guest
            // PID1 reads it, runs it, and writes its REAL exit code to EXIT_FILE,
            // which the host reads back after the VM stops. cwd "/" + a minimal
            // PATH is the guest environment (ExecSpec.cwd is a host path).
            let cmd_path = rootfs.join(CMD_FILE.trim_start_matches('/'));
            let exit_path = rootfs.join(EXIT_FILE.trim_start_matches('/'));
            // A stale exit file from a prior run must not be read as this run's
            // result — clear it before boot.
            let _ = std::fs::remove_file(&exit_path);
            let init_spec = InitSpec {
                command: spec.command.to_vec(),
                cwd: "/".to_string(),
                env: vec![("PATH".to_string(), GUEST_PATH.to_string())],
                // WP-NET2: when the run wants networking, the guest publishes its
                // DHCP IP to IP_FILE before spawning the (possibly long-running)
                // command, so the host supervisor can forward published ports.
                net: spec.net,
            };
            std::fs::write(&cmd_path, init_spec.to_json()).map_err(LightrError::Io)?;

            // WP-NET2: a networked run needs the shim to attach the NAT NIC +
            // `ip=dhcp`. The shim gates that on LIGHTR_VZ_NET (env), so ExecSpec.net
            // is the single switch that drives BOTH the guest (InitSpec.net above)
            // and the shim. Respect a user-set value; set before the FFI spawns any
            // thread (single-threaded here). A non-networked run leaves it unset,
            // so the memo/one-shot path keeps its faster no-NIC boot.
            if spec.net && std::env::var_os("LIGHTR_VZ_NET").is_none() {
                // Safety: single-threaded here, before the engine spawns the VM.
                unsafe { std::env::set_var("LIGHTR_VZ_NET", "1") };
            }

            let kernel_c = path_to_cstr(&kernel)?;
            let initrd_c = path_to_cstr(&initrd)?;
            let rootfs_c = path_to_cstr(rootfs)?;
            // store path: empty → the Swift shim mounts no store share. The
            // command travels via CMD_FILE on the rootfs, not argv/cmdline.
            let store_c = CString::new("").unwrap();

            // argv is still handed to the shim (it sets LIGHTR_CMD on the kernel
            // cmdline), but the guest reads CMD_FILE instead — pass it anyway for
            // forward-compat + console debugging.
            let argv_cstrings: Vec<CString> = spec
                .command
                .iter()
                .map(|s| {
                    CString::new(s.as_bytes()).map_err(|_| {
                        LightrError::InvalidRef(format!("invalid NUL in command arg: {s}"))
                    })
                })
                .collect::<Result<_>>()?;
            let mut argv_ptrs: Vec<*const libc::c_char> =
                argv_cstrings.iter().map(|c| c.as_ptr()).collect();
            argv_ptrs.push(std::ptr::null());

            // ── 2. Boot the VM and block until it stops (or fails) ──────────
            // F-203 (build-spec-parity.md §2.4): derive the VM caps from
            // `spec.limits` and hand them to the shim, which sets
            // `VZVirtualMachineConfiguration.memorySizeInBytes`/`.cpuCount`.
            //   cpus → ceil(millis/1000) vcpus, min 1 (a fractional core rounds
            //          UP to a whole vcpu — VZ has no sub-core granularity).
            //   memory → MB, rounded UP. Below the VZ floor the shim returns a
            //          config failure (< 0) → the honest `Err` below.
            //   `0` for either field means "use the shim default" (unlimited).
            // Fast-teardown: tell the shim the host path of the guest's durable
            // EXIT_FILE. The shim polls it and force-stops the VM the instant the
            // result is captured, instead of waiting for the guest's slow clean
            // poweroff + VZ stop-detection (~2s). Safe: PID1 fsyncs EXIT_FILE
            // before it would power off, and the rootfs is a throwaway CoW dir, so
            // nothing but EXIT_FILE is read back. Same-process, set before the FFI
            // call spawns any thread.
            unsafe { std::env::set_var("LIGHTR_VZ_EXITFILE", &exit_path) };
            let (memory_mb, cpu_count) = vz_caps(&spec.limits);
            // ADR-0018 dual-NIC: hand the GUEST-side socketpair fd to the shim
            // (-1 = none → today's single-NAT-NIC path). The fd is owned by the
            // L2 switch (a later WP); the shim wraps it non-owning.
            let net_fd: libc::c_int = spec.net_fd.unwrap_or(-1);
            // ADR-0018: pass the registry-assigned per-member mesh MAC as a C
            // string ("xx:xx:..") so the guest's eth1 emits it → the switch keys
            // DHCP/L2/DNS on the same MAC. None → null → the shim's pinned fallback.
            let net_mac_c: Option<std::ffi::CString> = spec.net_mac.map(|m| {
                std::ffi::CString::new(format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    m[0], m[1], m[2], m[3], m[4], m[5]
                ))
                .expect("formatted MAC has no interior NUL")
            });
            let net_mac_ptr: *const libc::c_char =
                net_mac_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let vm_status = unsafe {
                lightr_vz_run(
                    kernel_c.as_ptr(),
                    initrd_c.as_ptr(),
                    rootfs_c.as_ptr(),
                    store_c.as_ptr(),
                    memory_mb,
                    cpu_count,
                    net_fd,
                    net_mac_ptr,
                    argv_ptrs.len() as libc::c_int - 1, // exclude null sentinel
                    argv_ptrs.as_ptr(),
                )
            };
            // Shim return contract (WAVE-VZ fast exit channel):
            //   -1        = boot/config failure (no VM) → real error;
            //   0..=255   = the guest's REAL exit code, captured in real time from
            //               the console marker (no virtiofs lag) → use directly;
            //   -2        = the VM stopped without a marker (guest crashed before
            //               printing) → fall back to the durable EXIT_FILE.
            if vm_status == -1 {
                return Err(LightrError::InvalidRef(
                    "vz engine: VM boot/config failed".to_string(),
                ));
            }
            if vm_status >= 0 {
                return Ok(vm_status);
            }

            // ── 3. Fallback: read the guest's exit code from the rootfs share ──
            // Only reached when no console marker arrived. PID1 wrote EXIT_FILE
            // (fsync); a missing/unparsable file means the guest never reported
            // ⇒ GUEST_NO_REPORT_CODE (255), never a fabricated 0. Retry covers
            // virtiofs flush lag.
            for _ in 0..30 {
                if let Ok(s) = std::fs::read_to_string(&exit_path) {
                    if let Ok(code) = s.trim().parse::<i32>() {
                        return Ok(code);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(GUEST_NO_REPORT_CODE)
        }
    }

    fn path_to_cstr(p: &std::path::Path) -> Result<CString> {
        CString::new(p.as_os_str().as_encoded_bytes())
            .map_err(|_| LightrError::InvalidRef(format!("invalid path: {}", p.display())))
    }

    /// Derive `(memory_mb, cpu_count)` for the vz shim from the resource caps.
    /// `0` for a field means "use the shim default" (unlimited / VZ baseline).
    ///
    /// * cpu — `ceil(millis / 1000)`, min 1 vcpu (VZ has no sub-core grain).
    /// * mem — `ceil(bytes / MiB)`. The VZ memory floor is enforced by the shim
    ///   (config failure rather than a silent clamp).
    ///
    /// NOTE (WP-#90): `limits.pids_max` is intentionally NOT consumed here — the
    /// shim cannot set a guest per-container `pids.max` (the VM owns its own pid
    /// space, not a delegated cgroup). A `--pids-limit --engine vz` request is
    /// honest-errored upstream at the CLI (run handler) BEFORE the VM boots, so it
    /// is never silently dropped; nothing reaches this function with a pids cap.
    fn vz_caps(limits: &lightr_core::ResourceLimits) -> (u64, u64) {
        let cpu_count = match limits.cpu_millis {
            None => 0,
            Some(millis) => millis.div_ceil(1000).max(1),
        };
        let memory_mb = match limits.memory_bytes {
            None => 0,
            Some(bytes) => bytes.div_ceil(1024 * 1024).max(1),
        };
        (memory_mb, cpu_count)
    }

    #[cfg(test)]
    mod tests {
        use super::vz_caps;
        use lightr_core::ResourceLimits;

        #[test]
        fn unlimited_yields_zero_defaults() {
            assert_eq!(vz_caps(&ResourceLimits::default()), (0, 0));
        }

        #[test]
        fn cpu_rounds_up_to_whole_vcpus_min_one() {
            // 0.5 core → 1 vcpu; 1.5 → 2; 2.0 → 2; 2001m → 3.
            let c = |m| {
                vz_caps(&ResourceLimits {
                    memory_bytes: None,
                    cpu_millis: Some(m),
                    pids_max: None,
                })
                .1
            };
            assert_eq!(c(500), 1);
            assert_eq!(c(1500), 2);
            assert_eq!(c(2000), 2);
            assert_eq!(c(2001), 3);
            assert_eq!(c(1), 1);
        }

        #[test]
        fn memory_rounds_up_to_whole_mib() {
            let m = |b| {
                vz_caps(&ResourceLimits {
                    memory_bytes: Some(b),
                    cpu_millis: None,
                    pids_max: None,
                })
                .0
            };
            assert_eq!(m(1024 * 1024), 1);
            assert_eq!(m(1024 * 1024 + 1), 2);
            assert_eq!(m(512 * 1024 * 1024), 512);
            assert_eq!(m(1), 1);
        }
    }
}

#[cfg(all(target_os = "macos", feature = "vz"))]
pub(super) fn vz_engine_box() -> Box<dyn Engine> {
    Box::new(vz_impl::VzEngine)
}

/// Stub for builds without feature "vz" (or non-macOS) — probe gates before
/// this is ever reached, so this path is dead-code in practice.
#[cfg(not(all(target_os = "macos", feature = "vz")))]
struct VzEngineStub;

#[cfg(not(all(target_os = "macos", feature = "vz")))]
impl Engine for VzEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "vz engine requires macOS + the 'vz' build feature + a linux pack".to_string(),
        ))
    }
}

#[cfg(not(all(target_os = "macos", feature = "vz")))]
pub(super) fn vz_engine_box() -> Box<dyn Engine> {
    Box::new(VzEngineStub)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // ── KEY INVARIANT (WP-B exit channel) ──────────────────────────────────
    // There is NO code path where vz returns a hardcoded 0. The command is
    // handed to the guest via CMD_FILE on the shared rootfs; the exit code comes
    // back via EXIT_FILE on that same share (macOS has no host AF_VSOCK). The
    // shim return is a VM-lifecycle status only. A missing exit file ⇒ 255,
    // never a fabricated 0. These source-level tests pin that down so a future
    // edit can't silently restore a fake success.

    /// The Swift shim must NOT contain the fabricated `exitCode = 0` it used to,
    /// and must not name a guest exitCode at all — it reports only VM-lifecycle
    /// status (vmStatus); the real code is a file on the shared rootfs.
    #[test]
    fn swift_shim_has_no_fabricated_exit_code_zero() {
        let shim = include_str!("../../shim/vz.swift");
        assert!(
            !shim.contains("exitCode = 0"),
            "vz.swift must not fabricate a guest exit code of 0"
        );
        assert!(
            !shim.contains("exitCode"),
            "vz.swift must not name a guest exitCode at all — it reports only \
             VM-lifecycle status (vmStatus); the code is read from the rootfs file"
        );
        assert!(
            shim.contains("vmStatus"),
            "vz.swift must report a VM-lifecycle status (vmStatus), not the code"
        );
    }

    /// `VzEngine::run` delivers the command via CMD_FILE and reads the exit code
    /// from EXIT_FILE on the shared rootfs — it NEVER returns the shim's status
    /// as the exit code, and a missing file maps to 255 (not a fabricated 0).
    #[test]
    fn vz_exit_code_comes_from_the_rootfs_file_not_the_shim() {
        let src = include_str!("vz.rs");

        // Command delivered to the guest by writing CMD_FILE on the rootfs.
        assert!(
            src.contains("CMD_FILE") && src.contains("init_spec.to_json()"),
            "VzEngine::run must write the command spec to CMD_FILE"
        );
        // Exit code read back from EXIT_FILE on the rootfs, parsed as i32.
        assert!(
            src.contains("EXIT_FILE") && src.contains("parse::<i32>()"),
            "VzEngine::run must read the exit code from EXIT_FILE"
        );
        // The shim return is a lifecycle status (vm_status), only checked for
        // failure — never returned directly as the exit code.
        assert!(
            src.contains("let vm_status") && src.contains("vm_status < 0"),
            "the shim return must be handled as a lifecycle status (vm_status)"
        );
        // The honest no-report fallback is 255, explicitly NOT 0.
        assert!(
            src.contains("GUEST_NO_REPORT_CODE: i32 = 255"),
            "a missing guest exit file must map to 255, not a fabricated 0"
        );
    }
}
