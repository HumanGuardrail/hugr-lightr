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
