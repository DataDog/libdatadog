---
name: release-proposal-pr-review-bumps
description: Review a libdatadog (or similar Rust workspace) release-proposal PR that bumps multiple crate versions, verifying each crate's semver bump (major/minor/patch) is correct based on its ACTUAL per-crate public-API delta — not just the conventional-commit `!` markers. Use when asked to "review a release PR", "check version bumps", "verify the semver bumps", or review a "chore(release): proposal..." PR.
---

# Reviewing release-proposal version bumps

A release-proposal PR (usually authored by the release bot, e.g. `chore(release): proposal for ...`) bumps several crates at once. Each crate's bump is derived from the commits since its last release. The reviewer's job has **two layers**:

1. **Per-crate:** confirm each crate's bump matches the **real public-API change in that specific crate** (the bulk of this skill).
2. **Workspace-level:** confirm the bumps are *consistent across the dependency graph* — specifically that every major bump has cascaded through its reverse-dependency closure (see "The major-version cascade" below). The release bot routinely gets this wrong: it bumps/releases only crates that have their own commits, while silently rewriting path-dependency requirements everywhere else. That produces under-bumped dependents that force a new major of a shared crate onto consumers without a version bump — the single most damaging defect in these PRs. **Always run this check; it is easy to miss because the under-bumped crate's own diff looks innocent.**

## The core principle

A commit marked breaking (`!` in its conventional-commit title, e.g. `feat(data-pipeline)!: ...`) is often a **multi-crate sweep**. The breaking change usually lands in only ONE crate; the same commit may touch other crates with purely additive or internal edits. So:

- **Never** infer a crate's bump from the `!` marker alone.
- For each crate, look at **only that crate's slice** of each commit and classify the highest-severity change to *its own* public API.

Bump rules (per crate, based on the highest-severity change):
- **major** — a breaking public-API change: removed/renamed/signature-changed `pub` item; changed `pub` struct field type; changed/removed enum variant; removed trait method; dropped public trait impl (e.g. a `derive` removed in default builds); **plus any of the additive-but-breaking forms below**.
- **minor** — additive *and* checked non-breaking: new `pub` items, nothing removed or changed, **and none of the additions is one of the additive-but-breaking forms below**. (Promoting a `pub(crate)`/private item to `pub`, or renaming a non-`pub` item, counts as additive — it was never externally visible.)
- **patch** — internal only: private code, `#[cfg(test)]`/`mod tests`, benches, `[dev-dependencies]`, comments, bug fixes with no public-API change.

### Additive-but-breaking: "new `pub` item" is NOT automatically minor

A new item can break every downstream compile without removing or changing anything. Never classify a crate minor just because the diff only adds. For each new `pub` item, check which of these it is — the exemption must be **verified in the code**, not assumed:

- **New variant on a public enum** → breaks downstream exhaustive `match`. Minor only if the enum is `#[non_exhaustive]` *and already was before this commit* (adding `#[non_exhaustive]` is itself breaking). Check the enum's attributes at the base ref, and that it is reachable from the crate root.
- **New required trait method on a public trait** (no default body) → breaks every downstream `impl` of that trait. Minor only if the method has a default body, or the trait is genuinely not downstream-implementable (sealed via a private supertrait / private-type argument). A default body added to a trait everyone already implements is still an inference/ambiguity risk — see the last bullet.
- **New `pub` field on a constructible public struct** → breaks downstream struct literals and exhaustive struct patterns. Minor only if the struct is `#[non_exhaustive]` (before this commit) or already had a private field, i.e. it could never be built or destructured by literal outside the crate.
- **New `impl Trait for T`, new blanket impl, or a new inherent method shadowing a trait method** → breaks downstream type inference and method resolution (the same hazard as "What the automated level cannot see" #7). Strict SemVer calls these minor-with-possible-breakage, so minor is defensible — but only as an *explicit* judgment call: state the impl/method, and whether anything plausibly resolves the shadowed name or relies on inference there. Do not approve it silently as "just an addition".

`cargo-semver-checks` has a lint for each of the first three — `enum_variant_added` ("an exhaustive enum has a new variant"), `trait_method_added` ("a non-sealed public trait added a new method without a default implementation"), `constructible_struct_adds_field` — all typed `major` in `cargo semver-checks --list`, so an automated level of `minor` over one of these deserves a second look at the invocation (check the exit code, and see "What the automated level cannot see" for why a `major`-typed lint can still pass). The fourth form has no lint at all, so an automated `minor` there proves nothing.

## Watch for these subtle cases

1. **Sub-major bump carrying a `!` commit** (minor/patch crate that includes a breaking-marked commit) — the highest-priority thing to verify. Confirm the breaking part is NOT in this crate.
2. **Transitive breakage** — a crate that re-exports, or uses in a `pub` signature, a type from a dependency that changed. If the changed type leaks into the crate's public API, the crate breaks too. If it's only used internally / behind a trait with stable signatures, it does not.
   - Check: does the crate `pub use` the changed type? Does any `pub fn`/struct field expose it directly (vs. being generic over a trait whose method signatures are unchanged)?
3. **Feature-gated breaks** — a breaking change behind a non-default Cargo feature is weaker justification for a major under strict default-feature semver. Note it, but a default-surface break elsewhere still independently justifies major.
4. **Forced-major dependency bumps are often breaking** (do NOT reflexively treat as patch). When crate A goes **major**, every dependent's `Cargo.toml` requirement on A is rewritten to A's new major (`^1` → `^2`), forcing A's new major onto the dependent's consumers. Whether that obliges the dependent to *also* go major depends on whether A is **safe to duplicate** — run the two-part test in "The major-version cascade". Short version: if A is a public dependency of the dependent (exposed type, or a foreign-trait-impl on a public type) **or** A is unsafe to duplicate (singleton/global state, single-artifact link) and consumers use `^` ranges, the dependent must go **major** too. (The older guidance "dep bump = patch" is wrong for these.) Minor/patch dependency bumps of A (same major) never cascade — `^1.2` already unifies with `1.3.0`.
5. **Initial releases** (e.g. `1.0.0`, CHANGELOG newly added, previously `publish = false`/unpublished) — nothing to semver-diff against; just confirm the version is sane and the crate was genuinely unpublished.
6. **Test/bench-only commits** — patch is the safe, conservative choice even when arguably no bump was needed.
7. **Additive-only diff proposed as minor** — the easiest bump to wave through and a common under-bump. An enum variant, a required trait method, or a struct field can be the whole diff and still be major; run the additive-but-breaking list above rather than eyeballing "nothing removed".

## The major-version cascade (workspace-level check)

Two semver-incompatible majors of the same crate can be resolved into a single dependency tree by Cargo. Whether that is harmless or a "boom" depends entirely on whether the dependency is **safe to duplicate**. Whether the dependent needs a forced-major bump (the cascade) hinges on the same question. So before forcing majors up the graph, run the test below — don't blanket-cascade.

### The "safe to duplicate" test — BOTH checks must pass

A crate is safe to duplicate (two majors can coexist harmlessly) **only if both** of these hold. Failing *either* one makes duplication harmful and forces the cascade. Checking only the first is the classic mistake.

**(a) No process-global / singleton state.** Grep the crate's `src` for anything that must be unique per process:
   ```bash
   grep -rnE 'static |lazy_static|once_cell|OnceLock|OnceCell|Lazy|thread_local|#\[no_mangle\]|#\[export_name|#\[ctor|atexit|pthread_atfork|signal\(' <crate>/src --include=*.rs
   ```
   Hits on real globals, FFI exported symbols, ctors, or fork/signal/atexit handlers = duplication is unsafe (two copies fight over the same process resource or clash at link time, especially inside the single FFI/C `builder` artifact). Comments and instance-scoped registration (e.g. `AtomicWaker::register`, "worker registered *on a SharedRuntime instance*") do NOT count — confirm the constructor is an instance method (`Foo::new()`), not a global accessor (`fn global() -> &'static Foo`).

**(b) No shared types/traits crossing a crate boundary.** Even a globally-stateless crate is unsafe to duplicate if its types or **traits** are part of the *integration contract* between two other crates — because v1's type/trait is a different type from v2's, so a value/impl from a v1-built crate won't satisfy a v2 bound. Check the dependents, not just the dependency:
   - Does dependent X **expose the dep's type in its public API** (re-export, `pub fn` arg/return, `pub` field)? e.g. `pub fn set_shared_runtime(_: Arc<SharedRuntime>)`.
   - Does dependent X **implement a trait from the dep on one of X's public types**? A foreign-trait impl on a public type *is* public API. e.g. `impl libdd_shared_runtime::Worker for TelemetryWorker` — even though `SharedRuntime` never appears in telemetry's signatures, consumers rely on `TelemetryWorker: Worker`.
   - Does some *other* crate Y then **consume that across the boundary**? e.g. data-pipeline calls `shared_runtime.spawn_worker(telemetry_worker)` (production, `trace_exporter/builder.rs`), which requires `TelemetryWorker: <data-pipeline's shared_runtime>::Worker`. If telemetry is on `shared-runtime ^1` and data-pipeline on `^2`, the trait impl targets the wrong major → **hard compile error**, not just redundant copies.
   ```bash
   # type in dependent's public API:
   grep -rnE 'pub use .*<dep>|pub fn .*<Type>|pub .*: &?(mut )?<Type>|-> .*<Type>' <dependent>/src --include=*.rs | grep -v cfg.test
   # foreign-trait impls of the dep's traits on the dependent's types:
   grep -rnE 'impl .*<dep_trait> for ' <dependent>/src --include=*.rs | grep -v cfg.test
   # cross-crate consumption of that contract (e.g. spawn/register taking the impl):
   grep -rnE '<consume_fn>\(' <consumer>/src --include=*.rs | grep -v cfg.test
   ```

Worked example (`libdd-shared-runtime`): passes (a) — instance-based `SharedRuntime::new()`, zero globals — but **fails (b)**: `TelemetryWorker` implements its `Worker` trait and data-pipeline spawns that worker on a `SharedRuntime` in production. So telemetry, data-pipeline, and any `^`-range consumer (e.g. dd-trace-rs) must all agree on one shared-runtime major → the cascade is a genuine correctness requirement here, not conservative over-bumping.

### Classifying the dependent's bump once duplication is unsafe

- If the dep is a **public dependency** of the dependent (fails (b): exposed type *or* foreign-trait-impl-on-public-type) → the dependent's own public contract changed with the dep's major → **major is correct, not weird**. This is the real public-dependency case.
- If the dep is **private** to the dependent (passes (b): used only internally, never crossing a boundary) but **fails (a)** (singleton/global, or single-artifact link) → the dependent's API is technically unchanged, so strict SemVer would allow patch — but a patch/minor is auto-picked by `^`-range consumers during `cargo update`, silently dragging in the incompatible major. So bump **major anyway** to force a deliberate, loud upgrade. (Major doesn't *prevent* the diamond; it makes it opt-in instead of a silent surprise.)
- If the dep passes **both** (a) and (b) → genuinely safe to duplicate → **no cascade**; a patch re-release (for manifest coherence) or nothing is fine.

So a major bump of a non-duplicable shared crate **cascades as a major bump through its reverse-dependency closure of publishable crates**, in topological order. Each crate in the closure that gains a new-major requirement (directly or transitively, e.g. `crashtracker → telemetry → shared-runtime`) must itself go major, which in turn forces *its* dependents major, and so on.

Rules of thumb:
- **Cascade triggers ONLY on major dependency bumps.** Minor/patch dep bumps (same major) never cascade.
- **`publish = false` crates don't need a version bump** (no registry artifact / no `^`-range consumers), but their path-dep requirements must still be internally consistent.
- A crate **already in the release list can still be under-bumped** by this rule — e.g. a crate correctly classified `patch` on its *own* API but which directly depends on a major-bumped shared crate must be upgraded to `major`. Check in-list crates against the cascade too, not just the omitted ones.
- Confirm how downstream actually pins libdatadog. If every consumer pins the *whole workspace at one exact version*, the duplicate-major hazard can't arise and the cascade is moot — but for any crate consumed independently with `^` ranges, it is real.

## Workflow

1. **Fetch the PR** with `gh pr view <N> --json title,body,headRefName,baseRefName,files,commits`. The body lists each crate, its next version, the bump type, and the attributed commits. (Base ref may be another `release/...` branch in a stacked release — review only the crates in the body.)
2. **Resolve commits locally.** For each PR number in the body: `git log --oneline --all --grep="(#<pr>)" -1`. Confirm all are present.
3. **Map each commit's crate footprint** so you know which commits are multi-crate sweeps:
   ```
   git show --stat --format= <hash> | grep -oE '^ [a-zA-Z0-9_./-]+' | sed 's,/.*,,' | sort | uniq -c | sort -rn
   ```
4. **Fan out one subagent per crate** (run them concurrently — multiple Agent calls in one message). Give each subagent: the crate name, proposed next version + bump, the attributed commit hashes (flagging which are `!`-marked multi-crate sweeps), and the method below. Prioritize the sub-major-with-`!` cases.
5. **Each subagent's method:**
   - Inspect ONLY the crate's slice: `git show <hash> -- <crate-dir>/` (or `<crate-dir>/src/` to skip tests).
   - Verify public reachability: is the changed item reachable from the crate root (`pub mod` chain in `lib.rs`, `pub use` re-exports)? `#[cfg(test)]`/`mod tests`/`benches/` and private items don't count.
   - Classify highest severity (major/minor/patch) with **evidence**: file path, item name, before/after signature.
   - **Before returning `minor`, walk the additive-but-breaking list.** For every added `pub` item, say which form it is and why it is exempt: new enum variant → quote the enum's `#[non_exhaustive]` at the *base* ref; new trait method → quote its default body or the sealing mechanism; new struct field → quote the pre-existing private field or `#[non_exhaustive]`; new trait impl / shadowing inherent method → state the inference risk explicitly. "Nothing was removed" is not evidence for minor.
   - Check transitive breakage via re-exports and `pub` signatures (point 2 above) and `Cargo.toml` dep changes.
   - Return a verdict: is the proposed bump correct, too low, or too high — with cited evidence.
6. **Run the major-version cascade check** (workspace-level — do this whenever ANY crate in the proposal gets a *major* bump). For each major-bumped crate, compute its reverse-dependency closure among publishable workspace crates and confirm every crate in it is also bumped **major**. A helper to build the closure and surface under-bumped crates against the proposal head/base refs:
   ```bash
   python3 - "$HEAD_REF" "$BASE_REF" <<'PY'
   import subprocess, re, sys, os
   head, base = sys.argv[1], sys.argv[2]
   MAJOR_BUMPED = {"libdd-shared-runtime","libdd-trace-utils","libdd-data-pipeline"}  # set to the crates getting a MAJOR bump in this proposal
   def manifest(ref,d):
       try: return subprocess.check_output(["git","show",f"{ref}:{d}/Cargo.toml"],stderr=subprocess.DEVNULL).decode()
       except Exception: return ""
   def parse(ref,d):
       t=manifest(ref,d)
       if not t: return None
       ver=re.search(r'(?m)^version\s*=\s*"([^"]+)"',t)
       publish = not re.search(r'(?m)^publish\s*=\s*false',t)
       cut=len(t)
       for mk in ("[dev-dependencies]","[build-dependencies]"):   # normal deps only
           i=t.find(mk); cut=min(cut,i) if i!=-1 else cut
       deps=set(re.findall(r'(?m)^(libdd-[a-z0-9-]+)\s*=',t[:cut]))
       return {"ver":ver.group(1) if ver else "?","publish":publish,"deps":deps}
   dirs=[d for d in os.listdir(".") if os.path.isfile(os.path.join(d,"Cargo.toml"))]
   info={d:parse(head,d) for d in dirs}; info={k:v for k,v in info.items() if v}
   # transitive reverse-dependency closure
   targets=set(MAJOR_BUMPED); changed=True
   while changed:
       changed=False
       for d,m in info.items():
           if d not in targets and (m["deps"] & targets):
               targets.add(d); changed=True
   print(f"{'crate':28} {'pub':4} {'base_ver':10} {'head_ver':10} bumped? deps-on-major")
   for d in sorted(targets - MAJOR_BUMPED):
       m=info[d]; b=parse(base,d)
       bumped = "MAJOR" if (b and b['ver'].split('.')[0]!=m['ver'].split('.')[0]) else "** NOT-MAJOR **"
       direct=sorted(m["deps"] & MAJOR_BUMPED)
       print(f"{d:28} {'PUB' if m['publish'] else '-':4} {(b['ver'] if b else '?'):10} {m['ver']:10} {bumped:16} {direct}")
   PY
   ```
   Any `PUB` crate flagged `** NOT-MAJOR **` is a defect: it either needs adding to the release as a major bump, or (if already in the list) its bump needs raising to major. Remember the intra-closure requirement edges must also move to the new majors (e.g. `crashtracker → telemetry ^N`).
7. **Synthesize** a verdict table (crate | proposed | correct? | why) plus the cascade findings and any non-blocking notes (changelog accuracy, feature-gated breaks).

## What the automated level cannot see

`scripts/semver-level.sh` (and the `pr-title-semver-check` job built on it) runs
`cargo-semver-checks` plus a `cargo-public-api` diff. Treat its answer as a **floor, not a
verdict**: a `patch` result is only trustworthy for changes that touch none of the
categories below.

The list below is a checklist, not a guarantee — a gap can close when the tooling is upgraded.
Before citing one as a reason to override the automated level, confirm it still holds against
the PR's base ref:

- **The script's own logic and comments** — `scripts/semver-level.sh` (the two passes at
  `# 1) cargo-semver-checks` and `# 2) cargo-public-api diff`, combined by `max_level`; the
  header comment above `normalize_api_line` documents which signature deltas it deliberately
  drops as non-semver-significant).
- **The tool versions actually installed**, which decide whether a miss below is still a
  miss. Read them off the workflows rather than trusting any version quoted here:
  ```bash
  grep -rn 'cargo-semver-checks@\|cargo-public-api@' .github/workflows/
  ```
  At the time of writing both `pr-title-semver-check.yml` and `release-proposal-dispatch.yml`
  pin `cargo-semver-checks@0.48.0` and `cargo-public-api@0.52.0`. If the pin has moved, a
  category below may now be caught — re-check the specific lint before citing it as a gap.
- **Whether a lint exists at all**, for the `cargo-semver-checks` cases: `cargo semver-checks
  --list` (`--explain <lint>` for detail). Read its `type` column carefully — it is the
  semver update the lint *reports* (`major`/`minor`), **not** whether a violation fails the
  run. A lint can list as `major` and still be warn-level, printing the violation while
  exiting 0; neither `--list` nor `--explain` shows it, and 0.47.0 has no `--deny`/`--warn`
  override flag to force the issue (re-check `--help` for the pinned version).
  So the only local way to settle a "does the script see it?" question is a
  minimal two-crate repro plus `echo $?` on the actual invocation the script uses
  (`cargo semver-checks -p <crate> --color=never --all-features --baseline-rev <rev>`).

Manually check these whenever the automated level is `patch` or `minor`:

1. **`#[repr(C)]` field reordering.** `repr_c_plain_struct_fields_reordered` is
   **warning-level**: cargo-semver-checks prints the violation but still reports
   `Summary no semver update required` and exits 0, so the script never sees it and the level
   comes out `patch`. Note `--list` shows this lint as type `major` — that is the update it
   *would* require, and does not contradict the warn level; confirm by exit code, not by
   `--list`. This is the one that matters most here: a silent ABI break for every FFI
   consumer compiled against the old header. The rest of the repr family (`repr_c_removed`,
   `repr_align_changed`, `repr_packed_added`, `enum_repr_int_changed`) fails properly.
   **Check any diff that touches field order in a `#[repr(C)]` type.**
2. **Public dependency major bumps behind unchanged signatures.** `pub fn f(u: hyper::Uri)`
   renders identically whether `hyper` is 0.14 or 1.0; only the resolved dependency version
   moved. Overlaps with subtle case 2 above, and is the mechanism behind the cascade check.
3. **Non-host targets.** The script passes no `--target`, so only the host triple is
   analysed. Windows- and macOS-only `#[cfg]` API is never compared — relevant for
   crashtracker and common.
4. **Feature-gated API.** `cargo semver-checks` runs `--all-features` but `cargo public-api`
   runs default features only, and the public-api pass is the *only* thing that catches
   parameter and return type changes. So a signature change behind a non-default feature is
   caught by neither. (Compounds subtle case 3.)
5. **Crate renames.** A renamed crate is absent from the baseline, so it is classified as a
   *new* crate and reported `minor`. Renaming a published crate breaks every consumer.
6. **Declarative macro bodies.** Only removal of a `#[macro_export] macro_rules!` is linted;
   narrowing or dropping an arm is invisible to both tools.
7. **Trait-impl and inference breakage.** Adding `impl Trait for T`, or an inherent method
   that shadows a trait method downstream, reads as a plain addition.
8. **Behaviour.** New panics, changed error semantics, altered serde/wire representation —
   no signature tool can see these.
9. **The generated C API.** Both tools read rustdoc JSON, so nothing validates `builder`'s
   generated headers or pkg-config output. Per `AGENTS.md` the C FFI offers no ABI
   compatibility guarantee, so this is by design — but do not mistake a green semver check
   for FFI safety.

Two properties of the tooling that also affect how you reproduce a level locally: the script
needs a **clean working tree** (`cargo public-api diff` does a real `git checkout`), and it
needs `RUSTUP_TOOLCHAIN` overridden because `rust-toolchain.toml` pins an MSRV older than
cargo-semver-checks requires — see the `RUSTUP_TOOLCHAIN: ${{ env.RUST_VERSION }}` env on the
`Run semver checks on changed crates` step in `.github/workflows/pr-title-semver-check.yml`,
and the job-level `RUSTUP_TOOLCHAIN` env in `release-proposal-dispatch.yml`, for the
toolchain CI actually uses.

## Output

A concise table of per-crate verdicts with file:symbol evidence, then the **cascade verdict** (every major bump propagated through its publishable reverse-dependency closure? list any under-bumped/omitted crates and the major version each needs), then a short list of non-blocking observations. State plainly whether every bump is correct, or which need changing and to what.
