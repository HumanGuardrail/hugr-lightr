//! Container stats — real `/proc/<pid>` sampling on Linux, probe-truthful zero
//! elsewhere (WP-CRI-MVP / stats).
//!
//! PROVENANCE: `read_proc_stats` is TRANSCRIBED from the conformance reference
//! `lightr-cri-fake::read_proc_stats`. cpu = (utime+stime) ticks → nanos via
//! `_SC_CLK_TCK`; memory = VmRSS kB → bytes. On non-Linux there is no `/proc`,
//! so we report zeroed-with-timestamp (probe-truthful law — never fake a
//! number we did not measure).

/// Read `(cpu_usage_core_nanos, memory_working_set_bytes)` for `pid` from
/// `/proc`. Linux-only; gated on the fn itself so it is not dead code on the
/// windows gate (template 8a).
#[cfg(target_os = "linux")]
pub fn read_proc_stats(pid: u32) -> (u64, u64) {
    use std::fs;

    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 };
    let nanos_per_tick = 1_000_000_000u64.checked_div(clk_tck).unwrap_or(0);
    let cpu_nanos = if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // utime = 14th field (idx 13), stime = 15th (idx 14), space-separated.
        let fields: Vec<&str> = stat.split_whitespace().collect();
        if fields.len() > 14 {
            let utime: u64 = fields[13].parse().unwrap_or(0);
            let stime: u64 = fields[14].parse().unwrap_or(0);
            (utime + stime) * nanos_per_tick
        } else {
            0
        }
    } else {
        0
    };

    let mem_bytes = if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        let mut rss_kb: u64 = 0;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss_kb = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                break;
            }
        }
        rss_kb * 1024
    } else {
        0
    };

    (cpu_nanos, mem_bytes)
}

/// Non-Linux: no `/proc`. Probe-truthful zero (the timestamp is still real).
#[cfg(not(target_os = "linux"))]
pub fn read_proc_stats(_pid: u32) -> (u64, u64) {
    (0, 0)
}
