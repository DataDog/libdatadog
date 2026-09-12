# Technology Stack

**Project:** CI/LLM Benchmark Analysis Pipeline
**Researched:** 2026-06-15

## Recommended Stack

### Claude Code CLI

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| `@anthropic-ai/claude-code` | latest (`npm install -g`) | LLM analysis engine | Headless `-p` mode is the established CI pattern; `--allowedTools` and `--permission-mode bypassPermissions` give file access without interactive prompts. Matches PHP reference implementation. |

**Invocation pattern:**
```bash
claude --bare -p "$(cat /path/to/prompt.md)" \
  --allowedTools "Read,Glob,Grep" \
  --permission-mode bypassPermissions \
  --max-turns 10 \
  --output-format text
```

`--bare` skips CLAUDE.md discovery, MCP server loading, and keychain reads — required in CI for deterministic behavior. Auth comes exclusively from env vars (not keychain) when `--bare` is set.

**Do NOT use** `--dangerously-skip-permissions` — `--permission-mode bypassPermissions` is the correct flag for allowing pre-declared tools without prompts. The `--dangerously-skip-permissions` flag is broader and undocumented in stable releases.

**Do NOT pipe large benchmark JSON via stdin** — the CLI caps piped stdin at 10 MB (as of v2.1.128). Write JSON to a file and reference the path in the prompt instead.

### AI Gateway Authentication

| Technology | Purpose | Why |
|------------|---------|-----|
| `ANTHROPIC_BASE_URL` | Point CLI at Datadog AI Gateway | Official override env var; changes destination only, not request format |
| `ANTHROPIC_AUTH_TOKEN` | Bearer token for the gateway | Gateway expects `Authorization: Bearer <token>`, not `x-api-key` |
| Vault OIDC JWT → `rapid-ai-platform` audience | Obtain the bearer token | Same pattern as PHP reference (`dd-trace-php/.gitlab/libdatadog-latest.yml`) |
| `apiKeyHelper` in `--settings` JSON | Refresh token if TTL < job duration | Invoke Vault CLI in a helper script; set `CLAUDE_CODE_API_KEY_HELPER_TTL_MS` |

**Do NOT** set `ANTHROPIC_API_KEY` when using `ANTHROPIC_AUTH_TOKEN` — the CLI prioritizes `ANTHROPIC_API_KEY` and will attempt direct Anthropic API calls if it is set.

**Auth flow:**
```bash
# 1. Exchange GitLab CI OIDC token for Vault JWT
VAULT_TOKEN=$(vault write -field=token auth/jwt/login role=rapid-ai-platform jwt=$CI_JOB_JWT_V2)

# 2. Fetch bearer token from Vault secret
BEARER=$(vault kv get -field=token secret/ai-gateway/token)

# 3. Export for Claude Code
export ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io"
export ANTHROPIC_AUTH_TOKEN="$BEARER"
```

### Node.js Installation (no root)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| nvm | v0.40.1+ | Node version manager | No root required; installs Node into `$HOME/.nvm` |
| Node.js | 22 LTS | Runtime for Claude Code | Minimum requirement is Node 18; LTS 22 is current stable |

**Pattern for CI:**
```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
nvm install 22
npm install -g @anthropic-ai/claude-code
```

**Alternative** if the CI base image (`registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1`) already has Node 18+: skip nvm and run `npm install -g @anthropic-ai/claude-code` directly. Check with `node --version` in `before_script`.

**Do NOT use** `sudo npm install -g` — the CI user is non-root and this will fail.

### Criterion Benchmark Output

| Technology | Purpose | Why |
|------------|---------|-----|
| `cargo-criterion` with `--message-format=json` | Machine-readable micro-benchmark output | Official supported format; one JSON object per line on stdout with `"reason": "benchmark-complete"` messages |
| `critcmp --export <baseline>` | Serialize baselines outside `target/` | Persists comparison data as a single JSON artifact across CI jobs/stages |

**Key fields in `benchmark-complete` messages:**
- `id` — benchmark name
- `typical.estimate` + `typical.unit` — best single performance number (slope if available, else mean)
- `mean`, `median` — with `lower_bound`, `upper_bound`, `unit`
- `change.mean.estimate` + `change.change` — `"NoChange"` | `"Improved"` | `"Regressed"` vs previous run

**Baseline comparison workflow in CI:**
```bash
# On main branch artifact (previous job or downloaded artifact):
cargo bench -- --save-baseline main
critcmp --export main > main-baseline.json

# On PR branch:
cargo bench -- --save-baseline pr
critcmp --export pr > pr-baseline.json

# Feed both files to Claude for analysis
```

**Do NOT** rely on Criterion's internal `target/criterion/` JSON files — they are a private implementation detail and format may change without notice. Use `cargo-criterion --message-format=json` or `critcmp --export` output only.

**Do NOT** use the deprecated CSV output.

### dd-trace-py Benchmark Output

| Technology | Purpose | Notes |
|------------|---------|-------|
| `bm.Scenario` custom framework | dd-trace-py's own benchmark harness | Scenarios yield callables; run via `scripts/perf-run-scenario` |
| Artifacts directory (`--artifacts ./artifacts/`) | Stores per-run results | Path: `artifacts/<run-id>/<scenario>/<version>/` |
| viztracer JSON (when `PROFILE_BENCHMARKS=1`) | Chrome Trace Event format for profiling | Only produced when profiling flag is set; not the primary perf number |

**Current status:** The raw numeric output format of `bm.Scenario` (non-profiling) is not publicly documented. The `scripts/perf-run-scenario` command writes results to an artifacts directory, but the exact JSON schema is internal to dd-trace-py. **Prototype with mocked data that mirrors what Augusto's triggering workstream delivers.** Define a schema contract in the mock and document it so it can be validated against real output when triggering lands.

**Do NOT** try to parse viztracer JSON as the primary performance metric — it is profiling trace data (Chrome Trace Event format), not summary statistics.

### GitHub PR Comments

| Technology | Purpose | Why |
|------------|---------|-----|
| `gh` CLI | Post PR comments | Simpler than raw API calls; handles pagination, auth headers, and error codes. Available in `dd-octo-sts-ci-base` image. |
| `dd-octo-sts` token | Authenticate `gh` against `DataDog/libdatadog` | Short-lived (1h) OIDC-exchange token; no long-lived PAT stored in CI secrets |
| `GH_TOKEN` env var | Auth for `gh` CLI | `gh` reads this automatically; no `gh auth login` needed in CI |

**Invocation:**
```bash
gh pr comment "$CI_MERGE_REQUEST_IID" \
  --repo DataDog/libdatadog \
  --body-file analysis.md
```

**Do NOT** use a static PAT stored as a GitLab CI variable — dd-octo-sts tokens are the correct pattern for Datadog CI. Consult the internal `dd-octo-sts` docs for the exact OIDC exchange steps from GitLab CI.

**Do NOT** call the GitHub REST API directly with `curl` — the `gh` CLI handles retry, rate limits, and token refresh.

### Supporting Tools

| Tool | Version | Purpose | Why |
|------|---------|---------|-----|
| `jq` | system | Parse JSON benchmark output | Universal; available in all CI images |
| `gh` CLI | system (from base image) | GitHub API interactions | Pre-installed in `dd-octo-sts-ci-base` |
| Vault CLI | system (from base image) | OIDC token exchange | Pre-installed in Datadog CI images |

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| LLM invocation | Claude Code CLI `--bare -p` | Direct Anthropic Messages API via `curl` | CLI handles retries, streaming, tool execution, and context management; gateway auth is the same either way |
| LLM invocation | Claude Code CLI | Python/TypeScript Agent SDK | Overkill for a single-shot analysis prompt; adds a language dependency; CLI is sufficient |
| Benchmark comparison | `cargo-criterion --message-format=json` + `critcmp --export` | Raw `target/criterion/` JSON files | Internal format, unstable |
| Benchmark comparison | `cargo-criterion` | `cargo bench` with libtest harness | libtest harness does not support `--save-baseline`; causes "unrecognized option" errors with Criterion |
| Node install | nvm | `sudo npm install -g` | No root in CI |
| Node install | nvm | Pre-built Node Docker layer | CI image is fixed; nvm is the portable fallback |
| GitHub comments | `gh` CLI + `dd-octo-sts` | Static PAT in GitLab CI variable | PATs are long-lived; `dd-octo-sts` is the Datadog-standard short-lived token pattern |
| GitHub comments | `gh` CLI | GitHub REST API via `curl` | More code, no retry/rate-limit handling |

## Installation

```bash
# Node + Claude Code CLI (if not pre-installed in base image)
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
export NVM_DIR="$HOME/.nvm" && [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
nvm install 22
npm install -g @anthropic-ai/claude-code

# Criterion tooling (add to workspace Cargo.toml dev-dependencies)
# cargo-criterion: install via cargo install cargo-criterion
# critcmp: cargo install critcmp

# jq and gh are expected to be present in dd-octo-sts-ci-base
```

## Sources

- [Claude Code headless/CI docs](https://code.claude.com/docs/en/headless)
- [Claude Code GitLab CI/CD docs](https://code.claude.com/docs/en/gitlab-ci-cd)
- [Claude Code LLM gateway configuration](https://code.claude.com/docs/en/llm-gateway)
- [cargo-criterion external tools / JSON format](https://bheisler.github.io/criterion.rs/book/cargo_criterion/external_tools.html)
- [critcmp — Criterion baseline comparison](https://github.com/BurntSushi/critcmp)
- [dd-trace-py benchmark docs](https://ddtrace.readthedocs.io/en/latest/benchmarks.html)
- [octo-sts overview](https://edu.chainguard.dev/open-source/octo-sts/overview/)
