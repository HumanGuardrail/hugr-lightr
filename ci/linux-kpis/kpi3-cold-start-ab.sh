#!/usr/bin/env bash
# KPI 3 — Real-container cold-start / footprint A/B vs containerd.
#
# SOURCE OF TRUTH: lightr-cri handoff §3
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: starting a REAL container (nginx/agnhost) reaches serving in
# time/footprint at parity-or-better vs containerd, same image + host.
#
# PROBE: extend the lightr-cri bench harness (../lightr-cri/ci/bench.sh, schema
# lightr-cri.bench/v1) in_scope block to drive a real `crictl run` of a pullable
# image and curl it. The harness cold-start / RSS / recovery probes already
# exist — point them at a real workload backed by the real CAS backend.
#
# PASS BAR:
#   - time-to-serving (spawn → first 200 from curl)  <= containerd, OR within
#     the harness budget signed in lightr-cri docs/bench/
#   - RSS / idle footprint                            <= containerd
#   - same image + same host for both sides (A/B fairness)
#
# MEASURE WITH (real backend, on a Linux box):
#   # reuse the sibling harness read-only (DO NOT edit lightr-cri from here):
#   BACKEND=lightr bash ../lightr-cri/ci/bench.sh   # in_scope real workload
#   # the harness drives crictl run <nginx|agnhost>, curls it, records ms + RSS,
#   # and signs a lightr-cri.bench/v1 JSON. Run the containerd side identically.
#
# NOTE: this unblocks the runtime-tier critest networking specs (port-mapping
# ×2, portforward ×2) in ../lightr-cri/ci/critest-skips.txt — they need a real
# image serving HTTP in the pod netns.
#
# DORMANT GUARD: FAILS CLOSED until a Linux runner is attached and the real CAS
# backend can execute a pulled image's binary (the fake cannot). No unmeasured
# time/RSS is ever emitted.
set -euo pipefail

echo "KPI 3 — real-container cold-start/footprint A/B vs containerd"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §3"

HARNESS="../lightr-cri/ci/bench.sh"
if [ ! -f "$HARNESS" ]; then
  echo "::error::lightr-cri bench harness not found at $HARNESS (sibling repo, read-only)."
  exit 1
fi

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 3 not yet wired — real image execution over the CAS backend required."
  echo "Set KPI_BACKEND_READY=1 once the real backend can run a pulled image, then drive"
  echo "the sibling harness read-only: BACKEND=lightr bash $HARNESS"
  echo "Fail-closed: refusing to emit an unmeasured cold-start/RSS number."
  exit 1
fi

# --- real probe goes here (only reached once the backend capability lands) ---
echo "::error::KPI 3 probe body not implemented — invoke the sibling harness read-only with the real backend."
exit 1
