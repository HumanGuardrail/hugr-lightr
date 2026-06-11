# HuGR Lightr — Build Spec R3 (ecosystem: build, compose, docker compat)

- **Status:** FROZEN (owner R1→R4 mandate). Additive; R0/R1/R2 surfaces
  unchanged. Platform law: RUN steps / services execute via the chosen
  engine; on this Intel box that's `native` (no isolation — stated loudly,
  same as everywhere). The build GRAPH + memoization + compat translation
  are the load-bearing R3 value and are fully testable natively.
- Features: F-306 (build, step-memoized), F-305 (compose lazy), F-307
  (docker CLI compat), F-309 partial (healthcheck/secrets as run-spec).

## 1. New crate

```
crates/lightr-build/   # Dockerfile + compose parsers, build graph, lazy supervisor
```
deps: lightr-core/store/index/run/oci/engine + serde, serde_json. No tokio.

## 2. FROZEN — `lightr-build`: the Dockerfile build

```rust
pub struct BuildStep { pub instr: Instr, pub raw: String }
pub enum Instr {
    From { image_ref: String },     // a lightr ref OR oci import target
    Run { argv: Vec<String> },      // shell form → ["/bin/sh","-c",rest]; exec form JSON array
    Copy { src: Vec<String>, dest: String }, // from build context into the tree
    Env { key: String, val: String },
    Workdir { path: String },
    Cmd { argv: Vec<String> },      // recorded into the image metadata, not run
    Label { key: String, val: String },
}
pub fn parse_dockerfile(text: &str) -> lightr_core::Result<Vec<BuildStep>>;
//  line continuations (\), comments (#), blank lines; unknown instr ⇒
//  InvalidManifest("unsupported instruction: <X>"). ENV/WORKDIR affect later RUN.

pub struct BuildReport { pub name: String, pub root: lightr_core::Digest,
                         pub steps: u64, pub cached_steps: u64 }
/// Each step is content-keyed: key = BLAKE3(prev_layer_root ‖ instr_bytes ‖
/// for COPY: digests of the copied context files). A step whose key is in
/// the AC replays its result tree (cached_steps++); else it executes
/// (RUN via engine into the CoW working tree; COPY materializes context
/// files) and snapshots the new layer, storing layer-root under the key.
/// The final layer is published as ref `name` (parent = previous build).
pub fn build(context_dir: &std::path::Path, dockerfile: &std::path::Path,
             name: &str, engine: lightr_engine::EngineKind, store: &Store)
    -> lightr_core::Result<BuildReport>;
```
Memoization is the obliteration: an unchanged Dockerfile prefix is all
cache hits; only steps at/after the first change re-run (Bazel-class
incrementality on a plain Dockerfile, no config). Determinism caveat
(honesty, in `--explain`): RUN steps that read the clock/network are not
reproducible — recorded but flagged.

## 3. FROZEN — `lightr-build`: lazy compose

```rust
pub struct Service { pub name: String, pub image_ref: String,
                     pub command: Option<Vec<String>>, pub ports: Vec<(u16,u16)>,
                     pub env: Vec<(String,String)>, pub eager: bool }
pub struct Compose { pub services: Vec<Service> }
pub fn parse_compose(yaml: &str) -> lightr_core::Result<Compose>;
//  MINIMAL compose subset (hand-rolled, NO yaml dep): services:, image:,
//  command:, ports: ["H:C"], environment:, x-lightr-eager: true.
//  (Document the supported subset; unknown keys ignored, not errored.)

/// `up`: for each service bind a listener on its host port (a few KB each);
/// the FIRST connection triggers resume/start of that service (detached run
/// via lightr_run::spawn_detached) and proxies the socket through. Eager
/// services start immediately. Returns once listeners are bound (ms).
/// Per-stack ephemeral supervisor (TTL); no resident daemon (ADR-0015).
pub fn compose_up(c: &Compose, store: &Store, ttl_secs: u64) -> lightr_core::Result<ComposeHandle>;
pub struct ComposeHandle { pub stack_dir: std::path::PathBuf, pub services: Vec<String> }
pub fn compose_down(stack_dir: &std::path::Path) -> lightr_core::Result<()>;
```
Lazy law: a service nobody connects to never starts → ~0 RAM. First-packet
latency = the service's start/resume cost (visible, honest). `x-lightr-eager`
or `--eager` opts a service into immediate start.

## 4. FROZEN — docker CLI compat (`lightr-cli`)

`lightr docker <args…>` translates a useful docker subset to lightr verbs
(pure arg translation; prints a one-line note to stderr of the lightr verb
it ran, for transparency):
- `docker build -t <tag> [-f Dockerfile] <ctx>` → `build`
- `docker run [img] [cmd…]` → import-if-needed + `run --engine <default>`
- `docker pull <img>` → `oci pull`
- `docker images` → `engine`-style listing of refs (reuse list_refs)
- `docker ps` → `ps`
- `docker compose up/down` → compose
Unsupported docker subcommand ⇒ exit 2 with "lightr docker: unsupported
'<x>' — supported: build|run|pull|images|ps|compose". NO silent no-op.

## 5. FROZEN — CLI additions

| Verb | Form | Exit |
|---|---|---|
| `build` | `lightr build [-f Dockerfile] [-t <ref>] [--engine k] <context>` | 0 · 2 usage/bad-ref · 1 build error |
| `compose up` | `lightr compose up [-f compose.yml] [--eager] [--ttl 3600]` | 0 (listeners bound) · 1 error |
| `compose down` | `lightr compose down [-f compose.yml]` | 0 |
| `docker` | `lightr docker <args…>` | translated verb's code · 2 unsupported |

`build`/`compose` get `--json` + flow through `--events`. `-t`/`--name`
ref validated.

## 6. FROZEN — Acceptance A22–A26

- **A22 build memoizes** — context with a Dockerfile: `FROM scratch` (empty
  base) / `COPY . /src` / `RUN /bin/sh -c 'echo built > /out'`. `build -t
  @t/b` → steps=3 cached=0; second `build` (unchanged) → cached==steps
  (all hits, side-effect proof: RUN writes to a counter file OUTSIDE the
  tree — count stays 1 across two builds). Change the COPY'd file → rebuild
  → the RUN step re-executes (counter==2), earlier steps still cached.
- **A23 build hydrate** — after A22, `hydrate @t/b <dest>`: `/src/...` +
  `/out` present with expected content.
- **A24 compose lazy** — compose.yml with 2 services binding ports, no
  `--eager`: `compose up` returns in <1 s and `ps`/process check shows
  **zero service processes** running; connect a TCP socket to service-1's
  port → within a poll window a service process appears (lazy start);
  `compose down` → none remain. (Use a trivial service cmd: `/bin/sh -c
  'nc -l <port>'`-style or a tiny echo loop the test can drive — pick a
  portable primitive; document it.)
- **A25 docker compat** — `docker build -t @t/d <ctx>` builds (stderr notes
  the lightr verb); `docker ps` lists; `docker frobnicate` exits 2 with
  the supported-list message.
- **A26 build determinism flag** — a RUN step reading `date` is flagged
  non-reproducible in `build --explain` stderr (recorded, not failed).

## 7. Wave partition

| WP | Owner | Model | Scope |
|---|---|---|---|
| R3-W0 scaffold | lead | — | crate + workspace dep + stubs + clap skeleton |
| R3-W1 | `crates/lightr-build/**` (Dockerfile: parse+build graph) | sonnet | §2 |
| R3-W2 | `crates/lightr-build/**` (compose: parse+lazy supervisor) | sonnet | §3 — SAME crate as W1 ⇒ run W1 then W2 sequential on this crate (NOT parallel; they share lib.rs) |
| R3-W3 | `crates/lightr-cli/**` | sonnet | §4 §5 verbs + docker translation |
| R3-W4 | `crates/lightr-acceptance/**` | sonnet | §6 A22–A26 |
| critic | read-only | opus | suite vs §6 + parity |

NOTE the W1/W2 share-crate constraint: dispatch W1, merge, then W2 on top
(or one agent does both §2+§3 sequentially). W3/W4 parallel after the
build crate lands. Gates/laws per v2 §10.
