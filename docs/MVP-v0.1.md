# MVP v0.1 — the first slice

One sprint of scope, built on what already exists. The goal is a binary a
Mac dev can install and feel the thesis in under a minute — not a platform.

## In scope

- **`lightr run <ref|path> -- <cmd>`** — the whole product in one verb:
  resolve → memo check → hydrate (cache-first, via clw pipeline) →
  execute **native** → memoize on success.
- **`lightr snapshot` / `lightr hydrate` / `lightr status`** — re-exposed clw
  verbs under the lightr UX (same engine, friendlier surface).
- **Local-only mode by default**: `~/.clw/cache` as the only store, no
  account, no network required for the happy path. CoreLink remote is a
  flag/config away (Stage 2), not a requirement.
- **Engine = `native` only.** Zero isolation, stated loudly. The value
  demonstrated in v0.1 is reproducibility + instant hydrate + memoization.
- macOS arm64 first; Linux x86_64 if free.

## Explicitly out of scope (v0.1)

- microVMs (`vz`, `fc`) — v0.2+; requires the lazy-rootfs work.
- Linux namespaces engine.
- OCI image import.
- Any server-side change to CoreLink. Pure client, like clw.
- Teams/auth/billing — that is Stage 2 of the funnel.

## Open decisions (settle before building)

1. **Seam with clw**: vendor the crates, path-dep within a monorepo, or
   transcribe the wire contract with shared conformance vectors (the
   runners↔hugit house pattern)? Leaning: depend on clw crates directly —
   same org, same language, and clw is explicitly a client library; the
   conformance-vector pattern exists for *cross-repo frozen seams*, which
   this is not (yet).
2. **License**: the funnel requires free local, but free ≠ open source.
   MIT/Apache maximizes adoption; BSL protects against a hyperscaler
   wrapping the client. Strategic call, not technical.
3. **Distribution**: brew tap (`brew install hugr-lightr`, bin `lightr`) +
   curl|sh + GitHub releases. Signed/notarized for macOS.
4. **Ref syntax**: `@tenant/name` vs `tenant/name@version` — pick one
   before the first public demo; it's the product's visible grammar.
5. **Name collision check**: verify `lightr` binary name conflicts on
   brew/crates.io before announcing (e.g. crates.io `lightr` is a famous
   Rust crate — the *crate* may need to be `hugr-lightr` while the *binary*
   stays `lightr`).

## Definition of done (v0.1)

- Fresh Mac, `brew install hugr-lightr`, `lightr run` on a real Node/Rust
  workspace: first run executes and snapshots; second run on a clean
  checkout hydrates from local cache in seconds; identical re-run returns
  the memoized result in milliseconds without executing.
- `docker stats`-style honesty: a `lightr` doing nothing consumes nothing —
  `ps aux | grep lightr` returns nothing between runs.
- README quickstart reproduces end-to-end on a machine that has never
  seen the repo.
