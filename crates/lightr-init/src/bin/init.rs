//! lightr-init PID1 binary entry. The REAL Linux boot path (mount(2) of the
//! rootfs virtiofs share, chroot into it, file-based command-in / exit-out, and
//! a clean power-off) is behind `#[cfg(target_os = "linux")]`; the host build is
//! a stub that refuses to run (this binary is only PID1 inside a microVM). The
//! lifecycle logic + its honesty invariants live in the host-tested library
//! (crates/lightr-init/src/lib.rs).

// ── Linux guest PID1 (real boot path) ─────────────────────────────────────
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use lightr_init::{
        run_init, ExitSink, GuestOps, InitSpec, CMD_FILE, EXIT_FILE, ROOTFS_DEST, ROOTFS_TAG,
    };
    use std::ffi::CString;
    use std::io::{self, Write};
    use std::process::ExitCode;

    pub fn main() -> ExitCode {
        // The guest exit code reaches the host via EXIT_FILE on the rootfs share
        // (written by FileSink inside run_init), NOT via PID1's own status.
        // Whatever happens, flush + power off cleanly so the file is durable on
        // virtiofs and the VM reaches a clean .stopped (not a "killed init"
        // kernel panic). A boot failure leaves no EXIT_FILE → the host maps the
        // missing file to a real non-zero (GUEST_NO_REPORT_CODE), never a fake 0.
        if let Err(e) = run_init(&mut LinuxOps, &mut FileSink) {
            eprintln!("lightr-init: boot failed: {e}");
        }
        sync_and_poweroff()
    }

    /// Flush every filesystem (so EXIT_FILE is durable on virtiofs) and power the
    /// VM off. Never returns: PID1 must not exit (that panics the kernel); the
    /// clean power-off is what the host's VZ observes as `.stopped`.
    fn sync_and_poweroff() -> ! {
        // Safety: sync() takes no args; reboot(RB_POWER_OFF) requests power-off
        // (PID1 holds CAP_SYS_BOOT). If reboot returns, pause forever.
        unsafe {
            libc::sync();
            libc::reboot(libc::RB_POWER_OFF);
        }
        loop {
            unsafe {
                libc::pause();
            }
        }
    }

    /// Real OS actions for the guest PID1.
    struct LinuxOps;

    impl GuestOps for LinuxOps {
        fn mount_rootfs(&mut self) -> io::Result<()> {
            mount_virtiofs(ROOTFS_TAG, ROOTFS_DEST)
        }

        fn read_spec(&mut self) -> io::Result<InitSpec> {
            // The host wrote the command JSON to CMD_FILE on the rootfs share;
            // before chroot it is visible at ROOTFS_DEST + CMD_FILE.
            let path = format!("{ROOTFS_DEST}{CMD_FILE}");
            let bytes = std::fs::read(&path)?;
            InitSpec::from_json(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }

        fn enter_rootfs(&mut self) -> io::Result<()> {
            // BOOT-PATH: chroot into the mounted rootfs so the command resolves
            // there (the initrd holds only /init). chdir("/") after. PID1 stays
            // in the rootfs so FileSink writes EXIT_FILE onto the rootfs share.
            let root = CString::new(ROOTFS_DEST)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul in rootfs"))?;
            // Safety: valid C string; return code checked.
            if unsafe { libc::chroot(root.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::chdir(b"/\0".as_ptr() as *const libc::c_char) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn spawn_wait(
            &mut self,
            cmd: &[String],
            cwd: &str,
            env: &[(String, String)],
        ) -> io::Result<i32> {
            // BOOT-PATH: std::process drives fork/exec/waitpid. spawn() surfaces
            // ENOENT as an Err (run_init maps that to 127); wait() yields the real
            // status carrying the guest's true exit code.
            if cmd.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
            }
            let mut c = std::process::Command::new(&cmd[0]);
            c.args(&cmd[1..])
                .current_dir(if cwd.is_empty() { "/" } else { cwd })
                .env_clear()
                .envs(env.iter().cloned());

            let status = c.spawn()?.wait()?;
            Ok(exit_code(status))
        }
    }

    /// mount("<tag>", "<dest>", "virtiofs", 0, NULL); ensure the mountpoint
    /// exists first.
    fn mount_virtiofs(tag: &str, dest: &str) -> io::Result<()> {
        std::fs::create_dir_all(dest)?;
        let source = CString::new(tag)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul tag"))?;
        let target = CString::new(dest)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul dest"))?;
        let fstype = CString::new("virtiofs").expect("static");
        // Safety: pointers valid for the call; data is NULL (virtiofs takes no
        // mount data); return code checked.
        let rc = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                fstype.as_ptr(),
                0,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Map an `ExitStatus` to an exit code (128+signal on signal termination).
    fn exit_code(status: std::process::ExitStatus) -> i32 {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            code
        } else if let Some(sig) = status.signal() {
            128 + sig
        } else {
            1
        }
    }

    /// The real exit sink: writes the guest's REAL exit code as a decimal integer
    /// to EXIT_FILE on the (writable) rootfs share, fsync'd so it survives the
    /// power-off; the host reads it back after the VM stops. Replaces the AF_VSOCK
    /// sink — macOS has no host AF_VSOCK (decisions-log 2026-06-12). Never
    /// synthesizes a success: it writes exactly what `run_init` computed.
    struct FileSink;

    impl ExitSink for FileSink {
        fn report(&mut self, code: i32) -> io::Result<()> {
            // BOOT-PATH: EXIT_FILE resolves inside the rootfs (PID1 has chrooted)
            // → the rootfs share root → the host's materialized rootfs dir.
            let mut f = std::fs::File::create(EXIT_FILE)?;
            write!(f, "{code}")?;
            f.sync_all()?;
            Ok(())
        }
    }
}

// ── Host stub (non-Linux): this binary only makes sense as a guest PID1 ────
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("lightr-init is the microVM guest PID1; not runnable on the host");
    std::process::exit(1);
}
