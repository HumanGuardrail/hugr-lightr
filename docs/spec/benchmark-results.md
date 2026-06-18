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
| 3 | **materialize (CoW)** | get 1 GB of content into a usable dir | `clonefile` hydrate from CAS | `docker cp <cid>:/data <dest>` — the same 1 GB is cp'd INTO the container in (untimed) setup, then the timed extract out (full byte copy across the VM) |
| 8 | **cold-run** | run a trivial container once | import a tiny image into a fresh store + run | `docker run --rm alpine:latest true` |
| 4 | **re-run** | run the SAME job again | memo HIT (replay, no re-exec) | `docker run … true` again — Docker has no memo, it re-does the work |
| 2 | **idle processes** | footprint at rest | `ps` count of resident `lightr` procs (= 0, daemonless) | `ps` count of the docker daemon/VM procs |
| 4/8 | **build (memoized 2nd)** | build an unchanged 3-step Dockerfile again | memo cache hit (`FROM scratch`) | `docker build` again, warm cache — an equivalent `FROM alpine` 3-step (Docker cannot build `scratch`+`RUN`; both measure cached-rebuild overhead) |

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

Measured by `lightr bench-compare --vs docker --workload all` on the **release**
binary. Every number was produced by the harness on the stated box; none is an
estimate. Figures are the **median of 3 back-to-back runs**, with the factor
**range** across runs shown so the result is tamper-evident.

**Runs:** 2026-06-18, 3 controlled runs (+1 earlier) · **box:** macOS / x86_64
(Intel), data volume ~94% full (adds noise — disclosed) · **competitors
present:** docker 28.3.2 (linux engine). OrbStack + Apple `container` absent from
`$PATH` → not compared (honest, no fabricated cells).

| indicator | lightr (median) | docker (median) | factor (median) | factor range (3 runs) |
|---|---|---|---|---|
| install footprint | **4.3 MB** | 1962 MB | **452×** | 452× (deterministic) |
| materialize (CoW, 1 GB) | **322 ms** | 38 422 ms | **119×** | 109–144× |
| cold-run (import + run) | **421 ms** | 3 574 ms | **12×** | 7.2–17× |
| re-run (memo hit) | **105 ms** | 3 948 ms | **54×** | 30–65× |
| idle processes | **0** | 8 | **∞** | 0 vs 7–9 (always) |
| build (memoized 2nd) | **17.6 ms** | 1 434 ms | **81×** | 80–203× |

**Verdict: Lightr wins every adversarial axis in every run.** Docker's absolute
ms is noisy (it crosses the macOS VM), so the factors vary — but the direction
never flips and even the **worst of three runs** is decisive: ≥7× cold-run,
≥30× re-run, ≥80× build, ≥100× materialize, 452× install, and a daemonless
0-vs-7+ idle footprint with no finite multiple. The factors, not the absolute ms,
are the result.

## Honest boundaries (what is NOT claimed here)

- **Snapshot (indicator #5)** and **disk-dedup (#6)** are not in the head-to-head:
  `docker commit` is not a faithful mirror of stat-index snapshot, and a fair
  10-workspace disk-dedup race needs building 10 images. They are measured
  Lightr-side by the `bench` verb; a fair Docker mirror is future work, not a
  claimed win.
- **Head-to-head ≠ absolute perf-bar targets.** This table proves the *relative*
  obliteration of Docker on this box. The *absolute* `performance-bar.md` targets
  land thus on this Intel box: **install ≤10 MB — met** (4.3 MB); **materialize
  1 GB ≤100 ms — approached** (248.9 ms for 1024×1 MB files; binds tighter on
  faster storage / Apple Silicon); **re-run ≤10 ms — not met on the shipped CoW
  path** (106.5 ms) — it binds to the views-O(1) layer on Apple Silicon. Every
  absolute gap is still a decisive head-to-head win (160×, 48×): Docker re-does
  the work every time (no memo), so even the un-optimized CoW path crushes it.
- **Apple-Silicon headline.** Numbers are measured on the box named in the run
  header. The harness prints, and this doc repeats, that the Apple-Silicon
  headline binds only when run on Apple Silicon.
