# lightr — Distribution Packaging

> **LICENSE GATE: These artifacts are prepared but MUST NOT be published until
> ADR-0008 (license) is Accepted; the binary ships `license = UNLICENSED`,
> `publish = false`.**

See `docs/adr/0008-license.md`. Until that ADR reaches status **Accepted** with
a concrete license string, no artifact from this directory may be uploaded,
tapped, or otherwise made public. The release checklist enforces this gate.

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

### 3. GitHub Releases

`packaging/release.sh` builds a release tarball (`lightr-<version>-<os>-<arch>.tar.gz`)
into `packaging/dist/` (gitignored) and prints the sha256 needed for the
Homebrew formula and install.sh. The script intentionally does **not** upload
anything — uploading is a manual step gated on ADR-0008 acceptance.

---

## Building a release artifact (local, gated)

```sh
bash packaging/release.sh
```

Output: `packaging/dist/lightr-<version>-<os>-<arch>.tar.gz` + sha256 printed
to stdout.

**Do not upload until ADR-0008 is Accepted.**

---

## File map

| File | Purpose |
|---|---|
| `packaging/README.md` | This file |
| `packaging/install.sh` | curl\|sh installer (license-gated, fails loudly until released) |
| `packaging/lightr.rb` | Homebrew formula template (TODOs for url/sha256) |
| `packaging/release.sh` | Local release-build recipe (no upload) |
| `packaging/dist/` | Gitignored build output |
