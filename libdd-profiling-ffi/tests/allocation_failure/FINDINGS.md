# Allocation-failure findings for the profiling FFI

Experimental report for [PROF-16808](https://datadoghq.atlassian.net/browse/PROF-16808). The same content is rendered as [FINDINGS.html](FINDINGS.html) by `discovery/render_findings.py` (`--check` fails when the HTML is stale). No production code was changed: every remediation below is a proposal for Phase B, which starts only after a human review of this report.

## Executive summary

- An allocation failure inside the profiling FFI used by the .NET profiler almost never returns an error. In the Linux normal build, 3,115 of 4,269 one-shot injected failures (fail-once) abort the process and 29 panic across the `extern "C"` boundary (also an abort). With sticky failures (fail-from), 4,233 of 4,269 runs abort: the error path itself allocates.
- Two root causes cover most aborts: infallible allocations (`Vec`, `Box`, `format!`, `String` clones, `indexmap` and third-party crates) on paths that otherwise return `Result` (F3, F4, F5, F6), and error construction that allocates after an allocation failure was detected correctly (F2, F8).
- The zstd encoder constructor panics when its context allocation fails (F1). It is called from `Profile_new`, `Profile_serialize` and `Exporter_send_blocking`, so every one of those can abort the host process. A draft upstream issue is included; upgrading zstd is not the fix.
- One silent data-loss path: `Profile_serialize` returns success with samples missing when the observation decoder cannot allocate (F7).
- Two non-allocation findings: release builds accept a sample label that makes every later serialize of that profile fail (F9), and the file endpoint's dump server leaks a socket in `/tmp` for each exporter created with a file endpoint (F10).
- No post-call contract violation was observed in any libdatadog scenario. But most post-error behaviour the harness checks is not documented in `profiling.h` (contract audit below), so the checks are provisional, and enumeration stays report-only. Gating PRs requires the contract work and the harness changes listed in "From report-only to a PR gate".
- Linux and Windows agree on every profiling-core finding. Windows has fewer aborts (3,262 against 7,348 in the normal build) because the container and cgroup parsing of F5 is Linux only; Windows instead aborts in an environment-variable read at `Exporter_new`. The dump-server socket leak (F10) is Unix only. macOS ran only through the generic test job, which hides per-scenario results.

## Evidence

The harness (`tests/allocation_failure`) runs each scenario once without failure injection (baseline), then once per allocation ordinal with one failed allocation (fail-once) and with every allocation failing from that ordinal (fail-from). Outcomes: `oom-error` (the API returned an error), `success-fallback` (the API succeeded despite the failure), `contract-violation` (handles not in their checked state afterwards), `panic`, `abort`, `timeout` and `nondeterministic` (the allocation sequence differs from the baseline). Arena chaos (`LIBDD_ALLOCATION_CHAOS_TEST=1`) also injects failures into the profiling arenas.

Linux, local (aarch64, harness commit 98e0e1bdb). Toy scenarios excluded (`toy_contract_violation` produces the only two `contract-violation` outcomes, by design):

| Run | abort | panic | success-fallback | nondeterministic | contract-violation | oom-error |
|---|---:|---:|---:|---:|---:|---:|
| normal build | 7,348 | 29 | 85 | 58 | 0 | 1,018 |
| arena chaos | 7,551 | 29 | 87 | 55 | 0 | 1,230 |
| release build, promoted fuzz scenarios only | 963 | 16 | 58 | 19 | 0 | 108 |

Windows, CI (`windows-latest`, harness commit 98e0e1bdb): [Test run 44848768358](https://github.com/DataDog/libdatadog/actions/runs/44848768358), job [allocation failure reports #windows-latest](https://github.com/DataDog/libdatadog/actions/runs/44848768358/job/136675253731), artifact [allocation-failure-reports-windows](https://github.com/DataDog/libdatadog/actions/runs/44848768358/artifacts/14684637518) (`normal/<scenario>.md` and `arena-chaos/<scenario>.md`, 57 reports each; GitHub keeps artifacts for a limited time). Toy scenarios excluded:

| Run | abort | panic | success-fallback | nondeterministic | contract-violation | oom-error |
|---|---:|---:|---:|---:|---:|---:|
| normal build | 3,262 | 29 | 76 | 61 | 0 | 1,018 |
| arena chaos | 3,456 | 28 | 78 | 68 | 0 | 1,230 |

macOS: only the generic `cargo nextest` job ran the harness (it passes; per-scenario outcomes are not visible there). Two harness changes came from macOS CI: unwinding there allocates while the worker records the injected stack, which turned fail-from outcomes into aborts (diagnostic stack allocations now bypass injection), and fail-from runs cannot always be unwound, so their stacks reuse the fail-once stack of the same ordinal. No macOS-only library finding is known; adding macOS to the report job is a follow-up.

OS differences:

| Difference | Linux | Windows |
|---|---|---|
| Host identity at `Exporter_new` (F5) | aborts in `libdd-host-identity/src/entity_id/unix/*` (regex, cgroup parsing) | not compiled; aborts in `libdd-env/src/lib.rs:51` (`std::env::var`) instead |
| Dump-server socket (F10) | leaks `/tmp/libdatadog_dump_*.sock` | not compiled (Unix sockets) |
| Azure App Services environment read (F11) | `/proc/self/environ`, degrades silently | not compiled |
| Abort status | `SIGABRT` | exit code `0xc0000409` (`__fastfail`) |
| Innermost arena frame under chaos | `string_table/mod.rs:30`, `libdd-alloc/src/linear.rs:36` | `arena.rs:63` (inlining) |

Ordinal counts differ slightly between OSes and runs where the exporter send scenarios are timing dependent (F12).

Commands (all output under a scratch directory, never `/tmp`):

```
ALLOC_CHAOS_REPORT_DIR=<dir>/normal cargo nextest run -p libdd-profiling-ffi --test allocation_failure
LIBDD_ALLOCATION_CHAOS_TEST=1 ALLOC_CHAOS_REPORT_DIR=<dir>/arena-chaos cargo nextest run -p libdd-profiling-ffi --test allocation_failure
ALLOC_CHAOS_REPORT_DIR=<dir>/release cargo nextest run --cargo-profile release -p libdd-profiling-ffi --test allocation_failure -E 'test(/^fuzz::/)'
```

## Findings

Each finding lists the observed outcomes, the affected API, representative call sites (innermost libdatadog frame that allocates; full stacks are in the per-scenario reports), OS scope with the reports that show it, root cause, remediation, suggested owner (from `.github/CODEOWNERS`), dependencies and the proposed fix-ticket group (see "Proposed fix tickets"). Windows evidence: the per-scenario reports in the [Windows artifact](https://github.com/DataDog/libdatadog/actions/runs/44848768358/artifacts/14684637518). Linux evidence: the local reports, regenerated with the commands above.

### F1. zstd encoder construction panics, which aborts at the FFI boundary

- **Outcomes**: `panic` (29 per Linux build, 16 in release), then abort with "panic in a function that cannot unwind"; also `abort` in fail-from.
- **API / path**: `ddog_prof_Profile_new` (observation encoder), `ddog_prof_Profile_serialize` and `ddog_prof_Exporter_send_blocking` (profile encoder), every `reset` that creates a new encoder.
- **Call sites**: `libdd-profiling/src/profiles/compressor.rs:247` (`ZstdObservationCodec::new_encoder`) and `:159` (`ZstdProfileCodec::new_encoder`), both via `zstd::stream::write::Encoder::new`, `zstd::stream::raw::Encoder::with_dictionary` (zstd 0.14.3 `raw.rs:293`), `zstd_safe::CCtx::create` (zstd-safe 8.2.0 `lib.rs:371`).
- **Stack (Linux, `profile_new` fail-once #4)**: `ZSTD_createCCtx_advanced` → `CCtx::try_create` → `CCtx::create` → `expect` panics → `Profile::try_new` (`internal/profile/mod.rs:419`) → `ddog_prof_Profile_new` (`datatypes.rs:409`).
- **OS scope / evidence**: Linux and Windows (same call sites); reports `profile_new`, `profile_serialize_*`, `exporter_send_*`, `fuzz_seq_*`.
- **Root cause**: zstd's stream encoder constructors call the panicking `CCtx::create()`, although the decoder uses `DCtx::try_create()` and returns an `io::Error`. The panic unwinds into a non-unwinding `extern "C"` function, which aborts.
- **Remediation**: (1) preferred: build the encoder on a context created with `zstd_safe::CCtx::try_create()` (a small owned streaming encoder over `zstd_safe` compress-stream calls, or the zstd `with_context` constructors with an owned context), mapping `None` to an out-of-memory error; (2) report it upstream (draft below) so the constructors return `io::Result` errors; (3) not recommended: `catch_unwind` around construction, because the panic payload itself allocates under memory pressure.
- **Owner**: @DataDog/libdatadog-profiling.
- **Dependencies**: needs the non-allocating error representation from T1 to report the failure without another allocation; independent otherwise.
- **Ticket group**: T2.

### F2. Error construction allocates after a correctly detected allocation failure

- **Outcomes**: `abort` in fail-from (4,233 of 4,269 normal-build fail-from runs abort); fail-once also aborts where the error conversion itself is the failed allocation.
- **API / path**: every fallible FFI call: `Profile_add`, `Profile_serialize`, `Profile_new`, metadata calls, `Exporter_*`.
- **Call sites**: `try_reserve_exact(..)?` and `try_reserve(1)?` followed by an `anyhow::Error` allocation: `libdd-profiling/src/internal/profile/mod.rs:572`, `:620`, `:627`, `:606`, `:160`, `:180`, `:130`; `collections/identifiable/mod.rs:71`, `:82`; `collections/string_table/mod.rs:30`; `profiles/compressor.rs:59`, `:85`, `:251`, `:252`, `:315`; `internal/observation/timestamped_observations.rs:79`; `libdd-alloc/src/linear.rs:36`. Conversion to the FFI error: `libdd-common-ffi/src/error.rs:83` (`format!("{value:#}")`); context strings: `libdd-profiling-ffi/src/profiles/datatypes.rs:617`, `:815`, `:864`.
- **Stack (Linux, `profile_serialize_endpoints` fail-from #12)**: `Profile::expand_label_set` (`mod.rs:572`, `try_reserve_exact` fails) → `?` boxes an `anyhow::Error` → allocation fails → `handle_alloc_error` aborts; called from `Profile::encode` → `ddog_prof_Profile_serialize`.
- **OS scope / evidence**: Linux and Windows.
- **Root cause**: the out-of-memory error is reported through `anyhow::Error` (heap allocated), with `.context(..)` strings and a `format!` into the FFI `Error`. Under sticky memory pressure the error cannot be built, so the correct detection is wasted.
- **Remediation**: a dedicated out-of-memory error that never allocates: a zero-size or static variant in the internal error type (no `anyhow` boxing for `TryReserveError` and `AllocError`), and an FFI error or status that can carry a `'static` message without copying (`ProfileStatus`-style static errors for the profile API, a static variant of `ddog_Error` in `libdd-common-ffi`). Keep context strings off the out-of-memory path.
- **Owner**: @DataDog/libdatadog-profiling (profile internals, FFI) with @DataDog/apm-common-components-core (`libdd-common-ffi` `Error`).
- **Dependencies**: first in the order; F1, F3, F6, F7 and F8 need it to report their failures.
- **Ticket group**: T1.

### F3. Infallible allocations in the profile API

- **Outcomes**: `abort` in fail-once (the first failed allocation is fatal).
- **API / path**: `Profile_new`, `Profile_add`, `Profile_serialize`, `Profile_reset`, `Profile_set_endpoint`, `Profile_add_endpoint_count`, `Profile_add_upscaling_rule_*`.
- **Call sites**: `Profile::try_new` `to_vec` (`internal/profile/mod.rs:421`); `datatypes.rs:28` (`Profile::new` box); sample conversion `datatypes.rs:360`, `:368`; `StringTable::new` `reserve(32)` (`collections/string_table/mod.rs:109`); `timestamped_observations.rs:43`, `:97`; `trimmed_observation.rs:74` (`Box::from`); `observations.rs:126`, `:164`; `reset_and_return_previous` (`mod.rs:514`); `encode` (`mod.rs:681`); upscaling `upscaling.rs:80`, `:96`, `:97`, `:103`, `:144`, `:153`, `:186`, `:190`; `api/mod.rs:250` (`UpscalingInfo::check_validity` message); endpoint stats `endpoint_stats.rs:8`, `:22`; `mod.rs:138`; handle boxing `libdd-common-ffi/src/handle.rs:68`.
- **Stack (Linux, `profile_new` fail-once #0)**: `<[T]>::to_vec` → `Profile::try_new` (`mod.rs:421`) → `profile_new` (`datatypes.rs:494`) → `ddog_prof_Profile_new`.
- **OS scope / evidence**: Linux and Windows; `profile_*`, `set_endpoint_*`, `endpoint_count_*`, `upscaling_*`, `fuzz_seq_*`.
- **Root cause**: the profile API returns `Result`, but many collection operations still use infallible `Vec`, `Box`, `HashMap` and `IndexMap` methods. The arenas themselves are fallible: every direct arena scenario returns `oom-error`.
- **Remediation**: convert each listed site to `try_reserve`, `try_with_capacity` or pre-sized buffers allocated at `Profile_new`, and replace `Box::from(Vec)` and `to_vec` with fallible helpers. Add a clippy `disallowed-methods` lint (infallible `Vec::push`, `with_capacity`, `Box::new`, `to_vec`, `format!`) scoped to the profile hot path, so regressions show in review. `libdd-alloc` already provides fallible arenas.
- **Owner**: @DataDog/libdatadog-profiling.
- **Dependencies**: T1 first (otherwise each converted site aborts while building its error).
- **Ticket group**: T3.

### F4. Exporter construction and send allocate infallibly, mostly in dependencies

- **Outcomes**: `abort` in fail-once and fail-from.
- **API / path**: `ddog_prof_Exporter_new`, `ddog_prof_Exporter_send_blocking` (cold and warm runtime), `ddog_prof_Endpoint_*` conversion.
- **Call sites**: `libdd-profiling/src/exporter/profile_exporter.rs:97`–`:159` (`new`, `init_runtime`: tokio runtime), `:189` (`send_blocking`), `:224`–`:226` (`build`: reqwest request and multipart form), `:259`, `:260` (`send`), `:269`–`:362` (tags string, event JSON, multipart parts); `exporter/tls.rs:66`–`:85` (rustls configuration); `exporter/config.rs:26`; `libdd-networking/src/endpoint.rs:95`, `:118` (reqwest client builder); `libdd-profiling-ffi/src/exporter.rs:50`, `:109`, `:206`, `:223` (`parse_json`); `libdd-common/src/tag.rs:102` (`Tag::clone`).
- **Stack (Linux, `exporter_send_warm` fail-once #86)**: reqwest multipart request → `ProfileExporter::build` (`profile_exporter.rs:226`) → `ProfileExporter::send` (`:241`) → `send_blocking` → `ddog_prof_Exporter_send_blocking`.
- **OS scope / evidence**: Linux and Windows; `exporter_new`, `exporter_send_cold`, `exporter_send_warm`, `fuzz_seq_01`.
- **Root cause**: the exporter is built on tokio, hyper, reqwest, rustls and serde_json, which treat allocation failure as fatal. Their allocations cannot be made fallible from libdatadog.
- **Remediation**: decide the policy first (design ticket): (1) move all one-time allocations (runtime, TLS configuration, HTTP client, tags, endpoint) into `Exporter_new`, documented as an initialization-time call; (2) on the send path, keep libdatadog-owned allocations fallible (event JSON, tags string, multipart buffers sized from the encoded profile) and pre-reserve where the size is known; (3) document the residual risk of third-party allocations in `profiling.h`. Replacing the HTTP stack is out of scope.
- **Owner**: @DataDog/libdatadog-profiling (exporter) with @DataDog/apm-common-components-core (`libdd-networking`, `libdd-common`).
- **Dependencies**: T1 for the libdatadog-owned parts; the policy decision before any code change.
- **Ticket group**: T5.

### F5. Host identity aborts in `Exporter_new`

- **Outcomes**: `abort` (8,280 Linux ordinals in `exporter_new` across the normal and arena chaos builds, the largest single group; 4 on Windows).
- **API / path**: `ddog_prof_Exporter_new` → entity id and container id detection.
- **Call sites**: `libdd-host-identity/src/entity_id/unix/container_id.rs:22`, `:27` (`Regex::new` in `LINE_REGEX` and `CONTAINER_REGEX` lazy statics), `:37`, `:38`, `:45`, `:47`; `cgroup_inode.rs:36`–`:98`; `entity_id/unix/mod.rs:55`, `:99`. Windows: `libdd-env/src/lib.rs:51` (`std::env::var` in the `DD_EXTERNAL_ENV` lazy static, `libdd-host-identity/src/entity_id/mod.rs:96`).
- **Stack (Linux, `exporter_new` fail-once)**: regex compilation → `CONTAINER_REGEX` initializer (`container_id.rs:27`) → `extract_container_id` → `compute_entity_id` → `ProfileExporter::new` → `ddog_prof_Exporter_new`.
- **OS scope / evidence**: container and cgroup parsing on Linux only (`cfg(unix)`); the environment read only failed on Windows in these runs. Report `exporter_new`.
- **Root cause**: regex compilation allocates thousands of times in a lazy static on first use, cgroup parsing uses infallible `String` and `PathBuf` operations, and `std::env::var` allocates on Windows even when the variable is unset.
- **Remediation**: replace the two regexes with a hand-written parser of the cgroup line (fixed formats), or compute the entity and container ids and the external environment once at library initialization, outside the allocation-sensitive call; make file reads and path building fallible and degrade to "no container id" on failure.
- **Owner**: @DataDog/apm-common-components-core.
- **Dependencies**: none (degrading to "unknown" does not need T1).
- **Ticket group**: T5.

### F6. `ddog_Vec_Tag_push` aborts

- **Outcomes**: `abort` on every ordinal of the three tag scenarios.
- **API / path**: `ddog_Vec_Tag_push` (exporter tags, additional tags).
- **Call sites**: `libdd-common-ffi/src/tags.rs:39`, `:40` (key and value clones), `libdd-common/src/tag.rs:230` (`format!` in `Tag::new`), `libdd-common-ffi/src/vec.rs:112` (`Vec::push`).
- **Stack (Linux, `tag_push_empty` fail-once #0)**: `String` clone of the key → `ddog_Vec_Tag_push` (`tags.rs:39`).
- **OS scope / evidence**: Linux and Windows; `tag_push_empty`, `tag_push_at_capacity`, `tag_push_long`.
- **Root cause**: string clones, `format!` and `Vec::push` are infallible; the function already returns a result type that could carry an error.
- **Remediation**: `try_reserve` the vector and the strings, build the tag without `format!`, and return the static out-of-memory error from T1.
- **Owner**: @DataDog/apm-common-components-core.
- **Dependencies**: T1 (static FFI error).
- **Ticket group**: T4.

### F7. `Profile_serialize` drops samples silently when the observation decoder cannot allocate

- **Outcomes**: `success-fallback` (`fuzz_seq_03` fail-once #53 in the normal build, #52 in release); `abort` in fail-from at the same site.
- **API / path**: `ddog_prof_Profile_serialize` with timestamped samples.
- **Call sites**: `libdd-profiling/src/internal/observation/timestamped_observations.rs:94` (`self.decoder.read_u32(..).ok()?` in `TimestampedObservationsIterImpl::next`).
- **Stack**: zstd decoder read → `TimestampedObservationsIterImpl::next` (`:94`) → `ObservationsIntoIter::next` (`observations.rs:237`) → `Profile::encode` (`mod.rs:615`) → `ddog_prof_Profile_serialize`.
- **OS scope / evidence**: Linux and Windows (`fuzz_seq_03`, 2 Windows ordinals).
- **Root cause**: the iterator maps every read error to end of iteration, so an allocation failure in the decoder truncates the timestamped samples and serialize reports success.
- **Remediation**: make the iterator yield `Result` (or record the first error and check it after iteration) and fail the serialize with the out-of-memory error.
- **Owner**: @DataDog/libdatadog-profiling.
- **Dependencies**: T1.
- **Ticket group**: T3.

### F8. Dropping an empty handle allocates an error

- **Outcomes**: `abort` in fail-once and fail-from.
- **API / path**: `Drop` of `Handle<T>` (encoded profile, exporter, cancellation token) and `ToInner::to_inner_mut` on null handles; reached by `*_drop` on a consumed handle and by `Exporter_send_blocking`.
- **Call sites**: `libdd-common-ffi/src/handle.rs:56` (`anyhow::ensure!` in `take`, called from `Drop` at `:75`), `:34`, `:47` (`.context("Null pointer")`).
- **Stack (Linux, `profile_serialize_endpoints` fail-once #42)**: `Handle::drop` (`handle.rs:75`) → `take` (`:56`) → `anyhow!` message allocation → abort.
- **OS scope / evidence**: Linux and Windows.
- **Root cause**: `take` builds an `anyhow::Error` for the null case, and `Drop` calls `take` and discards the result.
- **Remediation**: `Drop` checks for null before calling `take`; null-handle errors use a static error (T1).
- **Owner**: @DataDog/apm-common-components-core.
- **Dependencies**: none for the `Drop` change; T1 for the error.
- **Ticket group**: T1.

### F9. Release builds skip sample label validation (not an allocation failure)

- **Outcomes**: in release, a sample with a string `local root span id` label is accepted by `Profile_add`, and every later `Profile_serialize` of that profile fails. Debug builds reject the add. Separately, the debug-only validation aborts on allocation failure (`internal/profile/mod.rs:1084`, `:1085`, `:1089`, and the `{label:?}` message at `api/mod.rs:133`).
- **API / path**: `ddog_prof_Profile_add`, then `ddog_prof_Profile_serialize`.
- **Call sites**: `try_add_sample` (`internal/profile/mod.rs:151`, `#[cfg(debug_assertions)] validate_sample_labels`).
- **OS scope / evidence**: all OSes (build-profile dependent); observed in the release fuzz runs, handled in `fuzzed.rs` by comparing with a setup reference run.
- **Root cause**: label validation is compiled only with `debug_assertions`, so shipped (release) artifacts accept invalid labels that poison the profile.
- **Remediation**: validate the cheap invariants (duplicate keys, `local root span id` numeric and non-zero) in release too, without allocating (no `HashMap`: compare keys pairwise or against interned ids), or drop the offending label or sample instead of poisoning the profile. Document the label rules in `profiling.h` (they are absent today).
- **Owner**: @DataDog/libdatadog-profiling.
- **Dependencies**: none.
- **Ticket group**: T6.

### F10. The file endpoint's dump server leaks a socket in `/tmp` (not an allocation failure)

- **Outcomes**: a randomly named `libdatadog_dump_<pid>_<random>.sock` file per exporter created with a file endpoint, never removed. Harness workers left 1,116 sockets on the development host before the exporter scenarios moved to a local agent.
- **API / path**: `ddog_Endpoint_file` → `ddog_prof_Exporter_new` → `libdd-networking` dump server (`cfg(unix)`).
- **Call sites**: `libdd-networking/src/dump_server.rs:46` (socket directory `/tmp`), `spawn_dump_server` (no `remove_file` anywhere in the file).
- **OS scope / evidence**: observed on Linux; macOS has the same `cfg(unix)` code (not observed); not applicable to Windows.
- **Root cause**: the socket path is bound and never unlinked, neither on exporter drop nor on server-thread exit.
- **Remediation**: unlink the socket when the server stops (guard object owned by the exporter), use a Linux abstract-namespace socket where available, or a private temporary directory removed on drop.
- **Owner**: @DataDog/apm-common-components-core.
- **Dependencies**: none.
- **Ticket group**: T7.

### F11. Benign `success-fallback` outcomes, and one silent degradation

- **Outcomes**: `success-fallback` (85 normal, 87 arena chaos, 58 release on Linux).
- **API / path**: `Profile_new`, `Profile_add`, `Profile_set_endpoint`, upscaling rules (`indexmap` growth); `Exporter_new` (Azure App Services detection).
- **Call sites and verdict**:
  - `libdd-profiling/src/collections/identifiable/mod.rs:71`, `:82`, `collections/string_table/mod.rs:109`, `internal/upscaling.rs:97`, `internal/profile/mod.rs:130`: benign. `indexmap` first tries an opportunistic `try_reserve_exact` up to the index capacity, then falls back to the exact request; nothing is lost.
  - `libdd-host-identity/src/azure_app_services.rs:86`: silent degradation. `std::fs::read("/proc/self/environ")` fails and the function returns an empty map, so Azure App Services metadata is missing from the exporter tags.
  - `libdd-profiling/src/collections/string_table/mod.rs:30` (arena chaos, `fuzz_seq_01`; `arena.rs:63` on Windows): harness masking, see F14.
- **OS scope / evidence**: `indexmap` fallbacks on Linux and Windows; the Azure read is Linux only (`/proc`). Reports `profile_*`, `set_endpoint_*`, `upscaling_*`, `exporter_new`, `fuzz_seq_*`.
- **Root cause**: by design in `indexmap`; for Azure, an I/O error (here out of memory) is treated as "not on Azure".
- **Remediation**: none for `indexmap`. Azure: log the read failure at debug level and retry detection on the next exporter creation; optional.
- **Owner**: @DataDog/apm-common-components-core (Azure detection).
- **Dependencies**: none.
- **Ticket group**: optional addition to T5.

### F12. Exporter send scenarios are nondeterministic

- **Outcomes**: `nondeterministic` (58 normal, 55 arena chaos, 19 release on Linux; 61 and 68 on Windows), only in `exporter_send_cold`, `exporter_send_warm` and `fuzz_seq_01` (which sends).
- **API / path**: `ddog_prof_Exporter_send_blocking`.
- **Call sites**: none (the outcome is a sequence mismatch): the report detail is "event N differs from the baseline" or "ordinal N not reached".
- **OS scope / evidence**: Linux and Windows, same three scenarios.
- **Root cause**: harness limitation. The send path allocates on tokio worker threads and in hyper connection handling, whose order depends on I/O timing, so the allocation sequence differs from the baseline. The harness already reports instead of failing these three scenarios.
- **Remediation**: run the exporter on a current-thread runtime with a deterministic in-process agent, or enumerate only the calling thread's allocations up to the first I/O wait.
- **Owner**: harness owner (PROF-16808 follow-up).
- **Dependencies**: none; required before the send scenarios can gate.
- **Ticket group**: T9.

### F13. Public contract gaps in `profiling.h`

- **Outcomes**: none observed: no `contract-violation` outside the toy scenario. The gap limits what the checks can claim (see "Contract audit").
- **API / path**: every exported profile and exporter symbol the harness calls, plus `ddog_Vec_Tag_*` and `ddog_Error_drop` (`common.h`).
- **Call sites**: doc comments above the exported functions in `libdd-profiling-ffi/src/profiles/datatypes.rs`, `libdd-profiling-ffi/src/exporter.rs`, `libdd-common-ffi/src/tags.rs` and `libdd-common-ffi/src/error.rs`.
- **OS scope / evidence**: all OSes (documentation).
- **Root cause**: the comments document safety preconditions and success behaviour, but not what a failed call leaves behind: whether the profile is still usable after a failed `add`, `serialize`, `reset` or metadata call, whether a failed constructor (`Profile_new`, `Exporter_new`) publishes a handle, or whether an exporter can send again after a failed send. `ddog_prof_Profile_reset`'s comment is stale: it documents a bool return and a `start_time` argument, but the function takes only the profile and returns `ProfileResult`. `ddog_Vec_Tag_new`, `ddog_Vec_Tag_drop` and the drops' effect on the handle are undocumented, and so are the label rules for `Profile_add` (F9).
- **Remediation**: document per exported symbol the ownership of every argument and result, the state after an error, and whether the call can be retried (wording proposals under "Contract audit"). No header change is made in this experimental PR.
- **Owner**: @DataDog/libdatadog-profiling (profile and exporter symbols) with @DataDog/apm-common-components-core (`ddog_Vec_Tag_*`, `ddog_Error_drop`).
- **Dependencies**: none; prerequisite to turning the harness into a PR gate.
- **Ticket group**: T8.

### F14. Harness limitations that affect the findings

- **Outcomes**: `success-fallback` that hides a failure (fuzz sequences); missing coverage for aborting workers; no per-scenario macOS results.
- **API / path**: the harness (`tests/allocation_failure`), not the library.
- **Call sites**: `fuzzed.rs` and `sequence.rs` (expected-error matching); the worker coverage profile written at exit; the macOS job in `.github/workflows/test.yml`.
- **OS scope / evidence**: masking seen on Linux (`string_table/mod.rs:30`) and Windows (`arena.rs:63`); coverage is local Linux only; macOS has no report job.
- **Root cause**: a fuzz sequence ignores errors from calls that also fail in the setup run (rejected inputs), so an injected failure inside such a call looks like the expected rejection. An aborting worker writes no LLVM profile (atexit does not run). The report job runs only on Windows, and the generic macOS job hides per-scenario outcomes.
- **Remediation**: compare error kinds once an out-of-memory error is distinguishable (T1); accept the coverage limit (it only affects discovery); add macOS to the report job. The 57 never-reached allocation-relevant branch outcomes in `SCENARIOS.md` (last section) stay a gap report, nothing in it is excused; most are APIs the .NET profiler does not call (profiles dictionary, managed string ids) or host-dependent paths (Azure App Services).
- **Owner**: harness owner.
- **Dependencies**: T1 for error-kind matching.
- **Ticket group**: T9.

## Contract audit

Each post-call assertion of `lifecycle.rs` and `fuzzed.rs`, classified against the doc comments that cbindgen copies into `profiling.h` (and `common.h` for `ddog_Vec_Tag_*` and `ddog_Error_drop`). "Documented": the header states it. "Undocumented ownership": necessary for memory safety, but not stated. "Provisional": a test assumption about post-error behaviour. "Out of contract": the header forbids the call; kept as an implementation observation that must not become a gate. The checks carry the same label in their doc comments.

| Check | Assertion | Header text | Class |
|---|---|---|---|
| `check_new_profile` | a returned profile is usable and must be dropped | "Must call `ddog_prof_Profile_drop` when you are done" | Documented |
| `check_new_profile` | a failed `Profile_new` publishes no profile | none | Undocumented ownership |
| `check_profile_usable` | after a failed `add` or `serialize`, add and serialize still work | `serialize`: "Drains the data, and then resets the profile" (success only) | Provisional |
| `check_meta` | a failed `set_endpoint`, `add_endpoint_count` or upscaling call can be retried, and the profile takes a sample | none | Provisional |
| sequence generator | upscaling rules only before the first serialize | "must be called before serialize and must not be called after" | Documented (respected) |
| sequence generator | non-zero `total_sampled`, `total_real`, `sampling_distance` | "This value must not be equal to 0" | Documented (respected) |
| `check_new_exporter` | a failed `Exporter_new` publishes no exporter; a returned one can send | none | Undocumented ownership, then provisional |
| `check_send` | `send_blocking` consumes the encoded profile whatever the result | "Takes ownership of the profile" | Documented |
| `check_send` | success returns a 2xx status | "returning the HttpStatus" | Documented |
| `check_send`, `check_exporter_sends` | the exporter can send again after a failed send | "Borrows the exporter" (ownership only) | Provisional |
| `check_encoded_bytes` | `bytes` leaves the encoded profile owned by the handle | "valid for use until the encoded_profile is modified" | Documented |
| `check_endpoint_agent`, `check_endpoint_agentless` | the endpoint returns the arguments it was given and borrows them | none | Undocumented ownership |
| `check_vec_tag_new` | a new tag vector is empty and accepts a push | no doc comment | Provisional |
| `check_drop_profile`, `check_drop_encoded`, `check_drop_exporter`, `check_drop_cancel` | drop empties the handle | none | Undocumented ownership |
| same | a second drop is a no-op | "has not previously been dropped" / "do not call this twice" / "has already been dropped" / "been dropped already" | Out of contract |
| `check_drop_error` | a second `ddog_Error_drop` stays cleared | "It should not be used after this" | Out of contract |
| `check_drop_tags` | drop consumes the vector | by-value signature only | Undocumented ownership |
| `fuzzed::check` | the profile takes a sample and serializes after any failed call, when it did in the setup run | none | Provisional |

Missing ownership and post-error behaviour is an API-contract gap and a prerequisite to turning this report-only experiment into a PR gate (T8). This PR changes no header; follow-up wording to propose:

- `ddog_prof_Profile_new`: "On error, no profile is returned and nothing needs to be dropped except the error."
- `ddog_prof_Profile_add`, `_serialize`, `_set_endpoint`, `_add_endpoint_count`, `_add_upscaling_rule_*`: "On error the profile is unchanged (or: reset) and remains valid; the call can be retried."
- `ddog_prof_Profile_reset`: rewrite to match the `ProfileResult` signature.
- `ddog_prof_Exporter_new`: "On error, no exporter is returned." `ddog_prof_Exporter_send_blocking`: "The exporter remains usable after an error."
- `*_drop`: "Leaves the handle empty; dropping an empty handle is a no-op" (if that is the intended contract; today the header forbids a second drop).
- `ddog_Vec_Tag_new`, `ddog_Vec_Tag_drop`: add ownership comments.
- `ddog_prof_Endpoint_agent`, `_agentless`: "The endpoint borrows its string arguments for its lifetime."

## Proposed fix tickets

Not created: Phase B starts after the human review of this report. Order: T1, then T2, T3, T4 and T6 in parallel; T5 after its design decision; T7 and T8 any time; T9 last.

| Ticket | Scope | Findings | Owner | Depends on |
|---|---|---|---|---|
| T1 | Non-allocating out-of-memory error: internal error type, FFI `Error` and `ProfileStatus` static errors, null-handle `Drop` | F2, F8 | libdatadog-profiling, apm-common-components-core | none |
| T2 | zstd encoder on `CCtx::try_create`; file the upstream issue | F1 | libdatadog-profiling | T1 |
| T3 | Fallible allocations in profile new, add, serialize, reset, upscaling, endpoints; decoder errors in serialize | F3, F7 | libdatadog-profiling | T1 |
| T4 | Fallible `ddog_Vec_Tag_push` | F6 | apm-common-components-core | T1 |
| T5 | Exporter allocation policy (design), then init-time allocations and fallible send-path buffers; host identity without regex | F4, F5, F11 (Azure logging) | libdatadog-profiling, apm-common-components-core | T1, design review |
| T6 | Release-build label validation without allocation; document label rules | F9 | libdatadog-profiling | none |
| T7 | Dump-server socket cleanup | F10 | apm-common-components-core | none |
| T8 | Per-symbol FFI contract (ownership, post-error state, retry) in the headers | F13 | libdatadog-profiling, apm-common-components-core | none |
| T9 | Harness to PR gate (see below), deterministic exporter scenarios, macOS reports | F12, F14 | harness owner | T8, and T1 to T3 for a green baseline |

## From report-only to a PR gate

Today the scenario enumeration is report-only: crash outcomes are recorded in the per-scenario Markdown reports and do not fail the test (only harness-internal errors and nondeterminism outside the send scenarios do). That is deliberate: almost every scenario crashes today, and most post-error checks are provisional. Before crashes can fail PRs:

1. Contracts (T8): every assertion must trace to a documented header statement; provisional and out-of-contract checks are removed or become documented.
2. Expected-outcome baseline: commit the expected outcome per scenario and ordinal class (for example "every ordinal returns `oom-error` or `success-fallback`"), keyed by finding id while fixes land, so a PR fails only on a regression or a new crash call site.
3. Determinism: exporter scenarios on a deterministic runtime and agent (F12), so `nondeterministic` can fail.
4. Platforms: run the report job on Linux, Windows and macOS, and fail on any OS-specific crash not in the baseline.
5. Fix wave: T1 to T3 at least, otherwise the gate is red from the start.

## Recommended automation

- **Machine-readable contract per exported ABI symbol**: a checked-in file (for example `ffi-contracts.yaml`) next to the cbindgen configuration with, per `ddog_*` symbol: argument ownership (borrowed, consumed, out), result ownership, state after an error, retry semantics and whether the call may allocate. The header doc comments are generated from it or checked against it.
- **Contract-completeness CI**: a check that fails when an exported symbol (from the cbindgen output or `nm` of the cdylib) has no contract entry, or when an entry has unfilled fields.
- **Agent skill that drafts contracts**: a skill that reads a symbol's implementation and current doc comment and drafts the contract entry for human review. Drafts are never merged without review, and the skill must not infer guarantees from implementation details alone.
- **Generated test registry and adapters**: generate the harness scenario table, the post-call checks and the `#[repr(C)]` mirrors from the contract file, so a new symbol gets scenarios and checks by construction.
- **Deterministic committed scenarios on PRs**: the committed lifecycle, metadata and promoted scenarios run on every PR (normal and arena chaos, Linux, Windows, macOS), with the expected-outcome baseline. The Windows report job added in this PR is the starting point.
- **Slow discovery nightly**: coverage and fuzzing (`discovery.py coverage`, `fuzz`, `fuzz-coverage`, `promote`) run nightly, never on PRs. New promotions open a PR with the regenerated `promoted.rs` and `SCENARIOS.md`.
- **Rolling coverage-gap issue**: the nightly job maintains one GitHub issue ("Allocation-failure coverage gaps") whose body is replaced on each run with the current never-reached allocation-relevant branch list from `SCENARIOS.md`. No source-line exception file: a gap disappears from the issue only when a scenario reaches it or the code goes away.

## Draft upstream issue for zstd-rs (not filed)

Target: [gyscos/zstd-rs](https://github.com/gyscos/zstd-rs). Do not file before the Phase B review; do not treat a zstd upgrade as the fix for F1.

```
Title: Stream encoder constructors panic when the compression context cannot be allocated

Versions: zstd 0.14.3, zstd-safe 8.2.0, zstd-sys 2.1.2+zstd.1.5.7.

zstd::stream::write::Encoder::new (and raw::Encoder::new, with_dictionary,
with_prepared_dictionary, with_ref_prefix) return io::Result, but create their
context with zstd_safe::CCtx::create(), which panics with "zstd returned null
pointer when creating new context" when ZSTD_createCCtx returns NULL
(src/stream/raw.rs:293, :333, :350).

The decoder side already handles this: create_dctx uses DCtx::try_create() and
returns io::Error ("zstd could not allocate a decompression context").

Reproduction: build zstd-safe with its Rust allocator support and install a
global allocator that returns null for the context allocation (or any custom
ZSTD allocator that fails), then call zstd::stream::write::Encoder::new(Vec::new(), 1).
Expected: Err(io::Error). Actual: panic. Called from an extern "C" function,
the panic aborts the process.

Proposal: use CCtx::try_create() in the encoder constructors and map None to an
io::Error, mirroring create_dctx (io::ErrorKind::OutOfMemory would be the most
precise kind).
```

## Phase B gate

This report closes Phase A. Phase B (fix tickets, production changes, the PR gate) starts only after a human reviews `FINDINGS.md` or `FINDINGS.html` and approves or amends the ticket groups above. No production fix tickets, issues or branches exist for these findings yet.
