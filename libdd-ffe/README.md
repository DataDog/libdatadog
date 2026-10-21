# Feature Flagging & Experimentation

This is an internal DataDog library that supports Feature Flag evaluation. It is intended for use in tracers and DataDog services. Expect frequent breaking changes and major bumps before it's stabilized.

## Subject experimentation opt-out

The caller selects `EvaluationOptions.experimentation_opt_out_mode`; the evaluator reads no environment variables.
Both enabled modes require a valid `dd_experimentation_opt_out_at` timestamp when the attribute is present.
Missing input preserves normal evaluation. Invalid input returns `InvalidContext` before flag lookup.

| Mode | Exposure-logging allocations |
| --- | --- |
| `Disabled` | Evaluate normally |
| `Immediate` | Skip when the subject supplies an opt-out timestamp |
| `FutureExperimentsOnly` | Skip when activation is equal to or later than opt-out, or activation is unknown |

Future mode uses `randomizationActivatedAt`, with nanosecond precision and timezone-aware comparisons.
Older allocations remain eligible; this does not prove previous enrollment.
Malformed activation rejects the affected flag in every mode. Missing or null activation is unknown.
Skipped allocations produce no exposure. Ordinary allocations remain available, and evaluation telemetry is unchanged.

The C API adds `ddog_ffe_get_assignment_with_opt_out_mode`.
The existing `ddog_ffe_get_assignment_with_options` interface and boolean layout remain unchanged for immediate-mode callers.

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
