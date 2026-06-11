# JSON schemas — the agent contract (normative)

- **Status:** normative. Every `--json` verb output and every `lightr mcp`
  tool input has a versioned JSON Schema. `lightr schema [--verb <v>]`
  prints them live from the binary; this doc is the human-readable mirror.
  An acceptance test (A28) asserts each verb's real `--json` output
  satisfies its declared schema's `required` set — the contract is
  machine-checked, not just documented.
- All schemas carry `"x-lightr-schema-version": 1`. A breaking change to any
  payload bumps the version (agents pin on it).

## How to read the live schemas
```
lightr schema            # { "<verb>": <schema>, ... } for all --json verbs
lightr schema --verb run # just the run output schema
```

## Covered verbs (R4)
`snapshot` · `hydrate` · `status` · `run` · `diff` · `gc` — each emits a
draft-07 object schema. Example (`run`):
```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "x-lightr-schema-version": 1,
  "type": "object",
  "properties": {
    "key":       { "type": "string", "description": "hex-encoded memo key" },
    "hit":       { "type": "boolean" },
    "exit_code": { "type": "integer" }
  },
  "required": ["key", "hit", "exit_code"]
}
```

## mcp tool inputs
`lightr mcp`'s `tools/list` returns the input schemas for
`lightr_snapshot` / `lightr_hydrate` / `lightr_status` / `lightr_run` /
`lightr_diff` (A15 asserts the tool set; A28 the output shapes). The mcp
contract is the agent-facing surface of the same payloads.

## Stability promise
Within a schema version, keys are only ADDED (never removed/retyped); agents
that read by key name stay correct. Removal or retype ⇒ version bump +
parity-audit entry. This is the F-501/F-507 (determinism-as-trust) guarantee
at the wire level.
