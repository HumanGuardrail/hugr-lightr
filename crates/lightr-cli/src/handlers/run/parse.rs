//! `lightr run` value parsers — `--tmpfs` / `--ulimit` / `size=` grammar.
//!
//! Split from `run/mod.rs` (godfile cap). Each parser is `pub(super)` — called
//! from `run()` in the parent module. Behaviour is byte-identical to the
//! pre-split inline versions (fail-closed `Err(2)` on any malformed input).

use lightr_engine::{TmpfsMount, Ulimit};

/// Parse the raw `--tmpfs` strings (Docker shape `DST[:opt[,opt...]]`) into the
/// engine's [`TmpfsMount`] list. Supported options (Docker parity): `size=<bytes
/// | N[kKmMgG]>` (an optional byte cap) and `mode=<octal>` (defaults to `1777`,
/// the sticky world-writable scratch mode). Exec is ALWAYS allowed (no MS_NOEXEC),
/// matching Docker's `--tmpfs` default of `nosuid,nodev`. A missing/empty target or
/// an unparseable option is an honest `Err(2)` (fail-closed — never a silent drop).
pub(super) fn parse_tmpfs(raw: &[String]) -> Result<Vec<TmpfsMount>, i32> {
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let (target, opts) = match entry.split_once(':') {
            Some((t, o)) => (t, Some(o)),
            None => (entry.as_str(), None),
        };
        if target.is_empty() {
            eprintln!("lightr: --tmpfs {entry}: empty target path");
            return Err(2);
        }
        let mut size: Option<u64> = None;
        let mut mode = "1777".to_string();
        if let Some(opts) = opts {
            for opt in opts.split(',').filter(|s| !s.is_empty()) {
                match opt.split_once('=') {
                    Some(("size", v)) => match parse_size(v) {
                        Some(b) => size = Some(b),
                        None => {
                            eprintln!("lightr: --tmpfs {entry}: bad size '{v}'");
                            return Err(2);
                        }
                    },
                    Some(("mode", v)) => {
                        // Validate it is octal so the mount option is well-formed.
                        if v.is_empty() || u32::from_str_radix(v, 8).is_err() {
                            eprintln!("lightr: --tmpfs {entry}: bad mode '{v}' (expected octal)");
                            return Err(2);
                        }
                        mode = v.to_string();
                    }
                    _ => {
                        eprintln!("lightr: --tmpfs {entry}: unknown option '{opt}'");
                        return Err(2);
                    }
                }
            }
        }
        out.push(TmpfsMount {
            target: target.to_string(),
            size,
            mode,
        });
    }
    Ok(out)
}

/// Parse the raw `--ulimit` strings (Docker shape `TYPE=SOFT[:HARD]`) into the
/// engine's [`Ulimit`] list (mirrors [`parse_tmpfs`]'s shape + fail-closed
/// `Err(2)`). Grammar:
///   * split on the FIRST `=` → (type, vals); vals split on `:` → soft[, hard].
///   * TYPE → resource: the libc `RLIMIT_*` integer (stored as NUMERIC constants
///     valid on Linux so this fn compiles identically host-side on macOS, where
///     some `libc::RLIMIT_*` differ — verified against Linux `<bits/resource.h>`).
///   * value: `unlimited` / `-1` ⇒ `u64::MAX` (RLIM_INFINITY); else a `u64`.
///     HARD omitted ⇒ `hard = soft` (Docker). A bad value/type ⇒ honest `Err(2)`.
pub(super) fn parse_ulimits(raw: &[String]) -> Result<Vec<Ulimit>, i32> {
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let (ty, vals) = match entry.split_once('=') {
            Some((t, v)) => (t.trim(), v.trim()),
            None => {
                eprintln!("lightr: --ulimit {entry}: expected TYPE=SOFT[:HARD]");
                return Err(2);
            }
        };
        let resource = match ulimit_resource(ty) {
            Some(r) => r,
            None => {
                eprintln!("lightr: --ulimit {entry}: unknown ulimit type '{ty}'");
                return Err(2);
            }
        };
        let (soft_s, hard_s) = match vals.split_once(':') {
            Some((s, h)) => (s.trim(), Some(h.trim())),
            None => (vals, None),
        };
        let soft = match parse_ulimit_value(soft_s) {
            Some(v) => v,
            None => {
                eprintln!("lightr: --ulimit {entry}: bad soft value '{soft_s}'");
                return Err(2);
            }
        };
        let hard = match hard_s {
            None => soft, // HARD omitted ⇒ hard = soft (Docker).
            Some(h) => match parse_ulimit_value(h) {
                Some(v) => v,
                None => {
                    eprintln!("lightr: --ulimit {entry}: bad hard value '{h}'");
                    return Err(2);
                }
            },
        };
        out.push(Ulimit {
            resource,
            soft,
            hard,
        });
    }
    Ok(out)
}

/// Parse a single `--ulimit` value: `unlimited`/`-1` ⇒ `u64::MAX` (RLIM_INFINITY),
/// else a plain `u64`. `None` on a malformed value (fail-closed at the caller).
pub(super) fn parse_ulimit_value(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if v.eq_ignore_ascii_case("unlimited") || v == "-1" {
        return Some(u64::MAX);
    }
    v.parse::<u64>().ok()
}

/// Map a Docker `--ulimit` TYPE name to its Linux `RLIMIT_*` resource integer.
/// NUMERIC constants (NOT `libc::RLIMIT_*`) so this compiles identically host-side
/// on macOS, where several `RLIMIT_*` numbers/symbols differ. These are the
/// `asm-generic/resource.h` values used by glibc on the COMMON Linux arches
/// (x86_64/aarch64/arm/… — the ns engine + CI target). Verified against the libc
/// crate's `linux_like/linux/gnu` table (NOFILE=7, NPROC=6, AS=9, RSS=5,
/// MEMLOCK=8). NOTE (honest caveat): a few legacy arches (mips/sparc/alpha) use a
/// DIFFERENT numbering for nofile/nproc/as/rss/memlock; this map targets the
/// generic/x86_64 family the ns engine actually runs on — a mips/sparc port would
/// need an arch-gated table (tracked, not in scope here).
pub(super) fn ulimit_resource(ty: &str) -> Option<libc::c_int> {
    let n: libc::c_int = match ty.to_ascii_lowercase().as_str() {
        "cpu" => 0,         // RLIMIT_CPU
        "fsize" => 1,       // RLIMIT_FSIZE
        "data" => 2,        // RLIMIT_DATA
        "stack" => 3,       // RLIMIT_STACK
        "core" => 4,        // RLIMIT_CORE
        "rss" => 5,         // RLIMIT_RSS
        "nproc" => 6,       // RLIMIT_NPROC
        "nofile" => 7,      // RLIMIT_NOFILE
        "memlock" => 8,     // RLIMIT_MEMLOCK
        "as" => 9,          // RLIMIT_AS
        "locks" => 10,      // RLIMIT_LOCKS
        "sigpending" => 11, // RLIMIT_SIGPENDING
        "msgqueue" => 12,   // RLIMIT_MSGQUEUE
        "nice" => 13,       // RLIMIT_NICE
        "rtprio" => 14,     // RLIMIT_RTPRIO
        _ => return None,
    };
    Some(n)
}

/// Parse a `size=` value: a plain byte count or an `N[kKmMgG]` suffix (Docker/
/// runc shape). Returns `None` on a malformed value (fail-closed at the caller).
pub(super) fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let (num, mult) = match v.chars().last().unwrap() {
        'k' | 'K' => (&v[..v.len() - 1], 1024u64),
        'm' | 'M' => (&v[..v.len() - 1], 1024 * 1024),
        'g' | 'G' => (&v[..v.len() - 1], 1024 * 1024 * 1024),
        '0'..='9' => (v, 1),
        _ => return None,
    };
    num.parse::<u64>().ok().map(|n| n * mult)
}
