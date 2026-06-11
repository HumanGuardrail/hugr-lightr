# Decisions log — owner authorizations (verbatim)

Append-only. The rigor compact requires human authorization for gate
changes; this file is where those authorizations are recorded verbatim.

## 2026-06-11 — Overnight delivery mandate

**Authorized-by:** Gustavo (owner), in session, end of day.

**Verbatim (goal directive):** "Esses sao nossos alvos, decomponha todos
eles em features, depois atualize o whitepaper, claude.md, product.md, etc.
A partir dai, vamos usar a tecnica de working backwards, comecando pelo
whitepaper do produto pronto. […] Depois de voce decompor o whitepaper
completo trabalhando backwards, formalize toda a documentacao de
especificacao. A partir dela voce vai comecar a criar os wps, crie wps
atomicos, sem overhead, usando sua skill de techlead, escolha bem os
modelos pra nao queimar token atoa. Bote pra fuder, e me entregue o
produto completo, impecavel, testado, sota […]"

**Verbatim (final, going to sleep):** "vou deixar voce memso setar a goal
ok?" · "boa noite, vou dormir. Por favor, entregue tudo impecavel padrao e
rigor sota. Tudo impecavel."

**Effect (lead's interpretation, logged for morning review):**
1. The doc cascade (whitepaper v2 → feature tree → canon → ADRs →
   build-spec v2) proceeds autonomously tonight.
2. ADRs 0009–0016 + reworked 0003/0005 are marked
   **Accepted (owner overnight mandate — subject to morning review)**
   instead of waiting for the interactive per-ADR hammer session; any of
   them can be reverted to Proposed by the owner in the morning.
3. The R0 wave (atomic WPs, model-routed, TechLead method) is dispatched
   tonight under this authorization. The three standing gates remain for
   anything beyond: rigor waivers stay human-only; no public
   distribution/release (ADR-0008 unresolved); no sibling-repo mutation.
4. Spikes that require external downloads/new VMs (S1–S3, S5) are NOT run
   tonight; only S4 (clonefile storm, local, read-safe) informs the wave.
   R0 scope deliberately excludes spike-dependent features (views/vz).

## 2026-06-11 (overnight) — Lead amendments during R0 integration

**Authorized-by:** lead under the overnight mandate; flagged for morning
review. All gates green after each amendment.

1. **Integrity law refined (spec §4/§7/§8, A7 split).** CoW materialization
   is metadata-only and cannot re-hash; the frozen A7 contradicted the
   O(metadata) bar. Resolution: verification lives where bytes are READ —
   manifests/refs/AC are always re-hashed (default fail-closed; A7b) — and
   the paranoid full re-hash is explicit: `lightr hydrate --verify` /
   `lightr_index::hydrate_verified` (A7a). fs-verity (R2, ADR-0009) closes
   the kernel-side gap. Also fixed: parallel materialize silently discarded
   errors (now fail-closed, first error aborts).
2. **Dep-list amendments (spec §2):** `blake3` allowed in lightr-run (key
   assembly needs a streaming hasher); `tempfile` allowed as a lightr-cli
   runtime dep (bench fixtures).
3. **Test-isolation law (all crates):** env-mutating tests serialize on a
   static lock and isolate LIGHTR_HOME in tempdirs; index temp-files are
   per-thread unique (PID alone collided under the parallel test runner).
