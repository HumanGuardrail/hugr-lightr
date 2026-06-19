# The three killer features Docker structurally can't match

Lightr beats Docker on dozens of axes, but three of them are not "faster" —
they are things Docker **structurally cannot do**. Every number below was
**measured by the `bench-compare` harness on the stated Intel box** and is
quoted verbatim from [`docs/spec/benchmark-results.md`](spec/benchmark-results.md).
Per the repo's tense law: these are measurements on **macOS / x86_64 (Intel)**,
not extrapolations, and not claims about other hardware.

> Run header for every number on this page: **2026-06-18, macOS / x86_64
> (Intel), docker 28.3.2** (median of 3 back-to-back runs; OrbStack + Apple
> `container` were absent from `$PATH`, so they are honestly not compared).

---

## 1. Memoized run / replay — run it twice, the second time is free

The first time you run a job, Lightr does the work and records
`{exit, stdout, stderr}` in its Action Cache, keyed by the content. The second
identical run is a **cache HIT replayed with no re-execution**. Docker has **no
memory** — it re-does the work every single time.

**Measured (Intel box):**

| | Lightr | Docker | Factor |
|---|---|---|---|
| re-run (native, memo hit) | **105 ms** | 3 948 ms | **54×** (range 30–65×) |
| re-run a **Linux container** (`vz`-memo) | **0.014 s** | ~1.30 s | **93×** — and **unbounded** |

The `vz`-memo number is the headline: a memoized **container** run replays from
the Action Cache **with no VM boot at all** — 14 ms regardless of the work it
replaces. Docker re-does the full ~1.3 s every time, and that grows with the
job. So the factor is **unbounded**: 93× on `echo`, far more on a 10-minute
build. Docker structurally cannot do this — it has no memo.

**One-command demo** (run the same job twice; watch the second be a HIT):

```sh
lightr run --input src -- make test    # 1st: memo MISS, does the work
lightr run --input src -- make test    # 2nd: memo HIT, replays instantly
```

The `lightr: memo HIT key=…` marker on stderr proves the second run never
re-executed. (For the container variant: add `--engine vz --rootfs <img>` and
the second run replays with **no VM boot**.)

---

## 2. Daemonless — zero resident processes

Docker keeps a daemon (and, on macOS, a Linux VM) running **24/7**, eating
2–4 GB of RAM whether or not you are using it. Lightr runs **nothing** between
invocations — `ps` proves it.

**Measured (Intel box):**

| | Lightr | Docker | Factor |
|---|---|---|---|
| idle resident processes | **0** | 8 | **∞** (0 vs 7–9, always) |

Zero idle processes means an honest **∞** multiple — there is no finite ratio
to a baseline of zero. Lightr's container modality reaches Docker's *warm*
container-start speed by **booting a VM from cold every time**, paying none of
the always-on idle cost. Same speed when it matters, none of the resident
weight.

**One-command demo** (prove nothing is resident):

```sh
pgrep -fl lightr || echo "no lightr process resident — daemonless"
```

(Compare: `pgrep -fl docker` or `ps aux | grep -i docker` will show the daemon
+ VM processes that never go away.)

---

## 3. Imageless + instant copy-on-write materialize

Docker's model is images and layers; getting content onto disk means copying
byte-for-byte (on macOS, across a VM boundary). Lightr is **imageless** — it
stores content-addressed objects and **CoW-clones** them into place, so
materializing is near-instant and an install is tiny.

**Measured (Intel box):**

| | Lightr | Docker | Factor |
|---|---|---|---|
| install footprint | **4.3 MB** (single binary) | 1 962 MB (`Docker.app`) | **452×** |
| materialize 1 GB (CoW) | **322 ms** | 38 422 ms | **119×** (range 109–144×) |
| get a real OS image ready from cold | **63 ms** (CAS → CoW) | 2 429 ms (`docker pull`) | **38.5×** |

A 452× smaller install, 1 GB materialized in **322 ms** instead of ~38 s, and a
real OS image ready from cold in **63 ms**. Docker has to move the bytes;
Lightr clones them.

**One-command demo** (materialize a ref CoW into a fresh dir, instantly):

```sh
lightr snapshot --dir . --name @me/proj      # content-address it once
lightr hydrate /tmp/fresh --name @me/proj    # CoW materialize — near-instant
```

---

## See it all at once

Run the whole head-to-head on your own machine:

```sh
lightr bench-compare --vs docker            # the full table
lightr bench-compare --vs docker --json     # machine-readable
```

Competitors absent from `$PATH` print **SKIP** — never a fabricated number.
A narrated runner that wraps this plus the memo-twice demo lives at
[`demos/killer-vs-docker.sh`](../demos/killer-vs-docker.sh).

---

## Bonus killers

Two more things Docker doesn't give you out of the box:

### Time-travel over your workspace
Because every snapshot is content-addressed with lineage, Lightr gives you
git-like history over *any* directory:

```sh
lightr undo --name @me/proj            # revert a ref to its previous version
lightr diff --name @me/proj --at 2     # what changed N versions back
lightr bisect --name @me/proj -- sh -c './run-tests.sh'   # find the regression
```

### Agent-native interface
Lightr is built to be driven by an agent or a script, not just a human:

```sh
lightr mcp                             # serve MCP (JSON-RPC 2.0) over stdio
lightr run --json -- make test         # stable machine-readable result on stderr
lightr run --explain -- make test      # self-narration: memo keys, CoW rung, counts
lightr run --events -- make test       # ndjson start/end events on stderr
lightr schema --verb run               # JSON Schema for any verb's --json output
```

`--json`, `--explain`, and `--events` are global flags — they work on every
verb.

---

> **Honesty footer.** Every factor on this page is a measurement from
> `bench-compare` on the **macOS / x86_64 (Intel)** box named in the run header
> (docker 28.3.2), copied from `docs/spec/benchmark-results.md`. Numbers on
> Apple Silicon, Linux, or Windows are **not** claimed here — the `vz` engine
> is runtime-validated end-to-end only on Intel x86_64 today
> (see [`docs/spec/parity-audit.md`](spec/parity-audit.md)). Docker's absolute
> milliseconds are noisy because they cross a VM; the **factors**, not the raw
> ms, are the result, and across three runs the direction never flips.
