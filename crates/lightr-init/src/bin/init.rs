//! lightr-init PID1 binary entry. The REAL Linux boot path (mount(2) syscalls
//! plus an AF_VSOCK exit sink) is wired behind `cfg(target_os = "linux")`; the
//! host build is a stub that refuses to run (this binary is only PID1 inside a
//! microVM). The lifecycle logic lives in the host-testable library.

// ── Linux guest PID1 (real boot path) ─────────────────────────────────────
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use lightr_init::{run_init, ExitSink, GuestOps, InitSpec, STORE_DEST};
    use std::ffi::CString;
    use std::io;
    use std::process::ExitCode;

    /// Host CID for AF_VSOCK (VMADDR_CID_HOST). PID1 reports the exit code here.
    const CID_HOST: u32 = 2;
    /// Fixed vsock port the host listener binds for exit-code frames.
    const EXIT_PORT: u32 = 1024;
    /// Canonical path of the spec on the mounted store share.
    const SPEC_PATH: &str = "/lightr/spec.json";

    pub fn main() -> ExitCode {
        match boot() {
            Ok(code) => {
                // Map the guest exit code into PID1's own exit code. Clamp to a
                // u8 for ExitCode; the authoritative value already went to the
                // host over vsock.
                ExitCode::from(code.clamp(0, 255) as u8)
            }
            Err(e) => {
                // A boot failure (mount/spec/vsock) is reported as a real error
                // — never a fabricated success.
                eprintln!("lightr-init: boot failed: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn boot() -> io::Result<i32> {
        // BOOT-PATH (validated by spike S5 on Apple Silicon)
        // The store share must be mounted before the spec can be read, so mount
        // it up front; run_init then mounts rootfs + store (store mount is
        // idempotent) and spawns. We mount store early purely to fetch the spec.
        let mut ops = LinuxOps;
        ops.mount_virtiofs(lightr_init::STORE_TAG, STORE_DEST)?;

        let spec_bytes = std::fs::read(SPEC_PATH)?;
        let spec = InitSpec::from_json(&spec_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut sink = VsockSink::connect(CID_HOST, EXIT_PORT)?;
        run_init(&spec, &mut ops, &mut sink)
    }

    /// Real OS actions for the guest PID1.
    struct LinuxOps;

    impl GuestOps for LinuxOps {
        fn mount_virtiofs(&mut self, tag: &str, dest: &str) -> io::Result<()> {
            // BOOT-PATH (validated by spike S5 on Apple Silicon)
            // mount("<tag>", "<dest>", "virtiofs", 0, NULL). The virtiofs tag is
            // the device source; ensure the mountpoint exists first.
            std::fs::create_dir_all(dest)?;

            let source = CString::new(tag)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul in tag"))?;
            let target = CString::new(dest)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul in dest"))?;
            let fstype = CString::new("virtiofs").expect("static");

            // Safety: all pointers are valid for the call's duration; data is
            // NULL (virtiofs takes no mount data); we check the return code.
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

        fn spawn_wait(
            &mut self,
            cmd: &[String],
            cwd: &str,
            env: &[(String, String)],
        ) -> io::Result<i32> {
            // BOOT-PATH (validated by spike S5 on Apple Silicon)
            // std::process drives fork/exec/waitpid; spawn() surfaces ENOENT as
            // an Err (run_init maps that to 127), and wait() yields the real
            // status. This is the path that carries the guest's true exit code.
            if cmd.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
            }
            let mut c = std::process::Command::new(&cmd[0]);
            c.args(&cmd[1..])
                .current_dir(cwd)
                .env_clear()
                .envs(env.iter().cloned());

            let status = c.spawn()?.wait()?;
            Ok(exit_code(status))
        }
    }

    /// Map an `ExitStatus` to an exit code (128+signal on signal termination),
    /// matching the engine's convention.
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

    /// The real exit sink: a little-endian i32 over AF_VSOCK to the host. This
    /// is what kills the vz shim's hardcoded `exitCode = 0` — the host reads the
    /// guest's true exit code off this socket.
    struct VsockSink {
        fd: libc::c_int,
    }

    impl VsockSink {
        fn connect(cid: u32, port: u32) -> io::Result<Self> {
            // BOOT-PATH (validated by spike S5 on Apple Silicon)
            // Safety: AF_VSOCK socket; we check the fd and own/close it.
            let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let sink = VsockSink { fd };

            // sockaddr_vm { svm_family, svm_reserved1, svm_port, svm_cid, .. }.
            let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
            addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
            addr.svm_port = port;
            addr.svm_cid = cid;

            // Safety: addr is a valid, fully-initialized sockaddr_vm for its len.
            let rc = unsafe {
                libc::connect(
                    sink.fd,
                    &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(sink)
        }
    }

    impl ExitSink for VsockSink {
        fn report(&mut self, code: i32) -> io::Result<()> {
            // BOOT-PATH (validated by spike S5 on Apple Silicon)
            // The host reads exactly 4 bytes, little-endian, as the guest exit
            // code. Write the full frame, never a synthesized success.
            let bytes = code.to_le_bytes();
            let mut written = 0usize;
            while written < bytes.len() {
                // Safety: writing `bytes.len() - written` bytes from a valid
                // slice pointer to our owned socket fd; return code checked.
                let n = unsafe {
                    libc::write(
                        self.fd,
                        bytes[written..].as_ptr() as *const libc::c_void,
                        bytes.len() - written,
                    )
                };
                if n < 0 {
                    return Err(io::Error::last_os_error());
                }
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "vsock closed before exit code fully sent",
                    ));
                }
                written += n as usize;
            }
            Ok(())
        }
    }

    impl Drop for VsockSink {
        fn drop(&mut self) {
            // Safety: fd is owned and only closed once.
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

// ── Host stub (non-Linux): this binary only makes sense as a guest PID1 ────
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("lightr-init is the microVM guest PID1; not runnable on the host");
    std::process::exit(1);
}
