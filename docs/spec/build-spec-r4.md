# HuGR Lightr — Build Spec R4 (beyond: deep-memo, schemas, parity polish)

- **Status:** FROZEN (owner R1→R4 mandate, final ring). Additive; all prior
  surfaces unchanged. Platform law unchanged.
- Features: F-403 (deep-memo nitro, opt-in), F-501/507 (versioned JSON
  schemas as a documented artifact + determinism trust), F-602 (bench
  expanded), final parity sweep vs feature-tree.

## 1. Deep-memo (opt-in nitro) — `lightr-run`

```rust
/// Process-tree memoization toggle on a run (build-spec-r4 §1, ADR-0016).
/// macOS path = spawn-shim interposition (DYLD); fragile with static
/// binaries/SIP, hence opt-in. Degrades HONESTLY to whole-run memo when
/// interception can't attach (logged, never silent).
pub struct DeepMemoConfig { pub enabled: bool }
```
- `lightr run --deep-memo -- <cmd>`: when enabled, set up a spawn-shim
  (build a small interposer dylib at install/first-use into
  `$LIGHTR_HOME/shims/`; inject via DYLD_INSERT_LIBRARIES for the child)
  that records each child process's (argv, cwd, read-set) and lets the run
  layer memoize sub-invocations whose key is in the AC. R4 scope: **the
  mechanism + honest fallback**, not a guarantee on every toolchain.
- If the shim can't attach (SIP-protected interpreter, static binary):
  print `lightr: deep-memo unavailable (<reason>) — falling back to
  whole-run memo` to stderr and proceed (exit behavior unchanged).
- Default OFF. The robust FS-view oracle (Linux, ADR-0016) is a future
  ring (needs the views layer) — documented, not built here.

## 2. Versioned JSON schemas (documented artifact) — `docs/spec/json-schemas.md`

A normative doc + a `lightr schema [--verb <v>]` command that prints the
JSON Schema for each verb's `--json` output (and the mcp tool inputs). Each
schema carries `"x-lightr-schema-version": 1`. The schemas mirror the
ACTUAL serialized structs (the doc is generated from / checked against the
real output — a test asserts every verb's `--json` validates against its
declared schema). This is the agent-contract: stable, versioned, testable.

```
lightr schema                 # all schemas as one JSON object {verb: schema}
lightr schema --verb run      # one
```

## 3. Bench expansion — `lightr-cli` bench

Add indicators (measured, machine-class; tense law): B9 oci-import (small
image), B10 build-cached (second build of unchanged Dockerfile — must be
near-zero, the headline incrementality number), B11 compose-up latency
(listeners bound). `--vs-docker` adds the docker column where docker is
present (build-cached vs docker build cache; import vs docker pull). These
are the "obliteration table" rows for the ecosystem features.

## 4. Parity sweep (the closing audit)

`docs/spec/parity-audit.md`: a feature-by-feature table mapping every
feature-tree F-id to its status (done/probe-only/deferred) with the
acceptance test or honest reason. The whitepaper's records table gets its
"measured" column filled from the bench. No new code — this is the
truth-ledger the tense law requires before any ring is claimed publicly.

## 5. CLI additions

| Verb | Form | Exit |
|---|---|---|
| `run --deep-memo` | flag on existing run | child's code (+ fallback note if unavailable) |
| `schema` | `lightr schema [--verb <v>]` | 0; 2 unknown verb |
| `bench` | gains B9–B11 (no new flags) | unchanged |

## 6. Acceptance A27–A30

- **A27 deep-memo honest fallback** — `run --deep-memo -- /bin/echo hi` on
  this host: either deep-memo attaches OR prints the fallback note; EITHER
  WAY exit 0 + stdout "hi". Assert: never crashes, never silently claims a
  capability it lacks (if no "deep-memo" word in stderr, the run still
  memoizes as a whole — second run is HIT). The test asserts the
  fallback-or-attach is loud and correctness holds.
- **A28 schema validates** — for each of snapshot/hydrate/status/run/diff/
  gc: run the verb with `--json`, fetch `schema --verb <v>`, assert the
  output is a JSON object whose keys ⊇ the schema's `required`; `schema`
  (all) parses and every entry has `x-lightr-schema-version`.
- **A29 bench expanded** — `bench --json` array includes B9/B10/B11 ids;
  B10 (build-cached) median < B-something(build-cold) — the incrementality
  claim is measured, not asserted in prose.
- **A30 parity audit present** — a doc-presence + lint test: `docs/spec/
  parity-audit.md` exists and every `F-\d{3}` id in feature-tree.md appears
  in it (no feature silently undocumented). (Pure test reading repo files.)

## 7. Wave partition

| WP | Owner | Model | Scope |
|---|---|---|---|
| R4-W0 scaffold | lead | — | stubs + clap skeleton + schema doc skeleton |
| R4-W1 | `crates/lightr-run/**` | sonnet | §1 deep-memo + fallback |
| R4-W2 | `crates/lightr-cli/**` | sonnet | §2 schema cmd + §3 bench B9–B11 |
| R4-W3 | `crates/lightr-acceptance/**` | sonnet | §6 A27–A30 |
| R4-W4 (lead) | `docs/**` | lead | §4 parity-audit.md + json-schemas.md + whitepaper measured column |
| critic | read-only | opus | final cross-ring parity verdict |

Deep-memo (W1) and schema/bench (W2) touch different crates ⇒ parallel;
W3 authored red-first. Gates/laws per v2 §10. This ring CLOSES the R1→R4
mandate; the closing critic verdicts on the whole product vs the
whitepaper.
