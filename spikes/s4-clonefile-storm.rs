// Spike S4 — clonefile storm (ADR-0009/B3 calibration)
// Standalone: rustc -O spikes/s4-clonefile-storm.rs -o /tmp/s4 && /tmp/s4
// Creates N small files + 1 big file, then materializes them into a dest
// tree via clonefile(2), measuring wall time for create / clone / copy.
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::time::Instant;

extern "C" {
    fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
}

fn clone_one(src: &Path, dst: &Path) -> bool {
    let s = CString::new(src.as_os_str().to_str().unwrap()).unwrap();
    let d = CString::new(dst.as_os_str().to_str().unwrap()).unwrap();
    unsafe { clonefile(s.as_ptr(), d.as_ptr(), 0) == 0 }
}

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let base = std::env::temp_dir().join(format!("s4-{}", std::process::id()));
    let src = base.join("src");
    let dst = base.join("dst");
    let cpy = base.join("cpy");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::create_dir_all(&cpy).unwrap();

    // create N small files (1 KiB) + one 64 MiB file
    let t0 = Instant::now();
    let body = vec![0xabu8; 1024];
    for i in 0..n {
        let sub = src.join(format!("d{:02}", i % 64));
        if i < 64 { let _ = fs::create_dir_all(&sub); }
        fs::File::create(sub.join(format!("f{i}.bin"))).unwrap().write_all(&body).unwrap();
    }
    let big = src.join("big.bin");
    fs::File::create(&big).unwrap().write_all(&vec![0xcdu8; 64 * 1024 * 1024]).unwrap();
    let t_create = t0.elapsed();

    // clone storm
    let t1 = Instant::now();
    let mut ok = 0usize;
    for i in 0..n {
        let sub_s = src.join(format!("d{:02}", i % 64));
        let sub_d = dst.join(format!("d{:02}", i % 64));
        if i < 64 { let _ = fs::create_dir_all(&sub_d); }
        if clone_one(&sub_s.join(format!("f{i}.bin")), &sub_d.join(format!("f{i}.bin"))) { ok += 1; }
    }
    let big_ok = clone_one(&big, &dst.join("big.bin"));
    let t_clone = t1.elapsed();

    // byte-copy baseline (same files)
    let t2 = Instant::now();
    for i in 0..n {
        let sub_s = src.join(format!("d{:02}", i % 64));
        let sub_c = cpy.join(format!("d{:02}", i % 64));
        if i < 64 { let _ = fs::create_dir_all(&sub_c); }
        fs::copy(sub_s.join(format!("f{i}.bin")), sub_c.join(format!("f{i}.bin"))).unwrap();
    }
    fs::copy(&big, cpy.join("big.bin")).unwrap();
    let t_copy = t2.elapsed();

    println!("S4 clonefile storm — n={n} small (1KiB) + 1×64MiB on {:?}", base);
    println!("  create : {:>8.1} ms", t_create.as_secs_f64() * 1e3);
    println!("  clone  : {:>8.1} ms  (ok={ok}/{n}, big_ok={big_ok}) → per-file {:.1} µs",
             t_clone.as_secs_f64() * 1e3, t_clone.as_secs_f64() * 1e6 / n as f64);
    println!("  copy   : {:>8.1} ms  → clone speedup ×{:.1}",
             t_copy.as_secs_f64() * 1e3, t_copy.as_secs_f64() / t_clone.as_secs_f64());
    let _ = fs::remove_dir_all(&base);
}
