# Phase 1: Auth & CI Scaffolding - Pattern Map

**Mapped:** 2026-06-15
**Files analyzed:** 3
**Analogs found:** 3 / 3

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `.gitlab/bench-analysis.yml` | CI job config | request-response | `.gitlab/benchmarks.yml` | role-match |
| `.github/chainguard/bench-analysis.write-pr.sts.yaml` | CI auth policy | request-response | `.github/chainguard/gitlab.github-access.write-contents.sts.yaml` | exact |
| `.gitlab-ci.yml` (add `include:` line) | CI config (modify) | — | `.gitlab-ci.yml` (existing include block) | exact |

## Pattern Assignments

### `.gitlab/bench-analysis.yml` (CI job config, request-response)

**Analog:** `.gitlab/benchmarks.yml`

**Job skeleton pattern** (lines 6-62, adapted):
```yaml
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
    # Tool probe (fail fast per D-07)
    - command -v authanywhere || { echo "ERROR: authanywhere not found in image"; exit 1; }
    # GitHub token (fetch before Node install; less expiry-sensitive than Vault JWT)
    - GH_TOKEN=$(dd-octo-sts token --scope DataDog/libdatadog --policy bench-analysis.write-pr)
    - export GH_TOKEN
    # Install Node LTS + Claude Code (D-04)
    - export NVM_DIR="${HOME}/.nvm"
    - '[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"'
    - nvm install --lts
    - npm install -g @anthropic-ai/claude-code
    # Fetch AI Gateway token immediately before invocation (D-06)
    - ANTHROPIC_AUTH_TOKEN=$(authanywhere --audience rapid-ai-platform)
    - export ANTHROPIC_AUTH_TOKEN
    - export ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io/anthropic"
    # Smoke test (D-09)
    - claude --bare -p 'echo hello' --allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions
  artifacts:
    paths:
      - artifacts/
    expire_in: 1 month
```

**Key differences from `benchmarks.yml` analog:**
- Tag: `gcp:general-purpose` (not `runner:apm-k8s-tweaked-metal`)
- Image: `dd-octo-sts-ci-base:2025.06-1` (not benchmarking-platform image)
- Rules: MR-trigger conditions (not branch-based)
- Timeout: `10m` (not `1h`)
- No `variables:` block with `KUBERNETES_SERVICE_ACCOUNT_OVERWRITE` (not needed)

**rules: trigger pattern** — from `.gitlab-ci.yml` existing job `trigger_internal_build` (lines 25-45):

The repo uses `$CI_EXTERNAL_PULL_REQUEST_IID` for GitHub-mirrored PRs (seen in `trigger_internal_build` variables at line 18). Both MR variables must be covered:
```yaml
rules:
  - if: $CI_MERGE_REQUEST_IID
    when: always
  - if: $CI_EXTERNAL_PULL_REQUEST_IID
    when: always
```

**artifacts pattern** — from `.gitlab/benchmarks.yml` (lines 48-52):
```yaml
artifacts:
  name: "reports"
  paths:
    - reports/
  expire_in: 3 months
```
New job uses `artifacts/` path and `1 month` expiry (REQUIREMENTS.md REPORT-01 says ≥ 30 days).

---

### `.github/chainguard/bench-analysis.write-pr.sts.yaml` (CI auth policy, request-response)

**Analog:** `.github/chainguard/gitlab.github-access.write-contents.sts.yaml`

**Full analog** (lines 1-11):
```yaml
issuer: https://gitlab.ddbuild.io

subject_pattern: "project_path:DataDog/.*"

claim_pattern:
  project_id: "2260"
  ref: "(main|release|igor/versioning/.*)"
  # ref_protected: "true"

permissions:
  contents: write
  pull_requests: write
```

**New policy** — copy the analog, drop the `ref` restriction, narrow permissions to `pull_requests: write` only:
```yaml
issuer: https://gitlab.ddbuild.io

subject_pattern: "project_path:DataDog/.*"

claim_pattern:
  project_id: "2260"
  # No ref restriction: bench-analysis runs on any PR branch (D-08)

permissions:
  pull_requests: write
```

**Why no ref restriction:** Feature branches can be named anything. The existing `gitlab.github-access.write-contents.sts.yaml` restricts to `main|release|...` which would break on every PR branch. The `self.write.pr.sts.yaml` GitHub-issuer analog confirms this approach (uses `subject_pattern` without narrow `ref`, scoped instead to workflow file path).

---

### `.gitlab-ci.yml` (modify — add `include:` line)

**Analog:** `.gitlab-ci.yml` (lines 8-11):
```yaml
include:
  - local: .gitlab/benchmarks.yml
  - local: .gitlab/fuzz.yml
```

**Change:** Append one line to the existing `include:` block:
```yaml
include:
  - local: .gitlab/benchmarks.yml
  - local: .gitlab/fuzz.yml
  - local: .gitlab/bench-analysis.yml
```

---

## Shared Patterns

### Tool probe / fail-fast (D-07)
**Source:** `.gitlab/fuzz.yml` (line 24) — inline tool-install pattern that exits on failure via shell `set -e` semantics.
**Apply to:** `bench-analysis.yml` `script:` before any auth step.
```bash
command -v authanywhere || { echo "ERROR: authanywhere not found in image"; exit 1; }
```

### nvm sourcing in non-interactive CI shell
**Source:** Known pitfall (RESEARCH.md Pitfall 1); no existing analog in repo.
**Apply to:** `bench-analysis.yml` Node install step.
```bash
export NVM_DIR="${HOME}/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"
nvm install --lts
npm install -g @anthropic-ai/claude-code
```

### needs: [] for independent job execution
**Source:** `.gitlab/benchmarks.yml` (line 8), `.gitlab/fuzz.yml` (line 10).
**Apply to:** `bench-analysis.yml` job definition.
```yaml
needs: []
```

## No Analog Found

No files in this phase lack a codebase analog. All three files have direct structural matches.

## Metadata

**Analog search scope:** `.gitlab/`, `.github/chainguard/`, `.github/workflows/`, `.gitlab-ci.yml`
**Files scanned:** 6
**Pattern extraction date:** 2026-06-15
