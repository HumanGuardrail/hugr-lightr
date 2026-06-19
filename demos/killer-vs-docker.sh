#!/usr/bin/env bash
#
# killer-vs-docker.sh — a narrated tour of the three Lightr killers Docker
# structurally can't match. Pure demo: it only runs REAL `lightr` verbs.
#
#   1. Daemonless        — `ps`/`pgrep` proves 0 resident lightr processes.
#   2. Memoized run      — run the SAME job twice; the 2nd is a HIT (no re-exec).
#   3. Head-to-head      — `lightr bench-compare --vs docker` (SKIP if absent).
#
# Numbers are produced live on THIS machine. Competitors absent from $PATH
# print SKIP — never a fabricated number. See docs/killer-features.md for the
# measured Intel-box figures.
#
# Usage:   demos/killer-vs-docker.sh
# Requires: a `lightr` on PATH (or build one: cargo build --release).

set -euo pipefail

# ── Locate the lightr binary ─────────────────────────────────────────────────
LIGHTR="${LIGHTR_BIN:-lightr}"
if ! command -v "$LIGHTR" >/dev/null 2>&1; then
  # Fall back to a freshly built release binary in this repo, if present.
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  if [ -x "$repo_root/target/release/lightr" ]; then
    LIGHTR="$repo_root/target/release/lightr"
  else
    echo "error: no 'lightr' on PATH and no target/release/lightr built." >&2
    echo "       build it first:  cargo build --release" >&2
    exit 1
  fi
fi

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
run()  { printf '\n$ %s\n' "$*"; eval "$*"; }

say "Lightr vs Docker — three structural killers (live on this machine)"
note "binary: $("$LIGHTR" --version)"

# ── Killer 1: daemonless ─────────────────────────────────────────────────────
say "Killer 1 — DAEMONLESS (0 resident processes)"
note "Docker keeps a daemon + (on macOS) a VM running 24/7 (2-4 GB)."
note "Lightr runs nothing between invocations. Proof:"
if pgrep -fl lightr >/dev/null 2>&1; then
  note "(some lightr process is active right now — e.g. this demo's parent)"
  pgrep -fl lightr || true
else
  printf '\n$ pgrep -fl lightr\n'
  echo "   <nothing> — no resident lightr process. Daemonless."
fi

# ── Killer 2: memoized run — run twice, 2nd is free ──────────────────────────
say "Killer 2 — MEMOIZED RUN (run twice; the 2nd is a cache HIT, no re-exec)"
note "Docker has no memory: it re-does the work every time. Lightr replays it."

demo_dir="$(mktemp -d)"
trap 'rm -rf "$demo_dir"' EXIT
echo "seed" > "$demo_dir/input.txt"

# A job with a visible side effect so a MISS (re-exec) vs HIT (replay) is obvious.
JOB="$LIGHTR run --dir '$demo_dir' --input '$demo_dir/input.txt' -- sh -c 'echo computed-at-\$(date +%s%N)'"

note "First run (expect: memo MISS — does the work):"
run "$JOB"
note "Second run (expect: memo HIT — replays instantly, identical output, no re-exec):"
run "$JOB"
note "The 'lightr: memo HIT key=...' marker on stderr proves the 2nd never re-executed."
note "Container variant: add --engine vz --rootfs <img> and the HIT replays with NO VM boot."

# ── Killer 3: head-to-head table ─────────────────────────────────────────────
say "Killer 3 — HEAD-TO-HEAD vs Docker (install / materialize / cold-run / re-run / idle / build)"
note "Competitors absent from \$PATH print SKIP — never a fabricated number."
if command -v docker >/dev/null 2>&1; then
  run "$LIGHTR bench-compare --vs docker"
else
  note "docker is not on PATH here — running Lightr-only axes (Docker cells will SKIP):"
  run "$LIGHTR bench-compare --vs docker"
fi

say "Done."
note "Measured Intel-box figures + copy-pasteable demos: docs/killer-features.md"
note "Full machine-readable table:  $LIGHTR bench-compare --vs docker --json"
