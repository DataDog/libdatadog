# Phase 1: Auth & CI Scaffolding - Research

**Researched:** 2026-06-15
**Domain:** GitLab CI, Vault OIDC auth (authanywhere), dd-octo-sts GitHub token federation, Claude Code CLI
**Confidence:** HIGH

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **D-01:** New included file `.gitlab/bench-analysis.yml`, referenced from `.gitlab-ci.yml` via `include: - local: .gitlab/bench-analysis.yml`. Matches the existing `benchmarks.yml` / `fuzz.yml` pattern.
- **D-02:** Runner tag: `gcp:general-purpose` — no specialized hardware needed.
- **D-03:** Trigger: every push to any PR branch (prototype behaviour). GitLab rules condition: `if: $CI_MERGE_REQUEST_IID` or branch pattern — planner to confirm exact rule syntax.
- **D-04:** Install via nvm + npm at job start: `nvm install --lts && npm install -g @anthropic-ai/claude-code`. No custom image for v1.
- **D-05:** CI base image: `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` (pinned, as stated in project constraints).
- **D-06:** Auth order: `authanywhere --audience rapid-ai-platform` → `ANTHROPIC_AUTH_TOKEN`, then `dd-octo-sts` → `GH_TOKEN`. Fetch `authanywhere` token immediately before the Claude invocation to minimize expiry window.
- **D-07:** Auth failure behaviour: fail the job immediately with a clear error message. No partial runs, no silent continue.
- **D-08:** The dd-octo-sts Chainguard policy file (REPORT-03) is created **in Phase 1** — auth scaffolding is the right place. File location: `.github/chainguard/` with `pull_requests: write` for PR branches (not restricted to `main`/`release`).
- **D-09:** Smoke test command: `claude --bare -p 'echo hello' --allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions`. Exit code 0 = pass.

### Claude's Discretion
- Exact `rules:` syntax for the PR trigger (planner to use standard GitLab MR trigger pattern).
- nvm version to install (use latest LTS).

### Deferred Ideas (OUT OF SCOPE)
- Label-based trigger (`benchmark` label) — v2 feature.
- Custom CI image with Claude Code pre-baked — deferred to v2.
- Degraded GitHub comment on auth failure — deferred to a later phase.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| CI-01 | A GitLab CI job exists in `.gitlab-ci.yml` (or an included file) that runs the benchmark analysis pipeline on libdatadog PRs | D-01 locked: `.gitlab/bench-analysis.yml` included from `.gitlab-ci.yml`. Existing pattern in `.gitlab/benchmarks.yml` and `.gitlab/fuzz.yml`. |
| CI-02 | The CI job authenticates with the Datadog AI Gateway via `authanywhere --audience rapid-ai-platform`, storing the bearer token as `ANTHROPIC_AUTH_TOKEN` | `authanywhere` is pre-installed in `dd-octo-sts-ci-base` image. Token must be fetched immediately before Claude invocation (D-06). |
| CI-03 | The CI job obtains a short-lived GitHub token via `dd-octo-sts` and exports it as `GH_TOKEN`; no static PATs are used | `dd-octo-sts token --scope DataDog/libdatadog --policy <policy-name>` pattern confirmed. A new policy file must be created in `.github/chainguard/` that allows PR branches (D-08). |
| CI-04 | The CI job invokes Claude Code CLI with `claude --bare -p` using `--allowedTools "Read,Write,Glob,Grep"` and `--permission-mode bypassPermissions` | `@anthropic-ai/claude-code` 2.1.177 confirmed on npm. All flags verified via local `claude --help`. ANTHROPIC_BASE_URL must be set to Datadog AI Gateway endpoint. |
</phase_requirements>

## Summary

Phase 1 wires up a new GitLab CI job that performs OIDC-based authentication with both Datadog's AI Gateway and GitHub, installs Claude Code CLI, and proves end-to-end invocability via a smoke test. All four requirements (CI-01 through CI-04) are straightforward given existing infrastructure in the repo.

The repo already has three precedents directly applicable: (1) `.gitlab/benchmarks.yml` and `.gitlab/fuzz.yml` show the exact include/job structure to follow; (2) `.github/chainguard/gitlab.github-access.write-contents.sts.yaml` is an existing GitLab-issuer dd-octo-sts policy — the new PR-branch policy follows this pattern identically but widens the `ref` claim to match any branch when a MR is present; (3) the `dd-octo-sts token` CLI is available on the local machine and its flags are confirmed.

The only open unknowns are Datadog-internal: whether `authanywhere` is pre-installed in the `dd-octo-sts-ci-base` image (likely yes, given the image name), and the exact behaviour of the AI Gateway `ANTHROPIC_BASE_URL` endpoint format. Both are low-risk — the job can probe `authanywhere` availability in `before_script` and fail fast with a clear error if absent.

**Primary recommendation:** Follow the `benchmarks.yml` job structure exactly. Create the new chainguard policy by copying and widening `gitlab.github-access.write-contents.sts.yaml`. Set `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` immediately before the `claude` invocation.

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| GitLab CI job definition | CI / GitLab YAML | — | Job runs on GitLab's runner infrastructure |
| AI Gateway auth token | CI runner (shell) | — | `authanywhere` CLI runs in the CI shell, mints a short-lived JWT |
| GitHub token federation | CI runner (shell) | GitHub (policy enforcement) | `dd-octo-sts` CLI exchanges a GitLab OIDC token for a GitHub installation token |
| Claude Code invocation | CI runner (shell) | Datadog AI Gateway (LLM backend) | `claude` CLI runs locally in the runner and routes requests through the gateway |
| dd-octo-sts policy | GitHub repo (`.github/chainguard/`) | dd-octo-sts service | Policy file lives in the GitHub repo; the dd-octo-sts service reads it to validate claims |

## Standard Stack

### Core

| Library / Tool | Version | Purpose | Why Standard |
|----------------|---------|---------|--------------|
| `@anthropic-ai/claude-code` | 2.1.177 | Claude Code CLI — non-interactive mode via `--bare -p` | Official Anthropic package; 11.8M downloads/wk; used in PHP reference pattern [VERIFIED: npm registry] |
| `authanywhere` | pre-installed in image | Vault OIDC JWT minter for `rapid-ai-platform` audience | Datadog internal standard for CI → AI Gateway auth; referenced in PROJECT.md constraints [ASSUMED: Datadog internal tooling] |
| `dd-octo-sts` CLI | latest in image | GitHub token federation via Chainguard/dd-octo-sts | Already used in this repo's GitHub Actions (release-proposal-dispatch.yml, rustfmt-auto.yml) [VERIFIED: codebase] |
| GitLab CI YAML | GitLab 17.x | Job definition language | Repo already uses GitLab CI; existing jobs are the template [VERIFIED: codebase] |

### Supporting

| Tool | Version | Purpose | When to Use |
|------|---------|---------|-------------|
| `nvm` | latest in image | Node.js version manager | Install Node LTS + npm before `@anthropic-ai/claude-code` (D-04) |
| `node` / `npm` | LTS (v22+) | Runtime for Claude Code CLI | Required because Claude Code is a Node.js binary |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `nvm install --lts` at job start | Pre-baked custom image | Custom image is cleaner but deferred to v2 per D-04 |
| `claude --bare -p` | Direct Anthropic API call | Direct API requires API key management; `claude` CLI handles gateway routing and tool use |

**Installation (in CI script):**
```bash
# Install Node LTS + Claude Code CLI
nvm install --lts
npm install -g @anthropic-ai/claude-code
```

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | Verdict | Disposition |
|---------|----------|-----|-----------|-------------|---------|-------------|
| `@anthropic-ai/claude-code` | npm | ~16 months (created 2025-02-24) | 11.8M/wk | github.com/anthropics/claude-code | OK | Approved — official Anthropic package [VERIFIED: npm registry] |

**Note on postinstall:** `@anthropic-ai/claude-code` ships a `node install.cjs` postinstall script. This is expected for a compiled CLI tool (downloads the appropriate binary for the platform). This is the official Anthropic package.

**Packages removed due to [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

## Architecture Patterns

### System Architecture Diagram

```
GitLab push to PR branch
        │
        ▼
  GitLab CI pipeline
  (bench-analysis job)
        │
        ├─► authanywhere --audience rapid-ai-platform
        │         │
        │         ▼
        │   ANTHROPIC_AUTH_TOKEN (short-lived Vault JWT)
        │
        ├─► dd-octo-sts token --scope DataDog/libdatadog --policy bench-analysis.write-pr
        │         │
        │         ▼
        │   GH_TOKEN (short-lived GitHub installation token)
        │
        ├─► nvm + npm install @anthropic-ai/claude-code
        │
        └─► claude --bare -p 'echo hello'
                  --allowedTools "Read,Write,Glob,Grep"
                  --permission-mode bypassPermissions
                        │
                        ▼ ANTHROPIC_BASE_URL → Datadog AI Gateway
                  exit 0 = smoke test passed
```

### Recommended Project Structure

```
.gitlab/
└── bench-analysis.yml          # New CI job definition (D-01)
.github/
└── chainguard/
    ├── gitlab.github-access.write-contents.sts.yaml   # Existing (contents:write for main/release)
    └── bench-analysis.write-pr.sts.yaml               # New: pull_requests:write for PR branches (D-08)
```

### Pattern 1: GitLab CI Job Structure (from existing benchmarks.yml)

**What:** A self-contained job in an included YAML file, with `needs: []` for independent execution and `rules:` for trigger conditions.

**When to use:** Always, per existing repo convention.

**Example (adapted from `.gitlab/benchmarks.yml`):**
```yaml
# Source: .gitlab/benchmarks.yml (codebase)
bench-analysis:
  tags: ["gcp:general-purpose"]
  needs: []
  image:
    name: registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1
  rules:
    - if: $CI_MERGE_REQUEST_IID
  timeout: 10m
  script:
    - # ... auth and invocation steps
  artifacts:
    paths:
      - artifacts/
    expire_in: 3 months
```

### Pattern 2: GitLab MR Trigger Rule

**What:** `$CI_MERGE_REQUEST_IID` is populated when a pipeline runs in merge request context (requires `workflow: rules:` or job-level `rules:` using merge request pipelines). [ASSUMED: standard GitLab CI syntax from training knowledge]

**When to use:** When a job must only run on PRs, not on direct branch pushes.

**Note:** The project's `.gitlab-ci.yml` uses `$CI_EXTERNAL_PULL_REQUEST_IID` for GitHub-mirrored PRs. For GitLab native MRs use `$CI_MERGE_REQUEST_IID`. The planner must verify which variable is populated in this GitLab setup. [ASSUMED: exact variable depends on GitLab project mirroring configuration]

**Recommended approach for prototype (trigger on any push to any branch):**
```yaml
rules:
  - when: always
```
Or to scope to MR context:
```yaml
rules:
  - if: $CI_MERGE_REQUEST_IID
    when: always
  - if: $CI_EXTERNAL_PULL_REQUEST_IID
    when: always
```

### Pattern 3: dd-octo-sts Policy File for GitLab Issuer (from existing .github/chainguard/)

**What:** A YAML file in `.github/chainguard/` that specifies which GitLab CI identities can receive which GitHub permissions.

**When to use:** Whenever a GitLab CI job needs to write to GitHub (PRs, contents, etc.).

**Example (new policy for PR branches — widened from existing `gitlab.github-access.write-contents.sts.yaml`):**
```yaml
# Source: .github/chainguard/gitlab.github-access.write-contents.sts.yaml (codebase pattern)
issuer: https://gitlab.ddbuild.io

subject_pattern: "project_path:DataDog/.*"

claim_pattern:
  project_id: "2260"
  # No ref restriction — allow any branch when running on a MR

permissions:
  pull_requests: write
```

**Key insight:** The existing `gitlab.github-access.write-contents.sts.yaml` restricts `ref` to `(main|release|...)`. The new policy for posting PR comments must omit the `ref` restriction or use a broad pattern, since feature branches can be named anything. [VERIFIED: codebase analysis]

### Pattern 4: authanywhere → ANTHROPIC_AUTH_TOKEN → Claude Code

**What:** Fetch a short-lived Vault JWT immediately before invoking Claude, export as `ANTHROPIC_AUTH_TOKEN`, set `ANTHROPIC_BASE_URL` to the AI Gateway endpoint.

**When to use:** Every invocation of Claude Code in CI.

**Example (from PROJECT.md AI Gateway description + PHP reference pattern [ASSUMED: PHP pattern not directly readable]):**
```bash
# Fetch token immediately before invocation (minimizes expiry window per D-06)
ANTHROPIC_AUTH_TOKEN=$(authanywhere --audience rapid-ai-platform)
export ANTHROPIC_AUTH_TOKEN
export ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io/anthropic"
export ANTHROPIC_HEADER_DD_AI_SOURCE="bench-analysis"

claude --bare -p 'echo hello' \
  --allowedTools "Read,Write,Glob,Grep" \
  --permission-mode bypassPermissions
```

**Note:** The exact AI Gateway URL path suffix (`/anthropic` vs just base domain), and required custom headers (`DD-AI-Source`, `DD-AI-Org-ID`, etc.) are [ASSUMED] from PROJECT.md description. The planner must treat the exact header names as requiring verification against the PHP reference or gateway docs.

### Anti-Patterns to Avoid

- **Static PATs in CI variables:** `GH_TOKEN` must come from `dd-octo-sts` on every run; never store a long-lived token as a GitLab CI variable.
- **Fetching `ANTHROPIC_AUTH_TOKEN` at job start:** Token may expire before Claude is invoked if nvm/npm installation takes time. Fetch immediately before `claude` invocation (D-06).
- **Silent auth failure:** If `authanywhere` or `dd-octo-sts` exits non-zero, the job must fail immediately with `set -e` or explicit `|| exit 1` (D-07).
- **Running as root:** The `dd-octo-sts-ci-base` image runs as a non-root user. Install nvm to `$HOME/.nvm` using the standard nvm install script or check if it's pre-installed.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Vault OIDC token minting | Custom curl against Vault | `authanywhere --audience rapid-ai-platform` | `authanywhere` handles the OIDC exchange, JWT formatting, and datacenter routing [ASSUMED] |
| GitHub token federation | Store a static PAT | `dd-octo-sts token` | Short-lived tokens, OIDC-based, already used in this repo for releases [VERIFIED: codebase] |
| Claude non-interactive invocation | Custom script driving the API | `claude --bare -p '...'` | `--bare -p` handles stdin/stdout, tool use, and output formatting for CI use |
| nvm installation check | Manual PATH manipulation | Check `nvm` availability then `nvm install --lts` | nvm may already be in the base image; blindly re-running install is idempotent |

**Key insight:** Every auth concern has a Datadog-internal tool. Never attempt to replicate these with raw API calls — the tooling handles key rotation, expiry, and environment-specific routing.

## Common Pitfalls

### Pitfall 1: nvm not sourced in non-interactive shells

**What goes wrong:** `nvm` is installed but `nvm: command not found` in CI because nvm requires sourcing `$NVM_DIR/nvm.sh` in bash profile files that don't execute in non-interactive CI shells.

**Why it happens:** GitLab CI `script:` blocks run in a non-login, non-interactive shell. `.bashrc` and `.profile` sourcing of nvm is skipped.

**How to avoid:** Explicitly source nvm before use:
```bash
export NVM_DIR="${HOME}/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"
nvm install --lts
```
Or check if nvm is already available as a direct command first.

**Warning signs:** `nvm: command not found` in CI logs even though the base image lists nvm as pre-installed.

### Pitfall 2: ANTHROPIC_BASE_URL path suffix mismatch

**What goes wrong:** Claude Code connects to the AI Gateway but gets 404 or auth errors because the URL path is wrong (e.g., missing `/anthropic` suffix or wrong versioned path).

**Why it happens:** The AI Gateway URL format for Claude Code may differ from the format used by direct Anthropic SDK calls. [ASSUMED]

**How to avoid:** Use the exact URL format from the PHP reference implementation. If unavailable, test with a minimal `claude --bare -p 'hello'` call first.

**Warning signs:** HTTP 404 or `{"error": "unknown route"}` in Claude Code output.

### Pitfall 3: dd-octo-sts policy ref restriction too narrow

**What goes wrong:** `dd-octo-sts token` fails with a claim mismatch error because the new policy file restricts `ref` to protected branches, but the job runs on a feature branch.

**Why it happens:** Copying the existing `gitlab.github-access.write-contents.sts.yaml` without removing or widening the `ref` claim pattern.

**How to avoid:** The new policy for `bench-analysis.write-pr` must either omit the `ref` claim or use a broad pattern. The existing `self.write.rustfmt.sts.yaml` (GitHub issuer) uses `subject_pattern: "repo:DataDog/libdatadog:pull_request"` without a ref restriction — similar approach needed for the GitLab issuer variant.

**Warning signs:** dd-octo-sts error like `claim mismatch: ref` in job logs.

### Pitfall 4: authanywhere not available in image

**What goes wrong:** `authanywhere: command not found` because it's not pre-installed in `dd-octo-sts-ci-base:2025.06-1`.

**Why it happens:** The image name suggests dd-octo-sts tooling is present, but `authanywhere` may require a separate install. [ASSUMED: availability unverified per STATE.md]

**How to avoid:** Add an early probe in `before_script`:
```bash
command -v authanywhere || { echo "ERROR: authanywhere not found in image"; exit 1; }
```
This surfaces the missing dependency immediately with a clear error (D-07).

**Warning signs:** The job script gets past auth setup with an empty `ANTHROPIC_AUTH_TOKEN`.

### Pitfall 5: CI_MERGE_REQUEST_IID vs CI_EXTERNAL_PULL_REQUEST_IID

**What goes wrong:** The trigger rule uses `$CI_MERGE_REQUEST_IID` but this variable is only populated in native GitLab MR pipelines, not in pipelines triggered by GitHub PR mirroring.

**Why it happens:** The repo is mirrored from GitHub. GitLab may run pipelines in "detached pipeline" mode for mirrored pushes, where `$CI_EXTERNAL_PULL_REQUEST_IID` is populated instead.

**How to avoid:** Use both conditions:
```yaml
rules:
  - if: $CI_MERGE_REQUEST_IID
    when: always
  - if: $CI_EXTERNAL_PULL_REQUEST_IID
    when: always
```
Or for prototype simplicity, use `when: always` to trigger on all pushes.

**Warning signs:** Job never appears in pipeline even when a PR exists.

## Code Examples

### Full job skeleton (`.gitlab/bench-analysis.yml`)

```yaml
# Source: .gitlab/benchmarks.yml (codebase) + .gitlab/fuzz.yml (codebase) — structural template
bench-analysis:
  tags: ["gcp:general-purpose"]
  needs: []
  image:
    name: registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1
  rules:
    - if: $CI_MERGE_REQUEST_IID
      when: always
    - if: $CI_EXTERNAL_PULL_REQUEST_IID
      when: always
  timeout: 10m
  script:
    # --- Probe for required tools ---
    - command -v authanywhere || { echo "ERROR: authanywhere not found"; exit 1; }
    # --- GitHub token (fetch early; GH_TOKEN doesn't expire as fast as Vault JWT) ---
    - GH_TOKEN=$(dd-octo-sts token --scope DataDog/libdatadog --policy bench-analysis.write-pr)
    - export GH_TOKEN
    # --- Install Node + Claude Code ---
    - export NVM_DIR="${HOME}/.nvm"
    - '[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"'
    - nvm install --lts
    - npm install -g @anthropic-ai/claude-code
    # --- Fetch AI Gateway token immediately before invocation ---
    - ANTHROPIC_AUTH_TOKEN=$(authanywhere --audience rapid-ai-platform)
    - export ANTHROPIC_AUTH_TOKEN
    - export ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io/anthropic"
    # --- Smoke test ---
    - claude --bare -p 'echo hello' --allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions
  artifacts:
    paths:
      - artifacts/
    expire_in: 1 month
```

### New dd-octo-sts policy file (`.github/chainguard/bench-analysis.write-pr.sts.yaml`)

```yaml
# Source: .github/chainguard/gitlab.github-access.write-contents.sts.yaml (codebase pattern, widened)
issuer: https://gitlab.ddbuild.io

subject_pattern: "project_path:DataDog/.*"

claim_pattern:
  project_id: "2260"
  # No ref restriction: bench-analysis runs on any PR branch

permissions:
  pull_requests: write
```

### `.gitlab-ci.yml` addition

```yaml
# Source: .gitlab-ci.yml (codebase) — existing include block pattern
include:
  - local: .gitlab/benchmarks.yml
  - local: .gitlab/fuzz.yml
  - local: .gitlab/bench-analysis.yml   # ADD THIS LINE
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Static PAT for GitHub API in CI | dd-octo-sts short-lived federation | ~2023 Datadog internal migration | No long-lived secrets; tokens auto-expire |
| Direct Anthropic API key | AI Gateway + Vault JWT (`authanywhere`) | Datadog AI Gateway adoption | Centralised auth, no per-project Anthropic keys |
| `claude` interactive mode | `claude --bare -p` non-interactive | Claude Code CLI v1+ | Enables scripted, non-TTY CI invocation |

**Deprecated/outdated:**
- Static GitLab CI variables for GitHub tokens: replaced by dd-octo-sts everywhere in this repo.
- `claude` without `--bare` in CI: `--bare` suppresses hooks, LSP sync, and keychain reads that break in headless CI environments.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `authanywhere` is pre-installed in `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` | Standard Stack, Pitfall 4 | Job fails at auth probe step; fix: add install step or use different image |
| A2 | `ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io/anthropic"` is the correct URL for Claude Code | Pattern 4, Pitfall 2 | Claude Code reports connection error; fix: consult PHP reference or gateway docs |
| A3 | `CI_MERGE_REQUEST_IID` is populated for GitHub-mirrored PR pipelines in this GitLab setup | Pitfall 5, Pattern 2 | Job never triggers on PRs; fix: also add `$CI_EXTERNAL_PULL_REQUEST_IID` rule |
| A4 | `authanywhere --audience rapid-ai-platform` outputs only the token (no wrapper JSON) | Pattern 4 code example | Token capture fails; fix: pipe through `jq -r '.token'` or similar |
| A5 | `nvm` is pre-installed in `dd-octo-sts-ci-base:2025.06-1` | Pitfall 1 | `nvm: command not found`; fix: install nvm via curl before use |
| A6 | No additional custom HTTP headers are required beyond `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_BASE_URL` for Claude Code to reach the AI Gateway | Pattern 4 | Gateway rejects request with 403 if headers like `DD-AI-Source` are required but absent |

## Open Questions

1. **Exact `authanywhere` output format**
   - What we know: The token must be exported as `ANTHROPIC_AUTH_TOKEN`; PROJECT.md says "bearer token"
   - What's unclear: Does `authanywhere` output raw token or JSON?
   - Recommendation: Check the PHP reference job (`dd-trace-php/.gitlab/libdatadog-latest.yml`) — it's the canonical usage. If inaccessible, add `| tr -d '\n'` as defensive measure.

2. **`ANTHROPIC_BASE_URL` exact path**
   - What we know: Gateway is at `https://ai-gateway.us1.ddbuild.io`; PROJECT.md mentions custom headers
   - What's unclear: Does Claude Code expect `/anthropic`, `/v1`, or just the base URL?
   - Recommendation: Planner should note this as a `checkpoint:human-verify` before the smoke test task, or source from the PHP reference.

3. **Whether `dd-octo-sts` CLI is used directly in GitLab CI (vs CI/CD variable injection)**
   - What we know: `dd-octo-sts token --scope DataDog/libdatadog --policy <name>` is the CLI pattern; the image is named `dd-octo-sts-ci-base`
   - What's unclear: The image may inject the token via environment variables automatically rather than requiring a CLI call
   - Recommendation: Treat CLI call as the safe default; the image name is suggestive but not conclusive.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` | All CI steps | Unknown (CI-only) | 2025.06-1 | No fallback — this is the pinned image per D-05 |
| `authanywhere` | CI-02 | Unknown (CI image) | Unknown | No fallback — required for AI Gateway auth |
| `dd-octo-sts` CLI | CI-03 | Pre-installed (image name implies it; also available locally at `/opt/homebrew/bin/dd-octo-sts`) | See `dd-octo-sts version` in image | No fallback |
| `nvm` | D-04 (Node.js install) | Unknown (CI image) | Unknown | `curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash` |
| `node` / `npm` | Claude Code install | Available (post-nvm) | LTS (v22+) | Install via nvm |
| `@anthropic-ai/claude-code` | CI-04 | Installed via npm | 2.1.177 | No fallback |

**Missing dependencies with no fallback:**
- `authanywhere` in CI image — unverified; must probe in `before_script`
- `dd-octo-sts` in CI image — highly likely given image name but unverified

**Missing dependencies with fallback:**
- `nvm` — can install via curl if not pre-installed

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | None (CI YAML validation + smoke test) |
| Config file | None — validation is the CI job itself |
| Quick run command | `gitlab-ci-lint .gitlab/bench-analysis.yml` (lint only) |
| Full suite command | Push to a PR branch and observe CI job output |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| CI-01 | Job exists and appears in pipeline | smoke | Push to PR branch → verify job appears | ❌ Wave 0 (new file) |
| CI-02 | `ANTHROPIC_AUTH_TOKEN` is non-empty after auth step | smoke | CI job log shows non-empty token export | ❌ Wave 0 |
| CI-03 | `GH_TOKEN` is non-empty after dd-octo-sts | smoke | CI job log shows non-empty GH_TOKEN | ❌ Wave 0 |
| CI-04 | `claude --bare -p 'echo hello' ...` exits 0 | smoke | CI job exits 0 overall | ❌ Wave 0 (new job) |

### Sampling Rate

- **Per task commit:** `gitlab-ci-lint` (YAML syntax check)
- **Per wave merge:** Push to a test PR branch and verify CI job runs to completion
- **Phase gate:** CI job exits 0 on a real PR branch with all 4 requirements satisfied

### Wave 0 Gaps

- [ ] `.gitlab/bench-analysis.yml` — the entire job definition (new file)
- [ ] `.github/chainguard/bench-analysis.write-pr.sts.yaml` — new dd-octo-sts policy
- [ ] `include:` line in `.gitlab-ci.yml` — one-line addition

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | yes | authanywhere (Vault OIDC) + dd-octo-sts (OIDC federation) — no passwords or API keys |
| V3 Session Management | no | Tokens are per-job, not sessions |
| V4 Access Control | yes | dd-octo-sts policy file restricts GitHub permissions to `pull_requests: write` only |
| V5 Input Validation | no | Phase 1 has no user-controlled inputs |
| V6 Cryptography | yes | TLS only (no custom crypto); JWT validation handled by authanywhere and dd-octo-sts |

### Known Threat Patterns for CI / OIDC token federation

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Static secret leakage (PAT in CI variable) | Information Disclosure | dd-octo-sts short-lived tokens; no static PATs |
| Token scope creep | Elevation of Privilege | Policy file grants only `pull_requests: write`; separate policy from `write-contents` policy |
| Token replay from stolen JWT | Repudiation / Spoofing | Short-lived Vault JWTs; fetch immediately before use (D-06) |
| Unauthorized branch triggering CI to mint tokens | Elevation of Privilege | `project_id: "2260"` claim pins policy to this specific GitLab project |

## Sources

### Primary (HIGH confidence)
- `.gitlab-ci.yml` (codebase) — existing include pattern confirmed
- `.gitlab/benchmarks.yml` (codebase) — structural template for job definition
- `.gitlab/fuzz.yml` (codebase) — supplementary structural reference
- `.github/chainguard/gitlab.github-access.write-contents.sts.yaml` (codebase) — confirmed GitLab issuer policy format
- `.github/workflows/rustfmt-auto.yml` (codebase) — confirmed dd-octo-sts action usage pattern
- `npm view @anthropic-ai/claude-code` — confirmed package version 2.1.177, 11.8M downloads/wk
- `claude --help` (local CLI) — confirmed `--bare`, `-p`, `--allowedTools`, `--permission-mode bypassPermissions` flags
- `dd-octo-sts token --help` (local CLI) — confirmed `--scope`, `--policy` flag syntax

### Secondary (MEDIUM confidence)
- PROJECT.md (codebase) — AI Gateway URL `https://ai-gateway.us1.ddbuild.io` and custom headers description
- STATE.md (codebase) — open concern about `authanywhere` availability in image

### Tertiary (LOW confidence / ASSUMED)
- PHP reference pattern for `authanywhere` token output format — not directly readable
- `ANTHROPIC_BASE_URL` path suffix for AI Gateway
- `authanywhere` availability in `dd-octo-sts-ci-base:2025.06-1` image

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all packages verified via npm registry and local CLI
- Architecture: HIGH — full existing CI structure read from codebase; dd-octo-sts policy pattern read from existing files
- Pitfalls: MEDIUM — based on codebase analysis plus known nvm/CI shell issues; authanywhere-specific pitfalls are ASSUMED
- Auth token details: LOW — exact authanywhere output format and AI Gateway URL path are unverified

**Research date:** 2026-06-15
**Valid until:** 2026-07-15 (stable tooling; nvm/Claude Code versions may increment but flags are stable)
