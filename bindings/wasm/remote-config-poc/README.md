# Shared remote-configuration WASM PoC

This prototype runs libdatadog's remote-configuration client in Node.js and .NET using the same WASM file. Each host supplies HTTP and timers. Libdatadog retains the agent and agentless protocols, TUF verification, file hashing, cached state, change tracking, and acknowledgement state.

The prototype is not a production tracer integration. Its generated scalar bindings are intentionally small. Options, host events, replies, and results use UTF-8 JSON. Binary HTTP bodies and configuration contents use base64. This proves ownership and interoperability, not a low-copy or optimized interface.

## Create and run the bindings

From this directory, install the pinned Rust toolchain and WASI target. The generator also requires `nightly-2026-07-26` rustfmt, matching the repository's formatter. Use Node.js 22, the .NET 10 SDK, and a Java 21 JDK for the demonstrated host programs. The Java build compiles to Java 11 bytecode.

The build requires LLVM's `clang` and `llvm-ar` for the WASI cryptography dependency, and Binaryen `wasm-opt` version 133. On macOS, install LLVM with Homebrew. `LLVM_BIN` can select another LLVM directory, including its trailing slash. `CC_wasm32_wasip1` and `AR_wasm32_wasip1` override the individual tools. `WASM_OPT` can select the optimizer executable. The release profile optimizes for size and strips symbols before Binaryen optimizes the final module.

```sh
rustup target add --toolchain 1.98.0 wasm32-wasip1
node scripts/build.mjs
node scripts/generate.mjs --check
node --test test/conformance.mjs
```

The build restores the pinned Wasmtime NuGet package for the .NET host. Wasmtime includes a native runtime dependency. Node uses its built-in WebAssembly engine. The Java host uses Endive's build-time compiler: the Gradle build turns every wasm function body into JVM bytecode ahead of time, so the runtime executes the generated classes instead of Endive's interpreter. No native dependency is added. No JavaScript engine runs inside .NET.

`abi.json` defines the ABI version, function names, and scalar arguments. `scripts/generate.mjs` emits Rust exports, JavaScript declarations and memory helpers, typed C# delegates and memory helpers, and the Java `RcAbi` class. Generated files are checked in. `--check` detects divergence.

```text
abi.json -> Rust exports -> libdd-remote-config -> one WASM reactor
        -> JavaScript binding -> Node HTTP/timers
        -> C# binding --------> .NET HttpClient/Task.Delay
        -> Java binding -----> Endive build-time compiled classes + java.net.http/ScheduledExecutor
```

The generator does not infer remote-configuration semantics. The Rust facade implements them through the existing upstream client. Rust serde types define JSON records. The host drivers translate operations into runtime services. Full generation of those record types remains future work.

`artifacts/remote-config.wasm` is the canonical output. Every host program accepts its path as their first argument; Node and .NET load the file, while the Java host keeps the argument only for CLI uniformity and executes the module compiled into its jar at build time. The manifest records its hash. The conformance runner loads the same file in all engines and uses a controlled HTTP service with independently constructed protocol responses. Set the `LANGUAGES` environment variable (for example `LANGUAGES=nodejs,java`) to run a subset of the host matrix.

## Ownership and asynchronous work

Buffers and clients have checked integer handles. An input buffer remains caller-owned and must be freed after the consuming call. Reading an output through either generated helper copies and frees it. No host retains a view into guest memory across a guest call. Buffer allocation has per-buffer and aggregate limits.

A poll yields HTTP and timeout operations. The host completes an operation by ID through `rc_complete`. Guest calls never overlap. The C# dispatcher awaits host work before entering the store again. The Node driver queues callbacks before completing the guest operation. The Java driver takes completions from a queue at the head of its poll loop, so no guest call is ever re-entered. Neither guest execution nor completion uses JavaScript promises inside WASM.

The Rust future owns a lease on the fetcher. Completion or cancellation returns it to the client. Operation IDs are not reused during that client's lifetime. Late and duplicate completions are rejected. Acknowledgement during an active poll is rejected explicitly. This prototype does not yet queue identity or product updates during a poll.

The upstream timeout fix makes `fetch_agent` use its supplied capability instance for timeout handling. Previously it constructed a new sleeper, which disconnected the timeout from an injected host queue. The native regression test checks that the supplied host receives the timeout.

## Agentless configuration

Supplying `apiKey` selects agentless mode. Set `url` to the HTTPS Datadog site URL and supply `hostname`. Libdatadog derives the configuration endpoint and sends the API key through the host transport. Treat client options and HTTP operation events as secrets. The fixture driver prints request events containing its dummy API key. Do not use that driver with real credentials.

Libdatadog uses its embedded TUF trust roots by default. The optional `configRoot` and `directorRoot` strings pass serialized root metadata to its existing root configuration API. These are trusted caller inputs fixed when the client is created. Remote responses cannot replace the initial trust roots. Root validation and agentless initialization run during the first poll. Cancellation restores the uninitialized client so a later poll can retry.

The agentless fixture signs metadata independently with Node's Ed25519 implementation and encodes the backend protobuf response. Both real WASM engines verify that response through libdatadog. The fixture driver supplies HTTP completions directly through the ABI, so this proves protocol interoperability without contacting Datadog or using real credentials. The separate HTTP scenarios exercise each host's transport and cancellation.

## Scope and validation

The scenarios exercise add, update, removal, unchanged polls, successful and failed acknowledgements, cancellation, timeout, rejected late completion, and cached state preservation. A corrupt file must not reach either consumer. The following request must report its hash error, matching the upstream agent protocol contract. Both hosts also cancel a real in-flight HTTP request and prove the client can poll again.

The reactor uses WASI Preview 1 for randomness and clocks. It receives no preopened directories or inherited environment. Its import check rejects JavaScript and Node modules. WASI remains a runtime compatibility choice that needs evaluation across supported tracer release lines.

Production proxy/TLS/suppression policy, dynamic client updates, concurrent guest entry, NativeAOT, older framework support, package publication, and performance budgets are outside this prototype.

The .NET project is an executable host example. Production adoption requires moving engine lifecycle and runtime policy into dd-trace-dotnet. Node adoption similarly requires its existing request policy and lifecycle, rather than shipping this sample transport unchanged.
