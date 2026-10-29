# Feature Flagging & Experimentation

This is an internal DataDog library that supports Feature Flag evaluation. It is intended for use in tracers and DataDog services. Expect frequent breaking changes and major bumps before it's stabilized.

## `flagevaluation` EVP privacy

- Each observation carries the consent captured by its SDK at evaluation time.
  Protected output is the default. Only explicit consent permits raw targeting
  identity and evaluation context in the EVP event; raw error messages are
  always replaced with standard OpenFeature error codes.
- Aggregation retains valid raw identity internally and hashes it only when
  building protected output (`sha256_` plus lowercase SHA-256 of exact UTF-8).
  A present empty identity stays empty; missing or invalid UTF-8 identity is
  omitted. Full buckets distinguish consent; degraded buckets do not and omit
  identity/context regardless of consent. Merging defensively ANDs consent.
- Context snapshots retain at most 256 leaves, inspect at most 256 entries per
  container and 1,280 nodes overall, limit depth to four, and omit keys/string
  values over 256 Unicode characters. The legacy JSON-taking FFI still parses
  the supplied JSON: these retention limits do **not** bound that parsing cost.
  A producer needing a bounded hot path must select bounded input before JSON
  conversion. The final encoder independently enforces privacy and limits.
- Field loss records `flagevaluation.context.truncated` with finite `reason`
  tags, or `flagevaluation.targeting_key.omitted` with `reason:invalid`.
  Each reason is counted once per represented evaluation, even if validation
  happens at multiple boundaries; it is not an evaluation drop. Existing
  degradation/drop metrics retain their names and units. Internal field-loss
  metadata is never sent in EVP. Event Debug output omits customer fields.

### Native integration and compatibility

Build the C caller against matching generated headers and native artifacts.
The added consent and internal metadata change C/event layouts; bincode is
positional and `serde(default)` does not provide old/new IPC compatibility.
Update native sender and sidecar receiver together, as existing integrations
do. PHP builds both from its pinned native source, sets `SIDECAR_VERSION` to
its release `VERSION`, and uses version-specific socket/pipe names. Custom
builds sharing a release label must not reuse an incompatible stale sidecar.
No new protocol negotiation is introduced here.

This library change does not enable PHP EVP emission or change flag evaluation
results. PHP capture/admission/lifecycle integration and its end-to-end release
validation are separate work. Native matched-build IPC/HTTP tests are not a
substitute for PHP upgrade tests or Windows CI.

## Dependent flags

Rules may mix ordinary attribute conditions with `flagEvaluation` references. A reference
compares the prerequisite's selected **variant key**, using `ONE_OF` or `NOT_ONE_OF` with a
string array. Conditions run in authored order and short-circuit. A mismatch rejects the
rule; a reached missing, disabled, malformed, or unassigned prerequisite fails the root
with `DependencyEvaluationFailed`, preserving its cause.

Each root evaluation uses one configuration, context, timestamp, and experimentation opt-out
policy. Completed prerequisites are memoized only within that invocation. Cycles fail;
`evaluatorParams.maxDependencyDepth` limits dependency edges (default 10, accepted range
0–255). Cached results retain the deepest dependency visited, including rejected rules,
so reuse from a deeper path cannot bypass the limit.

`Configuration::eval_flag_with_trace` additionally returns unique prerequisite evaluation
records, excluding the requested root. SDKs can log these evaluations even if a later
condition rejects their rule. `exposure_trace` lists the successful rule/split path in
depth-first authored order, including the replay position of memoized prerequisites. Only
records marked `exposure_eligible` appear in that trace; a failed root has no eligible
prerequisite exposures. SDKs must also apply each assignment's `do_log`, consent, and ordinary
exposure deduplication policy. The trace does not emit telemetry itself. SDKs should advertise
dependent-flag support only after integrating and testing both evaluation and exposure
reporting.

This API returns prerequisite evaluation records after the root evaluation completes. That is
the intended integration boundary: FFI consumers may accumulate and publish the returned records
as a batch once the root result is available. Evaluation records are retained even when the root
fails; unlike exposure candidates, they are not rolled back. Traversal-time callbacks are not
required.
