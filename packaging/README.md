# lightr — Distribution Packaging

> **GATES.** The LICENSE gate is **CLEARED** — the project is **Apache-2.0**
> (ADR-0008 Accepted 2026-06-12); `Cargo.toml` carries the SPDX, `LICENSE` +
> `NOTICE` are in the repo root. One gate remains before anything is
> published: **release timing** (GTM / launch after Runners M1, whitepaper
> §9.8) and the absence of a real release URL — so these artifacts still
> fail loudly and upload nothing.**

See `docs/adr/0008-license.md`. The artifacts here are wired to fail-loud
until a real release exists; flip `publish`/`RELEASES_URL` only when the GTM
timing call is made.

---

## Distribution channels (prepared, not live)

### 1. curl|sh installer

`packaging/install.sh` — a shell script suitable for:

```sh
curl -fsSL https://raw.githubusercontent.com/<org>/hugr-lightr/main/packaging/install.sh | sh
```

Detects OS (Darwin/Linux) and arch (arm64/x86\_64), downloads the matching
release tarball from GitHub Releases, verifies a sha256 checksum, and installs
the `lightr` binary to `~/.local/bin` (or `/usr/local/bin` with a prompt).

**Until the license gate is lifted and a real release is published, the script
will FAIL LOUDLY** — it prints:

```
ERROR: no published release yet (license-gated, ADR-0008)
```

and exits non-zero. It will never silently succeed with a placeholder URL.

### 2. Homebrew tap

`packaging/lightr.rb` — a Homebrew formula template for a private tap (e.g.
`hugr/tap`). Once the license gate is lifted and binaries are published on
GitHub Releases the TODO placeholders (url, sha256) are filled in and the
formula is pushed to the tap repo.

Install (future):

```sh
brew tap hugr/tap
brew install lightr
```

### 3. GitHub Releases (automated — `.github/workflows/release.yml`)

**CI release workflow:** `.github/workflows/release.yml` is triggered
exclusively by a `v*` tag push. No tag → no publish; nothing can accidentally
land in GitHub Releases from a branch push or PR.

Matrix: `macos-14` (arm64), `macos-13` (x86_64), `ubuntu-latest` (linux-x86_64).
Each job: `cargo build --release`, strip, package
`lightr-<version>-<os>-<arch>.tar.gz`, compute sha256. A final `release` job
assembles all tarballs + a `SHA256SUMS` file and uploads them to a GitHub
Release via `softprops/action-gh-release@v2`.

**Signing and notarization (macOS):** the signing steps are present but GATED.
When the secrets listed below are absent, the step prints:

```
signing skipped — Apple Developer secrets not set (owner provides)
```

and the artifact is named with an **`-unsigned`** suffix so it is never
labelled as signed. An unsigned artifact is _never_ silently mislabelled.

Required repository secrets (owner must provision before signing runs):

| Secret | Purpose |
|---|---|
| `APPLE_CERT` | Base64-encoded `.p12` Developer ID Application certificate |
| `APPLE_CERT_PASSWORD` | Passphrase for the `.p12` |
| `AC_API_KEY` | App Store Connect API key (JSON) for notarytool |
| `AC_API_KEY_ID` | Key ID component of the API key |

`packaging/release.sh` remains the **local equivalent** of the workflow's
build+package steps (no upload; used for local verification and for computing
sha256 values before a tag is cut).

---

## Building a release artifact (local, for verification)

```sh
bash packaging/release.sh
```

Output: `packaging/dist/lightr-<version>-<os>-<arch>.tar.gz` + sha256 printed
to stdout.

The script does **not** upload anything. Uploading is triggered only by
pushing a `v*` tag, which runs the CI workflow above.

---

## File map

| File | Purpose |
|---|---|
| `packaging/README.md` | This file |
| `packaging/install.sh` | curl\|sh installer (fails loudly until a real release URL is set) |
| `packaging/lightr.rb` | Homebrew formula template (TODOs for url/sha256 — filled after a tag) |
| `packaging/release.sh` | Local release-build recipe (no upload; mirrors workflow build steps) |
| `packaging/dist/` | Gitignored build output |
| `.github/workflows/release.yml` | Tag-triggered automated release pipeline |
