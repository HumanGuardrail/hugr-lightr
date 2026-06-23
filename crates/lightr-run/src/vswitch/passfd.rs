//! SCM_RIGHTS file-descriptor passing over a `UnixStream` (ADR-0018, Phase-2).
//!
//! De-risk spike S6-XPROC keystone: production spawns each container as a
//! SEPARATE detached supervisor (both `run` and `compose` detach), so the
//! per-network L2 [`super::VSwitch`] must be attached to from OTHER processes.
//! Each member process makes a `socketpair(AF_UNIX, SOCK_DGRAM)` — the guest NIC
//! end (`guest_fd`) it keeps; the switch end (`host_fd`) it must hand to the
//! switch process, which then owns it and calls [`super::VSwitch::add_member`]
//! exactly as the proven in-process path does.
//!
//! A raw fd integer is meaningless across a process boundary (it indexes the
//! sender's table); the kernel must DUP it into the receiver's table. The only
//! portable way to do that on unix is an `AF_UNIX` control message carrying
//! `SCM_RIGHTS` ancillary data. This module wraps `sendmsg`/`recvmsg` for
//! exactly one fd plus a small inline data payload (the member metadata) so the
//! whole attach is a single atomic message — the data and the fd cannot be torn
//! apart by the receiver.
//!
//! Unix-only: `SCM_RIGHTS`, `cmsghdr`, and `RawFd` are POSIX. The module is
//! `#[cfg(unix)]` at the `pub mod` site in `vswitch/mod.rs`; on windows it does
//! not exist and callers must fail closed (windows container networking is a
//! future ring — see `vswitch/mod.rs`).

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// Upper bound on the inline metadata payload `send_fd` will carry. The S6
/// attach payload is a fixed 33 bytes (6 MAC + 4 IP + 1 name-len + ≤22 name);
/// 256 leaves generous headroom while bounding the `recvmsg` data buffer.
const MAX_PAYLOAD: usize = 256;

/// Send `fd` plus an inline `data` payload over `stream` as a SINGLE message,
/// the fd travelling as `SCM_RIGHTS` ancillary data. The kernel dups `fd` into
/// the receiver; the sender still owns its own copy and remains responsible for
/// closing it (the host end is typically closed by the sender right after, since
/// the switch now holds an independent dup).
///
/// `data` must be non-empty (a zero-length payload makes a 0-byte datagram the
/// peer's `recv` cannot distinguish from EOF on some platforms) and at most
/// [`MAX_PAYLOAD`] bytes.
pub fn send_fd(stream: &UnixStream, fd: RawFd, data: &[u8]) -> io::Result<()> {
    if data.is_empty() || data.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "send_fd payload len {} out of 1..={MAX_PAYLOAD}",
                data.len()
            ),
        ));
    }

    // One iovec for the inline data (sendmsg requires ≥1 byte of real data to
    // reliably deliver the ancillary fd).
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    // Control buffer: our const is an UPPER bound on CMSG_SPACE(sizeof fd); the
    // actual `msg_controllen` MUST be the exact platform `CMSG_SPACE` value
    // (macOS `sendmsg` returns EINVAL if controllen exceeds the real cmsg span),
    // which is only available at runtime since `CMSG_SPACE` isn't `const`.
    let mut cmsg_buf = [0u8; cmsg_space_one_fd()];
    let controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) };

    // SAFETY: zero-init a msghdr, then fill the fields we set below. All
    // pointers point at live stack storage that outlives the `sendmsg` call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = controllen as _;

    // Fill the single SCM_RIGHTS control message with `fd`.
    // SAFETY: CMSG_FIRSTHDR returns a pointer within `cmsg_buf` (non-null
    // because msg_controllen > 0); we write exactly one fd into its data area,
    // whose size we reserved via cmsg_space_one_fd().
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::other("CMSG_FIRSTHDR returned null"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        let data_ptr = libc::CMSG_DATA(cmsg) as *mut RawFd;
        std::ptr::write_unaligned(data_ptr, fd);
    }

    // SAFETY: `msg` is a fully-initialised msghdr referencing live buffers.
    // Retry transient errors: under heavy parallel load the SCM_RIGHTS sendmsg
    // can be interrupted (EINTR) or, on macOS, transiently report EINVAL/EAGAIN
    // before the kernel completes the ancillary-fd handoff. Bounded (~100ms) so a
    // genuine structural error still surfaces rather than spinning forever.
    let mut tries = 0u32;
    loop {
        let r = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
        if r >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        let transient = matches!(
            err.raw_os_error(),
            Some(libc::EINTR) | Some(libc::EAGAIN) | Some(libc::EINVAL)
        );
        tries += 1;
        if transient && tries < 50 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        return Err(err);
    }
    Ok(())
}

/// Receive one fd + its inline payload sent by [`send_fd`]. Returns the
/// received fd (already dup'd into THIS process by the kernel — the caller owns
/// it and must close it, e.g. by handing it to `VSwitch::add_member`, which
/// wraps it in a `UnixDatagram` that closes on drop) and the payload bytes.
///
/// Fails closed: a message that carries no `SCM_RIGHTS` fd (peer EOF, or a
/// message without ancillary data) is an error rather than a silent `fd = -1`,
/// and any unexpected ancillary fds beyond the first are closed so they cannot
/// leak.
pub fn recv_fd(stream: &UnixStream) -> io::Result<(RawFd, Vec<u8>)> {
    let mut data = [0u8; MAX_PAYLOAD];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    let mut cmsg_buf = [0u8; cmsg_space_one_fd()];

    // SAFETY: zero-init msghdr then point it at the live stack buffers above.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_buf.len() as _;

    // SAFETY: `msg` references live buffers for the duration of the call.
    // Retry transient errors (EINTR/EAGAIN/EINVAL) under load, bounded ~100ms —
    // symmetric with `send_fd` (see its note).
    let mut tries = 0u32;
    let n = loop {
        let r = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
        if r >= 0 {
            break r;
        }
        let err = io::Error::last_os_error();
        let transient = matches!(
            err.raw_os_error(),
            Some(libc::EINTR) | Some(libc::EAGAIN) | Some(libc::EINVAL)
        );
        tries += 1;
        if transient && tries < 50 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        return Err(err);
    };
    if n == 0 {
        // Peer closed without sending — no fd will ever arrive.
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "recv_fd: peer closed before sending an fd",
        ));
    }

    // Walk the control messages, taking the first SCM_RIGHTS fd and closing any
    // extras so a misbehaving/duplicated message cannot leak descriptors.
    let mut got: Option<RawFd> = None;
    // SAFETY: CMSG_FIRSTHDR/NXTHDR walk only the bytes the kernel reported in
    // msg_controllen; each CMSG_DATA read stays within that control message.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data_ptr = libc::CMSG_DATA(cmsg) as *const RawFd;
                let fd = std::ptr::read_unaligned(data_ptr);
                if got.is_none() {
                    got = Some(fd);
                } else {
                    // Extra fd we did not ask for — close it, never leak.
                    libc::close(fd);
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    match got {
        Some(fd) => Ok((fd, data[..n as usize].to_vec())),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recv_fd: message carried no SCM_RIGHTS descriptor",
        )),
    }
}

/// `CMSG_SPACE(size_of::<RawFd>())` as a `const`, so the control buffer can be a
/// fixed-size stack array. `CMSG_SPACE` is not `const` in `libc`, so we compute
/// the worst-case (cmsghdr, aligned to a pointer, plus one aligned fd) by hand;
/// it is `>=` the platform value, and over-sizing the control buffer is safe.
const fn cmsg_space_one_fd() -> usize {
    // Pointer-aligned header + pointer-aligned single-fd payload. This upper
    // bound matches CMSG_SPACE(4) on every LP64 unix (header 16, data padded to
    // 8) and is never smaller, so the buffer always fits the real cmsg.
    let align = std::mem::size_of::<usize>();
    let hdr = align_up(std::mem::size_of::<libc::cmsghdr>(), align);
    let payload = align_up(std::mem::size_of::<RawFd>(), align);
    hdr + payload
}

const fn align_up(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}

#[cfg(test)]
#[path = "passfd_tests.rs"]
mod tests;
