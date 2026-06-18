# The humiliation benchmark — Lightr vs Docker, head-to-head

> **Status:** methodology FROZEN; numbers filled only from an authoritative run
> on named hardware. This document obeys the tense law (`performance-bar.md`,
> ADR-0012): **no number appears here that the harness did not measure on the
> stated box.** A competitor that is absent, or an op that timed out / failed,
> is a printed **SKIP** — never a fabricated number.

The harness is `lightr bench-compare` (`crates/lightr-cli/src/handlers/`). It runs
the SAME user-goal through Lightr and through each competitor present on `$PATH`,
side-by-side, and prints `indicator | lightr | docker | … | factor`, where
`factor = competitor / lightr` (the humiliation multiple) is shown ONLY where
**both** sides were measured.

```
lightr bench-compare --vs docker            # the full table (default: all axes)
lightr bench-compare --vs docker --json     # machine-readable
lightr bench-compare --vs docker --workload cold-run   # one axis
```

## The adversarial axes

Six of the eight `performance-bar.md` indicators have an honest, idiomatic Docker
mirror that is measurable on a dev box. Each row compares each tool's **idiomatic
command for the same user-goal**, over the **same bytes**.

| # | Indicator | User-goal | Lightr (measured) | Docker (measured) |
|---|---|---|---|---|
| 1 | **install footprint** | how big is the install? | size of the single `lightr` binary | `du` of the `Docker.app` bundle on disk |
| 3 | **materialize (CoW)** | get 1 GB of content into a usable dir | `clonefile` hydrate from CAS | `docker cp <container>:/data <dest>` (image carries the same 1 GB) |
| 8 | **cold-run** | run a trivial container once | import a tiny image into a fresh store + run | `docker run --rm alpine:latest true` |
| 4 | **re-run** | run the SAME job again | memo HIT (replay, no re-exec) | `docker run … true` again — Docker has no memo, it re-does the work |
| 2 | **idle processes** | footprint at rest | `ps` count of resident `lightr` procs (= 0, daemonless) | `ps` count of the docker daemon/VM procs |
| 4/8 | **build (memoized 2nd)** | build an unchanged Dockerfile again | memo cache hit | `docker build` again (warm layer cache) |

## Methodology (frozen — the fairness doctrine)

1. **Idiomatic, not rigged.** Each tool runs the command its own users would run
   for that goal. The commands are listed above and documented per-function in
   `bench_compete_docker.rs`. The thesis is structural (CAS + memo + CoW +
   daemonless), so a fair race already wins — fabricating would only destroy
   credibility.
2. **Same bytes.** The Docker probes reuse the exact `pub(crate)` fixture
   builders the Lightr side uses (`build_materialize_fixture`,
   `make_bench_dockerfile`), so the content compared is identical by
   construction, not by claim.
3. **Setup is untimed.** Image pull / build / `docker create` (and Lightr's
   snapshot-into-CAS) are setup. Only the **user-goal op** is timed.
4. **Median-of-N after one warmup**, on both sides (`SAMPLES`), so a single noisy
   sample never sets the number.
5. **Tense law — SKIP, never fabricate.** A competitor absent from `$PATH`, an op
   that times out (every spawned op has a hard wall-clock timeout), or a setup
   that fails, yields a printed `SKIP` with the reason. Lightr is always measured
   (it is the subject).
6. **Spawn-guard.** Competitor containers are spawned ONLY by the real CLI entry.
   Tests and CI run under `ProbePolicy::NeverSpawn`, so a present Docker still
   SKIPs — `cargo test` can never launch a container, even on a docker-equipped
   runner. (Locked by a test.)

## Results

> Filled from an authoritative run. Until then the cells read ⏳ (pending) — they
> are NOT zeros and NOT estimates.

**Run:** ⏳ pending · **box:** ⏳ · **competitors present:** ⏳

| indicator | lightr | docker | factor (docker/lightr) |
|---|---|---|---|
| install footprint | ⏳ | ⏳ | ⏳ |
| materialize (CoW) | ⏳ | ⏳ | ⏳ |
| cold-run | ⏳ | ⏳ | ⏳ |
| re-run | ⏳ | ⏳ | ⏳ |
| idle processes | ⏳ | ⏳ | — (0-baseline: no finite factor) |
| build (memoized 2nd) | ⏳ | ⏳ | ⏳ |

## Honest boundaries (what is NOT claimed here)

- **Snapshot (indicator #5)** and **disk-dedup (#6)** are not in the head-to-head:
  `docker commit` is not a faithful mirror of stat-index snapshot, and a fair
  10-workspace disk-dedup race needs building 10 images. They are measured
  Lightr-side by the `bench` verb; a fair Docker mirror is future work, not a
  claimed win.
- **The absolute ≤10 ms re-run** target (`performance-bar.md` #4) binds to the
  views-O(1) materialization layer on Apple Silicon. The shipped CoW path is
  slower in absolute ms but **still obliterates Docker's re-run**, which re-does
  the work every time (no memo). The head-to-head factor stands on the shipped
  path; the ≤10 ms headline is marked HW-gated wherever it appears.
- **Apple-Silicon headline.** Numbers are measured on the box named in the run
  header. The harness prints, and this doc repeats, that the Apple-Silicon
  headline binds only when run on Apple Silicon.
