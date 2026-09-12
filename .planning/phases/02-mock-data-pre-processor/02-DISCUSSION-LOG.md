# Phase 2: Mock Data & Pre-processor - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-06-16
**Phase:** 2-Mock Data & Pre-processor
**Areas discussed:** Input format, Pre-processor significance algorithm, Metrics, bp-analyzer availability, Fixture location, Fixture naming

---

## Input Format

| Option | Description | Selected |
|--------|-------------|----------|
| BP v1 schema directly | Fixtures already in converted.json format — pre-processor diffs two v1 files | ✓ |
| Raw Criterion + convert step | Criterion JSON converted to v1 first | |
| You decide | Pick whichever is simpler | |

**User's choice:** BP v1 schema directly — pointed to `artifacts.zip` as the reference format.
**Notes:** The artifact contains multiple benchmark files per run forming a corpus, not a single monolithic file. The `converted.json` format is canonical.

---

## Significance Algorithm

| Option | Description | Selected |
|--------|-------------|----------|
| CI-based (replicate BP algorithm) | Implement bootstrap CI from scratch | |
| Simple mean ratio + threshold | Custom threshold (e.g. >5% = regression) | |
| Use bp-analyzer CLI | Pre-existing Datadog tool handles CI-based analysis | ✓ |

**User's choice:** Use `bp-analyzer` CLI — user provided full bp-analyzer documentation confirming it handles bootstrap confidence intervals and is "probably best to rely on this for deterministic analysis".
**Notes:** `bp-analyzer compare pairwise --format=md` replaces the jq script from DATA-02. Output is markdown, not JSON. REQUIREMENTS.md DATA-02 is superseded.

---

## Metrics

| Option | Description | Selected |
|--------|-------------|----------|
| execution_time only | Wall time is the primary signal | |
| execution_time + instructions | Deterministic complement | |
| All metrics | Surface everything | ✓ |

**User's choice:** All metrics (execution_time, instructions, cpu_user_time, max_rss_usage).

---

## bp-analyzer Availability

| Option | Description | Selected |
|--------|-------------|----------|
| Pre-installed in the image | No install step needed | ✓ |
| Needs to be installed | Add install step | |
| Unknown — assume install needed | Safer assumption | |

**User's choice:** Pre-installed in `dd-octo-sts-ci-base:2025.06-1`.

---

## Fixture Location

| Option | Description | Selected |
|--------|-------------|----------|
| .gitlab/bench-analysis/fixtures/ | Co-located with CI config | ✓ |
| fixtures/bench-analysis/ | Top-level fixtures dir | |
| You decide | Idiomatic for repo | |

**User's choice:** `.gitlab/bench-analysis/fixtures/`

---

## Fixture Naming

| Option | Description | Selected |
|--------|-------------|----------|
| Real libdatadog benchmark names | normalize_service, span_concentrator, etc. | ✓ |
| Generic invented names | benchmark_a, trace_processing, etc. | |
| You decide | Most useful for analysis | |

**User's choice:** "Pick based on the examples bench results I gave you" — use real libdatadog benchmark names (normalize_service, normalize_name, span_concentrator, obfuscation) modeled on the artifact's BP v1 schema structure.

---

## Claude's Discretion

- Exact number of fixture files and benchmark scenarios (3–6 recommended)
- Exact bp-analyzer flag set beyond core `compare pairwise --format=md --outpath`
- Whether schema validation (non-empty output assertion) lives in the script or the CI job

## Deferred Ideas

- Real Criterion-to-BP-v1 converter for `bp-analyzer` — needed when real benchmark runs land (Augusto's workstream). Out of scope for v1.
- `--fail_on_regression` CI job failure on significant regression — v2; too risky without dedicated runners.
- Mock dd-trace-py fixtures — blocked on format from triggering workstream; v2.
