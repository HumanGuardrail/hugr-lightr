# Security policy

## Isolation posture (read before relying on any of it)

Lightr's isolation is **à la carte**, and each tier says honestly what it is:

- **`native` is reproducibility, NOT a sandbox.** It gives you memoized,
  content-addressed execution of a command as your own user, with no
  isolation boundary at all. Never run untrusted code under `native`.
- **Rootless `ns` (Linux namespaces) is not a hostile-tenant boundary.**
  It provides user/mount/pid/net namespaces, cgroup limits, caps, seccomp,
  and AppArmor — real isolation for *trusted* workloads, validated on
  GitHub-hosted Linux CI (`docs/benchmarks/RESULTS.md`). But rootless user
  namespaces share the host kernel; a kernel exploit crosses them. Do not
  use `ns` to contain an adversary.
- **`vz` (Virtualization.framework microVM) is a hardware boundary** —
  a real Linux guest kernel per run. Runtime-validated end-to-end on
  **Intel x86_64 macOS** (F-205/F-206 in `docs/spec/parity-audit.md`);
  arm64 is code-complete but not yet claimed validated.
- **`fc` (Firecracker) is the staged answer for hostile multi-tenancy.**
  It is not built yet and nothing here claims otherwise.

## Fail-closed philosophy

Unsupported or unenforceable paths return an explicit error — they never
silently degrade. A flag the engine can't enforce (e.g. `--pids-limit` on
`native`) errors instead of no-opping; pinned inputs are verified before
spawn; a container is reported `Running` only after its workload actually
`execv`s. If you find a path that silently degrades isolation instead of
erroring, that is a security bug — please report it.

## Reporting a vulnerability

Please report vulnerabilities **privately** to
**gustavomalleths@gmail.com** — do not open a public issue. Include a
reproduction (engine, platform, command line) if you can. You should
receive an acknowledgement within a few days; please allow a reasonable
window for a fix before public disclosure.
