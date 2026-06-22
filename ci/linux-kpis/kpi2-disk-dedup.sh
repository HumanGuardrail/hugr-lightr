#!/usr/bin/env bash
# KPI 2 — Disk for N similar images (dedup ratio).
#
# SOURCE OF TRUTH: lightr-cri handoff §2
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: on-disk bytes for N images scale with SHARED content, not
# N × per-image layers.
#
# PROBE:
#   1. import N images with overlapping layers into the CAS store
#   2. measure CAS on-disk size                                  (S_lightr)
#   3. measure containerd snapshotter store for the same set     (S_containerd)
#   4. report dedup ratio = (sum of per-image sizes) / S_lightr
#
# PASS BAR:
#   - dedup ratio > 1                       (sharing actually saves disk)
#   - S_lightr <= S_containerd (parity-or-better) for the same image set
#
# MEASURE WITH (real backend, on a Linux box):
#   for img in $IMAGES; do lightr pull "$img"; done   # real image import into CAS
#   du -sb "$LIGHTR_CAS_ROOT"                          # S_lightr
#   du -sb "$CONTAINERD_SNAPSHOT_ROOT"                 # S_containerd (A/B)
#
# DORMANT GUARD: FAILS CLOSED until a Linux runner is attached and real image
# import into the CAS store is wired. No unmeasured ratio is ever emitted.
set -euo pipefail

echo "KPI 2 — disk dedup ratio (N similar images)"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §2"

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 2 not yet wired — real image import into the CAS store required."
  echo "Set KPI_BACKEND_READY=1 once import lands; fill in the du-based probe below."
  echo "Fail-closed: refusing to emit an unmeasured dedup ratio."
  exit 1
fi

# --- real probe goes here (only reached once the backend capability lands) ---
echo "::error::KPI 2 probe body not implemented — wire the CAS on-disk du measurement + containerd A/B."
exit 1
