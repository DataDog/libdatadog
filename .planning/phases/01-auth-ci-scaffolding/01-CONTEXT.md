# Phase 1: Auth & CI Scaffolding - Context

**Gathered:** 2026-06-15
**Status:** Ready for planning

<domain>
## Phase Boundary

Wire up a new GitLab CI job that authenticates with the Datadog AI Gateway and GitHub, installs Claude Code CLI, and proves end-to-end invocability via a smoke test. No analysis logic runs — just auth and tooling in place.

</domain>

<decisions>
## Implementation Decisions

### Job Placement & Structure
- **D-01:** New included file `.gitlab/bench-analysis.yml`, referenced from `.gitlab-ci.yml` via `include: - local: .gitlab/bench-analysis.yml`. Matches the existing `benchmarks.yml` / `fuzz.yml` pattern.
- **D-02:** Runner tag: `gcp:general-purpose` — no specialized hardware needed.
- **D-03:** Trigger: every push to any PR branch (prototype behaviour). GitLab rules condition: `if: $CI_MERGE_REQUEST_IID` or branch pattern — planner to confirm exact rule syntax.

### Claude Code Installation
- **D-04:** Install via nvm + npm at job start: `nvm install --lts && npm install -g @anthropic-ai/claude-code`. No custom image for v1.
- **D-05:** CI base image: `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` (pinned, as stated in project constraints).

### Auth Sequence & Failure Handling
- **D-06:** Auth order: `authanywhere --audience rapid-ai-platform` → `ANTHROPIC_AUTH_TOKEN`, then `dd-octo-sts` → `GH_TOKEN`. Fetch `authanywhere` token immediately before the Claude invocation to minimize expiry window.
- **D-07:** Auth failure behaviour: fail the job immediately with a clear error message. No partial runs, no silent continue.
- **D-08:** The dd-octo-sts Chainguard policy file (REPORT-03) is created **in Phase 1** — auth scaffolding is the right place. File location: `.github/chainguard/` with `pull_requests: write` for PR branches (not restricted to `main`/`release`).

### Smoke Test
- **D-09:** Smoke test command: `claude --bare -p 'echo hello' --allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions`. Exit code 0 = pass. Uses the full flag set Phase 3 will use, proving the exact invocation pattern works.

### Claude's Discretion
- Exact `rules:` syntax for the PR trigger (planner to use standard GitLab MR trigger pattern).
- nvm version to install (use latest LTS).

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project Requirements
- `.planning/REQUIREMENTS.md` — CI-01, CI-02, CI-03, CI-04 are the four requirements for this phase
- `.planning/PROJECT.md` — Key Decisions table, Constraints section, PHP reference pattern description

### Existing CI Structure
- `.gitlab-ci.yml` — top-level CI file; new job is added via `include:` here
- `.gitlab/benchmarks.yml` — reference for the include pattern, runner tag, and image usage
- `.planning/ROADMAP.md` §Phase 1 — Success Criteria (4 items, all must be TRUE)

### Auth Reference
- No internal file exists yet. The PHP reference (`dd-trace-php/.gitlab/libdatadog-latest.yml`) is cited in PROJECT.md — researcher should locate and read it for exact `authanywhere` and `dd-octo-sts` invocation flags.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `.gitlab/benchmarks.yml` — full working example of a GitLab CI job in this repo: image pinning, runner tag, `rules:`, `artifacts:`, script structure. Use as the structural template.

### Established Patterns
- All CI jobs in this repo use `include: - local:` for modular job definitions.
- The `benchmarks.yml` job uses `needs: []` to run independently — new job should do the same.
- Artifact retention in `benchmarks.yml` uses `expire_in: 3 months` — benchmark-analysis job should use ≥ 30 days per REPORT-01.

### Integration Points
- `.gitlab-ci.yml` `include:` block — add `- local: .gitlab/bench-analysis.yml` here.
- `.github/chainguard/` directory — create the dd-octo-sts policy file here.

</code_context>

<specifics>
## Specific Ideas

- Auth token fetch (`authanywhere`) must happen immediately before `claude` invocation, not at job-start — avoids token expiry if installation takes time.
- Smoke test uses the *exact* Claude invocation flags Phase 3 will use (`--allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions`) so Phase 1 validates the full invocation path, not just CLI presence.

</specifics>

<deferred>
## Deferred Ideas

- Label-based trigger (`benchmark` label) — v2 feature, listed in REQUIREMENTS.md out-of-scope for v1.
- Custom CI image with Claude Code pre-baked — cleaner long-term, but deferred to v2.
- Degraded GitHub comment on auth failure — requires GitHub auth to have already succeeded; deferred to a later phase.

</deferred>

---

*Phase: 1-Auth & CI Scaffolding*
*Context gathered: 2026-06-15*
