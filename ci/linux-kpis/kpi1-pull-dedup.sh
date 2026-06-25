#!/usr/bin/env bash
# KPI 1 — Pull dedup (0-byte re-pull).
#
# SOURCE OF TRUTH: lightr-cri handoff §1
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: content already in the CAS is NOT re-stored on a second pull;
# the first pull writes only the novel blobs.
#
# WHAT IS MEASURED (honest): there is NO network bytes-in counter in lightr, so
# the signed metric is **new bytes written to the CAS object plane** —
#   du -sb "$LIGHTR_HOME/store/objects"
# measured as a delta around each pull/import. We measure the `objects/` plane
# (the content-addressed blob plane) and NOT the whole `store/`, because a second
# pull under a *different ref name* writes a tiny per-ref bookkeeping sidecar
# (imgmanifest/, imgmeta/, refs/) that is a POINTER, not content — including it
# would muddy a pure content-dedup signal. Every content-heavy byte (snapshot
# files, retained compressed layer blobs, config) is content-addressed via
# put_bytes and lands in objects/, so objects/ is the faithful "bytes written to
# the CAS" measure. We DO NOT claim "0 bytes over the network".
#
# PROBE:
#   1. cold CAS                          → baseline objects/ size (= 0)
#   2. pull image A into the COLD CAS    → record bytes written (B_A1)
#   3. pull image A again (diff ref)     → bytes written (B_A2): re-pull dedups
#   4. import image B (FROM A + 1 layer) → bytes written (B_B): only the novel
#                                          layer is stored; the shared A base is
#                                          deduped (per-file CAS objects)
#
# PASS BAR:
#   - B_A1 >  0           (cold pull writes novel blobs)
#   - B_A2 == 0           (re-pull writes ZERO new bytes to the CAS)
#   - 0 < B_B < B_A1      (only B's novel layer is stored; A's base is deduped)
#
# DORMANT GUARD: this script FAILS CLOSED until a Linux runner is attached
# (KPI_BACKEND_READY=1). It must NEVER emit a measured number it did not actually
# measure (tense discipline / Lightr law: no benchmark claimed as measured until
# a real run signs it).
set -euo pipefail

echo "KPI 1 — pull dedup (0 new CAS bytes on re-pull)"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §1"

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 1 not yet wired — set KPI_BACKEND_READY=1 on a Linux runner with"
  echo "a release lightr + docker on PATH. Fail-closed: refusing to emit an unmeasured number."
  exit 1
fi

# ── fail closed + LOUD on any missing required tool (never emit an unmeasured number) ──
require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "::error::KPI 1 requires '$1' on PATH but it is missing — fail-closed."
    exit 1
  fi
}
# `lightr` may be invoked by absolute path via $LIGHTR_BIN (the release binary the
# CI step builds); otherwise it must be on PATH.
LIGHTR_BIN="${LIGHTR_BIN:-lightr}"
if ! command -v "$LIGHTR_BIN" >/dev/null 2>&1 && [ ! -x "$LIGHTR_BIN" ]; then
  echo "::error::KPI 1 requires the 'lightr' binary (set LIGHTR_BIN or put it on PATH) — fail-closed."
  exit 1
fi
require docker
require du

# du -sb is GNU coreutils (apparent size, bytes). Verify the flag is supported
# rather than silently mis-measuring on a non-GNU du.
if ! du -sb . >/dev/null 2>&1; then
  echo "::error::KPI 1 requires GNU 'du -sb' (apparent bytes) — fail-closed."
  exit 1
fi

# Size of the content-addressed CAS object plane under a given LIGHTR_HOME.
# Absent objects/ ⇒ 0 (a freshly-created, never-written store).
obj_bytes() {
  local objs="$1/store/objects"
  if [ -d "$objs" ]; then du -sb "$objs" | awk '{print $1}'; else echo 0; fi
}

WORK="$(mktemp -d)"
export LIGHTR_HOME="$WORK/home"
mkdir -p "$LIGHTR_HOME"
cleanup() { rm -rf "$WORK" 2>/dev/null || true; }
trap cleanup EXIT

IMAGE_A="alpine:latest"

echo "== step 1: cold CAS baseline =="
BASE="$(obj_bytes "$LIGHTR_HOME")"
echo "baseline objects/ bytes = $BASE"

echo "== step 2: cold pull of $IMAGE_A (writes novel blobs) =="
"$LIGHTR_BIN" oci pull "$IMAGE_A" --name a1
AFTER_A1="$(obj_bytes "$LIGHTR_HOME")"
B_A1=$((AFTER_A1 - BASE))
echo "B_A1 (cold pull, new CAS bytes) = $B_A1"

echo "== step 3: re-pull of $IMAGE_A under a different ref (must dedup) =="
"$LIGHTR_BIN" oci pull "$IMAGE_A" --name a2
AFTER_A2="$(obj_bytes "$LIGHTR_HOME")"
B_A2=$((AFTER_A2 - AFTER_A1))
echo "B_A2 (re-pull, new CAS bytes) = $B_A2"

echo "== step 4: build image B = FROM $IMAGE_A + 1 novel layer, save, import =="
# GUARANTEE shared layers by construction: B is built FROM the SAME alpine:latest
# A was pulled from (both resolve docker.io/library/alpine:latest within this run
# → identical base content → identical per-file CAS objects → deduped on import).
# Classic builder (no BuildKit) keeps the image to base + exactly ONE new layer
# and a clean docker-save tar lightr can import.
docker pull "$IMAGE_A" >/dev/null
BCTX="$WORK/bctx"
mkdir -p "$BCTX"
cat >"$BCTX/Dockerfile" <<DOCKERFILE
FROM $IMAGE_A
RUN echo "lightr-kpi1-novel-layer" > /kpi1-b-marker
DOCKERFILE
DOCKER_BUILDKIT=0 docker build -t lightr-kpi1-b:latest "$BCTX" >/dev/null
docker save lightr-kpi1-b:latest -o "$WORK/b.tar"
"$LIGHTR_BIN" oci import "$WORK/b.tar" --name b1
AFTER_B="$(obj_bytes "$LIGHTR_HOME")"
B_B=$((AFTER_B - AFTER_A2))
echo "B_B (import B, new CAS bytes) = $B_B"

# ── report + gate ──────────────────────────────────────────────────────────────
echo ""
echo "================ KPI 1 — CAS pull dedup ================"
printf '%-44s %s\n' "B_A1  cold pull (new CAS bytes)"            "$B_A1"
printf '%-44s %s\n' "B_A2  re-pull SAME image (new CAS bytes)"   "$B_A2"
printf '%-44s %s\n' "B_B   import B (FROM A +1 layer)"           "$B_B"
echo "-------------------------------------------------------"

FAIL=0
if [ "$B_A1" -gt 0 ]; then
  echo "PASS  B_A1 > 0            (cold pull wrote $B_A1 novel bytes)"
else
  echo "FAIL  B_A1 > 0            (cold pull wrote nothing: $B_A1)"; FAIL=1
fi
if [ "$B_A2" -eq 0 ]; then
  echo "PASS  B_A2 == 0           (re-pull wrote ZERO new bytes to the CAS)"
else
  echo "FAIL  B_A2 == 0           (re-pull wrote $B_A2 new bytes — not pure dedup)"; FAIL=1
fi
if [ "$B_B" -gt 0 ] && [ "$B_B" -lt "$B_A1" ]; then
  echo "PASS  0 < B_B < B_A1      (only B's novel layer stored; A base deduped)"
else
  echo "FAIL  0 < B_B < B_A1      (B_B=$B_B vs B_A1=$B_A1 — shared base not deduped)"; FAIL=1
fi
echo "======================================================="
# Honest A/B note (no measurement here to keep KPI 1 self-contained): containerd
# is ALSO content-addressed and dedups a re-pull at its content store — the lightr
# win to emphasize is 0 daemon + 0 idle process, not "we dedup and they don't".
echo "note: containerd also content-dedups re-pulls; lightr's edge is 0 daemon + 0 idle."

if [ "$FAIL" -ne 0 ]; then
  echo "::error::KPI 1 FAILED — see the table above."
  exit 1
fi
echo "KPI1_RESULT=PASS B_A1=$B_A1 B_A2=$B_A2 B_B=$B_B"
