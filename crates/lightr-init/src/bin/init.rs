//! lightr-init PID1 binary entry. Real Linux syscalls + vsock are wired here
//! behind cfg(target_os="linux") in WP-B-init; the host build is a stub that
//! refuses to run outside a guest (this binary only makes sense as PID1).
fn main() {
    eprintln!("lightr-init is the microVM guest PID1; not runnable on the host");
    std::process::exit(1);
}
