# RELEASE — the owner's publish runbook (G-PUBLISH)

Publishing hugr-lightr is **owner-gated** (`G-PUBLISH`). Everything below is
code-complete and metadata-ready as of the go-live hardening wave (2026-06-17);
none of it fires until the owner executes these steps deliberately. The runtime
is validated end-to-end only on **Intel x86_64 vz** — the other tiers
(arm64 vz, Windows wsl, Linux ns) are hardware-gated and listed at the end as
separate press-go actions.

Status to keep honest while doing this: this runbook is a **procedure**, not a
claim that a release has happened. Do not mark anything "shipped" until the
GitHub Release exists and the artifacts verify.

---

## A. Flip the publish gate

`Cargo.toml` (`[workspace.package]`) ships `publish = false` on purpose. Flip it:

```toml
[workspace.package]
publish = true
```

Every publishable crate inherits this via `publish.workspace = true`. Two crates
override it locally to `publish = false` and **stay that way** — do not touch
them: `lightr-acceptance` (test harness) and `lightr-init` (guest PID1 binary).

> **BLOCKER to resolve before step B (see note at the bottom):** `lightr-engine`
> (publish=true) has a path dependency on `lightr-init` (publish=false). crates.io
> rejects a crate whose dependency is unpublished. This must be settled before
> `cargo publish lightr-engine` will succeed.

---

## B. `cargo publish` in dependency (topological) order

Order computed from each crate's internal `[dependencies]` (path deps on other
workspace crates). Each crate is published only after every crate it depends on.
`lightr-core` has no internal deps → first; `lightr-cli` depends on everything →
last. **Skip `lightr-acceptance` and `lightr-init` (publish=false).**

Internal dependency edges (publishable crates):

| crate | internal deps |
|---|---|
| `lightr-core` | _(none)_ |
| `lightr-store` | core |
| `lightr-index` | core, store |
| `lightr-run` | core, store, index |
| `lightr-oci` | core, store, index |
| `lightr-views` | core, store |
| `lightr-engine` | core, store, index, **init** ⚠️ (publish=false) |
| `lightr-build` | core, store, index, run, oci, engine |
| `lightr-cli` | core, store, index, run, oci, engine, build |

**Publish order (run from repo root, wait for each to land on crates.io before
the next so the index is fresh):**

```sh
cargo publish -p lightr-core
cargo publish -p lightr-store
cargo publish -p lightr-index
cargo publish -p lightr-run
cargo publish -p lightr-oci
cargo publish -p lightr-views
cargo publish -p lightr-engine    # ⚠️ requires the lightr-init blocker resolved first
cargo publish -p lightr-build
cargo publish -p lightr-cli       # the `lightr` binary
```

`lightr-run`, `lightr-oci`, and `lightr-views` are mutually independent (each
only needs core/store/index or core/store) — their relative order does not
matter, but each must come after `lightr-index`. `lightr-build` needs `engine`
and `oci`; `lightr-cli` needs `build` — so those two are strictly last in that
order.

Dry-run each first if desired: `cargo publish -p <crate> --dry-run`.

---

## C. Fill packaging placeholders from the GitHub Release

After `release.yml` produces the Release (step E) you have the asset URLs and the
`SHA256SUMS`. Fill the templates:

**`packaging/lightr.rb`** — replace these `__TODO_*` placeholders:
- `__TODO_VERSION__` → the release version (e.g. `0.1.0`)
- `__TODO_URL_DARWIN_ARM64__` + `__TODO_SHA256_DARWIN_ARM64__`
- `__TODO_URL_DARWIN_X86_64__` + `__TODO_SHA256_DARWIN_X86_64__`

  (URL pattern: `https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-<os>-<arch>.tar.gz`;
  sha256 values come from the `SHA256SUMS` file attached to the Release.)
  Note: the formula also carries Linux `__TODO_URL_LINUX_*` / `__TODO_SHA256_LINUX_*`
  slots — fill the ones your release matrix actually produced. Then push the
  formula to the `hugr/homebrew-tap` repo.

**`packaging/install.sh`** — replace both placeholders (the script fails loudly
while either still contains `__PLACEHOLDER__`):
- `RELEASES_URL="__PLACEHOLDER__RELEASES_URL__"` → the release-download base URL
- `VERSION="__PLACEHOLDER__VERSION__"` → the release version

---

## D. Configure GitHub Actions secrets (macOS signing)

`release.yml` applies macOS code-signing + notarization only when these repo
secrets are present (otherwise artifacts are clearly labeled unsigned, never
fake-signed). Owner supplies all four:

- `APPLE_CERT` — base64-encoded signing certificate (.p12)
- `APPLE_CERT_PASSWORD` — the .p12 password
- `AC_API_KEY` — App Store Connect API key (JSON)
- `AC_API_KEY_ID` — the API key id

(Configure under repo Settings → Secrets and variables → Actions.)

---

## E. Tag → trigger the release pipeline

`release.yml` is tag-triggered on `v*`. Tagging builds the **5-target matrix**
(macOS arm64 + x86_64, Linux x86_64 + aarch64, Windows x86_64), produces
`SHA256SUMS`, and creates the GitHub Release:

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The Release assets + `SHA256SUMS` are what step C consumes (so in practice: tag
first, then fill packaging from the produced Release, then push the brew
formula).

---

## F. Verify naming is still free (before publishing)

Just before step B, re-confirm both names are still unclaimed on crates.io (and
the brew tap name is free):

- crate `hugr-lightr` — free as of the wave; re-check `cargo search hugr-lightr`
  / the crates.io page.
- binary / crate `lightr` — free as of the wave; re-check `cargo search lightr`.

If either was taken in the interim, STOP and escalate to the owner — the naming
decision (ADR-0008) assumes both are free.

---

## Hardware-validation press-go (separate owner / HW actions)

These are runtime-validation gates, independent of the crates.io/GitHub publish
above. Each is code-complete with a runbook or CI job; **none is claimed
validated.** Only Intel x86_64 vz is runtime-validated today (F-205/F-206).

- **arm64 vz boot** — `spikes/s5-vz-boot-arm64/` (`run-s5-arm64.sh` + README +
  EXPECTED.md): on an Apple Silicon Mac, build `--features vz`, install a pack,
  `lightr run --engine vz`, assert the REAL guest exit code flows back.
- **Windows wsl** — `spikes/wsl-run/run-wsl.sh` (+README): on a Windows box with
  WSL2, run the wsl isolation engine and assert the runbook's expectations.
- **Linux ns** — the CI gate (`.github/workflows/ci.yml`, native ubuntu +
  aarch64 cross-check) and/or a Linux target box exercises the `ns` engine.

---

## Blocker noted during this runbook's authoring (resolve before publish)

`lightr-engine` is `publish=true` but path-depends on `lightr-init`
(`publish=false`). `cargo publish -p lightr-engine` will be **rejected** by
crates.io because `lightr-init` is not on the registry. Options for the owner to
decide before step B:

1. Make `lightr-init` publishable (flip its local `publish=false`), and insert
   it into the order immediately before `lightr-engine` (its only internal dep is
   none — it depends on no other workspace crate, so it can publish right after
   `lightr-core` or any time before `lightr-engine`); **or**
2. Restructure so `lightr-engine` does not require `lightr-init` at publish time
   (e.g. make the dependency optional / dev-only).

This is recorded as a fact found while reading the crate manifests — it is not
resolved here.
