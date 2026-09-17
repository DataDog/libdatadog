# Codebase Concerns

**Analysis Date:** 2026-06-15

## Tech Debt

**FFI Property Setter Error Handling:**
- Issue: Unhandled property names in FFI setters silently ignore errors instead of returning them
- Files: `libdd-telemetry-ffi/src/lib.rs:86`
- Impact: Callers cannot detect when an invalid property name is set; silent failures can lead to configuration not being applied
- Fix approach: Return an error status instead of `MaybeError::None` for unknown properties, update macro to propagate error conditions

**Unhandled Non-OK States in URI Parsing:**
- Issue: Multiple `.unwrap()` calls on `PathAndQuery::from_str` and `Uri::from_parts` without error handling
- Files: `libdd-data-pipeline/src/trace_exporter/mod.rs:117-134`
- Impact: Malformed URLs during trace export endpoint construction can panic in production; should bubble error up instead
- Fix approach: Replace `.unwrap()` with proper Result propagation and return TraceExporterError; add tests for edge-case URLs

**SQL Obfuscation Function Complexity:**
- Issue: `sql.rs` is 4310 lines with overly complex state machine for parsing and obfuscation
- Files: `libdd-trace-obfuscation/src/sql.rs`
- Impact: Difficult to maintain, test, and extend; high cognitive load for changes
- Fix approach: Break into smaller focused functions; separate parser state machine from obfuscation logic; add more unit tests for individual states

**Profiling FFI String Storage Memory Safety:**
- Issue: Multiple TODOs around whether `ManagedStringStorage` should take raw pointers like other Profile APIs
- Files: `libdd-profiling-ffi/src/string_storage.rs:49,65,102,142,169,201,223`
- Impact: API inconsistency creates confusion for FFI users; missing context parameter could lead to use-after-free if storage is freed before strings
- Fix approach: Standardize all FFI storage APIs to take `*mut ManagedStringStorage` parameter like other Profile APIs; audit all call sites

**Busy Loop in Child Process Reaper:**
- Issue: `reap_child_non_blocking` spins in a busy loop without any sleep, consuming CPU unnecessarily
- Files: `libdd-common/src/unix_utils/process.rs:45`
- Impact: Under load with many child processes, can cause high CPU usage; affects systems running crash tracking in signal handlers
- Fix approach: Add small sleep (e.g., 1-10ms) in the loop; consider using platform-specific wait mechanisms (epoll/kqueue)

**Tracer Metadata Schema Updates Pending:**
- Issue: `proc_info` and `sig_info` fields marked as needing schema updates
- Files: `libdd-crashtracker/src/crash_info/mod.rs:61,63`
- Impact: Schema mismatch between crash data collection and intake validation; could cause ingestion failures
- Fix approach: Update crash info schema version and integration tests to match new fields

**Unvalidated JSON Path Handling in SQL Obfuscation:**
- Issue: `keep_json_path` configuration option but unclear validation of path expressions
- Files: `libdd-trace-obfuscation/src/sql.rs:68`
- Impact: Malformed JSON paths could produce invalid SQL or leak sensitive data if not properly escaped
- Fix approach: Add comprehensive tests for JSON path edge cases; document path format requirements

## Known Bugs

**Arc Allocation Overflow in Profiling:**
- Symptoms: Reference count overflow when Arc<T> reaches max capacity; not handled gracefully
- Files: `libdd-profiling-ffi/src/profile_error.rs:105-107`
- Impact: Causes `ProfileError::ReferenceCountOverflow`; can crash if string storage creates too many interned strings
- Workaround: Monitor string count in production; limit number of unique strings per profile
- Fix approach: Either cap interning or add proactive quota checks; better error messages to identify when this occurs

**Profile Dictionary Missing Memory Recovery:**
- Symptoms: No clear memory cleanup path if dictionary operations fail mid-transaction
- Files: `libdd-profiling-ffi/src/profiles/profiles_dictionary.rs:245`
- Impact: On error, partial state may remain in Arc-based storage; could leak string references
- Workaround: Ensure successful operations complete; test error paths extensively
- Fix approach: Implement transactional semantics or rollback mechanism for failed operations

**Malformed URL Handling in Common:**
- Symptoms: Silently accepts malformed URLs instead of returning error
- Files: `libdd-common/src/lib.rs:284`
- Impact: Invalid endpoint configurations might fail late during request time rather than early validation
- Workaround: Pre-validate URLs in callers
- Fix approach: Add URL validation function; return Result from URL parsing; add input tests

## Security Considerations

**Unsafe UTF-8 Conversions Without Validation:**
- Risk: Multiple `from_utf8_unchecked` calls assume input is valid UTF-8 without checking
- Files: `libdd-profiling-ffi/src/profile_status.rs:204`, `libdd-profiling-ffi/src/profiles/utf8.rs:61`, `libdd-common-ffi/src/error.rs:68`, `libdd-common-ffi/src/slice.rs:130`, `libdd-profiling/src/profiles/collections/string_set.rs:101`
- Current mitigation: Comments indicate upstream validation; FFI boundary documentation requires caller to ensure UTF-8
- Recommendations: Document UTF-8 invariants clearly at FFI boundaries; consider Utf8Option::Validate wrapper in more places; add fuzzing tests for malformed UTF-8 inputs

**Transmute Operations for ID Type Conversions:**
- Risk: Unsafe transmute between different ID types (SetId, StringRef, MappingId2, FunctionId2) could cause type confusion if layouts change
- Files: `libdd-profiling/src/profiles/datatypes/*.rs`, `libdd-profiling/src/internal/profile/mod.rs:878`
- Current mitigation: IDs are transparent newtypes with same repr; comments note transmute usage
- Recommendations: Add compile-time assertions for layout equivalence; consider using `as` casting for transparent newtypes instead of transmute; add tests verifying ID type invariants

**Use-After-Free in FFE FFI Handle:**
- Risk: `.expect("detected use after free")` returns unwrapped reference; FFI caller could reuse freed handle causing undefined behavior
- Files: `datadog-ffe-ffi/src/handle.rs:46`
- Current mitigation: Documentation states caller must ensure validity; panic on null
- Recommendations: Consider returning error type instead of panicking; add handle validation in debug builds; document handle lifetime requirements in FFI headers

**Panic Across FFI Boundaries:**
- Risk: Multiple panic! calls in non-test code can propagate across FFI boundaries, causing undefined behavior in C/C++ callers
- Files: `libdd-profiling-ffi/src/string_storage.rs:288,304,313`, `libdd-common-ffi/src/option.rs:40`, `libdd-common-ffi/src/string.rs:88`
- Current mitigation: Most FFI entry points wrap with catch_unwind; some helper functions don't have this protection
- Recommendations: Audit all functions exposed at FFI boundary; add catch_unwind wrapper to all non-test panics; return error codes instead; enforce with clippy lint

**Raw Pointer Arithmetic Without Bounds Checking:**
- Risk: Unsafe pointer operations throughout profiling and FFI code
- Files: `libdd-profiling-ffi/src/profile_status.rs:175-185`
- Current mitigation: Vec invariants documented; SAFETY comments explain assumptions
- Recommendations: Extract pointer arithmetic into helper functions with documented invariants; add assertions in debug builds; consider using slice methods instead of raw pointers where possible

## Performance Bottlenecks

**Tracer Metadata Clone on Every Log:**
- Problem: `libdd-telemetry/src/worker/mod.rs:733` clones entire tracer metadata for each log entry
- Files: `libdd-telemetry/src/worker/mod.rs:733`
- Cause: Data model requires owned data; could be optimized with references or lazy evaluation
- Improvement path: Refactor to accept `&[Log]` instead of owned `Vec<Log>`; use Copy types for metadata that fit in registers

**String Interning Without Cache Eviction:**
- Problem: String table grows unbounded; no cache eviction for rarely-used strings in profiles
- Files: `libdd-profiling/src/collections/string_table/mod.rs:21`
- Cause: Each unique string is interned permanently
- Improvement path: Implement LRU or generational cache; add metrics for string table growth; consider hash-based deduplication instead

**Span Concentrator Hash Map Full Drain:**
- Problem: `HashMap::drain()` requires full iteration to remove expired spans; waiting for stabilized `extract_if`
- Files: `libdd-trace-stats/src/span_concentrator/mod.rs:210`
- Cause: Cannot efficiently remove subset of entries
- Improvement path: Switch to Rust 1.80+ with `extract_if` when MSRV allows; or use alternative data structure (BTreeMap with time-based index)

**SQL Obfuscation State Machine Memory:**
- Problem: Large state machine in `sql.rs` creates many intermediate allocations during parsing
- Files: `libdd-trace-obfuscation/src/sql.rs:635`
- Cause: Complex branching and string building for each token
- Improvement path: Use streaming iterator pattern instead of collecting; preallocate output buffer; profile hot paths

**Obfuscator Cache Missing Optimization:**
- Problem: Obfuscators are recreated on every obfuscation call instead of being cached
- Files: `libdd-trace-obfuscation/src/obfuscate.rs:140,150,160`
- Cause: Comment notes optimization opportunity but not implemented
- Improvement path: Cache compiled obfuscators per config; use Arc to share across threads; measure cache hit rate

## Fragile Areas

**Profiling Profile FFI Datatypes:**
- Files: `libdd-profiling-ffi/src/profiles/datatypes.rs`
- Why fragile: Complex FFI with manual memory management; 1140 lines with multiple unsafe blocks and transmute operations
- Safe modification: Add comprehensive property tests for FFI round-trips; test with miri; validate memory layout with assert_eq_size!
- Test coverage: Unit tests exist but integration coverage with actual profilers is limited

**Sidecar Server Core Logic:**
- Files: `datadog-sidecar/src/service/sidecar_server.rs`
- Why fragile: 1369 lines handling multiple concurrent protocol paths (Datadog, OTLP) with shared state; integration point for crash tracking, profiling, tracing
- Safe modification: Add comprehensive error injection tests; create integration tests that stress multiple paths concurrently; document invariants
- Test coverage: Mostly unit tests; missing stress tests and failure scenarios

**Crash Tracker Collector Windows API:**
- Files: `libdd-crashtracker/src/collector_windows/api.rs`
- Why fragile: Uses Windows PE parsing and debug info extraction; hardcoded assertions on module structure
- Safe modification: Add error handling path for malformed PE files; test against variety of Windows binaries; avoid unwrap() on PE fields
- Test coverage: Limited; relies on Windows-specific test binaries

**Data Pipeline Trace Exporter:**
- Files: `libdd-data-pipeline/src/trace_exporter/mod.rs`
- Why fragile: 2468 lines coordinating agent communication, retries, stats computation with multiple worker threads
- Safe modification: Carefully test error paths; use chaos engineering to test timeout/failure scenarios; document thread safety invariants
- Test coverage: Good unit test coverage but missing end-to-end failure injection tests

**Library Config with Remote Config Integration:**
- Files: `libdd-library-config/src/lib.rs`, `libdd-library-config/src/tracer_metadata.rs`
- Why fragile: 1367+ lines parsing protobuf with multiple expect/unwrap for type coercion; panic on unexpected variants in tests
- Safe modification: Replace panic! with proper error types; use type-safe Result wrapper for metadata parsing; test malformed protobuf
- Test coverage: Mock tests pass; real protobuf variations not tested

## Scaling Limits

**String Interning Capacity:**
- Current capacity: Effectively unlimited with Arc<str>; memory-limited only
- Limit: Will cause reference count overflow once unique strings exceed Arc's capacity (likely ~10^15 in practice)
- Scaling path: Implement quoted reference counting; add metrics for string table size; add configuration for max unique strings per profile

**HTTP Connection Pooling:**
- Current capacity: Single shared connection pool per http-client instance; reqwest backend has default pool limits
- Limit: High-concurrency SDKs may exhaust pool connections; no backpressure on exhaustion
- Scaling path: Make pool size configurable; add queue for pending requests; monitor pool saturation

**Span Concentrator Hash Map:**
- Current capacity: Unbounded memory for active spans; no eviction of old traces
- Limit: OOM when number of concurrent spans exceeds available memory
- Scaling path: Add configurable TTL and max-span limits; implement circular buffer with drop-oldest policy; add overflow metrics

**Crash Tracking Event Buffer:**
- Current capacity: In-memory queue before serialization
- Limit: Not clear from code review; depends on platform and memory constraints
- Scaling path: Add configurable buffer size; implement disk-backed overflow for production use

## Dependencies at Risk

**MSRV/Nightly Dependency on Unstable Features:**
- Risk: Code uses unstable Rust features waiting for stabilization (e.g., `Box<[I]>::into_iter`, `variant_count`, `str::floor_char_boundary`)
- Impact: Pins minimum supported Rust version; blocks upgrades
- Migration plan: Monitor feature stabilization; update MSRV when features stabilize; file issues if stabilization stalled

**Windows Platform Dependencies (libdd-crashtracker):**
- Risk: Windows-specific code using Windows crate APIs that may not be stable across versions
- Impact: Binary compatibility across Windows versions uncertain
- Migration plan: Test against multiple Windows versions in CI; lock critical Windows crate versions; document tested versions

**Protobuf Code Generation (libdd-trace-protobuf):**
- Risk: Hand-written FFI bindings for protobuf structures; divergence risk if schema updates
- Impact: Breaking changes to schema may require manual code updates
- Migration plan: Add schema validation tests; consider proto-gen migration; document manual override locations

## Missing Critical Features

**Proper URL Validation:**
- Problem: No validation that URLs are well-formed before using them in requests
- Blocks: Early error detection for misconfigured endpoints; clear error messages
- Path to implement: Add `Endpoint::validate()` method; use in builder patterns; add tests for malformed URLs

**Hash Caching in Tinybytes:**
- Problem: Hash recomputed on every access despite immutable data
- Blocks: Performance optimization for frequently-hashed spans
- Path to implement: Add `OnceCell<u64>` field to cache hash; measure performance impact; document trade-off

**Proactive Memory Quota Enforcement:**
- Problem: String interning and span storage have no quota; only fail when capacity exhausted
- Blocks: Graceful degradation under memory pressure
- Path to implement: Add configurable limits; return error when limits exceeded; expose metrics for monitoring

**Comprehensive Error Injection Testing:**
- Problem: Limited chaos/fault injection tests across async boundaries
- Blocks: Confidence in error handling; hard to reproduce production issues
- Path to implement: Use fail crate or similar for error injection; test all worker failure paths; add kill-switch tests

## Test Coverage Gaps

**FFI Boundary Panic Handling:**
- What's not tested: C callers receiving panics across FFI boundaries; unwinding behavior in C context
- Files: `libdd-telemetry-ffi/src/lib.rs`, `libdd-profiling-ffi/src/exporter.rs`, `datadog-sidecar-ffi/src/lib.rs`
- Risk: Undefined behavior if panic unwinds into C code; tests only cover Rust-side panic safety
- Priority: High - FFI safety is critical for production stability

**Windows Crash Handler Coverage:**
- What's not tested: Full crash scenario on Windows with real exceptions and signal handlers
- Files: `libdd-crashtracker/src/collector_windows/api.rs`
- Risk: Crash handler may fail silently on unexpected Windows error codes or exception types
- Priority: High - crash tracking must work under real crashes

**Malformed Input at FFI Boundaries:**
- What's not tested: Null pointers, invalid UTF-8, misaligned pointers passed from C
- Files: Multiple FFI files across `libdd-*/src/lib.rs`
- Risk: UB or panic when C callers pass invalid data
- Priority: High - production C callers may make mistakes

**Concurrent Sidecar Operations:**
- What's not tested: Multiple concurrent gRPC/HTTP requests under high load with shared state mutations
- Files: `datadog-sidecar/src/service/sidecar_server.rs`
- Risk: Race conditions in shared span/trace state; data corruption or panics under load
- Priority: High - sidecar runs in production services

**Data Pipeline Failure Scenarios:**
- What's not tested: Agent connection drops mid-operation; timeout during stats computation; retries with out-of-order responses
- Files: `libdd-data-pipeline/src/trace_exporter/mod.rs`, `libdd-data-pipeline/src/trace_buffer/mod.rs`
- Risk: Traces lost or duplicated on network failures; stats corruption
- Priority: Medium - covered by integration tests but missing unit-level failure injection

**Profiling Memory Allocation Failures:**
- What's not tested: Allocation failures (OOM) in middle of profile construction
- Files: `libdd-profiling/src/internal/profile/mod.rs`, `libdd-profiling/src/exporter/exporter_manager.rs`
- Risk: Partial profiles sent; crash due to panic on allocation failure
- Priority: Medium - mitigated by capacity overflow checks but edge cases remain

**Library Config Parsing Edge Cases:**
- What's not tested: Malformed protobuf with unexpected field types; recursive structures; size limits
- Files: `libdd-library-config/src/lib.rs`, `libdd-library-config/src/tracer_metadata.rs`
- Risk: Panics on unexpected data; unbounded memory usage on pathological input
- Priority: Medium - remote config is untrusted input

---

*Concerns audit: 2026-06-15*
