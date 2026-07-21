# Architecture Patterns

**Domain:** LLM-augmented CI benchmark analysis pipeline
**Researched:** 2026-06-15

## Reference Implementation

The dd-trace-py repository (`DataDog/dd-trace-py`) ships a production implementation of the same auth-and-invoke pattern: `.gitlab/scripts/summarize_failures.py` + `.gitlab/scripts/summarize-failures.system.md`. The auth flow, env var names, AI Gateway URL, and `authanywhere` binary usage were all sourced from that file. The implementation below adapts that pattern for benchmark analysis rather than failure summarization.

---

## Recommended Architecture

### Overview

```
GitLab CI job (single job, two stages inside it)
│
├── Stage 1 — Collect & pre-process
│   ├── Fetch benchmark artifacts (PR branch + main baseline)
│   ├── Run jq pre-processing script → diff-summary JSON
│   └── Write: artifacts/benchmark-diff.json
│
└── Stage 2 — LLM analysis
    ├── Auth: authanywhere → AI Gateway Bearer token
    ├── Auth: dd-octo-sts → GH_TOKEN
    ├── Invoke: claude --bare -p "$(cat .gitlab/benchmark-analysis-prompt.md)"
    │   (reads artifacts/benchmark-diff.json via Read tool)
    └── Output: artifacts/benchmark-report.md → post as PR comment
```

The pipeline is **a single GitLab job** during the prototype phase. The two conceptual stages are sequential shell steps inside that job, not separate GitLab stages. This avoids inter-job artifact passing complexity while the format is still being designed.

---

## Component Boundaries

| Component | Responsibility | Communicates With | File Location |
|-----------|---------------|-------------------|---------------|
| **GitLab CI job definition** | Declare image, rules, artifact paths, id_tokens | GitLab CI | `.gitlab/benchmarks.yml` (extend existing) or `.gitlab/benchmark-analysis.yml` (new include) |
| **Auth script** | Exchange CI OIDC JWT for AI Gateway Bearer + GH_TOKEN | Vault / dd-octo-sts / authanywhere | `.gitlab/scripts/setup-bench-auth.sh` |
| **Pre-processor script** | Parse Criterion JSON + mock dd-trace-py JSON, compute deltas, emit compact diff summary | Local files only | `.gitlab/scripts/process-benchmarks.sh` + `jq` |
| **System prompt** | Tell Claude what to analyze, what to write, and output format | Read by claude CLI at runtime | `.gitlab/scripts/benchmark-analysis-system.md` |
| **Runtime prompt** | One-liner task injected via `-p`; references file paths for the diff JSON | Passed as CLI argument | Inline string in the CI script, or `.gitlab/scripts/benchmark-analysis-prompt.md` |
| **Claude Code CLI** | LLM analysis; reads diff JSON and source tree; writes report | AI Gateway (HTTPS) | Invoked by the CI job |
| **Post-comment script** | Post `benchmark-report.md` as a GitHub PR comment | `gh` CLI → GitHub API | `.gitlab/scripts/post-bench-comment.sh` |
| **Mock data** | Criterion JSON + dd-trace-py JSON fixtures for both PR and main branches | Read by pre-processor | `.gitlab/benchmarks/mock/` |

---

## Data Flow (Sequence)

```
1. GitLab pushes to PR branch
        │
        ▼
2. CI job starts
   Image: registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1
   (contains: Node, gh CLI, dd-octo-sts, authanywhere, jq, Vault CLI)
        │
        ▼
3. Auth setup (setup-bench-auth.sh)
   a. authanywhere --audience rapid-ai-platform  →  ANTHROPIC_AUTH_TOKEN
   b. dd-octo-sts token --scope DataDog/libdatadog --policy <policy>  →  GH_TOKEN
   c. export ANTHROPIC_BASE_URL=https://ai-gateway.us1.ddbuild.io
        │
        ▼
4. Benchmark artifact collection
   a. [Prototype] cp .gitlab/benchmarks/mock/criterion-pr.json       artifacts/raw/criterion-pr.json
   b. [Prototype] cp .gitlab/benchmarks/mock/criterion-main.json     artifacts/raw/criterion-main.json
   c. [Prototype] cp .gitlab/benchmarks/mock/ddtracepy-pr.json       artifacts/raw/ddtracepy-pr.json
   d. [Prototype] cp .gitlab/benchmarks/mock/ddtracepy-main.json     artifacts/raw/ddtracepy-main.json
   [Real] download artifacts from the benchmark trigger job via GitLab API
        │
        ▼
5. Pre-processing (process-benchmarks.sh + jq)
   Input:  artifacts/raw/{criterion,ddtracepy}-{pr,main}.json
   Output: artifacts/benchmark-diff.json
   Content: compact structure with per-benchmark deltas,
            percent changes, and regression/improvement flags.
   Claude does NOT do the numeric diff — it reads the pre-computed result.
        │
        ▼
6. LLM analysis (claude --bare -p ...)
   Env: ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN, ANTHROPIC_API_KEY=not-set
   System prompt: .gitlab/scripts/benchmark-analysis-system.md
   Runtime prompt: "Read artifacts/benchmark-diff.json and write artifacts/benchmark-report.md"
   Allowed tools: Read, Glob, Grep, Bash(jq:*), Bash(grep:*), Write
   CWD: $CI_PROJECT_DIR
   Output: artifacts/benchmark-report.md
        │
        ▼
7. Post PR comment (post-bench-comment.sh)
   gh pr comment $CI_MERGE_REQUEST_IID \
     --repo DataDog/libdatadog \
     --body-file artifacts/benchmark-report.md
        │
        ▼
8. Upload CI artifact
   GitLab artifacts: paths: [artifacts/benchmark-report.md, artifacts/benchmark-diff.json]
   expire_in: 3 months
```

---

## Pipeline Structure

### GitLab CI job

```yaml
benchmark-analysis:
  stage: benchmarks          # or a new 'analysis' stage after benchmarks
  image: registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1
  tags: ["arch:amd64"]
  needs: []                  # prototype: no upstream benchmark job
  rules:
    - if: $CI_EXTERNAL_PULL_REQUEST_IID     # runs on every PR push
      when: always
      interruptible: true
    - when: manual
      allow_failure: true
  id_tokens:
    DDOCTOSTS_ID_TOKEN:
      aud: dd-octo-sts
  script:
    - bash .gitlab/scripts/setup-bench-auth.sh
    - bash .gitlab/scripts/process-benchmarks.sh
    - bash .gitlab/scripts/invoke-claude.sh
    - bash .gitlab/scripts/post-bench-comment.sh
  artifacts:
    name: benchmark-analysis
    paths:
      - artifacts/benchmark-report.md
      - artifacts/benchmark-diff.json
    expire_in: 3 months
    when: always
  variables:
    KUBERNETES_SERVICE_ACCOUNT_OVERWRITE: libdatadog
```

The job is split into four focused shell scripts rather than one long inline script. Each script has a single responsibility and can be tested independently.

### When real benchmarks land

Add `needs: ["benchmarks"]` and change artifact collection from mock files to downloading the real Criterion JSON and dd-trace-py JSON artifacts from the upstream benchmark job via the GitLab API. The pre-processor script is unchanged.

---

## System Prompt Structure

The system prompt lives in a **separate Markdown file** (`.gitlab/scripts/benchmark-analysis-system.md`), not inline in the CI YAML or the runtime prompt. This matches dd-trace-py's pattern and enables iteration without touching the CI definition.

### Sections

```markdown
## Role
You are a performance analyst for the `libdatadog` repository (Datadog's shared Rust
library). A GitLab CI job has produced benchmark comparison data. Your task is to produce
a concise, actionable performance report.

## Your inputs
- `artifacts/benchmark-diff.json` — pre-computed delta summary (see schema below).
  Contains: benchmark name, unit, pr_value, main_value, delta_pct, change_class
  (Regressed | Improved | NoChange | Unknown).
- The source tree at the current working directory.

## Schema
{ benchmarks: [ { id, suite, unit, pr_ns, main_ns, delta_pct, change } ] }

## What to do
1. Read artifacts/benchmark-diff.json.
2. Group benchmarks by change_class. For regressions > 5%, read the relevant source
   in the crate benches/ directory and note what the benchmark exercises.
3. Correlate regressions with crate boundaries — state which libdatadog crate owns
   each regressed benchmark.
4. Write artifacts/benchmark-report.md.

## Output format (benchmark-report.md)
...

## Rules
- Do not restate numbers Claude already has in the diff; add interpretation.
- Use GFM-compatible Markdown.
- Keep the report under 40 lines for easy reading in a PR comment.
- No preamble, no "I hope this helps".
```

The runtime prompt (passed via `-p`) is deliberately minimal: a single instruction referencing the file path. All analytical instructions live in the system prompt.

---

## Pre-processor Responsibility Boundary

**Claude does NOT compute numeric deltas.** A shell + jq script does:

```bash
# process-benchmarks.sh skeleton
jq -n \
  --slurpfile pr   artifacts/raw/criterion-pr.json \
  --slurpfile main artifacts/raw/criterion-main.json \
  '
  [ $pr[0].benchmarks[] as $b |
    $main[0].benchmarks[] | select(.id == $b.id) as $m |
    {
      id:        $b.id,
      suite:     $b.suite,
      unit:      $b.unit,
      pr_ns:     $b.typical_ns,
      main_ns:   $m.typical_ns,
      delta_pct: (($b.typical_ns - $m.typical_ns) / $m.typical_ns * 100),
      change:    (if (($b.typical_ns - $m.typical_ns) / $m.typical_ns) > 0.05 then "Regressed"
                  elif (($b.typical_ns - $m.typical_ns) / $m.typical_ns) < -0.05 then "Improved"
                  else "NoChange" end)
    }
  ]
  ' > artifacts/benchmark-diff.json
```

Rationale: LLMs are unreliable for arithmetic on large tables. Pre-computing the delta means Claude's job is interpretation and narrative, not computation. The 5% threshold is configurable in the script, not buried in a prompt.

---

## Authentication Architecture

Two separate auth paths are required:

```
Path A — AI Gateway (Claude)
  GitLab CI OIDC JWT (id_token aud: dd-octo-sts is for path B)
  → authanywhere --audience rapid-ai-platform
  → ANTHROPIC_AUTH_TOKEN (Bearer, short-lived)
  → ANTHROPIC_BASE_URL=https://ai-gateway.us1.ddbuild.io
  → ANTHROPIC_API_KEY=not-set (must be set to something, gateway ignores it)

Path B — GitHub (PR comment)
  GitLab CI OIDC JWT (id_token aud: dd-octo-sts)
  → dd-octo-sts token --scope DataDog/libdatadog --policy <policy>
  → GH_TOKEN (Bearer, short-lived ~1h)
  → gh pr comment uses GH_TOKEN automatically
```

The dd-octo-sts policy file (`.github/chainguard/`) must grant `pull_requests: write` for the CI job to post comments. The existing `gitlab.github-access.write-contents.sts.yaml` grants `contents: write` and `pull_requests: write` but restricts `ref` to `main|release|…` — a new policy file permitting PR branches is needed.

---

## Artifact Passing Strategy

| Artifact | Produced by | Consumed by | Retention |
|----------|-------------|-------------|-----------|
| `artifacts/raw/criterion-pr.json` | benchmark job (prototype: mock) | pre-processor | 3 days (intermediate) |
| `artifacts/raw/criterion-main.json` | benchmark job on main (prototype: mock) | pre-processor | 3 days (intermediate) |
| `artifacts/benchmark-diff.json` | pre-processor | Claude + CI artifact | 3 months |
| `artifacts/benchmark-report.md` | Claude | PR comment + CI artifact | 3 months |
| `artifacts/claude.stdout.log` | claude invocation | debugging | 3 months |

During the prototype all `artifacts/raw/` files are mock fixtures committed to the repo under `.gitlab/benchmarks/mock/`. When real benchmarks land, the pre-processor fetches them from the upstream job's GitLab artifact download URL.

**Artifact path convention:** everything under `artifacts/` in `$CI_PROJECT_DIR`. The GitLab artifact stanza publishes the whole directory. Intermediate raw files can be excluded from the published artifact with an `exclude:` block to save space.

---

## Suggested Implementation Order

The following order respects hard dependencies (each step builds on prior outputs):

1. **Auth scripts only** — write `setup-bench-auth.sh` that calls `authanywhere` and `dd-octo-sts`, exports the four env vars (`ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY`, `GH_TOKEN`), and exits 0. Verify manually in a CI job with an `echo` of each variable name (not value). Nothing else can proceed without working auth.

2. **Mock data fixtures** — commit Criterion JSON and dd-trace-py mock JSON under `.gitlab/benchmarks/mock/`. Keep them realistic (two or three benchmarks each, one regression, one improvement, one no-change). These unlock all downstream testing without real benchmark runs.

3. **Pre-processor script** — write `process-benchmarks.sh` + jq pipeline that reads mock fixtures and produces `artifacts/benchmark-diff.json`. Test the output schema locally with `jq . artifacts/benchmark-diff.json` before wiring to CI.

4. **System prompt + CI job skeleton** — write `benchmark-analysis-system.md` and the minimal `invoke-claude.sh` script. Run the job against mock data and verify `benchmark-report.md` is produced. Iterate on the system prompt until the output is useful. Do not add PR commenting yet — comment posting introduces a GitHub API call that complicates early iteration.

5. **PR comment posting** — write `post-bench-comment.sh` using `gh pr comment`. Wire the dd-octo-sts policy. Test by posting to a draft PR. Only after this is confirmed working, enable the job on every PR push.

6. **Integration** — switch `process-benchmarks.sh` to download real artifacts from the upstream benchmark job when `$BENCHMARK_JOB_ARTIFACT_URL` is set, falling back to mocks when it is not. This allows the job to be useful in parallel with Augusto's triggering workstream.

---

## Anti-Patterns to Avoid

### Pre-processor inside the prompt
**What:** Telling Claude "here are two JSON files, compute the percent change for each benchmark".
**Why bad:** LLMs make arithmetic errors on tables of numbers; Claude will occasionally produce wrong delta values that look plausible. Credibility of the report depends on correct numbers.
**Instead:** Shell + jq computes all numbers; Claude only interprets.

### Inline system prompt in YAML
**What:** Putting the full system prompt as a multiline string in `.gitlab-ci.yml` or in the CI script.
**Why bad:** YAML escaping of Markdown (backticks, `#`, `*`) is error-prone; the prompt cannot be iterated without touching the CI definition and triggering a full pipeline run.
**Instead:** Separate `.md` file read at runtime via `--system-prompt-file` or passed as a variable to the invoke script.

### Running claude as root
**What:** The CI job user is non-root (`dog` in `dd-octo-sts-ci-base`). Running `sudo npm install -g @anthropic-ai/claude-code` will fail.
**Instead:** nvm install into `$HOME/.nvm`, or use the AI Platform sandbox base image which pre-installs `claude` as the `dog` user.

### Passing raw full Criterion JSON to Claude
**What:** Dumping all `cargo bench --message-format=json` output directly into Claude's context.
**Why bad:** Criterion JSON is verbose — a 10-benchmark run produces 50 KB of JSON with duplicate fields (warmup, sample counts, individual sample times). This eats context window and makes the prompt hard to follow.
**Instead:** Pre-process to the diff schema (one object per benchmark, five fields).

### Long-lived PAT for GitHub
**What:** Storing a `GITHUB_TOKEN` with `repo` scope as a GitLab CI variable.
**Why bad:** Long-lived; not rotated; fails security audit.
**Instead:** dd-octo-sts with a scoped policy; token TTL is ~1h and automatically rotated per job.

### One giant CI job script
**What:** A 200-line `script:` block in the CI YAML.
**Why bad:** Untestable, unreadable, cannot be run locally for debugging.
**Instead:** Four focused shell scripts (auth, collect, analyze, comment), each invokable independently.

---

## Scalability Considerations

| Concern | At prototype | When benchmarks land | At steady state |
|---------|-------------|---------------------|-----------------|
| Context window | Mock data is tiny; no problem | Pre-processor keeps diff compact regardless of benchmark count | Add a "top-N regressions only" filter in the pre-processor if benchmark count > 50 |
| Job duration | <5 min (mock data + Claude call) | Depends on benchmark job duration (upstream); analysis step stays <5 min | Analysis step stays decoupled from benchmark duration |
| AI Gateway rate limits | Low volume (one run per PR push) | Same | Add `--max-turns 5` ceiling to bound token usage per invocation |
| PR comment size | Small | May grow with many benchmarks | Pre-processor can cap report to top-10 changes by magnitude |

---

## Sources

- `DataDog/dd-trace-py`: `.gitlab/scripts/summarize_failures.py` — authanywhere auth flow, claude-agent-sdk invocation pattern, AI Gateway env vars
- `DataDog/dd-trace-py`: `.gitlab/scripts/summarize-failures.system.md` — system prompt structure reference
- `DataDog/dd-trace-py`: `.gitlab/scripts/post-pr-comment.sh` — pr-commenter vs gh CLI comparison
- `DataDog/libdatadog`: `.gitlab/benchmarks.yml` — existing Criterion benchmark job structure
- `DataDog/libdatadog`: `.github/chainguard/gitlab.github-access.write-contents.sts.yaml` — dd-octo-sts policy pattern
- `DataDog/libdatadog`: `.github/workflows/rustfmt-auto.yml` — dd-octo-sts-action usage from GitHub Actions (same token mechanism)
- `DataDog/datadog-images`: `ai-platform-agent-sandbox-base-image/1.1.0/Dockerfile` — CI image with claude pre-installed
- `DataDog/datadog-images`: `profiling-ai-evaluation/profiling_ai_evaluation/files/entrypoint.sh` — `--dangerously-skip-permissions` vs proper headless invocation
