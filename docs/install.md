# Installing Lightr

Lightr is a single static binary — `lightr` — with no daemon and no runtime
services. "Install" means: get the binary onto your `PATH`, and (only if you
want to run Linux containers in a microVM on macOS) install one Linux pack and
codesign the binary with the virtualization entitlement.

> **Honesty note.** Pre-built binaries, Homebrew, and crates.io are
> **available on release** but currently **owner-gated** (`G-PUBLISH`). The
> binary/crate names are cleared and the packaging metadata is ready, but no
> public release has shipped yet — do not expect `brew install lightr` or
> `cargo install lightr` to work today. The runbook the owner follows to
> publish is [`docs/RELEASE.md`](RELEASE.md). Until then, **build from
> source** (below). Likewise, only the **Intel x86_64 macOS `vz`** path is
> runtime-validated end-to-end; the other platform engines are code-complete
> but hardware-gated — see the matrix at the bottom.

---

## Prerequisites

- **Rust toolchain.** The repo pins its toolchain in
  [`rust-toolchain.toml`](../rust-toolchain.toml); `rustup` reads it
  automatically when you build inside the repo. If you do not have `rustup`,
  install it from <https://rustup.rs>.
- **Git**, to clone the repo.
- **(macOS, `vz` engine only)** Apple's Virtualization.framework (built into
  macOS) plus the ability to ad-hoc codesign (`codesign`, ships with Xcode
  command-line tools). Building the Linux pack additionally needs a Linux
  cross-toolchain / Docker — see [`docs/build.md`](build.md).

---

## Install from source

From the repository root:

```sh
# Build the release binary (all engines except vz):
cargo build --release

# The binary lands at:
#   target/release/lightr
```

Put it on your `PATH` (pick one):

```sh
# Option A — symlink into a dir already on PATH
ln -sf "$(pwd)/target/release/lightr" /usr/local/bin/lightr

# Option B — copy it
cp target/release/lightr /usr/local/bin/lightr
```

Verify:

```sh
lightr --version
# → lightr 0.1.0 (<git-sha>, <build-date>)
```

`--version` embeds the git SHA and build date so you always know exactly which
commit a binary came from.

### Enabling the Linux-container engine on macOS (`--features vz`)

The `vz` engine — which boots a real Linux microVM via Apple's
Virtualization.framework — is behind a Cargo feature. Build with it on:

```sh
cargo build --release --features vz
```

On macOS the `vz` binary needs the virtualization entitlement to talk to the
hypervisor. Ad-hoc codesign it with the entitlements file shipped in the repo:

```sh
codesign --entitlements packaging/vz.entitlements -s - target/release/lightr
```

The entitlements file is [`packaging/vz.entitlements`](../packaging/vz.entitlements);
it grants exactly one key, `com.apple.security.virtualization`. The `-s -`
performs an **ad-hoc** signature, which is sufficient for local development. A
proper Developer ID signature is required only for distribution.

> Without this codesign step, a `--features vz` binary will fail to start a VM
> on macOS (the hypervisor denies the unentitled process).

---

## The Linux pack (for the `vz` engine)

The `vz` engine boots a **pack**: a Linux `kernel` plus an `initrd` whose
`/init` is Lightr's guest PID 1 (`lightr-init`), plus a `pack.json` manifest.
Build one with the bundled recipe:

```sh
scripts/build-linux-pack.sh [--out <dir>] [--arch aarch64|x86_64]
```

This assembles `<out>/kernel`, `<out>/initrd`, and `<out>/pack.json`. The
kernel source is named and pinned in the script (mainline Linux tracked
against Apple's Containerization config). If a required cross-toolchain is
missing, the script detects it, prints the exact fix, and exits non-zero — it
never fabricates a kernel.

> See [`docs/build.md`](build.md) for the full kernel/init build details,
> including the no-Docker arm64 path using Apple's prebuilt Containerization
> kernel.

Register the pack into your Lightr home directory:

```sh
lightr engine install-pack <dir>
# → installed linux pack → ~/.lightr/packs/linux
```

`install-pack` structurally validates the pack (the `initrd` must be a real
cpio with an executable `/init`, the `kernel` must be non-empty) before copying
it to `~/.lightr/packs/linux/`. A malformed pack is rejected loudly.

Confirm the engine sees it:

```sh
lightr engine ls
# native    available     native process execution (no isolation — not a sandbox)
# ns        unavailable   ns engine requires Linux (this host is macos)
# vz        available     vz engine ready (pack: ~/.lightr/packs/linux)
# wsl       unavailable   wsl engine requires Windows + WSL2 (this host is macos)
```

`engine ls` probes each engine honestly and reports its real availability and
reason. Then:

```sh
lightr run --engine vz --rootfs @docker/alpine -- /bin/sh -c 'exit 7'
# → exits 7 (the REAL guest exit code, returned over the file channel)
```

---

## Shell completions and the man page

Lightr can print a completion script for your shell to stdout, and a roff man
page:

```sh
# Completions (bash | zsh | fish | powershell | elvish):
lightr completions zsh  > ~/.zfunc/_lightr          # zsh example
lightr completions bash > /usr/local/etc/bash_completion.d/lightr

# Man page:
lightr man > /usr/local/share/man/man1/lightr.1
```

Adjust the destination to wherever your shell / `man` looks. Both are generated
from the live CLI definition, so they never drift from the actual flags.

---

## Verify the install

```sh
lightr --version          # prints version + git-sha + build-date
lightr --help             # top-level command list + examples
lightr engine ls          # which execution engines are available here
```

Nothing runs in the background after any of these — Lightr is daemonless
(`pgrep lightr` returns nothing between invocations).

---

## Data directory

Lightr keeps all state under a single home directory:

- Default: `~/.lightr`
- Override: set the **`LIGHTR_HOME`** environment variable to any path.

Inside it you will find `store/` (the content-addressed store), `index/`,
`run/` (per-run directories: logs, control files), `packs/` (installed Linux
packs), `compose/` (compose stacks), and `units/` (generated supervisor units,
once you use `supervise install`). Deleting `~/.lightr` resets Lightr
completely.

---

## Platform support matrix (honest)

One codebase targets every desktop; the isolation engine differs per platform.
"Compiles + cross-checks clean" is **not** the same as "runtime validated" —
this table mirrors `docs/spec/parity-audit.md` ("Platform coverage") and marks
each tier exactly.

| Platform | Core (CAS / run / build) | Isolation engine | Runtime validated? |
|---|---|---|---|
| **macOS Intel x86_64** | ✅ | `vz` (x86_64 guest) | ✅ **runtime-validated end-to-end** (F-205/F-206, Intel i7-9750H, macOS 15.3.2) |
| macOS Apple Silicon | ✅ (same code) | `vz` (arm64 guest) | 🟡 **not validated** — code-complete; runbook at `spikes/s5-vz-boot-arm64/` |
| Linux x86_64 | ✅ (same code) | `ns` (namespaces) | 🟡 **not validated** — code-complete; CI / target box gated |
| Linux aarch64 | ✅ (same code) | `ns` | 🟡 **not validated** — code-complete; CI cross-check gated |
| Windows x86_64 | 🟡 code-complete | `wsl` (ns inside WSL2) | 🟡 **not validated** — code-complete; runbook (Windows box) gated |

**Read this literally:** only the **macOS Intel x86_64 `vz`** path has been run
end-to-end and proven. Apple Silicon `vz`, Linux `ns`, and Windows `wsl` are
written and compile/cross-check clean, with runbooks under `spikes/`, but are
**hardware-gated and not claimed validated**. The daemonless core (store,
memoized `run`/`build`, OCI import, time-axis verbs, compose, docker compat,
agent surface) is the same code on every platform and is fully tested.

---

## Distribution channels (available on release — not live today)

These exist as code-complete, metadata-ready packaging but are **owner-gated**
and have **not** shipped a public release yet:

- **Homebrew** — formula at [`packaging/lightr.rb`](../packaging/lightr.rb)
  (carries post-release placeholders until a Release exists).
- **`curl | sh` installer** — [`packaging/install.sh`](../packaging/install.sh)
  (fails loudly while its placeholders are unfilled — by design).
- **crates.io** — per-crate publish metadata is ready on all crates; the
  workspace ships `publish = false` until the owner flips the gate.
- **GitHub Releases** — a 5-target release matrix
  (`.github/workflows/release.yml`) is wired; macOS signing waits on owner
  secrets, and unsigned artifacts are clearly labeled.

Until any of those go live, **build from source** as above. The publishing
procedure is documented in [`docs/RELEASE.md`](RELEASE.md).
