//! `.dockerignore` build-context exclusion (WP-DF-IGNORE).
//!
//! TRANSCRIBED from Docker/moby's `.dockerignore` semantics (the
//! `moby/patternmatcher` package, formerly `builder/dockerignore` +
//! `pkg/fileutils`). The matcher decides, for a context-RELATIVE path, whether
//! that path is EXCLUDED from the build context — so `COPY`/`ADD` (incl
//! `COPY . /dst`) never see excluded paths AND the memo key (which hashes the
//! context contents) never folds them in.
//!
//! ## Pattern grammar (Docker-faithful)
//! - One pattern per line. `#`-prefixed lines are comments; blank lines are
//!   ignored. (Docker: a `#` only starts a comment at the LINE start.)
//! - Each pattern is `filepath.Clean`'d (`.`/redundant separators collapsed,
//!   trailing slash dropped) and split on `/` into segments.
//! - Per-segment matching is Go `filepath.Match`: `*` matches a run of non-
//!   separator chars, `?` matches one non-separator char. A `**` segment is the
//!   moby extension: it matches ZERO OR MORE path segments (so `**/*.log`
//!   matches a `.log` at any depth, and `a/**` matches everything under `a/`).
//! - A leading `!` makes the pattern an EXCEPTION (re-include). Exclusion is
//!   decided by the LAST pattern that matches a path (last-match-wins), so a
//!   later `!keep.log` re-includes a file an earlier `*.log` excluded.
//!
//! ## Directory rule
//! A pattern that matches a DIRECTORY excludes that directory AND everything
//! under it. We honor this by also treating a path as matched when an ancestor
//! prefix of it matches a (non-exception) pattern — mirroring moby's
//! `MatchesUsingParentResults`/parent-dir short-circuit. A trailing-slash
//! pattern (`dir/`) is cleaned to `dir` and behaves the same (Docker does not
//! distinguish file vs dir patterns by the trailing slash for matching).
//!
//! ## Self-exclusion of `.dockerignore` + the Dockerfile (transcribed + noted)
//! Docker ALWAYS keeps `.dockerignore` and the Dockerfile out of the image even
//! when `COPY .` would otherwise pick them up: the daemon appends implicit
//! patterns `!.dockerignore` and `!<Dockerfile>` so they CANNOT be re-included
//! either, then drops them from the build itself. The net effect callers care
//! about is "`COPY .` does not copy the Dockerfile or `.dockerignore`". We
//! transcribe that net effect directly (see [`DockerIgnore::is_excluded`]): the
//! two control files are unconditionally excluded from the copied context,
//! regardless of the user's patterns. NOTE (ambiguity, minimal choice): Docker's
//! true rule keeps the Dockerfile copyable only when EXPLICITLY named
//! (`COPY Dockerfile /x`); here exclusion is applied to context GLOB/recursion
//! results, and a literal `COPY Dockerfile /x` token is a non-glob literal that
//! bypasses the filter — so the explicit-copy escape hatch is preserved.
use std::path::Path;

/// A single parsed `.dockerignore` rule: a `filepath.Clean`'d, `/`-split
/// pattern plus whether it is an exception (`!`-prefixed).
struct Rule {
    /// `/`-separated, cleaned segments (e.g. `["**", "*.log"]`).
    segments: Vec<String>,
    /// `true` for a `!pattern` exception (re-include).
    exception: bool,
}

/// A compiled `.dockerignore`: the ordered rule list. `matches`/`is_excluded`
/// evaluate a context-relative path against it (last-match-wins).
///
/// `active` (WP-DF-IGNORE) is `true` when a `.dockerignore` file is PRESENT in
/// the context (even if it parses to ZERO rules). It gates the control-file
/// self-exclusion + the executor/key fast-path: Docker's "Dockerfile and
/// `.dockerignore` are always kept out of `COPY .`" rule is a `.dockerignore`-
/// era behavior, so it must NOT fire for a context with NO `.dockerignore`
/// (that case stays byte-identical to before this WP — the default `active=false`).
#[derive(Default)]
pub(crate) struct DockerIgnore {
    rules: Vec<Rule>,
    active: bool,
}

impl DockerIgnore {
    /// Parse `.dockerignore` text into a matcher. Blank + `#`-comment lines are
    /// dropped; a leading `!` flags an exception; each remaining pattern is
    /// cleaned and split into segments. Parsing implies a file is PRESENT, so the
    /// matcher is `active` (the control-file rule fires) even with zero rules.
    pub(crate) fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for raw in text.lines() {
            // Trim a trailing CR (CRLF files); a fully-blank or comment line is
            // skipped (Docker: `#` only starts a comment at the line start).
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (exception, body) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let body = body.trim();
            if body.is_empty() {
                continue;
            }
            let segments = clean_pattern(body);
            if segments.is_empty() {
                continue;
            }
            rules.push(Rule {
                segments,
                exception,
            });
        }
        DockerIgnore {
            rules,
            active: true,
        }
    }

    /// Read `<context>/.dockerignore` if present, returning a compiled matcher.
    /// A missing file ⇒ an INACTIVE matcher (`active=false`): nothing excluded
    /// and the control-file rule does NOT fire, so the build is byte-identical to
    /// a context with no `.dockerignore` (behavior-preserving).
    pub(crate) fn load(context_dir: &Path) -> Self {
        match std::fs::read_to_string(context_dir.join(".dockerignore")) {
            Ok(text) => Self::parse(&text),
            Err(_) => DockerIgnore::default(),
        }
    }

    /// `true` when NO `.dockerignore` is present (the fast path — callers skip the
    /// per-path filter entirely, preserving byte-identical pre-WP behavior). A
    /// PRESENT-but-empty `.dockerignore` is still ACTIVE (the control-file rule
    /// fires), so it is NOT inactive.
    pub(crate) fn is_inactive(&self) -> bool {
        !self.active
    }

    /// Whether a context-RELATIVE path is matched-for-exclusion by the patterns
    /// alone (last-match-wins; does NOT apply the control-file rule). A path is
    /// matched when a non-exception rule matches it OR an ancestor directory of
    /// it (directory exclusion is recursive), and no LATER exception rule
    /// re-includes it. `rel` uses `/` separators with no leading `./` or `/`.
    fn matches(&self, rel: &str) -> bool {
        let path_segs: Vec<&str> = split_rel(rel);
        let mut excluded = false;
        for rule in &self.rules {
            if rule_matches(&rule.segments, &path_segs) {
                excluded = !rule.exception;
            }
        }
        excluded
    }

    /// Whether a context-relative path is EXCLUDED from the build context. This
    /// is the public verdict: the user patterns (via [`Self::matches`]) PLUS the
    /// control-file exclusion of `.dockerignore` and the Dockerfile (Docker's
    /// always-out rule — see the module note). Both apply ONLY when the matcher is
    /// ACTIVE (a `.dockerignore` is present); an INACTIVE matcher (no file)
    /// excludes nothing — not even the control files — so the no-`.dockerignore`
    /// case stays byte-identical to before this WP. `rel` is the context-relative
    /// path (`/`-separated).
    pub(crate) fn is_excluded(&self, rel: &str) -> bool {
        if !self.active {
            return false;
        }
        let norm = rel.trim_start_matches("./").trim_start_matches('/');
        if norm == ".dockerignore" || norm == "Dockerfile" {
            return true;
        }
        self.matches(norm)
    }
}

/// Split a context-relative path into non-empty segments on `/` (and `\` on
/// windows, where `Path` components may use either), dropping `.`.
fn split_rel(rel: &str) -> Vec<&str> {
    rel.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// `filepath.Clean` a pattern body down to `/`-split segments: drop `.` and
/// empty segments and a trailing slash. (We do NOT resolve `..` — Docker leaves
/// it literal; a `..` segment simply won't match a normal context path.)
fn clean_pattern(body: &str) -> Vec<String> {
    body.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_string())
        .collect()
}

/// Does a cleaned pattern (segment list) match a path (segment list)? Handles
/// the `**` any-depth segment; other segments use [`segment_match`]
/// (`filepath.Match` for one component). A pattern also matches when it matches
/// a PREFIX of the path (an ancestor dir) — so excluding `dir` excludes
/// `dir/child` too (recursive directory exclusion).
fn rule_matches(pattern: &[String], path: &[&str]) -> bool {
    match_from(pattern, path, true)
}

/// Recursive matcher with `**` support. `allow_prefix` lets the pattern match an
/// ancestor of the path (directory-exclusion); the `**` recursion threads it
/// through so `a/**` still implies `a` matches `a/b`.
fn match_from(pattern: &[String], path: &[&str], allow_prefix: bool) -> bool {
    if pattern.is_empty() {
        // Whole pattern consumed: a true match iff the whole path is consumed,
        // OR (directory exclusion) the pattern matched a path PREFIX.
        return path.is_empty() || allow_prefix;
    }
    if pattern[0] == "**" {
        // `**` matches zero or more path segments: try consuming the `**` (skip
        // it) and try absorbing one path segment then retrying the same `**`.
        if match_from(&pattern[1..], path, allow_prefix) {
            return true;
        }
        if !path.is_empty() && match_from(pattern, &path[1..], allow_prefix) {
            return true;
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    if !segment_match(&pattern[0], path[0]) {
        return false;
    }
    match_from(&pattern[1..], &path[1..], allow_prefix)
}

/// Go `filepath.Match` for a SINGLE path segment: `*` matches a run of chars,
/// `?` matches exactly one char (neither crosses a separator, which is moot here
/// since `name` is already one segment). Mirrors the house `glob_match`
/// (`exec_fs.rs`) two-pointer wildcard with `*` backtracking, extended so a
/// literal pattern segment compares equal byte-for-byte.
fn segment_match(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
#[path = "dockerignore_tests.rs"]
mod tests;
