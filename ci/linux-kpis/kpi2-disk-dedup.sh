#!/usr/bin/env bash
# KPI 2 — Disk for N similar images (dedup ratio + containerd A/B).
#
# SOURCE OF TRUTH: lightr-cri handoff §2
#   ../lightr-cri/docs/handoff/bench-cas-kpis-request.md
#
# CLAIM TO SIGN: on-disk bytes for N images that share a base scale with the
# SHARED content, not with N × per-image layers.
#
# WHAT IS MEASURED (honest): the CAS object plane on disk —
#   du -sb "$LIGHTR_HOME/store/objects"
# (the content-addressed blob plane; see KPI 1 for why objects/ and not store/).
#
# PROBE:
#   1. build N=4 images by construction: all FROM alpine:latest, each adding ONE
#      unique tiny layer (`RUN echo img-K > /k`); docker save each to a tar.
#   2. per-image ISOLATED stores: import each tar into its OWN fresh cold store,
#      du its objects/ → standalone size. SUM = the honest "no sharing" baseline.
#   3. combined store: import all N tars into ONE fresh cold store → S_lightr.
#   4. dedup_ratio = SUM / S_lightr.
#   5. containerd A/B: import the same N tars into an ISOLATED containerd instance
#      (dedicated --root/--state/--address so the measurement is clean) and
#      du its content store → S_containerd.
#
# PASS BAR:
#   - dedup_ratio > 1            (HARD gate — sharing actually saves disk)
#   - S_lightr <= S_containerd   (SOFT/informational — parity-or-better). lightr
#       stores DECOMPRESSED per-file CAS objects (ready to run, no unpack step)
#       while containerd's content store holds COMPRESSED layer blobs and must
#       additionally unpack to a snapshot to run, so this is not apples-to-apples;
#       it is reported honestly and NEVER hard-fails the job on its own.
#
# DORMANT GUARD: FAILS CLOSED until a Linux runner is attached
# (KPI_BACKEND_READY=1). No unmeasured ratio is ever emitted.
set -euo pipefail

echo "KPI 2 — disk dedup ratio (N similar images) + containerd A/B"
echo "spec: ../lightr-cri/docs/handoff/bench-cas-kpis-request.md §2"

if [ "${KPI_BACKEND_READY:-0}" != "1" ]; then
  echo "::error::KPI 2 not yet wired — set KPI_BACKEND_READY=1 on a Linux runner with"
  echo "a release lightr + docker + containerd on PATH. Fail-closed: no unmeasured ratio."
  exit 1
fi

# ── fail closed + LOUD on any missing required tool ──────────────────────────────
require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "::error::KPI 2 requires '$1' on PATH but it is missing — fail-closed."
    exit 1
  fi
}
LIGHTR_BIN="${LIGHTR_BIN:-lightr}"
if ! command -v "$LIGHTR_BIN" >/dev/null 2>&1 && [ ! -x "$LIGHTR_BIN" ]; then
  echo "::error::KPI 2 requires the 'lightr' binary (set LIGHTR_BIN or put it on PATH) — fail-closed."
  exit 1
fi
require docker
require du
require bc
require ctr          # containerd CLI — presence required; the A/B *result* is soft
require containerd

if ! du -sb . >/dev/null 2>&1; then
  echo "::error::KPI 2 requires GNU 'du -sb' (apparent bytes) — fail-closed."
  exit 1
fi

obj_bytes() {
  local objs="$1/store/objects"
  if [ -d "$objs" ]; then du -sb "$objs" | awk '{print $1}'; else echo 0; fi
}

WORK="$(mktemp -d)"
cleanup() {
  # stop the isolated containerd if we started it
  if [ -n "${CTRD_PID:-}" ]; then sudo kill "$CTRD_PID" 2>/dev/null || true; fi
  sudo rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

IMAGE_A="alpine:latest"
N=4

echo "== step 1: build N=$N images (FROM $IMAGE_A + 1 unique layer) and save =="
docker pull "$IMAGE_A" >/dev/null
TARS=()
for k in $(seq 1 "$N"); do
  ctx="$WORK/ctx$k"
  mkdir -p "$ctx"
  cat >"$ctx/Dockerfile" <<DOCKERFILE
FROM $IMAGE_A
RUN echo "img-$k" > /k
DOCKERFILE
  DOCKER_BUILDKIT=0 docker build -t "lightr-kpi2-$k:latest" "$ctx" >/dev/null
  tar="$WORK/img$k.tar"
  docker save "lightr-kpi2-$k:latest" -o "$tar"
  TARS+=("$tar")
done

echo "== step 2: per-image ISOLATED stores (honest no-sharing baseline) =="
SUM=0
declare -a PER
for k in $(seq 1 "$N"); do
  home="$WORK/iso$k"
  mkdir -p "$home"
  LIGHTR_HOME="$home" "$LIGHTR_BIN" oci import "${TARS[$((k-1))]}" --name "iso$k" >/dev/null
  sz="$(obj_bytes "$home")"
  PER[k]="$sz"
  SUM=$((SUM + sz))
  echo "  image $k standalone objects/ bytes = $sz"
done
echo "SUM (sum of standalone sizes) = $SUM"

echo "== step 3: combined store (all N imported together) =="
COMBINED="$WORK/combined"
mkdir -p "$COMBINED"
for k in $(seq 1 "$N"); do
  LIGHTR_HOME="$COMBINED" "$LIGHTR_BIN" oci import "${TARS[$((k-1))]}" --name "img$k" >/dev/null
done
S_LIGHTR="$(obj_bytes "$COMBINED")"
echo "S_lightr (combined objects/ bytes) = $S_LIGHTR"

if [ "$S_LIGHTR" -le 0 ]; then
  echo "::error::KPI 2 measured S_lightr <= 0 — impossible; fail-closed."
  exit 1
fi
RATIO="$(echo "scale=2; $SUM / $S_LIGHTR" | bc)"
echo "dedup_ratio = SUM / S_lightr = $RATIO"

# ── step 5: containerd A/B (isolated instance; SOFT result) ──────────────────────
echo "== step 5: containerd A/B (isolated --root/--state/--address) =="
S_CONTAINERD="n/a"
CTR_ROOT="$WORK/ctrd-root"
CTR_STATE="$WORK/ctrd-state"
CTR_SOCK="$WORK/ctrd.sock"
sudo mkdir -p "$CTR_ROOT" "$CTR_STATE"
# Start a dedicated containerd so its content store is not polluted by host state.
# Best-effort: any failure here is a SOFT skip (the hard gate is dedup_ratio).
# shellcheck disable=SC2024  # the log redirect is INTENTIONALLY opened by the
# (non-root) shell into the user-owned $WORK dir; the root containerd just writes
# through that fd. We do NOT want the log under root ownership.
sudo containerd --root "$CTR_ROOT" --state "$CTR_STATE" --address "$CTR_SOCK" \
  >"$WORK/ctrd.log" 2>&1 &
CTRD_PID=$!
# wait up to ~15s for the control socket to appear
ctrd_up=0
for _ in $(seq 1 30); do
  if sudo test -S "$CTR_SOCK"; then ctrd_up=1; break; fi
  sleep 0.5
done

if [ "$ctrd_up" -ne 1 ]; then
  echo "WARN  isolated containerd did not come up in time — SOFT skip of the A/B."
  echo "      (last log lines:)"; sudo tail -5 "$WORK/ctrd.log" 2>/dev/null || true
else
  imp_ok=1
  for k in $(seq 1 "$N"); do
    if ! sudo ctr --address "$CTR_SOCK" image import "${TARS[$((k-1))]}" >/dev/null 2>>"$WORK/ctrd.log"; then
      imp_ok=0; break
    fi
  done
  if [ "$imp_ok" -ne 1 ]; then
    echo "WARN  'ctr image import' failed — SOFT skip of the A/B."
    sudo tail -5 "$WORK/ctrd.log" 2>/dev/null || true
  else
    # Measure containerd's content store (compressed layer blobs) — the
    # comparable content plane. Whole-root included as an informational extra.
    CONTENT_DIR="$CTR_ROOT/io.containerd.content.v1.content"
    if sudo test -d "$CONTENT_DIR"; then
      S_CONTAINERD="$(sudo du -sb "$CONTENT_DIR" | awk '{print $1}')"
    else
      S_CONTAINERD="$(sudo du -sb "$CTR_ROOT" | awk '{print $1}')"
    fi
    echo "S_containerd (content store bytes) = $S_CONTAINERD"
  fi
fi

# ── report + gate ──────────────────────────────────────────────────────────────
echo ""
echo "================ KPI 2 — disk dedup ratio ================"
for k in $(seq 1 "$N"); do
  printf '  image %d standalone objects/ bytes  %s\n' "$k" "${PER[$k]}"
done
printf '%-40s %s\n' "SUM (no-sharing baseline)"   "$SUM"
printf '%-40s %s\n' "S_lightr (combined)"         "$S_LIGHTR"
printf '%-40s %s\n' "S_containerd (content store)" "$S_CONTAINERD"
printf '%-40s %s\n' "dedup_ratio = SUM / S_lightr" "$RATIO"
echo "---------------------------------------------------------"

FAIL=0
# HARD gate: sharing must save disk. Integer compare (ratio>1 iff SUM>S_lightr).
if [ "$SUM" -gt "$S_LIGHTR" ]; then
  echo "PASS  dedup_ratio > 1        (ratio=$RATIO — shared base deduped)"
else
  echo "FAIL  dedup_ratio > 1        (ratio=$RATIO — N images did NOT share on disk)"; FAIL=1
fi

# SOFT bar: parity-or-better vs containerd. Informational ONLY — never hard-fails.
if [ "$S_CONTAINERD" = "n/a" ]; then
  echo "SOFT  S_lightr <= S_containerd : SKIPPED (containerd measurement unavailable)"
elif [ "$S_LIGHTR" -le "$S_CONTAINERD" ]; then
  echo "SOFT  S_lightr <= S_containerd : PASS ($S_LIGHTR <= $S_CONTAINERD)"
else
  echo "SOFT  S_lightr <= S_containerd : INFO ($S_LIGHTR > $S_CONTAINERD) — expected:"
  echo "      lightr stores DECOMPRESSED per-file CAS objects (ready to run); containerd's"
  echo "      content store holds COMPRESSED blobs and must still unpack to run. Not a fail."
fi
echo "========================================================="

if [ "$FAIL" -ne 0 ]; then
  echo "::error::KPI 2 FAILED — dedup_ratio bar not met (see table)."
  exit 1
fi
echo "KPI2_RESULT=PASS dedup_ratio=$RATIO SUM=$SUM S_lightr=$S_LIGHTR S_containerd=$S_CONTAINERD"
