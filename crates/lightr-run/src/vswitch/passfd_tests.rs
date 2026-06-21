// Tests for SCM_RIGHTS fd passing. Included as `#[cfg(test)] mod tests;` from
// passfd.rs. Deterministic: a single socketpair'd UnixStream pair in ONE
// process — no subprocess needed (the cross-process proof is the s6 example).
use super::*;
use std::io::Write;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};

/// Round-trip a fd across a socketpair'd control-stream pair: the "switch" end
/// receives a fd the "member" end passes, and that received fd must be a LIVE,
/// INDEPENDENT handle onto the same open file description (datagram delivered on
/// the original fd is read through the received one).
#[test]
fn send_recv_fd_round_trip() {
    let (member_ctl, switch_ctl) = UnixStream::pair().expect("control pair");

    // The "guest NIC" socketpair: we pass `host` over the control stream and
    // keep `guest` to prove the passed fd is wired to the same channel.
    let (guest, host) = UnixDatagram::pair().expect("data pair");

    let payload = b"mac+ip+name";
    send_fd(&member_ctl, host.as_raw_fd(), payload).expect("send_fd");

    let (recv_raw, got_payload) = recv_fd(&switch_ctl).expect("recv_fd");
    assert_eq!(got_payload, payload, "inline payload must round-trip");
    assert!(recv_raw >= 0, "received fd must be valid");
    assert_ne!(
        recv_raw,
        host.as_raw_fd(),
        "kernel must dup into a DIFFERENT fd number in (this) receiver table"
    );

    // SAFETY: `recv_raw` is a fd the kernel just dup'd into this process and we
    // are its sole owner; wrap it so it closes on drop.
    let received = unsafe { UnixDatagram::from_raw_fd(recv_raw) };

    // Prove the received fd is the SAME open file description as `host`: a
    // datagram written by `guest` (the peer of `host`) must arrive on it.
    guest.send(b"FRAME-XYZ").expect("guest send");
    let mut buf = [0u8; 64];
    let n = received.recv(&mut buf).expect("recv on passed fd");
    assert_eq!(
        &buf[..n],
        b"FRAME-XYZ",
        "passed fd is wired to the same channel"
    );

    // And the original `host` still works independently (sender keeps its copy).
    guest.send(b"AGAIN").expect("guest send 2");
    let n2 = host.recv(&mut buf).expect("recv on original host fd");
    assert_eq!(&buf[..n2], b"AGAIN");
}

/// An empty payload is rejected (a 0-byte datagram is indistinguishable from
/// EOF on some platforms, which would break `recv_fd`'s fail-closed contract).
#[test]
fn empty_payload_rejected() {
    let (a, _b) = UnixStream::pair().expect("pair");
    let (_g, h) = UnixDatagram::pair().expect("data pair");
    let err = send_fd(&a, h.as_raw_fd(), b"").expect_err("must reject empty payload");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// An over-long payload is rejected (bounds the receiver's fixed data buffer).
#[test]
fn oversize_payload_rejected() {
    let (a, _b) = UnixStream::pair().expect("pair");
    let (_g, h) = UnixDatagram::pair().expect("data pair");
    let big = vec![0u8; MAX_PAYLOAD + 1];
    let err = send_fd(&a, h.as_raw_fd(), &big).expect_err("must reject oversize payload");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// A control message that carries data but NO ancillary fd must fail closed
/// (never return a bogus fd). We write a plain byte through the stream (kept
/// open) and call `recv_fd`, which must report the missing descriptor rather
/// than succeed with a garbage fd.
#[test]
fn no_fd_fails_closed() {
    let (mut a, b) = UnixStream::pair().expect("pair");
    a.write_all(b"x").expect("plain write");
    a.flush().ok();
    let err = recv_fd(&b).expect_err("must fail when no SCM_RIGHTS fd present");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    drop(a); // kept alive across the recv above so we read data, not EOF
}

/// Peer-closed-before-send is an `UnexpectedEof`, not a silent bad fd.
#[test]
fn peer_eof_fails_closed() {
    let (a, b) = UnixStream::pair().expect("pair");
    drop(a); // peer closes without ever sending
    let err = recv_fd(&b).expect_err("must fail on EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}
