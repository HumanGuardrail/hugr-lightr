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
        STDERR_FILE, STDOUT_FILE,
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
        match run_init(&mut LinuxOps, &mut FileSink) {
            Ok(code) => {
                // Real-time exit signal over the console (hvc0): the host taps the
                // console stream and force-stops the VM the instant it sees this
                // marker, bypassing the slow virtiofs EXIT_FILE visibility (~1.3s).
                // EXIT_FILE is still written by FileSink inside run_init (fallback).
                let mut out = io::stdout();
                let _ = writeln!(out, "LIGHTR_EXIT:{code}");
                let _ = out.flush();
            }
            Err(e) => {
                eprintln!("lightr-init: boot failed: {e}");
            }
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

            // BOOT-PATH: redirect the command's stdout/stderr to capture files on
            // the (writable) rootfs share so the HOST can MEMOIZE the run — the vz
            // memo replays {exit, stdout, stderr} from the Action Cache on a HIT.
            // We hold a second handle to each file (try_clone) so we can fsync it
            // AFTER wait(); the originals are moved into the child's stdio. The
            // files resolve inside the rootfs (PID1 has chrooted) → the rootfs
            // share root → the host's materialized rootfs dir, like EXIT_FILE.
            let stdout_file = std::fs::File::create(STDOUT_FILE)?;
            let stderr_file = std::fs::File::create(STDERR_FILE)?;
            let stdout_sync = stdout_file.try_clone()?;
            let stderr_sync = stderr_file.try_clone()?;

            let mut c = std::process::Command::new(&cmd[0]);
            c.args(&cmd[1..])
                .current_dir(if cwd.is_empty() { "/" } else { cwd })
                .env_clear()
                .envs(env.iter().cloned())
                .stdout(std::process::Stdio::from(stdout_file))
                .stderr(std::process::Stdio::from(stderr_file));

            let status = c.spawn()?.wait()?;

            // CRITICAL ORDERING: make the capture files durable on virtiofs BEFORE
            // run_init reports the exit (which the host taps via the console
            // marker). When the host sees the marker, stdout/stderr/exit are all
            // fsync'd. Both files were redirected into the child; the child has
            // exited (wait returned), so all writes are flushed to the kernel —
            // sync_all persists them through the share.
            stdout_sync.sync_all()?;
            stderr_sync.sync_all()?;

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
