# HuGR Lightr — Build Spec R1 (runtime parity + time axis + agent surface)

- **Status:** FROZEN (owner R1→R4 mandate, decisions-log 2026-06-12).
  Additive to build-spec v2: public R0 surfaces do NOT change; new surfaces
  below are law. Agents transcribe; ambiguity ⇒ BLOCKED.
- Features: F-202 (exec/logs/ps/stop), F-008 (gc), F-303 (--mount),
  F-401 (undo/diff), F-402 (bisect), F-503 (plan), F-504 (--events),
  F-505 (mcp). F-203 (limits) deferred to ns/vz tiers (decisions-log).

## 1. FROZEN — `lightr-store` additions (reflog + gc support)

```rust
impl Store {
    /// Append-only ref history. ref_put() now ALSO appends the encoded
    /// record to refs-log/<2hex>/<rest-hex>/<n> (n = next integer, 0-based).
    /// Returns records newest-first; index 0 = current, 1 = previous…
    pub fn ref_log(&self, name: &str) -> Result<Vec<RefRecord>>;

    /// All ref names is not recoverable from hashes — refs-names/<2hex>/<rest>
    /// stores the NAME bytes at ref_put time. Enumerate them.
    pub fn list_refs(&self) -> Result<Vec<String>>;

    /// Enumerate all AC values (decoded by caller).
    pub fn list_ac(&self) -> Result<Vec<Vec<u8>>>;

    /// Delete one object by digest (gc sweep only; objects are 0444 —
    /// chmod then remove). NOT exposed via CLI except `gc`.
    pub fn remove_object(&self, d: &Digest) -> Result<()>;
}
```
ref_put writes three things atomically-enough (temp+rename each): current
ref file (LWW), name record, log entry. Log entries are immutable.

## 2. FROZEN — run control (`lightr-run` additions)

Run instances live at `$LIGHTR_HOME/run/<id>/` where
`id = <unix_nanos>-<pid>` (sortable, unique):
```
spec.json      # {cwd, command, env_keys, mounts, detached, created_at}
pid            # supervisor child's pid (written once child spawns)
status         # "running" | "exited <code>" (written by supervisor)
stdout.log / stderr.log
ctl.sock       # unix socket owned by the supervisor (absent ⇒ not running)
```

```rust
pub struct RunHandle { pub id: String, pub dir: PathBuf }

/// Detached run: re-exec self as `lightr __supervise <dir>`; parent returns
/// immediately with the handle. Detached runs are NEVER memoized (services
/// aren't pure) — documented.
pub fn spawn_detached(spec: &RunSpec, store: &Store) -> Result<RunHandle>;

/// Supervisor body (hidden CLI verb __supervise calls this): spawn child
/// (stdout/stderr → log files), write pid+status, serve ctl.sock
/// (newline-JSON: {"op":"status"}→{"status":...}, {"op":"signal","sig":n}
/// →{"ok":true}), reap, write exit status, exit. No daemon: dies with job.
pub fn supervise(dir: &Path) -> Result<i32>;

pub struct RunInfo { pub id: String, pub running: bool, pub exit_code: Option<i32>,
                     pub command: Vec<String>, pub created_at_unix: u64 }
pub fn ps(store_home: &Path) -> Result<Vec<RunInfo>>;          // scan run dirs
pub fn logs(dir: &Path, stream: LogStream, follow: bool) -> Result<()>; // stream to stdout
pub fn stop(dir: &Path, grace_secs: u64) -> Result<i32>;       // TERM→grace→KILL via ctl/pid
pub enum LogStream { Stdout, Stderr, Both }

/// exec parity (native tier): run `command` with the TARGET run's cwd/env
/// context from spec.json (no isolation boundary to enter natively —
/// documented; the verb keeps CLI parity with the future vz tier).
pub fn exec_in(dir: &Path, command: &[String]) -> Result<i32>;
```

`RunSpec` gains `pub mounts: Vec<Mount>` where
`pub struct Mount { pub ref_name: String, pub target: String }` — before
exec/key-build, each mount is hydrated CoW into `<cwd>/<target>` (target
must be relative + inside cwd; error otherwise). Mount manifests are part
of the memo key (digest of each mount ref's root, in order).

## 3. FROZEN — ops additions (`lightr-index`)

```rust
pub struct GcReport { pub objects_total: u64, pub reachable: u64,
                      pub swept: u64, pub bytes_freed: u64, pub run_dirs_removed: u64 }
/// Mark: closure of every ref-log manifest + every AC record's out/err
/// digests + every manifest object itself. Sweep: unreachable objects;
/// exited run dirs older than min_age_secs. dry_run ⇒ counts only.
pub fn gc(store: &Store, dry_run: bool, min_age_secs: u64) -> Result<GcReport>;

/// Manifest-vs-manifest diff (path-sorted merge; reused by status/diff/bisect).
pub struct DiffReport { pub added: Vec<String>, pub removed: Vec<String>, pub changed: Vec<String> }
pub fn diff_manifests(old: &Manifest, new: &Manifest) -> DiffReport;

/// undo: re-point name to ref_log[1] (error RefNotFound if no history).
pub fn undo(store: &Store, name: &str) -> Result<RefRecord>;

/// bisect: binary-search ref_log indices [0..n) hydrating each candidate
/// into a tempdir and running `cmd` memoized (inputs=[that tempdir]);
/// returns (first_bad_index, record) where cmd exits ≠0 — assumes newest
/// (idx 0) is bad, oldest is good; validates both ends first, errors
/// InvalidRef("bisect: endpoints not bad/good") otherwise.
pub fn bisect(store: &Store, name: &str, cmd: &[String]) -> Result<(usize, RefRecord)>;
```

## 4. FROZEN — CLI additions (`lightr-cli`)

| Verb | Form | Exit |
|---|---|---|
| `run -d` | `lightr run -d … -- cmd` | prints `id=<id>` to stdout, exit 0 (no memo) |
| `__supervise` | hidden | internal |
| `ps` | `lightr ps [--json]` | 0 |
| `logs` | `lightr logs <id> [--stderr|--both] [-f]` | 0; 2 unknown id |
| `stop` | `lightr stop <id> [--grace 10]` | child's exit; 2 unknown id |
| `exec` | `lightr exec <id> -- cmd…` | child's exit; 2 unknown id |
| `gc` | `lightr gc [--force] [--min-age 3600] [--json]` | 0 (dry-run DEFAULT prints plan; --force sweeps) |
| `undo` | `lightr undo --name <ref> [--json]` | 0; 2 no-history/not-found |
| `diff` | `lightr diff --name <ref> [--at 1] [--json]` — ref@{at} vs ref@{0}; with `--dir <p>`: dir vs ref@{0} | 0 same · 1 different · 2 not-found |
| `bisect` | `lightr bisect --name <ref> -- cmd…` | 0 found (prints index+root) · 1 endpoints-invalid · 2 not-found |
| `plan` | `lightr plan <snapshot|hydrate|run> <same args>` | 0; prints would-do (snapshot: files/bytes/new-objects WITHOUT ingesting; hydrate: files/bytes/dest; run: key + HIT/MISS prediction) — read-only, never mutates |
| `mcp` | `lightr mcp` | serves MCP on stdio until EOF |
| `--mount` | `run --mount <ref>:<rel-target>` (repeatable) | 2 on bad grammar/abs target |
| `--events` | global flag: ndjson to stderr `{"ev":"start|end","verb":…,"ok":bool,…}` one start + one end per invocation | — |

`mcp` law (hand-rolled JSON-RPC 2.0 over stdio, Content-Length-free,
line-delimited): handle `initialize` (protocolVersion echo, serverInfo
lightr/<version>, capabilities.tools), `tools/list` → snapshot/hydrate/
status/run/diff with JSON-schema'd inputs mirroring CLI flags, `tools/call`
→ execute and return one text content block with the SAME JSON the
`--json` flag prints. Unknown method → JSON-RPC error -32601. No new deps.

## 5. FROZEN — Acceptance additions (A9–A16, `lightr-acceptance`)

- **A9 detach lifecycle** — `run -d -- sh -c 'echo one; sleep 30'`: id
  printed; `ps` shows running; `logs <id>` contains "one"; `stop <id>`
  exits; `ps` shows exited; **no lightr processes** after stop (A4 law).
- **A10 exec** — detached sleeper; `exec <id> -- /bin/pwd` prints the
  run's cwd; exit 0.
- **A11 gc** — snapshot twice (2 versions); corrupt nothing; `gc` (dry)
  reports 0 sweepable (all reachable via reflog); overwrite ref so old
  version unreachable?? — reflog keeps it reachable BY DESIGN: assert gc
  keeps history. Then `gc --force --min-age 0` after removing… SKIP-trap:
  to create garbage, put_bytes an orphan object via a snapshot to a
  throwaway ref then delete its ref files manually (test does it via fs) →
  gc dry reports 1+ sweepable; --force removes; objects on disk gone;
  store still passes A1 roundtrip for live ref.
- **A12 undo/reflog** — snapshot v1, modify, snapshot v2; `undo` → hydrate
  yields v1 bytes; `diff --name @x --at 1` (now) exits 1 and names the
  changed path… (after undo, @{0}=v1, @{1}=v2 → diff shows change). exit
  codes per table.
- **A13 bisect** — 4 snapshots where a marker file flips good→bad at
  version k; `bisect --name @x -- sh -c 'test ! -f bad.marker'` finds k;
  second bisect run is mostly memo HITs (assert ≥1 `memo HIT` in stderr).
- **A14 plan** — `plan snapshot` on dirty tree prints counts and store
  object count UNCHANGED after; `plan run` predicts MISS, then real run,
  then `plan run` predicts HIT.
- **A15 mcp** — spawn `lightr mcp`, write initialize + tools/list +
  tools/call(status) JSON-RPC lines, read responses: id-matched, tools ≥5,
  call returns valid JSON text block. EOF terminates process (no daemon).
- **A16 events** — `--events run …` stderr contains exactly one
  `"ev":"start"` and one `"ev":"end"` ndjson line each parseable.

## 6. Wave partition (CONFLICT-FREE by crate, same law as v2 §10)

| WP | Owner | Model | Scope |
|---|---|---|---|
| R1-W0 scaffold | lead | — | stub signatures above into crates; clap verbs skeleton |
| R1-W1 | `crates/lightr-store/**` | sonnet | §1 reflog/list/remove + tests |
| R1-W2 | `crates/lightr-run/**` | sonnet | §2 supervisor/ps/logs/stop/exec/mounts + tests |
| R1-W3 | `crates/lightr-index/**` | sonnet | §3 gc/diff/undo/bisect + tests |
| R1-W4 | `crates/lightr-cli/**` | sonnet | §4 verbs + mcp + events + plan |
| R1-W5 | `crates/lightr-acceptance/**` | sonnet | §5 A9–A16 |
| critic | read-only | opus | A9–A16 coverage vs §5 + feature-tree |

Gates per WP and integration identical to v2 §6/§10. Bench: no new
budgets in R1 (gc/ps are not hot paths); B-suite must stay green.
