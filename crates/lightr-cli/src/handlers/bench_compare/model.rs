//! Data model types and detection helpers for `bench-compare`.

use std::path::PathBuf;

// ──────────────────────────────────────────────────────────────────────────────
// Fixture sizing
// ──────────────────────────────────────────────────────────────────────────────

/// Default `materialize` tree size for a REAL run: 1 GB (build-spec-parity §5.1 —
/// the ~10 MB bench fixture is too small for indicator #3). Tests MUST override
/// this with a tiny size via `MaterializeSize::small()`.
#[derive(Clone, Copy)]
pub(crate) struct MaterializeSize {
    /// number of 1 MiB files to write
    pub(crate) files_1mib: usize,
}

impl MaterializeSize {
    /// 1 GB tree — the real headline workload.
    pub(crate) fn real() -> Self {
        MaterializeSize { files_1mib: 1024 }
    }
    /// Tiny tree for tests (4 MiB) — keeps the unit/integration path fast and
    /// CI-safe. NEVER used for a published number.
    #[cfg(test)]
    pub(crate) fn small() -> Self {
        MaterializeSize { files_1mib: 4 }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Workload selection
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Workload {
    Install,
    Materialize,
    ColdRun,
    ReRun,
    Idle,
    Build,
    ColdImage,
}

impl Workload {
    pub(crate) const ALL: [Workload; 7] = [
        Workload::Install,
        Workload::Materialize,
        Workload::ColdRun,
        Workload::ReRun,
        Workload::Idle,
        Workload::Build,
        Workload::ColdImage,
    ];

    /// Parse the `--workload` flag. `all` ⇒ every workload. Unknown ⇒ `None`.
    pub(crate) fn select(flag: &str) -> Option<Vec<Workload>> {
        match flag {
            "all" => Some(Workload::ALL.to_vec()),
            "install" => Some(vec![Workload::Install]),
            "materialize" => Some(vec![Workload::Materialize]),
            "cold-run" => Some(vec![Workload::ColdRun]),
            "re-run" => Some(vec![Workload::ReRun]),
            "idle" => Some(vec![Workload::Idle]),
            "build" => Some(vec![Workload::Build]),
            "cold-image" => Some(vec![Workload::ColdImage]),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Competitor runtimes
// ──────────────────────────────────────────────────────────────────────────────

/// A competitor container runtime we compare Lightr against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Runtime {
    Docker,
    OrbStack,
    AppleContainer,
}

impl Runtime {
    /// Column header for the table / JSON key.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::OrbStack => "orbstack",
            Runtime::AppleContainer => "container",
        }
    }

    /// Parse a `--vs` token. Accepts the OrbStack aliases `orbstack` and `orb`.
    /// Unknown ⇒ `None` (the caller turns that into an honest error, never a row).
    pub(crate) fn parse(token: &str) -> Option<Runtime> {
        match token {
            "docker" => Some(Runtime::Docker),
            "orbstack" | "orb" => Some(Runtime::OrbStack),
            "container" => Some(Runtime::AppleContainer),
            _ => None,
        }
    }

    /// Binary names to probe on `$PATH`, in order. The first that exists wins.
    pub(crate) fn binaries(self) -> &'static [&'static str] {
        match self {
            Runtime::Docker => &["docker"],
            Runtime::OrbStack => &["orb", "orbstack"],
            Runtime::AppleContainer => &["container"],
        }
    }
}

/// Resolve `--vs` tokens into an ordered, de-duplicated runtime list.
/// Returns `Err(token)` on the FIRST unknown token (fail closed — never silently
/// drop a misspelled competitor and pretend it was skipped).
pub(crate) fn parse_runtimes(vs: &[String]) -> Result<Vec<Runtime>, String> {
    let mut out: Vec<Runtime> = Vec::new();
    for tok in vs {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        match Runtime::parse(t) {
            Some(rt) => {
                if !out.contains(&rt) {
                    out.push(rt);
                }
            }
            None => return Err(t.to_string()),
        }
    }
    Ok(out)
}

/// Find a runtime's binary on `$PATH`. Pure `$PATH` scan (mirrors `bench.rs`
/// `which_docker`); does NOT execute the binary.
pub(crate) fn which_on_path(names: &[&str]) -> Option<PathBuf> {
    let path_os = std::env::var_os("PATH")?;
    which_in(names, &path_os)
}

/// Scan an explicit PATH value for the first existing binary among `names`.
/// Factored out so tests can drive detection WITHOUT mutating the process-global
/// `$PATH` (which would race other parallel tests).
pub(crate) fn which_in(names: &[&str], path_os: &std::ffi::OsStr) -> Option<PathBuf> {
    for name in names {
        for dir in std::env::split_paths(path_os) {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Detection result for one runtime: present (with the resolved path) or absent.
pub(crate) struct Detected {
    pub(crate) runtime: Runtime,
    pub(crate) path: Option<PathBuf>,
}

impl Detected {
    pub(crate) fn present(&self) -> bool {
        self.path.is_some()
    }
}

/// Detect each requested runtime on `$PATH`. Present/absent only — no probing.
pub(crate) fn detect_all(runtimes: &[Runtime]) -> Vec<Detected> {
    runtimes
        .iter()
        .map(|&rt| Detected {
            runtime: rt,
            path: which_on_path(rt.binaries()),
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Cells: a measurement is Measured(value+unit), Skipped(reason), or NA.
// ──────────────────────────────────────────────────────────────────────────────

/// What unit a cell carries — so the table/JSON render honestly and the factor
/// is only computed between like units. (Timings are ms; idle footprint is a
/// process count. RSS-in-MB is a marketing-time extension; not wired here
/// because an honest competitor RSS requires spawning its daemon/VM.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Unit {
    Millis,
    Count,
    /// Megabytes on disk — install footprint (indicator #1).
    Mb,
}

impl Unit {
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Unit::Millis => "ms",
            Unit::Count => "",
            Unit::Mb => "MB",
        }
    }
}

/// A single cell of the table. NEVER a fabricated number: a competitor that is
/// absent (or that could not run the op honestly) is `Skip`, not a guess.
#[derive(Clone, Debug)]
pub(crate) enum Cell {
    /// A real measured value in the row's unit.
    Measured(f64),
    /// Skipped — carries why (absent runtime, unsupported op, timeout).
    Skip(&'static str),
    /// Not applicable for this runtime/row (e.g. there is no "Lightr daemon").
    Na,
}

impl Cell {
    pub(crate) fn measured_value(&self) -> Option<f64> {
        match self {
            Cell::Measured(v) => Some(*v),
            _ => None,
        }
    }
}

/// One row of the head-to-head table.
pub(crate) struct CmpRow {
    pub(crate) indicator: &'static str,
    pub(crate) unit: Unit,
    pub(crate) lightr: Cell,
    /// One cell per requested runtime, aligned to `detected` order.
    pub(crate) competitors: Vec<Cell>,
}

impl CmpRow {
    /// The humiliation multiple for column `i`: competitor / lightr. Only where
    /// BOTH cells are measured AND lightr is non-zero (we never divide by zero,
    /// and a 0-baseline factor would be a fabricated infinity).
    pub(crate) fn factor(&self, i: usize) -> Option<f64> {
        let l = self.lightr.measured_value()?;
        let c = self.competitors.get(i)?.measured_value()?;
        if l > 0.0 {
            Some(c / l)
        } else {
            None
        }
    }

    /// The best (max) factor across competitors — the headline multiple for the
    /// row. `None` if no competitor was measured against a measured lightr.
    pub(crate) fn best_factor(&self) -> Option<f64> {
        (0..self.competitors.len())
            .filter_map(|i| self.factor(i))
            .fold(None, |acc, f| match acc {
                Some(m) if m >= f => Some(m),
                _ => Some(f),
            })
    }
}

/// Whether the head-to-head is allowed to SPAWN competitor containers.
///
/// `Spawn` is set ONLY by the real CLI entry (`run`). Tests and CI construct
/// `NeverSpawn`, so a present competitor is an honest SKIP and `cargo test` can
/// never launch a container — even on a docker-equipped GitHub runner. This is a
/// structural tense-law guard, not a convention.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ProbePolicy {
    Spawn,
    NeverSpawn,
}
