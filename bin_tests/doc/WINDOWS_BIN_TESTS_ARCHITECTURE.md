# Windows bin_tests Architecture

## Overview

The Windows bin_tests infrastructure provides integration testing for crash tracking functionality on Windows platforms. Unlike the Unix implementation which relies on signal handlers and Unix domain sockets, the Windows architecture integrates with the Windows Error Reporting (WER) system, requiring a fundamentally different approach to test design and execution.

## Design Philosophy

### Core Principles

**1. CI-First Test Design**
The architecture is designed specifically for CI/testing environments where the Windows Error Reporting service is unavailable:
- **Current Implementation**: Uses a custom WER simulator that mimics WER behavior without requiring the WER service
- **Production Crashtracker**: Uses WER's native out-of-process handlers via `WerRegisterRuntimeExceptionModule`
- **Test Focus**: Validates that the crashtracker code works correctly, using the simulator as a test double for WER

Note: The tests themselves do not currently support running with real WER. They exclusively use the simulator approach.

**2. Separation of Crash Generation and Analysis**
Following WER's architecture, crash handling occurs in a separate process. This isolation provides:
- Protection against heap corruption during crash handling
- Ability to use full system APIs in the handler
- Realistic testing of production crash handling flow

**3. Coverage Collection Support**
The architecture is designed from the ground up to support code coverage collection through:
- Environment variable propagation to spawned processes
- Multiple process profraw file generation
- Integration with cargo-llvm-cov workflow

**4. Platform Parity with Unix Tests**
While implementation differs fundamentally, the testing API mirrors the Unix bin_tests structure to maintain consistency across platforms.

## Architectural Components

### 1. Test Type System

**Purpose**: Defines the vocabulary of Windows-specific crash scenarios and test configurations.

**Key Abstractions**:
- **WindowsCrashType**: Represents Windows-specific exceptions (access violations, divide-by-zero, stack overflow, illegal instructions)
- **WindowsTestMode**: Defines test behavioral configurations (basic, multithreaded, deep stack scenarios)

**Design Rationale**: Windows exceptions are fundamentally different from Unix signals. Access violations have sub-types (null, read, write), and Windows has exception codes (0xC0000005, 0xC0000094) rather than signal numbers. The type system captures these Windows-specific semantics.

### 2. Validation Framework

**Purpose**: Provides structured validation of Windows crash reports.

**WindowsPayloadValidator Pattern**:
The validator uses a fluent API pattern specifically adapted for Windows crash reports, validating:
- Exception codes and types
- Thread context information
- Stack traces with Windows-specific properties
- Module enumeration from ToolHelp API
- Registry key state

**Design Choice**: Separate from Unix validation because Windows crash reports have different structure:
- No signal information (si_code, si_addr)
- Exception codes instead of signals
- Thread IDs instead of TID from siginfo
- Different stack trace format from StackWalkEx

### 3. Test Runner Architecture

**Responsibility**: Orchestrates Windows crash test execution with WER integration.

**Key Differences from Unix**:

**Asynchronous Crash Handling**:
- Unix: Signal handler runs synchronously in crashed process
- Windows: WER handler runs asynchronously in separate process

**Consequence**: Test runner must poll for crash report file creation rather than immediate validation.

**Registry Management**:
The test runner handles WER registration requirements:
- Creating registry keys before test execution
- Verifying registration success
- Cleaning up registry state after tests
- Ensuring test isolation through unique registry keys

**Process Spawning Strategy**:
Unlike Unix tests which spawn and wait, Windows tests must:
1. Spawn crash binary with WER initialized
2. Wait for crash to occur
3. Poll for WER handler completion
4. Validate resulting crash report file

### 4. WER Simulator Design

**Problem**: Windows Error Reporting service is typically unavailable in CI environments (GitHub Actions, containers, headless systems).

**Solution**: Custom WER simulator that mimics WER's out-of-process crash handling without requiring the WER service.

**Important Distinction**: The WER simulator is a test infrastructure component, not part of the production crashtracker. In production, applications use the real WER system via `WerRegisterRuntimeExceptionModule`. The simulator allows testing the crashtracker's WER integration without needing the actual WER service.

**Architecture Benefits**:

**Process Isolation**:
- Crash handler runs in completely separate process
- No allocations in crashed process memory space
- Safe to use full system APIs (StackWalkEx, ToolHelp)

**Synchronization Design**:
Uses Windows named events for IPC:
- `CrashReady_{PID}`: Crash binary signals initialization complete
- `SimulatorReady_{PID}`: Simulator signals ready to receive crashes
- `CrashEvent_{PID}`: Crash binary signals exception occurred
- `DoneEvent_{PID}`: Simulator signals processing complete

**Memory Access Strategy**:
The simulator uses `ReadProcessMemory` to access:
- WER context structure from crashed process
- Exception code location
- Thread context
- Module information

This approach mimics real WER behavior where the out-of-process handler reads crashed process memory.

**Why Named Events?**:
- Kernel-managed synchronization primitives
- Work across process boundaries
- Support timeouts for fault tolerance
- Atomic signaling with no race conditions

### 5. Coverage Collection Architecture

**Challenge**: Coverage from spawned processes must contribute to overall coverage report.

**Solution Strategy**:

**Build-Time Instrumentation**:
When `cargo llvm-cov` builds test binaries, it sets `RUSTFLAGS` with `-C instrument-coverage`. The build system propagates these flags to spawned `cargo build` commands to ensure all test binaries are instrumented.

**Runtime Coverage Propagation**:
The `LLVM_PROFILE_FILE` environment variable contains patterns like `%p` (process ID) that are expanded by each process at runtime:
- Main test process: `cargo-test-1000-abc.profraw`
- Spawned crash binary: `cargo-test-1001-def.profraw`
- WER simulator: `cargo-test-1002-ghi.profraw`

**Workflow Integration**:
Separate Windows coverage job runs in parallel with Linux:
- Both upload coverage to Codecov
- Codecov merges reports based on commit SHA
- Flags allow filtering by platform

**Design Rationale**: This approach requires no code changes in test binaries—coverage "just works" when environment variables are propagated correctly.

## Testing Workflow

### Standard Windows Test Flow

**1. Setup Phase**:
- Create temporary directory for outputs
- Generate unique test identifiers (PID-based)
- Create synchronization primitives (named events)

**2. Initialization Phase**:
- Build test artifacts with instrumentation
- Initialize WER context in test binary
- Register crash handler (real WER or simulator)

**3. Execution Phase**:
- Spawn crash binary as separate process
- Wait for initialization signal
- For simulator mode: spawn WER simulator process
- Wait for simulator ready signal

**4. Crash Phase**:
- Crash binary triggers exception
- Signals crash occurred (no allocations!)
- Waits for handler completion (with timeout)
- Process terminates

**5. Handler Phase**:
- WER handler/simulator wakes up
- Opens handles to crashed process
- Reads memory, walks stacks, enumerates modules
- Generates crash report JSON
- Signals completion

**6. Validation Phase**:
- Test runner reads crash report file
- Parses JSON payload
- Runs validator chain
- Verifies expected exception properties
- Checks stack traces and module lists

**7. Cleanup Phase**:
- Close event handles
- Remove temporary files
- Clean up registry keys (if applicable)

### Parallel Coverage Collection Flow

**Coverage CI Workflow**:
```
coverage-linux (ubuntu-latest)
  ├─ Build with instrumentation
  ├─ Run Unix bin_tests
  ├─ Spawned processes write profraw files
  ├─ Generate lcov.info
  └─ Upload to Codecov (flag: linux)

coverage-windows (windows-latest)
  ├─ Build with instrumentation
  ├─ Run Windows bin_tests
  ├─ Spawned processes write profraw files
  ├─ WER simulator writes profraw file
  ├─ Generate lcov-windows.info
  └─ Upload to Codecov (flag: windows)

Codecov Server:
  ├─ Receives both uploads (same commit SHA)
  ├─ Merges line-by-line coverage
  └─ Presents unified report
```

## Design Trade-offs

### Process Isolation vs Simplicity

**Decision**: Use out-of-process crash handling (matching WER design).

**Trade-off**: More complex synchronization and IPC, but:
- Safer (no allocations in crashed process)
- More realistic (matches production behavior)
- More flexible (can use full API surface in handler)

**Rationale**: The complexity cost is paid once in infrastructure. All tests benefit from the safety and realism.

### WER Simulator vs Real WER

**Decision**: Use custom WER simulator exclusively for tests.

**Trade-off**: Simulator requires significant infrastructure and doesn't test the actual WER registration path, but:
- Enables testing in CI environments without WER service (critical requirement)
- Provides deterministic behavior for tests
- Allows detailed debugging and instrumentation
- Matches production WER flow closely enough to validate crash handling logic

**Limitation**: Tests do not validate the actual `WerRegisterRuntimeExceptionModule` registration or the real WER callback invocation. Those aspects require manual testing on systems with WER enabled.

**Rationale**: CI testing is critical. Without simulator, Windows tests could only run on local machines with WER, which would severely limit coverage and create friction for contributors.

### Polling vs Event-Driven File Detection

**Decision**: Poll for crash report file creation with timeout.

**Trade-off**: Polling introduces delay and complexity, but:
- WER handler completion is inherently asynchronous
- File system events are unreliable in tests
- Timeout provides fault tolerance
- Polling interval tuned for test speed vs reliability

**Rationale**: The asynchronous nature of WER requires some form of waiting. Polling is simple and robust.

### Named Events vs Other IPC

**Decision**: Use Windows named events for synchronization.

**Trade-off**: Windows-specific API, but:
- Kernel-managed (no race conditions)
- Work across processes naturally
- Support timeouts natively
- Manual reset semantics match use case

**Alternatives Rejected**:
- Pipes: Too much data transfer overhead for simple signaling
- Files: Race-prone, requires polling, cleanup issues
- Shared memory: Complex setup, requires additional synchronization

### Memory Context Passing vs Serialization

**Decision**: Pass WER context address to simulator via command line.

**Trade-off**: Requires `ReadProcessMemory`, but:
- Matches real WER behavior exactly
- Tests the actual crash context reading code
- Avoids serialization overhead in crashed process
- Validates that context structure is correct

**Rationale**: Testing should match production as closely as possible. Real WER reads context from memory, so tests should too.

## Platform Differences

### Unix vs Windows Test Architecture

**Fundamental Differences**:

**Signal Handling vs Exception Handling**:
- Unix: Install signal handlers that run in-process
- Windows: Register with WER, handler runs out-of-process

**Synchronous vs Asynchronous**:
- Unix: Signal handler completes before process terminates
- Windows: Process may terminate before handler completes

**IPC Mechanism**:
- Unix: Unix domain sockets for data transfer
- Windows: ReadProcessMemory for data access, events for signaling

**Receiver Process**:
- Unix: Requires separate receiver process for data processing
- Windows: WER handler IS the receiver (combines roles)

**Test Complexity**:
- Unix: Simpler synchronization, immediate results
- Windows: Complex synchronization, asynchronous validation

**Coverage Collection**:
- Unix: Main process + receiver process profraw files
- Windows: Main process + crash binary + WER simulator profraw files

### Implications for Test Design

**Test Timing**:
Windows tests require longer timeouts due to asynchronous handler execution. Unix tests can validate immediately after process exit.

**Test Isolation**:
Windows tests must ensure unique registry keys and event names to prevent cross-test interference. Unix tests have natural isolation through PID-based socket paths.

**Artifact Count**:
Windows tests build additional binaries (WER simulator, crash test app). Unix tests reuse fewer artifacts across tests.

**Failure Modes**:
Windows tests have more failure points (registry, events, memory access, file polling). Each requires specific diagnostic messages.

## Extension Points

### Adding New Crash Types

To test a new Windows exception type:
1. Add variant to `WindowsCrashType` enum
2. Map to Windows exception code (NTSTATUS)
3. Implement crash trigger in test binary
4. Define expected exception code in validator
5. Add test case using new crash type

No changes needed to test runner or simulator—they're crash-type agnostic.

### Adding New Test Modes

To test new crash scenarios:
1. Add variant to `WindowsTestMode` enum
2. Implement behavior in crash binary
3. Define validation expectations
4. Create test case with custom validator

Example scenarios: COM initialization, DLL loading, thread-local storage.

### Customizing Validation

For specialized validation needs:
- Create custom validator closure
- Chain with standard WindowsPayloadValidator
- Access full JSON payload for complex checks
- Use test fixtures for file-based validation

### Supporting Alternative Handlers

**Current State**: Tests exclusively use WER simulator.

**Potential Future Enhancement**:
The architecture could be extended to support:
- Real WER service testing (for production validation on WER-enabled systems)
- Alternative crash handler implementations
- Pluggable handler selection via test configuration

This would require:
- Test configuration flag for handler type
- Conditional process spawning logic in test runner
- Platform capability detection

## Future Considerations

### Performance Optimization

**Current State**: Tests run sequentially to simplify debugging.

**Potential Improvements**:
- Parallel test execution (with unique registry keys and events)
- Cached binary builds (reduce compilation time)
- Shared WER simulator process (reduce spawn overhead)

**Trade-off**: Parallel tests are faster but harder to debug. Start simple, optimize if needed.

### Enhanced Diagnostics

**Current State**: Basic error messages and debug output.

**Potential Improvements**:
- Structured logging with severity levels
- Performance metrics (handler invocation time, stack walk duration)
- Memory dumps on unexpected failures
- Timeline visualization of synchronization events

### Cross-Platform Test API

**Current State**: Separate Windows and Unix test APIs.

**Potential Improvements**:
- Platform-agnostic test trait
- Unified test configuration type
- Shared validation primitives
- Common test macro for simple cases

**Rationale**: As Windows coverage grows, reducing duplication becomes valuable.

### Alternative Handler Testing

**Current State**: Only WER handler tested.

**Future**: Support testing alternative crash handlers:
- Breakpad integration
- Crashpad compatibility
- Custom handler implementations

This would validate that crash tracking works with multiple handler backends.

## Testing Philosophy

### Integration Over Isolation

Windows bin_tests focus on end-to-end flows rather than unit testing individual components. This approach validates:
- Real WER registration and invocation
- Actual exception triggering and handling
- Complete crash report generation pipeline
- Full file I/O and serialization paths

**Rationale**: Crash tracking is about system integration. Mocking WER, exceptions, or file I/O wouldn't validate the real behavior.

### Realistic Test Environment

Tests aim to match production as closely as possible:
- Use real Windows exceptions
- Invoke actual WER APIs
- Read process memory like WER does
- Generate real crash report files

**Difference from Production**: WER simulator replaces WER service, but behavior is equivalent for test purposes.

### Fail-Fast with Context

When tests fail, they should:
- Indicate which validation step failed
- Show expected vs actual values
- Provide context about test configuration
- Include debug output from handler process

**Implementation**: Rich error types, verbose logging, debug files written to temp directories.

### Coverage as First-Class Concern

Test infrastructure considers coverage collection from the start:
- Environment variable propagation built-in
- Multiple process profraw files expected
- CI workflow includes coverage verification
- Documentation explains coverage architecture

**Rationale**: Without coverage, we can't know what the tests actually exercise.

## Conclusion

The Windows bin_tests architecture balances several competing concerns:
- **Realism**: Match production WER behavior as closely as possible
- **Testability**: Enable testing without WER service in CI
- **Safety**: Handle crashes in separate process like real WER
- **Observability**: Collect coverage from all spawned processes
- **Maintainability**: Clear abstractions and extension points

The result is a comprehensive testing framework that validates Windows crash tracking in realistic scenarios while supporting both local development and CI environments. The architecture's flexibility allows future extension while maintaining strong type safety and clear separation of concerns.

