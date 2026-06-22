#!/usr/bin/env bash
# KPI 1 — Pull dedup (0-byte re-pull).
#
# SOURCE OF TRUTH: lightr-cri handoff §1
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: content already in CAS re-transfers 0 bytes on a second pull;
# first pull transfers only the novel blobs.
#
# PROBE:
#   1. pull image A into a COLD CAS   → record bytes-in (B_A1)
#   2. pull image A again            → assert bytes-in ~= 0           (B_A2)
#   3. pull image B sharing layers with A → assert only B's novel bytes (B_B)
#
# PASS BAR:
#   - B_A2 == 0            (re-pull moves zero bytes)
#   - B_B  <  B_A1         (shared layers are not re-fetched)
#
# MEASURE WITH (real backend, on a Linux box):
#   lightr pull <A>   # cold; bytes-in via the CAS fetch path / a bytes-in counter
#   lightr pull <A>   # warm; assert 0
#   lightr pull <B>   # shares layers with A; assert only novel blobs
# A/B vs containerd on identical OCI images (same runner, same images).
#
# DORMANT GUARD: this script FAILS CLOSED until (a) a Linux runner is attached
# and (b) the real PullImage-over-CAS bytes-in counter is wired. It must NEVER
# emit a measured number it did not actually measure (tense discipline / Lightr
# law: no benchmark claimed as measured until a real run signs it).
set -euo pipefail

echo "KPI 1 — pull dedup (0-byte re-pull)"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §1"

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 1 not yet wired — real PullImage-over-CAS bytes-in counter required."
  echo "Set KPI_BACKEND_READY=1 once the real backend exposes a bytes-in counter and"
  echo "fill in the probe below. Fail-closed: refusing to emit an unmeasured number."
  exit 1
fi

# --- real probe goes here (only reached once the backend capability lands) ---
echo "::error::KPI 1 probe body not implemented — wire the lightr pull bytes-in measurement."
exit 1
