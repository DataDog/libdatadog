# Domain Pitfalls

**Domain:** LLM-augmented CI benchmark analysis pipeline (GitLab CI → Claude via AI Gateway → GitHub PR comment)
**Researched:** 2026-06-15
**Source grounding:** dd-trace-py reference implementation (`summarize_failures.py`, `post-pr-comment.sh`, `summarize-failures.system.md`); libdatadog existing `.gitlab/benchmarks.yml`; PROJECT.md constraints

---

## Critical Pitfalls

### Pitfall 1: authanywhere token is short-lived and call order matters

**What goes wrong:** `authanywhere` (used to get AI Gateway + BTI tokens) issues tokens with short TTLs. If you fetch both tokens at job start and then do a slow data-collection phase before invoking Claude, the AI Gateway token has expired by the time you call it.

**Why it happens:** The auth chain is two-stage — a JWT audience exchange happens at call time, not at shell startup. If you store the `Authorization: Bearer ...` string early and reuse it later, it expires. The dd-trace-py reference avoids this by fetching both tokens in parallel only just before they are used (the authanywhere download + auth happen inside the analysis function, not in `before_script`).

**Consequences:** Claude invocation fails with 401/403 from the AI Gateway. The CI job may succeed (if Claude failure is soft) but no report is posted, silently.

**Prevention:**
- Fetch the AI Gateway token as late as possible, immediately before invoking the Claude SDK/CLI.
- Fetch BTI and AI tokens in parallel (as the reference does with `ThreadPoolExecutor`) but only once you are ready to use both.
- Do not store bearer tokens in CI variables with long TTLs; always re-fetch in the same script invocation.

**Detection:**
- `authanywhere` exits non-zero; HTTP 401 from `ai-gateway.us1.ddbuild.io`.
- Claude SDK raises an auth exception before producing any output.

**Phase:** Phase 1 (auth scaffolding) — get this right before adding anything else.

---

### Pitfall 2: Secrets leaking into CI logs via ANTHROPIC_CUSTOM_HEADERS

**What goes wrong:** `ANTHROPIC_CUSTOM_HEADERS` contains the bearer token on the same line as other non-secret headers. If CI logs are set to verbose (`set -x` or `--debug`), the entire header string — including the `Authorization: Bearer <token>` — is printed to stdout and stored in the GitLab job log, which is accessible to anyone who can see the pipeline.

**Why it happens:** The environment variable pattern used by the reference (`ANTHROPIC_CUSTOM_HEADERS = "source: claude-code\n...\nAuthorization: Bearer <token>"`) is correct for the SDK but dangerous if the calling shell is in debug mode.

**Consequences:** Bearer token visible in GitLab job logs. Any team member (or automation) with pipeline read access can extract and replay the token before it expires.

**Prevention:**
- Never run `set -x` in the same shell block that sets or reads `ANTHROPIC_CUSTOM_HEADERS`.
- In bash scripts, mask the token immediately: `gitlab-ci` has a `mask_variable` mechanism but it only works for CI variables defined in the UI, not dynamically injected strings. Use `--mask-variable` in GitLab CI config instead.
- Prefer to set `ANTHROPIC_CUSTOM_HEADERS` as a masked CI variable or construct it without logging the resolved value. The dd-trace-py reference sets it inline inside a Python `os.environ` dict update — this is safe as long as the Python subprocess does not log its own environment.
- Separate script blocks: one block with `set -e` only for secret resolution, a second block for everything else.

**Detection:**
- Search job log for `Authorization: Bearer` — if present, a secret leaked.
- Add a CI secret-scanning lint step.

**Phase:** Phase 1 (auth scaffolding) and Phase 2 (Claude invocation) — verify both blocks have no `set -x`.

---

### Pitfall 3: Claude produces no output file and the job silently succeeds

**What goes wrong:** Claude is invoked but does not write the expected output file (e.g., `benchmark-report.md`). The posting step checks `[ -s "$REPORT_FILE" ] || exit 0` (as `post-pr-comment.sh` does) and silently no-ops. The CI job exits 0, the PR has no comment, and nobody notices.

**Why it happens:** Multiple causes: Claude's `--max-turns` limit is hit before the Write tool is invoked; the system prompt instructs writing to a path that doesn't exist in the container's working directory; the context window is exhausted mid-analysis and Claude stops before writing the file; or an allowed-tools list that omits `Write` prevents the file from ever being created.

**Consequences:** Benchmark analysis silently disappears. Contributors think the pipeline is healthy. No regression is surfaced.

**Prevention:**
- Always check `ALLOWED_TOOLS` includes `Write`.
- After the Claude invocation, assert the output file exists and is non-empty — if not, exit non-zero so the job is visible as failed.
- Set `--max-turns` high enough for the task (analysis + one Write call = minimum 2 turns; realistic: 8–15). Start with 20 and tune down.
- Save `claude.stdout.log` as a CI artifact unconditionally (as the reference does) so you can debug what Claude actually did.
- The system prompt must name the exact output path; "write a report" is ambiguous. Write to a specific absolute path like `$CI_PROJECT_DIR/benchmark-report.md`.

**Detection:**
- `[ -s benchmark-report.md ] || { echo "Claude produced no output"; exit 1; }` after the Claude call.
- Artifact `claude.stdout.log` missing or empty.

**Phase:** Phase 2 (Claude invocation), hardened in Phase 3 (report posting).

---

### Pitfall 4: GitHub comment body exceeds 65,535 characters

**What goes wrong:** The GitHub REST API for PR comments has a hard 65,535-character body limit. If the benchmark report is verbose — especially if it includes per-benchmark tables for hundreds of Criterion benchmarks — the `gh pr comment` or GitHub API call returns HTTP 422 Unprocessable Entity and the comment is not posted.

**Why it happens:** Criterion JSON output for a large workspace can be extensive. If the system prompt does not enforce length limits, Claude will produce a thorough report that exceeds the limit.

**Consequences:** Comment posting fails. If the error is not checked, the CI job exits 0 and the PR has no comment.

**Prevention:**
- System prompt must include an explicit character budget: "The report must not exceed 60,000 characters."
- Before posting, truncate at a safe limit (e.g., 60,000 chars) and append a note: "Report truncated — full analysis in CI artifact."
- Prefer summary-first format: global verdict at the top, details below, so truncation is graceful.
- The `pr-commenter` internal service (used by dd-trace-py) may have its own limits distinct from the raw GitHub API — test both paths.

**Detection:**
- GitHub API returns 422; `gh` CLI exits non-zero with `body is too long` message.
- Add a `wc -c benchmark-report.md` check before posting.

**Phase:** Phase 3 (report posting).

---

### Pitfall 5: LLM hallucinating benchmark insights not grounded in the data

**What goes wrong:** Claude receives benchmark numbers and invents causal explanations ("this regression is likely due to increased allocation pressure in the serializer") that are not derivable from the JSON alone. The report sounds authoritative but the diagnosis is fabricated.

**Why it happens:** Claude is trained to be helpful and explanatory. Without a hard constraint, it will speculate beyond what the data shows — especially for micro-benchmark regressions where many causes are plausible.

**Consequences:** Contributors chase phantom root causes. Trust in the system erodes when the analysis is wrong. Worse: a real regression is dismissed because the explanation sounds wrong.

**Prevention:**
- System prompt must include a grounding constraint: "Do not explain why a regression occurred unless the cause is directly visible in the diff or in the benchmark name. State 'root cause unknown from benchmark data alone' for unexplained changes."
- Instruct Claude to quote the actual numbers ("main: 1.2µs, PR: 1.8µs, +50%") rather than vague descriptions.
- The report format should separate observed facts (numbers, % change, whether within noise margin) from interpretation (which is optional and clearly labeled as inference).

**Detection:**
- Review a few reports manually during Phase 2 iteration. Look for claims not traceable to the JSON input.

**Phase:** Phase 2 (system prompt design) — this is a prompt engineering problem, not a code problem.

---

### Pitfall 6: Machine variance making every micro-benchmark appear as a regression

**What goes wrong:** Criterion benchmarks report sub-microsecond results. Between the baseline run and the PR run, the CI machine's load, turbo boost state, memory layout, or OS scheduler decisions introduce 5–20% variance. Claude flags these as regressions.

**Why it happens:** Criterion does include a confidence interval and a `change` field with `threshold` — but if the system prompt ignores these fields, Claude will compare mean times only and report noise as signal.

**Consequences:** Alert fatigue. Contributors stop reading the benchmark reports. Real regressions are missed in the noise.

**Prevention:**
- System prompt must instruct Claude to use Criterion's `change.mean.estimate` vs. `change.mean.confidence_interval` to filter out changes within the noise margin. Only flag changes where the confidence interval is entirely on one side of zero.
- For absolute changes below 100ns, always label as "within noise margin" regardless of percentage.
- When baseline and PR run on different machines or at different times, note this explicitly in the report header.
- The mock data should include both noisy benchmarks (to verify they are not flagged) and clear regressions (to verify they are flagged).

**Detection:**
- In mock data testing, include a benchmark with +5% change within confidence interval — verify Claude does not flag it as a regression.

**Phase:** Phase 1 (mock data design) and Phase 2 (system prompt), hardened in Phase 4 (real data).

---

## Moderate Pitfalls

### Pitfall 7: `--allowedTools` missing critical tools causing silent partial analysis

**What goes wrong:** The allowed tools list for Claude CLI (`--allowedTools` or via `ClaudeAgentOptions`) does not include `Bash(jq:*)` or `Bash(grep:*)`. Claude cannot parse or filter the JSON benchmark data efficiently, takes many turns doing it with Read + internal processing, hits the turn limit, and stops.

**Prevention:**
- Grant: `Read`, `Write`, `Glob`, `Bash(jq:*)`, `Bash(grep:*)`, `Bash(wc:*)`, `Bash(ls:*)`. See the dd-trace-py reference for a worked example.
- Do not grant `Bash` unrestricted — `Bash(cargo bench:*)` in a benchmark analysis job would re-run benchmarks inside the analysis step.

**Warning signs:** `claude.stdout.log` shows Claude trying complex string manipulation to work around missing tools; turn count is exhausted on parsing rather than analysis.

**Phase:** Phase 2 (Claude invocation scaffolding).

---

### Pitfall 8: dd-octo-sts token scoped too broadly or too narrowly

**What goes wrong:** The `dd-octo-sts` call specifies a policy that either (a) lacks permission to post PR comments on `DataDog/libdatadog`, causing a silent 403, or (b) grants write access to the entire repo, violating least-privilege.

**Prevention:**
- The policy must grant `pull_requests: write` on `DataDog/libdatadog` only.
- Test with a dry-run: `gh api repos/DataDog/libdatadog/pulls/1/comments --method GET` using the obtained token before wiring up the write path.
- Use `id_tokens: DDOCTOSTS_ID_TOKEN: aud: dd-octo-sts` in the GitLab CI job definition (as dd-trace-py does).

**Warning signs:** `gh pr comment` exits non-zero with HTTP 403; `dd-octo-sts token` succeeds but the subsequent API call fails.

**Phase:** Phase 1 (auth scaffolding) and Phase 3 (report posting).

---

### Pitfall 9: PR comment creates a new comment on every push instead of updating

**What goes wrong:** Every push to the PR branch creates a new comment. After a few iterations, the PR has a wall of "Benchmark Analysis" comments, each superseding the previous one.

**Why it happens:** The posting step uses `gh pr comment --create` without checking for an existing comment to update.

**Prevention:**
- Use the `pr-commenter` internal service (as `post-pr-comment.sh` does) which supports `PATCH` semantics — it finds an existing comment with a matching header and updates it in place.
- If using the GitHub API directly: list existing PR comments, search for one matching a unique marker (e.g., `<!-- benchmark-analysis -->`), and PATCH it if found, POST if not.
- Embed a unique HTML comment marker in every report: `<!-- benchmark-analysis-libdatadog -->`.

**Warning signs:** Multiple identical-header comments accumulate on the PR.

**Phase:** Phase 3 (report posting).

---

### Pitfall 10: Criterion JSON format differs between cargo-criterion versions

**What goes wrong:** The benchmark runner produces a Criterion JSON file. The exact structure (field names, units, which fields are present) changes between `criterion` 0.4, 0.5, and the `cargo-criterion` binary. If the mock data is written for one version but the real benchmark runner uses another, the system prompt's JSON navigation instructions are wrong.

**Prevention:**
- Pin the `criterion` and `cargo-criterion` versions in the benchmark environment (already done implicitly by `Cargo.lock`).
- Document the exact JSON fields the system prompt relies on (particularly `change.mean.estimate`, `change.mean.confidence_interval`, `unit`).
- Mock data must be generated from a real `cargo bench --message-format=json` run, not hand-crafted.

**Warning signs:** Claude reads the JSON but cannot find the `change` or `estimates` fields; reports "no change data found."

**Phase:** Phase 1 (mock data) — validate JSON shape before anything else.

---

### Pitfall 11: GitLab artifact not available when the analysis job runs

**What goes wrong:** The analysis job has a `needs:` reference to the benchmark job. If the benchmark job uploads artifacts but the analysis job starts before artifact upload completes (race in GitLab's artifact finalization), the analysis job cannot find the benchmark JSON.

**Prevention:**
- Use `needs: [{job: benchmarks, artifacts: true}]` — GitLab guarantees artifacts are available before the dependent job starts when `artifacts: true` is set.
- Add an existence check at the start of the analysis script: `[ -f "$BENCHMARK_JSON" ] || { echo "Benchmark artifact missing"; exit 1; }`.

**Warning signs:** The analysis job starts, finds no input file, and exits 0 (if the guard is missing).

**Phase:** Phase 2 (CI wiring).

---

### Pitfall 12: Non-zero exit from Claude SDK/CLI failing the entire CI job

**What goes wrong:** Claude exits non-zero due to a context window exhaustion, a turn limit hit, or a tool error. If the analysis job has `allow_failure: false` and no retry, the CI pipeline fails with a cryptic error, blocking the PR.

**Prevention:**
- Benchmark analysis should be `allow_failure: true` during the prototype phase. Promote to `allow_failure: false` only after the pipeline has run stably on real data for several weeks.
- Separately, distinguish between "Claude produced no output" (soft failure: post a stub comment, exit 0) and "auth failed" (hard failure: exit 1 to surface the infra problem).
- Set `retry: 1` for transient auth/network failures.

**Warning signs:** CI blocks PR merges due to Claude analysis failures unrelated to the benchmark results.

**Phase:** Phase 2 (Claude invocation) and Phase 3 (CI job tuning).

---

## Minor Pitfalls

### Pitfall 13: nvm / Node.js installation in a no-root container

**What goes wrong:** The Claude Code CLI (if used as a CLI binary rather than the Python SDK) requires Node.js. The CI image may not have Node pre-installed, and `apt-get install` without root fails.

**Prevention:**
- Check whether the target image (`dd-octo-sts-ci-base:2025.06-1` or the benchmarking image) has Node pre-installed before writing nvm install logic.
- If Node is needed: install nvm to `$HOME/.nvm` (no root required), source it, install Node LTS, add to PATH. Do this in `before_script`.
- Alternatively: use the Python `claude-agent-sdk` (as dd-trace-py does) instead of the Claude Code CLI binary — it bundles its own native binary and avoids the Node dependency entirely.

**Warning signs:** `npm: command not found` or `claude: command not found` after the install block.

**Phase:** Phase 1 (environment setup).

---

### Pitfall 14: Misleading percentage changes for benchmarks with small absolute values

**What goes wrong:** A benchmark that runs in 50ns and regresses to 60ns shows as +20%, which sounds alarming. But 10ns absolute difference is within hardware noise. The report flags it as a critical regression.

**Prevention:**
- System prompt must include: "For benchmarks with absolute time < 500ns, note that percentage changes may not be meaningful due to measurement noise. Flag these as 'micro-benchmark — verify with longer runs.'"
- Add an absolute-change column alongside percentage-change in the report table.

**Warning signs:** Report leads with percentage changes on benchmarks measured in nanoseconds with no absolute values shown.

**Phase:** Phase 2 (system prompt).

---

### Pitfall 15: `CI_EXTERNAL_PULL_REQUEST_IID` missing for non-PR pipelines

**What goes wrong:** The PR comment posting step uses `$CI_EXTERNAL_PULL_REQUEST_IID` or `$CI_MERGE_REQUEST_IID` to identify the GitHub PR. This variable is only set for pipelines triggered by a PR/MR. If the job runs on a branch push without an open PR (e.g., during initial development), the variable is empty and `gh pr comment` either fails or posts to the wrong PR.

**Prevention:**
- Guard the posting step: `[ -n "${CI_EXTERNAL_PULL_REQUEST_IID:-}" ] || { echo "Not a PR pipeline, skipping comment"; exit 0; }`.
- During the prototype phase where the job runs on every push, ensure the trigger condition also checks that a PR exists.

**Warning signs:** `gh pr comment` fails with "no PR found for branch" or posts to a random open PR.

**Phase:** Phase 3 (report posting).

---

## Phase-Specific Warnings

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| Auth scaffolding (Vault → AI Gateway + BTI → GitHub) | Token expiry if fetched too early; secret leakage with `set -x` | Fetch tokens late; no debug shell mode |
| Mock data construction | Criterion JSON shape mismatch with real output | Generate from real `cargo bench --message-format=json` |
| System prompt design | LLM hallucination; noise flagged as regression; report too long | Grounding constraint; use Criterion confidence intervals; 60k char cap |
| Claude invocation wiring | No output file; wrong allowed tools; non-zero exit blocking PRs | Assert output exists; tune allowed tools; `allow_failure: true` initially |
| PR comment posting | Comment proliferation; 65k char limit; wrong PR target | Use PATCH semantics or `pr-commenter`; truncate; guard on PR variable |
| Real benchmark data integration | Machine variance; Criterion version mismatch; artifact availability | Use `needs: artifacts: true`; pin versions; noise-aware system prompt |

---

## Sources

- dd-trace-py reference implementation: `/repos/dd-trace-py/.gitlab/scripts/summarize_failures.py` — concrete auth flow (authanywhere → BTI → AI Gateway), allowed tools list, Claude SDK usage pattern
- dd-trace-py PR comment posting: `/repos/dd-trace-py/.gitlab/scripts/post-pr-comment.sh` — pr-commenter PATCH semantics, bearer token handling
- dd-trace-py system prompt: `/repos/dd-trace-py/.gitlab/scripts/summarize-failures.system.md` — GFM formatting rules, grounding constraints, output format
- libdatadog existing benchmarks: `.gitlab/benchmarks.yml` — artifact upload pattern, benchmark job structure
- PROJECT.md constraints: CI image, auth chain (Vault JWT → rapid-ai-platform), no-root constraint, dd-octo-sts scoping
