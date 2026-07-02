#!/usr/bin/env bash
# One-command owner gate: fmt + clippy + build + full test suite + push.
# Runs on the owner's box (toolchain + credentials live there; the agent
# sandbox has neither — this script is the whole human step).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== fmt =="
cargo fmt --all -- --check
echo "== clippy (-D warnings) =="
cargo clippy --workspace --all-targets -- -D warnings
echo "== build (debug bin for acceptance) =="
cargo build --workspace
echo "== test (full workspace) =="
cargo test --workspace 2>&1 | tee /tmp/lightr-test.log \
  | grep -E "^test result" \
  | awk '{p+=$4; f+=$6} END{printf "TOTAL: %d passed, %d failed\n", p, f}'
if grep -qE "^test result: FAILED" /tmp/lightr-test.log; then
  echo "FAILING SUITES:"
  grep -B1 -E "^test result: FAILED" /tmp/lightr-test.log | head -20
  exit 1
fi
echo "== push =="
git push origin main
echo "ALL GREEN + PUSHED ✔"
