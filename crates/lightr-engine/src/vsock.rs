//! Host-side vsock exit-code receiver (build-spec-prod §WP-B-vsock).
//!
//! This is the host half of the honesty contract that kills the vz shim's
//! hardcoded `exitCode = 0`. The guest's PID1 (`lightr-init`, see
//! `crates/lightr-init/src/bin/init.rs`) connects from inside the microVM to
//! the host over `AF_VSOCK` (`VMADDR_CID_HOST = 2`, port `1024`) and writes its
//! process's REAL exit code as a single little-endian `i32` frame. The host
//! listens, accepts, reads exactly that frame, and yields the code — which then
//! becomes `VzEngine::run`'s return value. Nobody fabricates a success.
//!
//! ## Seam for testing
//!
//! The frame parser is isolated behind [`read_exit_frame`], which reads from
//! any [`std::io::Read`]. It is unit-tested with an in-memory `Cursor` — no VM,
//! no socket. The REAL `AF_VSOCK` bind/accept lives in
//! [`VsockExitReceiver::bind`] / [`VsockExitReceiver::recv`] and is marked
//! `// BOOT-PATH (S5)`: it is only exercised when a microVM actually boots
//! (Apple Silicon spike S5). On this Intel host the type exists and compiles;
//! its frame parser is the part under test.

use std::io::Read;

/// Host CID for `AF_VSOCK` (`VMADDR_CID_HOST`). The guest connects here.
/// Must match `CID_HOST` in `crates/lightr-init/src/bin/init.rs`.
pub const CID_HOST: u32 = 2;

/// Fixed vsock port the host binds for exit-code frames.
/// Must match `EXIT_PORT` in `crates/lightr-init/src/bin/init.rs`.
pub const EXIT_PORT: u32 = 1024;

/// Read exactly one exit-code frame: 4 bytes, little-endian, as an `i32`.
///
/// This is the SOLE parser of the guest's exit frame — the single source of
/// truth for a vz run's exit code. It is deliberately tiny and pure so it can
/// be unit-tested against a `Cursor` without a VM or a socket.
///
/// A short read (fewer than 4 bytes before EOF) is an error: the guest either
/// crashed mid-report or never reported, and the caller must surface that as a
/// real failure rather than invent a code.
pub fn read_exit_frame(r: &mut impl Read) -> std::io::Result<i32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Host-side receiver for the guest's exit-code frame.
///
/// [`bind`](VsockExitReceiver::bind) opens the `AF_VSOCK` listener (the
/// BOOT-PATH part) BEFORE the VM boots; the caller then runs
/// [`recv`](VsockExitReceiver::recv) on a thread to accept the guest and read
/// its single frame, joining after the VM stops. Keeping the parse behind
/// [`read_exit_frame`] means the interesting logic is testable without any of
/// the socket machinery.
pub struct VsockExitReceiver {
    #[cfg(target_os = "macos")]
    listener: vsock_listener::Listener,
}

impl VsockExitReceiver {
    /// Bind the host vsock listener on [`CID_HOST`]:[`EXIT_PORT`].
    ///
    /// BOOT-PATH (S5): the real `AF_VSOCK` bind only succeeds when the VM
    /// subsystem is live (Apple Silicon spike S5). On other hosts this returns
    /// an `Io` error from `bind`, which is the honest outcome — there is no VM
    /// to report a code, so there is nothing to fake.
    #[cfg(target_os = "macos")]
    pub fn bind() -> std::io::Result<Self> {
        // BOOT-PATH (S5)
        let listener = vsock_listener::Listener::bind(EXIT_PORT)?;
        Ok(VsockExitReceiver { listener })
    }

    /// Wait for the guest to connect, then read its single exit frame via
    /// [`read_exit_frame`].
    ///
    /// BOOT-PATH (S5): blocks on `accept(2)` for the guest connection.
    #[cfg(target_os = "macos")]
    pub fn recv(self) -> std::io::Result<i32> {
        // BOOT-PATH (S5)
        let mut conn = self.listener.accept()?;
        read_exit_frame(&mut conn)
    }
}

/// Real `AF_VSOCK` listener — macOS host half of the channel `lightr-init`
/// writes to. Every fn here is BOOT-PATH (S5): bind/accept only work when the
/// Virtualization subsystem brokers the guest connection, validated on Apple
/// Silicon. The bytes off the accepted connection are parsed by the shared,
/// tested [`read_exit_frame`].
#[cfg(target_os = "macos")]
mod vsock_listener {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// A bound, listening `AF_VSOCK` socket.
    pub struct Listener {
        fd: OwnedFd,
    }

    /// An accepted `AF_VSOCK` connection from the guest.
    pub struct Conn {
        fd: OwnedFd,
    }

    impl Listener {
        /// BOOT-PATH (S5): `socket(AF_VSOCK) → bind(CID_ANY:port) → listen`.
        pub fn bind(port: u32) -> std::io::Result<Self> {
            // BOOT-PATH (S5)
            // Safety: AF_VSOCK socket; the returned fd is checked and adopted by
            // an OwnedFd which closes it exactly once on drop.
            let raw = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
            if raw < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Safety: `raw` is a fresh, valid, owned fd.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };

            // Bind to (VMADDR_CID_ANY, port): accept the guest on any local CID.
            let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
            addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
            addr.svm_port = port;
            addr.svm_cid = libc::VMADDR_CID_ANY;

            // Safety: addr is a fully-initialized sockaddr_vm of the given len;
            // the fd is valid for the call.
            let rc = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
                )
            };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Safety: valid listening fd; backlog 1 (a single guest reports).
            if unsafe { libc::listen(fd.as_raw_fd(), 1) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Listener { fd })
        }

        /// BOOT-PATH (S5): block on `accept(2)` for the guest connection.
        pub fn accept(&self) -> std::io::Result<Conn> {
            // BOOT-PATH (S5)
            // Safety: valid listening fd; we ignore the peer address (NULL/NULL).
            let raw = unsafe {
                libc::accept(
                    self.fd.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if raw < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Safety: `raw` is a fresh, valid, owned connection fd.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            Ok(Conn { fd })
        }
    }

    impl Read for Conn {
        /// BOOT-PATH (S5): `read(2)` off the accepted vsock connection.
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // BOOT-PATH (S5)
            // Safety: valid connection fd; buf is a valid writable slice of the
            // given length; the return code is checked.
            let n = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(n as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind};

    #[test]
    fn read_exit_frame_reads_positive_le_i32() {
        let mut c = Cursor::new(42i32.to_le_bytes().to_vec());
        let code = read_exit_frame(&mut c).expect("4 bytes => i32");
        assert_eq!(code, 42, "little-endian 42 must decode to 42");
    }

    #[test]
    fn read_exit_frame_reads_zero() {
        // Zero is a legitimate REAL exit code here — it is only forbidden as a
        // *fabricated* default. A guest that genuinely exited 0 sends 0.
        let mut c = Cursor::new(0i32.to_le_bytes().to_vec());
        assert_eq!(read_exit_frame(&mut c).unwrap(), 0);
    }

    #[test]
    fn read_exit_frame_reads_negative_code() {
        let mut c = Cursor::new((-1i32).to_le_bytes().to_vec());
        assert_eq!(read_exit_frame(&mut c).unwrap(), -1);
    }

    #[test]
    fn read_exit_frame_reads_large_code() {
        // 128 + signal style codes (e.g. 137 = 128+SIGKILL) must round-trip.
        let mut c = Cursor::new(137i32.to_le_bytes().to_vec());
        assert_eq!(read_exit_frame(&mut c).unwrap(), 137);
    }

    #[test]
    fn read_exit_frame_short_read_is_err() {
        // Only 3 bytes available: the guest crashed mid-report. This MUST be an
        // error, never a silently-zero-padded code.
        let mut c = Cursor::new(vec![1u8, 0, 0]);
        let err = read_exit_frame(&mut c).expect_err("short read must error");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_exit_frame_empty_is_err() {
        // No bytes at all: guest never reported (e.g. never connected/closed
        // immediately). Also an error — the caller turns this into 255.
        let mut c = Cursor::new(Vec::<u8>::new());
        let err = read_exit_frame(&mut c).expect_err("empty must error");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_exit_frame_ignores_trailing_bytes() {
        // Exactly 4 bytes are consumed; anything after the frame is the caller's
        // problem, not part of the code.
        let mut bytes = 7i32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        let mut c = Cursor::new(bytes);
        assert_eq!(read_exit_frame(&mut c).unwrap(), 7);
    }

    #[test]
    fn host_and_guest_ports_agree() {
        // The host listener and the guest sink must target the same channel,
        // or no frame is ever delivered. (Guest values are duplicated in
        // crates/lightr-init/src/bin/init.rs as CID_HOST/EXIT_PORT.)
        assert_eq!(CID_HOST, 2, "VMADDR_CID_HOST");
        assert_eq!(EXIT_PORT, 1024, "exit-code vsock port");
    }
}
