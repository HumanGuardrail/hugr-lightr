//! WP-#114: rootless subuid/subgid RANGE resolution for real non-root `--user`
//! on the `ns` engine.
//!
//! The rootless `ns` userns maps a SINGLE id (`"0 <outer> 1"`) — only container
//! root exists inside, so a non-root `--user` cannot be honored (#113 fails it
//! closed). To run a workload as a real non-root in-container uid we need a
//! subordinate-id RANGE map, which an unprivileged process CANNOT write itself
//! (the kernel only allows the single self-map); it must be written from OUTSIDE
//! the userns by the setuid-root `newuidmap`/`newgidmap` helpers, which authorize
//! the request against `/etc/subuid` + `/etc/subgid`.
//!
//! This module is the PURE part of that feature — parsing the subid files and
//! locating the helpers. It has no libc/namespace code, so its unit tests run on
//! any host. The namespace plumbing that consumes it (the parent/child pipe-sync
//! dance) lives in `ns.rs` (Linux-only).

use std::path::PathBuf;

/// A contiguous subordinate-id allocation `[base, base + count)` granted to a user
/// in `/etc/subuid` or `/etc/subgid` (line format `owner:base:count`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubIdRange {
    pub base: u32,
    pub count: u32,
}

/// Parse the FIRST subordinate-id range for `uname`/`uid` out of subid-file
/// CONTENT (pure ⇒ testable). Lines are `owner:base:count`; `owner` matches either
/// the user NAME or the numeric uid (both forms appear in the wild — Debian/Ubuntu
/// write the name, some tooling writes the uid). Blank lines, `#` comments, and
/// malformed / non-numeric lines are skipped; a `count == 0` range is useless and
/// skipped. Returns `None` when the user has no usable allocation.
pub fn parse_subid(content: &str, uname: &str, uid: u32) -> Option<SubIdRange> {
    let uid_s = uid.to_string();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split(':');
        let owner = match f.next() {
            Some(o) => o.trim(),
            None => continue,
        };
        if owner != uname && owner != uid_s {
            continue;
        }
        let base: u32 = match f.next().and_then(|s| s.trim().parse().ok()) {
            Some(b) => b,
            None => continue,
        };
        let count: u32 = match f.next().and_then(|s| s.trim().parse().ok()) {
            Some(c) => c,
            None => continue,
        };
        if count == 0 {
            continue;
        }
        return Some(SubIdRange { base, count });
    }
    None
}

/// Read + parse a subid file from disk (e.g. `/etc/subuid`). A missing / unreadable
/// file ⇒ `None` (the caller falls back to the single-uid honest-error path).
pub fn lookup_subid(path: &str, uname: &str, uid: u32) -> Option<SubIdRange> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_subid(&content, uname, uid)
}

/// Locate a helper binary (`newuidmap` / `newgidmap`) by scanning `$PATH` and then
/// the usual absolute fallbacks. Returns the first existing path, or `None` when
/// the `uidmap` package is not installed.
pub fn find_helper(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            let cand = PathBuf::from(dir).join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let cand = PathBuf::from(dir).join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_by_name() {
        let c = "runner:165536:65536\n";
        assert_eq!(
            parse_subid(c, "runner", 1001),
            Some(SubIdRange {
                base: 165536,
                count: 65536
            })
        );
    }

    #[test]
    fn matches_by_numeric_uid_when_name_differs() {
        // file keyed by uid, lookup name is something else
        let c = "1001:100000:65536\n";
        assert_eq!(
            parse_subid(c, "someone", 1001),
            Some(SubIdRange {
                base: 100000,
                count: 65536
            })
        );
    }

    #[test]
    fn first_matching_range_wins() {
        let c = "runner:10:5\nrunner:999:7\n";
        assert_eq!(
            parse_subid(c, "runner", 0),
            Some(SubIdRange { base: 10, count: 5 })
        );
    }

    #[test]
    fn skips_comments_blanks_and_malformed() {
        let c = "# a comment\n\n  \nrunner:notanum:65536\nrunner:200:0\nrunner:300:9\n";
        // first runner line has a non-numeric base → skip; second has count 0 → skip;
        // third is the first usable range.
        assert_eq!(
            parse_subid(c, "runner", 0),
            Some(SubIdRange {
                base: 300,
                count: 9
            })
        );
    }

    #[test]
    fn no_match_is_none() {
        assert_eq!(parse_subid("alice:1:2\n", "bob", 42), None);
    }

    #[test]
    fn handles_trailing_fields_and_whitespace() {
        // extra colon-fields after count are ignored; surrounding spaces tolerated.
        let c = " runner : 5000 : 1000 : extra \n";
        assert_eq!(
            parse_subid(c, "runner", 0),
            Some(SubIdRange {
                base: 5000,
                count: 1000
            })
        );
    }
}
