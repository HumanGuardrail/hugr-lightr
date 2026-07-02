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

Every publishable crate inherits this via `publish.workspace = true`. One crate
overrides it locally to `publish = false` and **stays that way** — do not touch
it: `lightr-acceptance` (test harness). `lightr-init` inherits the workspace gate
and is publishable (it is a dependency of `lightr-engine`).

---

## B. `cargo publish` in dependency (topological) order

Order computed from each crate's internal `[dependencies]` (path deps on other
workspace crates). Each crate is published only after every crate it depends on.
`lightr-core` and `lightr-init` have no internal deps → first tier; `lightr-cli`
depends on everything → last. **Skip `lightr-acceptance` (test harness only).**

Internal dependency edges (publishable crates):

| crate | internal deps |
|---|---|
| `lightr-core` | _(none)_ |
| `lightr-init` | _(none)_ |
| `lightr-store` | core |
| `lightr-index` | core, store |
| `lightr-run` | core, store, index |
| `lightr-oci` | core, store, index |
| `lightr-views` | core, store |
| `lightr-engine` | core, store, index, init |
| `lightr-build` | core, store, index, run, oci, engine |
| `lightr-cli` | core, store, index, run, oci, engine, build |

**Publish order (run from repo root, wait for each to land on crates.io before
the next so the index is fresh):**

```sh
# Tier 1 — no internal deps (order between these two does not matter)
cargo publish -p lightr-core
cargo publish -p lightr-init
# Tier 2
cargo publish -p lightr-store
cargo publish -p lightr-index
# Tier 3 — mutually independent, each needs core/store/index
cargo publish -p lightr-run
cargo publish -p lightr-oci
cargo publish -p lightr-views
# Tier 4
cargo publish -p lightr-engine
# Tier 5
cargo publish -p lightr-build
# Tier 6
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
  formula to the `HumanGuardrail/homebrew-tap` repo.

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

## RESOLVED 2026-06-17: lightr-init now inherits the publish gate

`lightr-init` is a real library (`InitSpec`/`CMD_FILE`/`EXIT_FILE` consumed by
`lightr-engine`) and now uses `publish.workspace = true`. No publish blocker remains.
