# AI usage accounting

A small, content-free Rust library for AI integrations. LiteLLM is the first consumer;
Trajectory's distinction between inclusive and uncached input informed the API.
Trajectory itself is unchanged.

Adapters supply **one request's** reported counts and whether input includes caches.
The library returns:

- Disjoint token/cache/tool quantities, without counting reasoning or caches twice.
- Reported observations (including fractional audio/video seconds).
- Issues for missing, invalid, conflicting, or ambiguous usage.
- Shared context-length buckets and basic label validation.

Missing is not zero. Unknown cache-write TTL stays unknown. Ambiguous multimodal
usage retains observations instead of inventing a disjoint token partition.
Counts above 2^53 are rejected so downstream metric transports do not round them.

Callers still own SDK field extraction, authentication, configuration, retry/fallback
tracking, and export. Never send prompts, completions, credentials, or arbitrary
metadata into this library. Label validation is not a general secret scrubber.

There are no runtime dependencies, environment reads, threads, or network calls.
The initial consumer uses its existing Rust/Python extension; C and Go bindings,
session accounting, transcript parsing, pricing, and provider-ID discovery are
outside this first version.
