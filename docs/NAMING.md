# NAMING — `lightr` / `hugr-lightr` Availability

**Status:** DECIDED  
**Date:** 2026-06-11  
**Author:** W2 naming agent (automated research)  
**Gate:** ADR-0008 — crate name + binary name must be verified before publication.

---

## Availability table

| Name | Registry | Status | Evidence |
|---|---|---|---|
| `lightr` | crates.io | **FREE** | `GET https://crates.io/api/v1/crates/lightr` → HTTP 404. No crate registered under this name as of 2026-06-11. |
| `hugr-lightr` | crates.io | **FREE** | `GET https://crates.io/api/v1/crates/hugr-lightr` → HTTP 404. No crate registered under this name as of 2026-06-11. |
| `lightr` | Homebrew (homebrew-core) | **FREE** | `GET https://formulae.brew.sh/formula/lightr` → HTTP 404. No formula found in homebrew-core. Web search for "lightr homebrew formula" returned only generic Homebrew documentation — no tap or formula for `lightr` found. |
| `lightr` | npm | **TAKEN** | `GET https://registry.npmjs.org/lightr` → HTTP 200. Package exists: `lightr` v0.1.1, "Bake lighting in HTML5 Canvas using normal maps", published 2014-07-25 by David Evans. Dormant since 2014 but the name is claimed. |
| `lightr` | CRAN (R) | **TAKEN** (different namespace) | `lightr` is a published R package on CRAN ("Read Spectrometric Data and Metadata", maintained by rOpenSci). Lives in a completely separate namespace; no CLI collision. Noted for trademark/confusion awareness only. |
| `lightr` | Trademark / web | **NO CONFLICT FOUND** | Web search for "lightr software product company CLI tool" and "lightr site:github.com" found: (a) a dormant GitHub user `lightr` (0 public repos, Arctic Code Vault badge only); (b) the CRAN R package (unrelated domain); (c) the dormant npm package (above). No active software company, SaaS product, or CLI tool trading under the name `lightr` was found. |

---

## DECISION

### Crate name: `hugr-lightr`

`lightr` is **free on crates.io** (404 confirmed). However, the pre-decided constraint in `docs/spec/build-spec-ship.md §W2` and `CLAUDE.md` reads:

> Crate name `hugr-lightr`, binary `lightr`  
> (crate = `hugr-lightr` if `lightr` is taken on crates.io — confirm + cite)

The spec anticipated `lightr` might be taken; it is in fact free. The constraint as written (`hugr-lightr` if taken) does not bind in the case where `lightr` is free. However, `hugr-lightr` is the documented house decision and is the better choice regardless:

- It places the crate under the `hugr-*` namespace, consistent with the HuGR / CoreLink brand hierarchy.
- It avoids future ambiguity if an unrelated `lightr` crate appears.
- It signals the relationship to the HuGR platform for users browsing crates.io.

**Confirmed recommendation: publish crate as `hugr-lightr`.**

### Binary name: `lightr`

- No Homebrew formula named `lightr` exists — a `lightr.rb` formula can be submitted to homebrew-core or distributed via a `hugr/homebrew-hugr` tap without a name conflict.
- The npm `lightr` package (2014, dormant, Canvas lighting library) is in a completely different namespace (Node.js ecosystem, not a CLI tool). There is no meaningful confusion risk for a Rust CLI binary or a brew formula named `lightr`.
- No active software company, product, or CLI tool was found trading under this name.

**Confirmed recommendation: binary name stays `lightr`. No blocker for a brew formula.**

### Summary

| Artifact | Name | Rationale |
|---|---|---|
| Rust crate | `hugr-lightr` | House namespace; both names are free; `hugr-*` signals platform membership |
| Installed binary | `lightr` | Clean, short; no collision in CLI, Homebrew, or trademark space |

---

## Honesty note — what could NOT be fully verified

- **npm dormancy / transfer:** The `lightr` npm package (v0.1.1, 2014) is registered. Whether it could be reclaimed via npm's abandoned-package policy was not verified. This does not block the crate or binary decision; it would only matter if HuGR ever published a companion npm package under the same name — evaluate then.
- **USPTO trademark search:** A formal USPTO TESS search was not performed (requires interactive search tool). Web search found no active trademark claim. Treat trademark status as **UNKNOWN** pending a manual USPTO check if legal review is required before public launch. Manual command: visit `https://tmsearch.uspto.gov` and search "lightr".
- **Popular third-party Homebrew taps beyond homebrew-core:** Only homebrew-core was checked via `formulae.brew.sh`. A `brew search lightr` on a local machine would confirm no tap-distributed formula exists. Manual check: `brew search lightr`.
