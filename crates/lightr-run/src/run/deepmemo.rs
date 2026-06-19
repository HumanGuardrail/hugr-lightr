//! Deep-memo (opt-in nitro, ADR-0016): deep_memo_available, run_memoized_deep.
//! R4 additions — frozen contract: build-spec-r4.md §1 (bodies: R4-W1)

use lightr_core::Result;
use lightr_store::Store;

use super::memo::run_memoized;
use super::paths::lightr_home;
use super::types::{DeepMemoConfig, RunOutcome, RunSpec};

/// Probe whether the deep-memo spawn-shim mechanism is available on this host.
///
/// Returns `(available, reason)`.
///
/// R4 scope: no prebuilt dylib ships yet, so this always returns `(false,
/// reason)`.  A future WP that ships the dylib flips this to `(true, "")` by
/// (a) checking `$LIGHTR_HOME/shims/lightr_shim.dylib` exists and
/// (b) confirming DYLD injection is allowed for the target interpreter.
/// The caller (CLI W2) is responsible for surfacing `reason` to the user
/// when `available` is false and `--deep-memo` was requested.
///
/// Note: DYLD_INSERT_LIBRARIES injection is blocked for SIP-protected
/// system interpreters (e.g. `/bin/sh`); that check belongs here once
/// a real shim path is probed.
pub fn deep_memo_available() -> (bool, String) {
    // Probe: does the shim dylib exist at $LIGHTR_HOME/shims/lightr_shim.dylib?
    let shim_path = lightr_home().join("shims").join("lightr_shim.dylib");
    if !shim_path.exists() {
        return (
            false,
            format!(
                "deep-memo unavailable (no shim installed at {}) \
                 \u{2014} falling back to whole-run memo",
                shim_path.display()
            ),
        );
    }
    // Shim exists: future WP validates DYLD injection is permitted and
    // loads the dylib. For now, treat presence as insufficient (not yet
    // integrated) and return unavailable.
    (
        false,
        "deep-memo unavailable (shim present but not yet integrated) \
         \u{2014} falling back to whole-run memo"
            .to_string(),
    )
}

/// run_memoized with optional deep-memo (build-spec-r4 §1, ADR-0016).
///
/// Behaviour:
/// - `cfg.enabled == false`: exactly `run_memoized(spec, store)` — no change.
/// - `cfg.enabled == true`: calls `deep_memo_available()`; since R4 ships no
///   prebuilt shim, this always returns `(false, reason)`, so the function
///   falls back to `run_memoized`.  The CLI (W2) surfaces the reason string
///   to the user via `deep_memo_available()`.  **No sub-process memoization
///   is faked; the fallback is to whole-run memo, honestly.**
///
/// The `RunOutcome` returned is identical to `run_memoized` in all cases.
pub fn run_memoized_deep(
    spec: &RunSpec,
    store: &Store,
    cfg: &DeepMemoConfig,
) -> Result<RunOutcome> {
    if !cfg.enabled {
        return run_memoized(spec, store);
    }

    // Probe the shim mechanism.  On this host deep-memo is not yet available
    // (R4 ships no dylib); the CLI is responsible for printing the reason.
    let (_available, _reason) = deep_memo_available();
    // _available is always false in R4; _reason is consumed by the CLI layer.
    // Honest fallback: whole-run memoization, correctness preserved.
    run_memoized(spec, store)
}
