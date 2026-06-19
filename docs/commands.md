# Lightr CLI reference

The complete command surface of the `lightr` binary. This is a reference —
every verb, its key flags, its exit-code meaning, and a real example. The
authoritative source for the surface is the clap definition in
`crates/lightr-cli/src/main.rs`; this document tracks it.

Run `lightr <verb> --help` for the exhaustive per-verb flag list (generated
live from the same definition).

## Conventions

- `--` separates Lightr's own flags from the command you want it to run.
  Anything after `--` is passed through verbatim (e.g.
  `lightr run -- echo hello`).
- Flags shown as `[default: X]` may be omitted.
- "ref" / "name" means a Lightr store reference, e.g. `@me/proj`,
  `@docker/alpine`. Ref names are lowercase; an invalid ref name exits **2**.

## Global flags

These work on every verb (place them before the verb, e.g. `lightr --json …`):

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output with stable keys. |
| `--explain` | Structured self-narration to **stderr** (memo keys, CoW rung, counts), lines prefixed `lightr: explain `. |
| `--events` | Emit JSON-RPC start/end events to **stderr** (ndjson). |
| `-h, --help` | Print help. |
| `-V, --version` | Print version (`lightr <ver> (<git-sha>, <build-date>)`). |

## Environment variables

| Variable | Meaning |
|---|---|
| `LIGHTR_HOME` | Lightr's data directory. Default `~/.lightr`. |
| `LIGHTR_REGISTRY_AUTH` | Base64 `user:pass` for registry auth; **wins over** `~/.docker/config.json`. Used by `oci pull` / `oci push`. |
| `DOCKER_CONFIG` | If set, Lightr reads `$DOCKER_CONFIG/config.json` for registry creds instead of `~/.docker/config.json`. |

## Data directory layout (`~/.lightr`)

`store/` (content-addressed objects), `index/`, `run/` (per-run dirs: logs +
control), `packs/linux/` (installed `vz` pack), `compose/` (compose stacks),
`units/` (generated supervisor units). Delete the directory to reset Lightr.

## The exit-code law

Lightr uses a small, consistent exit-code contract
(`crates/lightr-cli/src/exit.rs`):

| Code | Meaning |
|---|---|
| **0** | OK / clean. |
| **1** | Runtime error, or `status` reports the directory is **dirty** (drifted from the ref). Also: any `LightrError` that is not a ref problem (I/O, integrity, network/registry failure). |
| **2** | Usage error, **ref-not-found**, or **invalid ref** — clap usage errors also exit 2. |
| (passthrough) | `run` passes the **child command's** exit code straight through (e.g. `lightr run -- sh -c 'exit 7'` exits 7). |

---

# Core store verbs

## `snapshot`

Snapshot a directory into the store under a ref.

```
lightr snapshot [--dir <DIR>] --name <REF>
```

- `--dir <DIR>` directory to snapshot `[default: .]`
- `--name <REF>` **required** ref to store it under
- Exit: 0 ok; 2 invalid ref; 1 I/O error.

```sh
lightr snapshot --dir . --name @me/proj
```

## `hydrate`

Materialize a ref into a directory using copy-on-write.

```
lightr hydrate <DEST> --name <REF> [--verify]
```

- `<DEST>` **required** destination directory (positional)
- `--name <REF>` **required** ref to materialize
- `--verify` re-hash every object before materializing (paranoid path)
- Exit: 0 ok; 2 ref-not-found / invalid ref; 1 I/O / integrity error.

```sh
lightr hydrate /tmp/fresh --name @me/proj
```

## `status`

Compare a directory against a ref.

```
lightr status [--dir <DIR>] --name <REF>
```

- `--dir <DIR>` `[default: .]`, `--name <REF>` **required**
- Exit: **0 if clean, 1 if dirty** (drifted); 2 invalid/missing ref.
- With `--json`: `{clean, added, removed, changed}`.

```sh
lightr status --name @me/proj --json
```

## `gc`

Garbage-collect unreachable objects. **Dry-run by default.**

```
lightr gc [--force] [--min-age <SECS>] [--json]
```

- (no flag) dry-run — reports what *would* be swept.
- `--force` actually sweep.
- `--min-age <SECS>` only sweep objects older than this `[default: 3600]`.
- `--json` → `{objects_total, reachable, swept, bytes_freed, run_dirs_removed}`.
- Exit: 0 ok; 1 error.

```sh
lightr gc                 # preview
lightr gc --force         # actually reclaim
```

---

# Run / execution

## `run`

Run a command, memoized. The exit code passes through from the child.

```
lightr run [OPTIONS] -- <COMMAND>...
```

Key flags:

| Flag | Meaning |
|---|---|
| `--dir <DIR>` | Working directory `[default: .]`. |
| `--input <PATH>` | Declare an input path (repeatable) — part of the memo key. |
| `--env <NAME>` | Declare an env var name to include in the key (repeatable). |
| `-d, --detach` | Spawn a detached run; prints `id=<id>`, exits 0. |
| `-p, --publish <HOST:CONTAINER>` | Publish a port (repeatable). **Requires `-d`.** |
| `--mount <REF:TARGET>` | Mount a store ref into the run cwd at relative `TARGET` (repeatable). |
| `--engine <ENGINE>` | `native` (default) · `ns` · `vz`. |
| `--rootfs <REF>` | Hydrate a ref CoW into a temp dir and hand it to the engine as rootfs. **Incompatible with `native` → exit 2.** |
| `--deep-memo` | Process-tree memoization (opt-in; honest fallback to whole-run memo). |
| `--memory <SIZE>` | Memory cap (`512m`, `1g`, `2048k`, or bare bytes). |
| `--cpus <N>` | CPU cap as core count (`0.5`, `1`, `1.5`). |
| `--secret <NAME=REF>` | Inject a store-backed secret file (repeatable). |
| `--config <NAME=REF>` | Inject a store-backed config file (repeatable). |
| `--health-cmd <CMD>` | Healthcheck command (probed when detached). |
| `--health-interval <SECS>` | `[default: 30]`. |
| `--health-retries <N>` | `[default: 3]`. |

Behavior:

- Prints a memo marker to **stderr** before output: `lightr: memo HIT key=<hex16>`
  or `lightr: memo MISS key=<hex16>`. A HIT replays cached stdout/stderr/exit
  with no re-execution.
- With `--json`: child streams still flow; a final `{"key","hit","exit_code"}`
  line is written to **stderr** prefixed `lightr-json: `.
- Memoized paths: (a) `native` without `--rootfs`; (b) `vz` + `--rootfs` and
  **not** detached (the `vz`-memo path — a HIT replays with **no VM boot**).
  All other engine combinations run unmemoized.
- Exit: **the child's exit code**; 2 for bad flags (e.g. `native`+`--rootfs`,
  `-p` without `-d`, a bad `--engine` string, a bad `--mount`/`--secret` value).

```sh
lightr run --input src -- make test                          # native, memoized
lightr run --rootfs @docker/alpine -- echo hi                # CoW rootfs (native)
lightr run -d -p 8080:80 --engine vz --rootfs @img -- /server  # detached vz service
lightr run --engine vz --rootfs @docker/alpine -- /bin/sh -c 'exit 7'  # → 7
```

## `exec`

Exec a command in an existing run's context.

```
lightr exec <ID> -- <COMMAND>...
```

- `<ID>` **required** run id; command after `--` is **required**.
- Exit: passes through / 2 on unknown id or bad grammar.

```sh
lightr exec 1781836130249671000-92679 -- ps aux
```

## `ps`

List running / exited run instances.

```
lightr ps [--json]
```

```sh
lightr ps
# 1781836130249671000-92679  exited 143  sh
```

## `logs`

Stream logs from a run.

```
lightr logs <ID> [--stderr] [--both] [-f|--follow]
```

- `<ID>` **required**. Default streams stdout; `--stderr` streams stderr;
  `--both` streams both; `-f/--follow` tails.
- Exit: 0 ok; 2 unknown id.

```sh
lightr logs 1781836130249671000-92679 -f
```

## `stop`

Stop a running instance.

```
lightr stop <ID> [--grace <SECS>]
```

- `<ID>` **required**; `--grace <SECS>` grace period before kill `[default: 10]`.
- Exit: 0 ok; 2 unknown id.

```sh
lightr stop 1781836130249671000-92679 --grace 5
```

---

# Time-axis verbs

## `undo`

Revert a ref to its previous version.

```
lightr undo --name <REF> [--json]
```

- `--name <REF>` **required**. Exit: 0 ok; 2 ref-not-found.

```sh
lightr undo --name @me/proj
```

## `diff`

Diff a ref against a previous version (or against a directory).

```
lightr diff --name <REF> [--at <N>] [--dir <DIR>] [--json]
```

- `--name <REF>` **required**; `--at <N>` how many versions back `[default: 1]`;
  `--dir <DIR>` diff against a directory instead.
- `--json` → `{added, removed, changed}`.

```sh
lightr diff --name @me/proj --at 2
```

## `bisect`

Binary-search a ref's history to find a regression.

```
lightr bisect --name <REF> -- <COMMAND>...
```

- `--name <REF>` **required**; the test command after `--` is **required**
  (exit 0 = good, non-zero = bad, the usual bisect convention).

```sh
lightr bisect --name @me/proj -- sh -c './run-tests.sh'
```

---

# OCI / images

## `oci import`

Import an OCI layout directory or a `docker save` tar into the store.

```
lightr oci import <PATH> --name <REF> [--json]
```

- `<PATH>` **required** (OCI layout dir or tar); `--name <REF>` **required**.
- Output: `name=<ref> root=<hash16> layers=<n> files=<n>`.
- Exit: 0 ok; 2 invalid ref; 1 I/O / malformed image.

```sh
lightr oci import ./alpine.tar --name @docker/alpine
```

## `oci pull`

Pull an image from a registry and import it.

```
lightr oci pull <IMAGE> --name <REF> [--json]
```

- `<IMAGE>` e.g. `alpine`, `nginx:1.25`, `ghcr.io/owner/repo:tag`.
- Private registries: see `LIGHTR_REGISTRY_AUTH` / `~/.docker/config.json` above.
- Exit: 0 ok; 2 invalid ref; **1 on network / registry failure**.

```sh
lightr oci pull alpine:latest --name @docker/alpine
```

## `oci push`

Push a stored ref to a registry as a synthesized single-layer OCI image.

```
lightr oci push <STORE_REF> <TARGET> [--json]
```

- `<STORE_REF>` the local ref (e.g. `@me/img`); `<TARGET>` the registry ref
  (e.g. `ghcr.io/owner/repo:tag`). Both positional, **required**.
- Output: `target=<t> manifest=<digest> layers=<n> size=<bytes>`.
- Exit: 0 ok; 2 invalid / unknown store ref; 1 registry / I/O failure.

```sh
lightr oci push @me/img localhost:5000/myimg:latest
```

---

# Build / compose / compat

## `build`

Build an image from a Dockerfile, step-memoized.

```
lightr build <CONTEXT> [-f <FILE>] [-t <NAME>] [--engine <ENGINE>]
```

- `<CONTEXT>` **required** build-context dir.
- `-f, --file <FILE>` Dockerfile path `[default: <context>/Dockerfile]`.
- `-t, --name <NAME>` ref to store the result under `[default: latest]`.
- `--engine <ENGINE>` `native` (default) · `ns` · `vz`.
- Exit: 0 ok; 1 build error; 2 usage.

```sh
lightr build . -t @app/web
```

## `compose up` / `compose down`

Manage a compose stack with **lazy** services (nothing runs until connected).

```
lightr compose up   [-f <FILE>] [--eager] [--ttl <SECS>]
lightr compose down [-f <FILE>]
```

- `up`: `-f` compose file `[default: compose.yml]`; `--eager` start everything
  immediately (override lazy); `--ttl <SECS>` supervisor TTL `[default: 3600]`.
- `down`: `-f` identifies the stack (defaults to the newest stack dir).

```sh
lightr compose up -f compose.yml
lightr compose down
```

## `docker`

Docker CLI compatibility shim — translates a docker subcommand to lightr and
prints the translation to stderr (`lightr docker: → lightr <verb> …`).

```
lightr docker <ARGS>...
```

- Supported subset: **`build` · `run` · `pull` · `images` · `ps` · `compose`**.
- An unsupported subcommand exits **2** with:
  `lightr docker: unsupported '<x>' — supported: build|run|pull|images|ps|compose`.

```sh
lightr docker build -t myref .
lightr docker images
```

---

# Engine management

## `engine ls`

List available engines and their honest availability.

```
lightr engine ls [--json]
```

```sh
lightr engine ls
# native    available     native process execution (no isolation — not a sandbox)
# vz        available     vz engine ready (pack: ~/.lightr/packs/linux)
```

## `engine install-pack`

Install a Linux kernel+initrd pack for the `vz` engine.

```
lightr engine install-pack <DIR>
```

- `<DIR>` **required** — a directory containing `kernel` and `initrd` (and
  optionally `pack.json`). The pack is structurally validated before install
  and copied to `~/.lightr/packs/linux/`.
- Exit: 0 ok; **1** if a file is missing or the pack is malformed.

```sh
lightr engine install-pack build/linux-pack
# → installed linux pack → ~/.lightr/packs/linux
```

---

# Restart policies (no daemon of ours)

## `supervise install` / `uninstall` / `list`

Generate an **OS-supervisor** unit (launchd on macOS, systemd user unit on
Linux) for a restart policy. Lightr ships **no daemon**; this writes a unit
under `~/.lightr/units/` and prints the opt-in command you run to load it. It
never auto-loads anything.

```
lightr supervise install --name <NAME> [--restart <POLICY>] [--dir <DIR>] -- <COMMAND>...
lightr supervise uninstall --name <NAME>
lightr supervise list
```

- `--name <NAME>` **required**.
- `--restart <POLICY>` `no | always | on-failure[:N] | unless-stopped`
  `[default: always]`.
- `--dir <DIR>` working dir `[default: .]`; command after `--` is **required**.
- Windows is an honest `Unsupported` error (Task Scheduler is a future ring).

```sh
lightr supervise install --name web --restart on-failure:5 -- /usr/local/bin/server
lightr supervise list
lightr supervise uninstall --name web
```

---

# Benchmarks

## `bench`

Measure the indicator table on **this** machine.

```
lightr bench [--vs-docker] [--check] [--json]
```

- `--vs-docker` include a docker version-overhead probe.
- `--check` enforce the CI perf budget (exit non-zero if a target is missed).

```sh
lightr bench --check
```

## `bench-compare`

Head-to-head "humiliation" benchmark vs Docker / OrbStack / Apple `container`
on identical workloads. Competitors absent from `$PATH` print **SKIP** (never a
fabricated number).

```
lightr bench-compare [--vs <list>] [--workload <name>] [--json]
```

- `--vs <list>` comma-separated `[default: docker,orbstack,container]`.
- `--workload <name>` one of `all` (default), `install`, `materialize`,
  `cold-run`, `re-run`, `idle`, `build`, `cold-image`.

```sh
lightr bench-compare --vs docker
lightr bench-compare --vs docker --workload re-run --json
```

See `docs/spec/benchmark-results.md` for measured numbers (Intel box).

---

# Agent-facing surface

## `mcp`

Serve the MCP (Model Context Protocol) over stdio — line-delimited JSON-RPC
2.0. Reads requests on stdin, writes responses on stdout; EOF exits 0.

```
lightr mcp
```

## `schema`

Print the JSON Schema for a verb's `--json` output.

```
lightr schema [--verb <VERB>]
```

- No `--verb`: prints schemas for all verbs (one JSON object).
- `--verb <VERB>`: just that verb. An **unknown verb exits 2**.

```sh
lightr schema --verb run
```

## `completions`

Print a shell completion script to stdout.

```
lightr completions <SHELL>
```

- `<SHELL>` one of `bash | zsh | fish | powershell | elvish` (required; an
  unknown shell exits 2).

```sh
lightr completions zsh > ~/.zfunc/_lightr
```

## `man`

Print the roff man page to stdout.

```sh
lightr man > /usr/local/share/man/man1/lightr.1
```

---

## Plan (dry-run)

Dry-run planning operations — predict the effect without writing to the store.

```
lightr plan snapshot [--dir <DIR>] --name <REF>
lightr plan hydrate  <DEST> --name <REF>
lightr plan run      [--dir <DIR>] [--input ...] [--env ...] [--mount ...] -- <COMMAND>...
```

- `plan run` predicts memoization (would it be a HIT or MISS) for a run.

```sh
lightr plan run --input src -- make test
```

---

> Note: `lightr` also defines two **internal, hidden** subcommands used only by
> Lightr to supervise its own detached runs and compose stacks
> (`__supervise`, `__compose-supervise`). They are implementation details, not
> part of the user-facing CLI, and you should never invoke them directly.
