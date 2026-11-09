---
name: add-remote-config-capability
description: Use this skill whenever the user wants to add, register, or reserve a new RemoteConfigCapabilities variant in libdd-remote-config (e.g. "add a new RC capability", "register capability X", "add bit N to RemoteConfigCapabilities", "advertise support for Y via remote config"). Verifies the bit index against the authoritative org-wide registry in ddoghq/dd-source before editing.
---

# Add Remote Config Capability

The `RemoteConfigCapabilities` enum in `libdd-remote-config/src/lib.rs` is a downstream mirror of an org-wide registry shared across every Datadog tracer. Never pick "the next free bit" by reading only this repo — reconcile with the upstream registry first.

## Source of truth

`domains/remote-config/shared/libs/rc/capabilities.go` in `ddoghq/dd-source`. Each capability is a `Capability{Name: "...", bitIndex: N}` block — the `Name` is the SCREAMING_SNAKE_CASE identifier and `bitIndex` is the allocated bit.

If you cannot access `ddoghq/dd-source`, ask the user to fetch the file and paste its contents.

## Steps

### 1. Collect inputs from the user

You need two things:
- The **capability name** (Rust variant, PascalCase, e.g. `AsmRawResponseBody`). The serde serialization uses `SCREAMING_SNAKE_CASE` — e.g. `ASM_RAW_RESPONSE_BODY` — which is the form the upstream registry uses.
- The **bit index** the user wants, if they have one. Otherwise you'll derive it in step 3.

### 2. Fetch the upstream registry

Read the file linked above and extract the full `(bit, name)` list.

### 3. Reconcile

The upstream registry is the only place a `(name, bit)` pair can be *reserved*. Libdatadog may only mirror a pair that already exists upstream — otherwise a concurrent `ddoghq/dd-source` change can claim the same bit before the proposed upstream registration lands, recreating the collision this skill is meant to prevent.

- If the user **supplied a bit index**, confirm the upstream registry maps it to the expected SCREAMING_SNAKE_CASE name. On mismatch (or empty slot upstream), **stop** — ask the user to land the `ddoghq/dd-source` PR first, then re-run this skill (which will re-fetch and re-verify).
- If the user **did not supply a bit index**, suggest the lowest free bit (`max(upstream_bits) + 1`) as a *proposal only* and **stop**. Do not edit libdatadog until the user confirms the `(name, bit)` pair has been merged into `ddoghq/dd-source`; then re-run this skill and re-verify against the latest upstream file.

Also verify the chosen bit is not already present in `libdd-remote-config/src/lib.rs`.

### 4. Edit `libdd-remote-config/src/lib.rs`

Append the new variant to the end of `pub enum RemoteConfigCapabilities`, keeping ascending numeric order:

```rust
    LlmObsActivation = 48,
    NewCapabilityName = 50,
```

The enum is `#[non_exhaustive]` and uses `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`, so the Rust variant must round-trip to the exact upstream `Name` field (`NewCapabilityName` → `NEW_CAPABILITY_NAME`). Double-check.

### 5. Commit message

Follow Conventional Commits. Adding a variant to `#[non_exhaustive] #[repr(C)] enum RemoteConfigCapabilities` is *not* a breaking change in Rust (the attribute forces downstream matches to carry a wildcard arm) and the C FFI explicitly does not guarantee ABI backward-compatibility (callers pin to specific libdatadog versions — see `AGENTS.md`), so a plain `feat:` is correct:

```
feat(remote-config): add <CapabilityName> capability
```

Body should link the upstream `ddoghq/dd-source` PR / RFC where the bit was reserved.

### 6. Remind the user

- The `ddoghq/dd-source` registry must be updated first (or in lockstep) — libdatadog is downstream.
- Other tracer repos (dd-trace-php, dd-trace-go, dd-trace-py, etc.) each maintain their own mirror. Ask whether matching PRs are needed there.
