#!/usr/bin/env bash
# KPI 4 — AppArmor conformance (critest, currently skipped).
#
# SOURCE OF TRUTH: lightr-cri handoff §4
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: critest AppArmor specs pass (profile actually applied to the
# container), removing them from ../lightr-cri/ci/critest-skips.txt.
#
# PROBE: run critest's AppArmor specs against the real backend on a Linux box
# whose kernel has AppArmor (GitHub-style Linux runners do). The gap is the
# backend APPLYING the profile at container start — once it does, the specs that
# are currently listed in ../lightr-cri/ci/critest-skips.txt go GREEN.
#
# PASS BAR:
#   - critest AppArmor specs GREEN against the real backend
#   - the corresponding lines are removable from
#     ../lightr-cri/ci/critest-skips.txt (verify, do NOT edit the sibling repo)
#
# MEASURE WITH (real backend, on a Linux box):
#   aa-status                                  # confirm AppArmor is in-kernel
#   # run critest AppArmor specs against `lightr cri serve` (real backend):
#   critest -focus="AppArmor" -runtime-endpoint unix:///run/lightr-cri.sock
#
# DORMANT GUARD: FAILS CLOSED until a Linux runner with AppArmor is attached and
# the real backend applies the LSM profile at container start.
set -euo pipefail

echo "KPI 4 — AppArmor profile applied (critest)"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §4"

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 4 not yet wired — real LSM/AppArmor profile application at container start required."
  echo "Set KPI_BACKEND_READY=1 once the backend applies the profile, then run the"
  echo "critest AppArmor focus against the real lightr cri serve endpoint."
  echo "Fail-closed: refusing to claim AppArmor conformance without a real critest run."
  exit 1
fi

# --- real probe goes here (only reached once the backend capability lands) ---
echo "::error::KPI 4 probe body not implemented — wire critest -focus=AppArmor against the real backend."
exit 1
