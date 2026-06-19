# Troubleshooting & FAQ

Short answers to common questions and issues. For the full command surface see
[`docs/commands.md`](commands.md); for the honest platform status see
[`docs/spec/parity-audit.md`](spec/parity-audit.md).

---

## General

**Do I need a daemon running?**
No. Lightr is **daemonless** — nothing of Lightr's runs between invocations.
`pgrep lightr` (or `ps`) returns nothing when you are not actively running a
command. There is no service to start, enable, or keep alive.

**Does it work offline?**
Yes — the core does. `snapshot`, `hydrate`, `status`, `run` (native), `build`,
`gc`, the time-axis verbs, `compose`, the agent surface — none of them touch
the network. The **only** verbs that reach out are `oci pull` and `oci push`
(they talk to a registry). Everything else is local-first and works with no
connection.

**Do I need to log in / create an account?**
No. The local product touches no servers and needs no account.

**Where does Lightr keep its data?**
Under a single home directory:
- Default: `~/.lightr`
- Override: set `LIGHTR_HOME=/some/path`.

Inside it: `store/` (content-addressed objects), `index/`, `run/` (per-run dirs
with logs + control files), `packs/linux/` (installed `vz` pack), `compose/`,
and `units/` (generated supervisor units). Deleting `~/.lightr` resets Lightr
completely.

**Where are a run's logs?**
Per-run, under `~/.lightr/run/<id>/`. The easiest way to read them is
`lightr logs <id>` (add `--stderr`, `--both`, or `-f` to follow). Get the id
from `lightr ps`.

---

## Running containers (the `vz` engine)

**`lightr run --engine vz` fails to start a VM.**
Two prerequisites must both be satisfied on macOS:
1. **A Linux pack is installed.** Run `lightr engine ls` — the `vz` row should
   say `available  vz engine ready (pack: ~/.lightr/packs/linux)`. If it says a
   pack is missing, build and install one:
   `scripts/build-linux-pack.sh` then `lightr engine install-pack <dir>`
   (see [`docs/install.md`](install.md)).
2. **The binary is codesigned with the virtualization entitlement.** A
   `--features vz` binary must be ad-hoc signed:
   `codesign --entitlements packaging/vz.entitlements -s - target/release/lightr`.
   Without it, macOS denies the unentitled process access to the hypervisor.

Also note: `vz` requires `--rootfs <ref>` (the Linux image to boot). `vz`
without `--rootfs`, and the `ns`/`wsl` engines on macOS, return an honest error
rather than pretending to work.

**`engine ls` shows `ns` / `wsl` / `vz` as unavailable.**
That is the honest probe, not a bug. `ns` needs Linux; `wsl` needs Windows +
WSL2; `vz` needs macOS + an installed pack (and a codesigned binary). The
detail column tells you exactly why on this host.

**Is the `vz` engine validated on my machine?**
Only **Intel x86_64 macOS** is runtime-validated end-to-end today. Apple
Silicon `vz`, Linux `ns`, and Windows `wsl` are code-complete but
hardware-gated and **not** claimed validated — see the matrix in
[`docs/spec/parity-audit.md`](spec/parity-audit.md). On those platforms expect
runbook-stage behavior, not a guaranteed boot.

---

## Registry / images

**How do I authenticate to a private registry for `oci pull` / `oci push`?**
Lightr resolves credentials in this order:
1. **`LIGHTR_REGISTRY_AUTH`** — a base64-encoded `user:pass` string; this
   always wins.
2. **`~/.docker/config.json`** — the `auths.<registry>.auth` field (so if
   you have already `docker login`-ed, it just works).
3. **`$DOCKER_CONFIG/config.json`** — used instead of `~/.docker` if
   `DOCKER_CONFIG` is set.

```sh
export LIGHTR_REGISTRY_AUTH="$(printf 'user:pass' | base64)"
lightr oci pull ghcr.io/owner/repo:tag --name @me/img
```

**`oci pull` failed with exit 1.**
Exit **1** is a runtime error — for `oci pull` that typically means a
network/registry failure (auth, 404, rate-limit, connectivity). Exit **2**
instead means the `--name` you gave is an invalid ref. Check the one-line
`lightr: <msg>` on stderr for the specifics.

**Can I import a `docker save` tarball without a registry?**
Yes: `lightr oci import ./image.tar --name @docker/img`. It accepts both an OCI
layout directory and a `docker save` tar, fully offline.

---

## macOS Gatekeeper (downloaded binaries)

**"lightr" cannot be opened because Apple cannot check it for malicious
software / the developer cannot be verified.**
This happens to **downloaded** binaries that are not yet Developer-ID signed
(signed releases are owner-gated and not live yet — see
[`docs/RELEASE.md`](RELEASE.md)). As a stopgap, remove the quarantine
attribute macOS added on download:

```sh
xattr -d com.apple.quarantine /path/to/lightr
```

This is only needed for a binary you downloaded. A binary you built yourself
with `cargo build --release` is not quarantined. (Building from source avoids
the issue entirely.)

---

## Cleanup & disk

**How do I reclaim disk space?**
Use `lightr gc`. It is a **dry-run by default** — it tells you what it *would*
sweep:

```sh
lightr gc                 # preview: "would sweep N objects (B bytes), M run dirs — pass --force"
lightr gc --force         # actually reclaim
```

By default it only sweeps objects older than 3600 s; tune with
`--min-age <secs>`. Use `--json` for `{objects_total, reachable, swept,
bytes_freed, run_dirs_removed}`. `gc` is safe to run while writes are happening
(it takes an exclusive lock so it can't sweep a live write).

**How do I fully reset Lightr?**
Delete the home directory: `rm -rf ~/.lightr` (or `$LIGHTR_HOME`). This removes
the store, index, run dirs, packs, and units — everything.

---

## Reading exit codes

Lightr's exit codes are consistent and scriptable:

| Code | Meaning |
|---|---|
| **0** | OK / clean. |
| **1** | Runtime error, **or** `status` found the directory **dirty** (drifted from the ref), **or** an I/O / integrity / registry failure. |
| **2** | Usage error, **ref-not-found**, or **invalid ref** (clap usage errors are also 2). |
| (child's code) | `run` passes the executed command's exit code straight through. |

So in CI: `lightr status --name @me/proj` exits 0 if the tree matches and 1 if
it drifted; `lightr run -- ./test.sh` exits with whatever `test.sh` returned.
A `2` almost always means "you typed something wrong, or named a ref that
doesn't exist."

---

## Still stuck?

- `lightr <verb> --help` prints the exhaustive, always-current flag list for
  any verb (generated from the live CLI definition).
- `lightr --explain <verb> …` narrates what Lightr is doing (memo keys, CoW
  rung, counts) on stderr.
- `lightr engine ls` tells you exactly which engines are usable here and why.
- The honest, feature-by-feature status ledger is
  [`docs/spec/parity-audit.md`](spec/parity-audit.md).
