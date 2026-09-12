# Phase 3: Claude Analysis - Research

**Researched:** 2026-06-17
**Domain:** Claude Code CLI non-interactive mode, LLM system prompt design, CI shell scripting
**Confidence:** HIGH

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| ANALYSIS-01 | System prompt file instructs Claude to produce a global verdict (pass/warn/fail), list regressions/improvements with noise guard applied, and explicitly prohibits hallucinating causes not visible in the diff or benchmark name | Prompt design patterns derived from Phase 1 Claude CLI invocation (confirmed working in CI) and project-specific constraints on LLM hallucination. |
| ANALYSIS-02 | Shell script invokes Claude with the system prompt and benchmark diff, produces `artifacts/benchmark-report.md`, and asserts the output file is non-empty | Claude CLI flag set (`--bare -p --system-prompt --allowedTools --permission-mode`) confirmed in Phase 1 RESEARCH.md. Non-empty assertion pattern established in preprocess.sh. |
| ANALYSIS-03 | PR diff (from `git diff main...HEAD`) is included in Claude's context so it can identify files/functions that overlap with regressing benchmarks | `git diff main...HEAD` is available inside the CI job (git is in the base image). The diff must be injected into the prompt text, not via a separate file-read tool call, since the benchmark comparison already uses Read. |
</phase_requirements>

## Summary

Phase 3 adds two files to the existing pipeline: a system prompt markdown file and a shell invocation script. The pipeline already authenticates, runs `bp-analyzer`, and produces `artifacts/benchmark-comparison.md`. Phase 3 replaces the current smoke test in `bench-analysis.yml` with a real Claude invocation that reads the comparison, receives the PR diff as context, and writes `artifacts/benchmark-report.md`.

The Claude CLI invocation pattern is already proven in Phase 1. The key new concerns are: (1) prompt engineering — what instructions produce useful, grounded, non-hallucinating output; (2) context assembly — how to get both the benchmark comparison and the PR diff into Claude's context without exceeding token limits; (3) output verification — the `analyze.sh` script must assert the report is non-empty and fail CI if Claude produced nothing.

`git diff main...HEAD` is the right PR diff command (three-dot diff finds the merge base with main, excluding commits already on main). In CI this runs against the checkout the CI runner already has.

**Primary recommendation:** One system prompt file (`.gitlab/bench-analysis/analyze-prompt.md`) + one invocation script (`.gitlab/bench-analysis/analyze.sh`) + wire both into `bench-analysis.yml` after `preprocess.sh`.

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Benchmark comparison input | CI artifact (`artifacts/benchmark-comparison.md`) | — | Produced by Phase 2 preprocess.sh; Phase 3 reads it |
| PR diff extraction | CI shell script | git (in image) | `git diff main...HEAD` runs in the runner's checkout |
| Prompt assembly | CI shell script (`analyze.sh`) | — | Embeds diff + comparison path into the claude invocation |
| LLM analysis | Claude Code CLI | Datadog AI Gateway | `claude --bare -p` routes through the gateway |
| Report output | `artifacts/benchmark-report.md` | — | Written by Claude via `Write` tool |
| Non-empty assertion | CI shell script | — | Same pattern as preprocess.sh |

## Standard Stack

### Core

| Tool | Version | Purpose | Why Standard |
|------|---------|---------|--------------|
| Claude Code CLI (`@anthropic-ai/claude-code`) | pre-installed by Phase 1 | Non-interactive LLM invocation | Already proven in CI; Phase 1 established the exact flag set [VERIFIED: codebase] |
| `git diff` | system | PR diff extraction | Available in `dd-octo-sts-ci-base`; standard git operation [ASSUMED] |
| Bash | system | Invocation script | Matches all existing patterns in `.gitlab/bench-analysis/` [VERIFIED: codebase] |

### Supporting

| Tool | Version | Purpose | When to Use |
|------|---------|---------|-------------|
| Markdown system prompt file | — | Persistent Claude instructions | Keeps prompt out of YAML; reviewable as a doc file |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `--system-prompt-file` flag | Inline `-p` heredoc | File is reviewable, diffable, and reusable; inline heredoc in YAML is brittle |
| `git diff main...HEAD` | `git diff origin/main...HEAD` | In CI the remote is available; use `origin/main` for safety to avoid detached-HEAD edge cases |
| Inject diff as prompt text | Pass diff path and use `Read` tool | Prompt-text injection is simpler and avoids a second tool call; safer for token budget management |

**No new installation:** Claude Code CLI is installed by Phase 1 steps already in `bench-analysis.yml`.

## Package Legitimacy Audit

> No external packages are installed by this phase. Claude Code CLI is already installed by Phase 1. No npm/pip/cargo installs occur.

**Packages removed due to SLOP verdict:** none
**Packages flagged as suspicious:** none

## Architecture Patterns

### System Architecture Diagram

```
artifacts/benchmark-comparison.md   (Phase 2 output)
         |
         v
.gitlab/bench-analysis/analyze.sh
         |
         |--- git diff origin/main...HEAD --> PR_DIFF (shell variable)
         |
         |--- claude --bare -p <prompt> \
         |      --system-prompt-file .gitlab/bench-analysis/analyze-prompt.md \
         |      --allowedTools "Read,Write" \
         |      --permission-mode bypassPermissions \
         |      --model anthropic/claude-sonnet-4-6
         |
         v
artifacts/benchmark-report.md    (assert non-empty → CI pass/fail)
```

### Recommended Project Structure

```
.gitlab/
├── bench-analysis.yml                       # CI job (add analyze.sh step here)
└── bench-analysis/
    ├── analyze-prompt.md                    # NEW: Claude system prompt
    ├── analyze.sh                           # NEW: Claude invocation script
    ├── preprocess.sh                        # Phase 2 (unchanged)
    ├── preprocess.bats                      # Phase 2 (unchanged)
    └── fixtures/
        ├── baseline.json
        └── candidate.json
```

### Pattern 1: Claude `--bare -p` non-interactive invocation

**What:** Passes a prompt string to Claude non-interactively; Claude executes, writes output via tools, then exits.
**When to use:** Any CI context where Claude must run without human interaction.
**Example:**
```bash
# Source: Phase 1 bench-analysis.yml (proven in CI)
claude --bare \
  -p "$(cat <<'EOF'
Read artifacts/benchmark-comparison.md.
Also, here is the PR diff:
${PR_DIFF}

Write a benchmark analysis report to artifacts/benchmark-report.md.
EOF
)" \
  --system-prompt-file .gitlab/bench-analysis/analyze-prompt.md \
  --model anthropic/claude-sonnet-4-6 \
  --allowedTools "Read,Write" \
  --permission-mode bypassPermissions
```
Source: [VERIFIED: codebase — Phase 1 bench-analysis.yml confirmed working]

### Pattern 2: Non-empty output assertion

**What:** Shell check that exits non-zero if Claude produced an empty or missing file.
**When to use:** Any time a script must fail CI if the LLM produced no output.
**Example:**
```bash
# Source: preprocess.sh (established in Phase 2)
if [ ! -s artifacts/benchmark-report.md ]; then
  echo "ERROR: benchmark-report.md is empty — Claude produced no output" >&2
  exit 1
fi
echo "benchmark-report.md generated ($(wc -l < artifacts/benchmark-report.md) lines)"
```
Source: [VERIFIED: codebase — preprocess.sh]

### Pattern 3: PR diff extraction

**What:** Shell command to get the diff between the PR branch and main's merge base.
**When to use:** When Claude needs to correlate benchmark regressions with changed code.
**Example:**
```bash
# Three-dot diff: finds the common ancestor of HEAD and origin/main
# This excludes commits on main that aren't in the PR (correct for PR analysis)
PR_DIFF=$(git diff origin/main...HEAD -- '*.rs' '*.toml' | head -c 50000)
```
Notes:
- `head -c 50000` caps the diff at ~50 KB to stay within token budget.
- Filter `*.rs` and `*.toml` — the benchmarks are Rust; non-Rust diffs add noise.
- If the diff is empty (no changes), Claude should still run — it will report "no relevant code changes found". [ASSUMED]

### Pattern 4: System prompt structure for benchmark analysis

**What:** Markdown instructions that constrain Claude to produce a structured, grounded report.
**When to use:** As the `--system-prompt-file` content for the analyze invocation.
**Recommended structure:**
```markdown
You are a performance analysis assistant for the libdatadog Rust library.

## Task
Analyze the benchmark comparison provided to you and produce a structured report.

## Output format
Write the report to `artifacts/benchmark-report.md` with these sections:
1. **Verdict**: one of `pass` / `warn` / `fail`
   - `fail`: any benchmark is classified `worse` by bp-analyzer
   - `warn`: any benchmark is classified `unsure`
   - `pass`: all benchmarks are `same` or `better`
2. **Regressions**: list benchmarks classified `worse`, with their metric delta
3. **Improvements**: list benchmarks classified `better`, with their metric delta
4. **Noise / Unchanged**: list benchmarks classified `same` or `unsure`
5. **Suspect code changes** (only if Regressions is non-empty): list files or
   functions from the PR diff that overlap with regressing benchmarks, by name only.
   If no overlap is visible, write "No overlapping changes identified."

## Rules
- Base your verdict and lists entirely on the classification labels in the
  benchmark comparison (`worse` / `better` / `same` / `unsure`). Do not
  re-interpret the numbers.
- In "Suspect code changes", name only files or functions that appear in BOTH
  the PR diff AND the benchmark name or the file path of the benchmarked code.
  Do not speculate about causes not visible in the diff.
- Do not mention confidence intervals or p-values — the comparison already
  applied noise filtering.
- Keep the report under 400 lines.
```
Source: [ASSUMED — prompt design based on project requirements and standard LLM prompt engineering practices]

### Anti-Patterns to Avoid

- **Passing `benchmark-comparison.md` content inline in `-p`:** The file can be several hundred lines. Use `--allowedTools "Read,Write"` and let Claude read it with `Read`; only inject the PR diff inline since it is dynamic.
- **Using `git diff HEAD~1`:** This gives only the last commit, not the full PR diff. Use `git diff origin/main...HEAD`.
- **No token cap on PR diff:** Large PRs can produce diffs exceeding 100 KB. Always cap with `head -c` before injecting.
- **Inline prompt in YAML:** Multiline prompts in GitLab YAML `script:` blocks are brittle (quoting, escaping). Use a separate script file and `--system-prompt-file`.
- **Relying on `claude` exit code alone for emptiness detection:** `claude --bare` may exit 0 even if it wrote nothing (e.g., tool call failed silently). Always check `[ -s artifacts/benchmark-report.md ]` explicitly.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Statistical significance of benchmark delta | Custom threshold logic | `bp-analyzer` verdict labels in comparison markdown | bp-analyzer already applied bootstrap CI; re-interpreting numbers risks contradicting its verdict |
| Benchmark report formatting | Custom template renderer | Claude's `Write` tool + system prompt format instructions | LLM handles prose; structured instructions are sufficient |
| Token budget management | Custom chunking system | `head -c 50000` cap on the diff | 50 KB ≈ ~12K tokens — well within Claude's context window; no chunking needed for v1 |

**Key insight:** The jq/statistics layer was fully delegated to `bp-analyzer` in Phase 2. Phase 3 should fully delegate prose generation to Claude. The shell script's only job is to assemble context and assert output existence.

## Common Pitfalls

### Pitfall 1: `git diff` unavailable or gives wrong range in CI

**What goes wrong:** In some CI configurations, the git checkout is shallow (depth 1), which means `origin/main` ref is not fetched. `git diff origin/main...HEAD` fails with "unknown revision".
**Why it happens:** GitLab CI by default fetches with `--depth=20`. The merge base of `origin/main` and `HEAD` may not exist in a shallow clone.
**How to avoid:** Add `git fetch origin main --depth=50` before the diff command. Or use `git diff $(git merge-base origin/main HEAD)...HEAD` with an explicit fetch.
**Warning signs:** `fatal: unknown revision or path 'origin/main'` in CI output.

### Pitfall 2: Claude writes nothing (empty report) and exits 0

**What goes wrong:** Claude encounters a tool error (e.g., `artifacts/` directory doesn't exist) and exits 0 without writing the report. The CI job passes but the artifact is missing.
**Why it happens:** `claude --bare` exit code reflects the CLI process exit, not whether all tool calls succeeded.
**How to avoid:** Always run `mkdir -p artifacts` before invoking Claude (already done by preprocess.sh). Assert `[ -s artifacts/benchmark-report.md ]` after the invocation.
**Warning signs:** Empty `artifacts/` directory after a "successful" Claude run.

### Pitfall 3: Prompt injection via PR diff

**What goes wrong:** A malicious (or accidentally misleading) commit message or file content in the PR diff contains text that overrides Claude's instructions.
**Why it happens:** The PR diff is injected as text into the prompt. If it contains strings like "Ignore previous instructions and...", it may affect Claude's behavior.
**How to avoid:** The CI context is DataDog-internal (not public); risk is low for v1. For defense-in-depth, wrap the diff injection with clear delimiter markers:
```
<pr_diff>
{PR_DIFF}
</pr_diff>
```
and reference it by delimiter name in the system prompt.
**Warning signs:** Report contains unexpected content unrelated to benchmarks.

### Pitfall 4: `--system-prompt-file` path resolution

**What goes wrong:** `analyze.sh` uses a relative path for `--system-prompt-file`, but CI runs it from a different working directory.
**Why it happens:** The GitLab runner's working directory may not be the repo root.
**How to avoid:** Use `${CI_PROJECT_DIR}` or construct the path relative to `${BASH_SOURCE[0]}` (BATS-style). In bench-analysis.yml, the script is invoked as `bash .gitlab/bench-analysis/analyze.sh` from the repo root — document this assumption.
**Warning signs:** `Error: system prompt file not found` in CI output.

## Code Examples

### analyze.sh — full invocation script skeleton

```bash
#!/usr/bin/env bash
# Source: established pattern from preprocess.sh (Phase 2)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPT_FILE="${SCRIPT_DIR}/analyze-prompt.md"
COMPARISON="${COMPARISON:-artifacts/benchmark-comparison.md}"
REPORT="${REPORT:-artifacts/benchmark-report.md}"

# Fail fast if benchmark comparison is missing
if [ ! -s "${COMPARISON}" ]; then
  echo "ERROR: ${COMPARISON} is missing or empty — run preprocess.sh first" >&2
  exit 1
fi

# Fetch PR diff (filter to Rust/TOML, cap at 50 KB)
# git fetch origin main --depth=50 ensures merge-base is available in shallow clones
git fetch origin main --depth=50 2>/dev/null || true
PR_DIFF=$(git diff origin/main...HEAD -- '*.rs' '*.toml' 2>/dev/null | head -c 50000 || echo "(git diff unavailable)")

mkdir -p artifacts

# NVM sourcing required in non-interactive CI shell (Phase 1 pattern)
export NVM_DIR="$HOME/.nvm"
# shellcheck source=/dev/null
[ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"

claude --bare \
  -p "$(printf 'Read %s and write a benchmark analysis report to %s.\n\n<pr_diff>\n%s\n</pr_diff>' \
    "${COMPARISON}" "${REPORT}" "${PR_DIFF}")" \
  --system-prompt-file "${PROMPT_FILE}" \
  --model anthropic/claude-sonnet-4-6 \
  --allowedTools "Read,Write" \
  --permission-mode bypassPermissions

# Assert non-empty output (pattern from preprocess.sh)
if [ ! -s "${REPORT}" ]; then
  echo "ERROR: ${REPORT} is empty — Claude produced no output" >&2
  exit 1
fi

echo "${REPORT} generated ($(wc -l < "${REPORT}") lines)"
```

### bench-analysis.yml — insertion point

```yaml
# After the existing preprocess.sh line:
    - bash .gitlab/bench-analysis/preprocess.sh
# Add:
    - bash .gitlab/bench-analysis/analyze.sh
# Remove or keep the smoke test:
    # (smoke test from Phase 1 can be removed once analyze.sh is wired)
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Inline `-p` prompt in YAML | `--system-prompt-file` + separate script | Phase 3 | Prompt is reviewable and diffable as a doc file |
| Smoke test only | Real analysis invocation | Phase 3 | CI now produces a usable report artifact |

**Deprecated/outdated:**
- Smoke test (`claude --bare -p 'Read the root Cargo.toml...'`): replaced by analyze.sh in this phase. The smoke test line in bench-analysis.yml should be removed when analyze.sh is wired.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `git` is available in `dd-octo-sts-ci-base:2025.06-1` | Architecture Patterns, Pattern 3 | analyze.sh will fail at `git diff` step; fix: use `command -v git` probe |
| A2 | `git diff origin/main...HEAD` produces a useful diff in CI (shallow clone depth ≥ merge base) | Common Pitfalls, Pitfall 1 | Diff will be empty or error; mitigated by `git fetch origin main --depth=50` in analyze.sh |
| A3 | `--system-prompt-file` is a supported flag in `@anthropic-ai/claude-code` version pre-installed by Phase 1 | Standard Stack | If unsupported, inject the system prompt contents into `-p` directly instead |
| A4 | 50 KB diff cap is sufficient — no PR will have > 50 KB of Rust/TOML changes that matter for benchmarks | Pattern 3 | For large PRs the diff is truncated; risk is minor (analysis continues with partial context) |
| A5 | `claude --bare -p` respects `--system-prompt-file` when the prompt is constructed with `printf` / heredoc (no quoting issues in bash) | Pattern 1 | Shell quoting bugs could cause prompt truncation; test locally before CI rollout |

## Open Questions (RESOLVED)

1. **Does `--system-prompt-file` exist in the installed Claude Code version?**
   - What we know: Phase 1 confirmed `claude --bare -p --allowedTools --permission-mode` work in CI.
   - What's unclear: Whether `--system-prompt-file` is in the exact CLI version installed (it was added in claude-code ~0.2.x).
   - **RESOLVED:** Accepted risk for v1 with an in-task probe rather than a fallback branch. Plan 03-01 Task 2's `<verify>` block runs `claude --help | grep -q system-prompt-file` so flag availability is checked at execution time; if the probe fails, the task fails fast and the executor inlines the prompt into `-p` (fallback from A3). The CI context is Datadog-internal and the CLI version is controlled by Phase 1, so a hard dependency on the flag is acceptable. (See Assumptions Log A3.)

2. **Should analyze.sh remove the existing smoke test from bench-analysis.yml?**
   - What we know: The smoke test currently reads Cargo.toml as a validation of Claude invocability.
   - What's unclear: Whether the user wants to keep the smoke test alongside the real invocation (belt-and-suspenders) or replace it.
   - **RESOLVED:** Replace the smoke test. Plan 03-01 Task 3 removes the Phase 1 smoke-test step (`claude --bare -p 'Read the root Cargo.toml...'`) and wires `analyze.sh` in its place — the real analysis invocation is itself a Claude-invocability smoke test, so the standalone smoke test is redundant. This matches the "State of the Art" deprecation note above.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| Claude Code CLI | analyze.sh | ✓ | installed by Phase 1 | — |
| `git` | PR diff extraction | ✓ (assumed in base image) | system | Skip diff section of prompt if absent |
| `artifacts/benchmark-comparison.md` | analyze.sh input | ✓ | produced by Phase 2 preprocess.sh | Script exits with error if missing |
| Datadog AI Gateway + ANTHROPIC_AUTH_TOKEN | Claude invocation | ✓ | set by Phase 1 auth steps | — |

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | Bats (Bash Automated Testing System) |
| Config file | none — tests use `#!/usr/bin/env bats` shebang |
| Quick run command | `bats .gitlab/bench-analysis/analyze.bats` |
| Full suite command | `bats .gitlab/bench-analysis/` |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| ANALYSIS-01 | System prompt file exists and contains the required sections (verdict, regressions, improvements, no-hallucination rule) | smoke | `grep -q 'pass\|warn\|fail' .gitlab/bench-analysis/analyze-prompt.md` | ❌ Wave 0 |
| ANALYSIS-02 | analyze.sh produces non-empty benchmark-report.md given a pre-built comparison | integration | `bats .gitlab/bench-analysis/analyze.bats` | ❌ Wave 0 |
| ANALYSIS-03 | PR diff is included in the prompt (analyze.sh constructs prompt with `<pr_diff>` section) | unit | `grep -q 'pr_diff' .gitlab/bench-analysis/analyze.sh` | ❌ Wave 0 |

### Sampling Rate

- **Per task commit:** `grep -q 'pass\|warn\|fail' .gitlab/bench-analysis/analyze-prompt.md && grep -q 'pr_diff' .gitlab/bench-analysis/analyze.sh`
- **Per wave merge:** `bats .gitlab/bench-analysis/analyze.bats`
- **Phase gate:** Full suite green before `/gsd-verify-work`

### Wave 0 Gaps

- [ ] `.gitlab/bench-analysis/analyze.bats` — covers ANALYSIS-02 (script exists, non-empty output assertion present)
- [ ] `.gitlab/bench-analysis/analyze-prompt.md` — covers ANALYSIS-01 (prompt file exists with required sections)
- [ ] `.gitlab/bench-analysis/analyze.sh` — covers ANALYSIS-02, ANALYSIS-03

*(Bats framework is pre-installed in CI image and used by preprocess.bats in Phase 2 — no install needed.)*

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | Auth handled in Phase 1 |
| V3 Session Management | no | Stateless CI job |
| V4 Access Control | no | Token scoped to `pull_requests: write` only (Phase 1) |
| V5 Input Validation | yes | PR diff injected into prompt — use `<pr_diff>` delimiters to bound the untrusted input |
| V6 Cryptography | no | No new crypto; ANTHROPIC_AUTH_TOKEN managed by Phase 1 |

### Known Threat Patterns for {shell + LLM prompt injection}

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Prompt injection via PR diff content | Tampering | Wrap diff in `<pr_diff>...</pr_diff>` delimiters; system prompt references delimiters by name |
| Token leakage via Claude output | Information Disclosure | `--allowedTools "Read,Write"` restricts Claude to file I/O only; no network access |
| Arbitrary file write via Claude | Tampering | Claude writes only to `artifacts/`; no other paths in prompt |

## Sources

### Primary (HIGH confidence)

- [VERIFIED: codebase] `.gitlab/bench-analysis.yml` — Phase 1 proven invocation pattern
- [VERIFIED: codebase] `.gitlab/bench-analysis/preprocess.sh` — established non-empty assertion and script structure patterns
- [VERIFIED: codebase] `.planning/phases/01-auth-ci-scaffolding/01-RESEARCH.md` — Claude CLI flag verification
- [VERIFIED: codebase] `.planning/phases/02-mock-data-pre-processor/02-CONTEXT.md` — Phase 2 output format (benchmark-comparison.md) and locked decisions

### Secondary (MEDIUM confidence)

- [CITED: REQUIREMENTS.md ANALYSIS-01,02,03] — acceptance criteria for this phase
- [CITED: .planning/ROADMAP.md §Phase 3] — success criteria (3 items)

### Tertiary (LOW confidence)

- [ASSUMED] `--system-prompt-file` Claude CLI flag availability — verified in-task at implementation time via `claude --help | grep system-prompt-file` (see Open Questions RESOLVED #1)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — Claude CLI and Bash patterns proven in CI by Phases 1 and 2
- Architecture: HIGH — pipeline structure is fully determined by Phase 1/2 outputs
- Pitfalls: MEDIUM — git shallow clone and prompt injection are known vectors; exact CI behavior unverified locally
- Prompt design: MEDIUM — structure is sound but actual Claude output quality depends on system prompt tuning

**Research date:** 2026-06-17
**Valid until:** 2026-07-17 (stable domain; Claude CLI API unlikely to break)
