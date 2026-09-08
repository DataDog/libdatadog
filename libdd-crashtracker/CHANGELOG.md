# Changelog



## [2.0.1](https://github.com/datadog/libdatadog/compare/libdd-crashtracker-v2.0.0..libdd-crashtracker-v2.0.1) - 2026-08-25

### Fixed

- Hold the collector connection open through symbolization ([#2384](https://github.com/datadog/libdatadog/issues/2384)) - ([e235e76](https://github.com/datadog/libdatadog/commit/e235e768cae55e37ba5b1de52acb7052cbde5a58))
- Stop resolving thread symbols with libunwind in the receiver ([#2361](https://github.com/datadog/libdatadog/issues/2361)) - ([24c833f](https://github.com/datadog/libdatadog/commit/24c833fe595ab162bbcf370d93dc19fb11ef46bc))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-crashtracker-v1.0.0..libdd-crashtracker-v2.0.0) - 2026-08-18

### Added

- Retrieve c assert message for linux when `__assert_fail` is dynamically loaded ([#2268](https://github.com/datadog/libdatadog/issues/2268)) - ([3e3454a](https://github.com/datadog/libdatadog/commit/3e3454aa172ad7ef9423537e62d169d09bba1452))
- Send debug log when no data is received at all ([#2321](https://github.com/datadog/libdatadog/issues/2321)) - ([194becd](https://github.com/datadog/libdatadog/commit/194becd3d384733ffe9709418d5e9d85033657a0))
- Collect all native stacks for unhandled exception and also crashing thread ([#2155](https://github.com/datadog/libdatadog/issues/2155)) - ([73e73f6](https://github.com/datadog/libdatadog/commit/73e73f66d8dc1cbff49c6be9306f191be1297ba3))
- Add experimental frame count field ([#2099](https://github.com/datadog/libdatadog/issues/2099)) - ([53dd5eb](https://github.com/datadog/libdatadog/commit/53dd5ebe493c186fa639c264b93c81fa1f2e8cb2))
- Collect all threads ([#1878](https://github.com/datadog/libdatadog/issues/1878)) - ([a8f0aaa](https://github.com/datadog/libdatadog/commit/a8f0aaaa4c904ba9fb368119d1a3d33b0255d2d6))
- Improve parity between errors intake payload and telemetry intake payload ([#1823](https://github.com/datadog/libdatadog/issues/1823)) - ([703e1e2](https://github.com/datadog/libdatadog/commit/703e1e256c502f3f1533d91f4bb80aa1275ae1fc))
- Emit ucontext registers as structured data ([#1787](https://github.com/datadog/libdatadog/issues/1787)) - ([15860bb](https://github.com/datadog/libdatadog/commit/15860bbc54eef039f0cd19396fbc9ff5f82c1ec1))
- Report unhandled exceptions ([#1596](https://github.com/datadog/libdatadog/issues/1596)) - ([eb48c1a](https://github.com/datadog/libdatadog/commit/eb48c1a8c6b1f115e0cb1f357ca300e46c089e25))
- Include `Kind` in crash ping and clarify requirements ([#1595](https://github.com/datadog/libdatadog/issues/1595)) - ([27de9f3](https://github.com/datadog/libdatadog/commit/27de9f37d5ece4e0d737703efc879b88f7040540))
- Emit crashing thread name in crash report for linux crashes ([#1485](https://github.com/datadog/libdatadog/issues/1485)) - ([c9d6835](https://github.com/datadog/libdatadog/commit/c9d68358e2acab9461c2b6403f5e2426b823b756))
- Make telemetry worker wasm-compatible for the TraceExporter ([#2172](https://github.com/datadog/libdatadog/issues/2172)) - ([73f23a2](https://github.com/datadog/libdatadog/commit/73f23a2c03be39971c966e444785d63b6fd52e81))
- Include dependencies and integrations in app-extended-heartbeat ([#1962](https://github.com/datadog/libdatadog/issues/1962)) - ([91fd13c](https://github.com/datadog/libdatadog/commit/91fd13c8a0ca5335fe39940f8764cd825bbef7e8))
- Integrate obfuscation to the stats exporter [APMSP-2764] ([#1819](https://github.com/datadog/libdatadog/issues/1819)) - ([540f186](https://github.com/datadog/libdatadog/commit/540f18646d58bd18984990fbed85254b3678ac7f))

### Changed

- Bump to 29.0.0 ([#1702](https://github.com/datadog/libdatadog/issues/1702)) - ([001bd56](https://github.com/datadog/libdatadog/commit/001bd56fcbba34fa4ec3f9798a6c4fbcddeffa40))
- Give libdd-libunwind-sys its own version ([#1743](https://github.com/datadog/libdatadog/issues/1743)) - ([bb2b2bb](https://github.com/datadog/libdatadog/commit/bb2b2bb83decae7b71066c84c950caddd7f99dd2))
- Fix crashtracker receiver binary rpath setting ([#1652](https://github.com/datadog/libdatadog/issues/1652)) - ([b13e787](https://github.com/datadog/libdatadog/commit/b13e787309bad5636bbb64f56437a3cd8999af60))
- Update imports to linux only ([#2036](https://github.com/datadog/libdatadog/issues/2036)) - ([5aa4113](https://github.com/datadog/libdatadog/commit/5aa4113a1a8a76dad7f31e29b3d84e7dc6e4b82c))
- Use weaker mem ordering for OP_COUNTERS ([#1744](https://github.com/datadog/libdatadog/issues/1744)) - ([fa18a2b](https://github.com/datadog/libdatadog/commit/fa18a2b35fdca8821101be670ee3958089bc0556))
- Use default-features=false for aws-lc-sys ([#1625](https://github.com/datadog/libdatadog/issues/1625)) - ([5bb62b1](https://github.com/datadog/libdatadog/commit/5bb62b1aecfb67ed22d14e834989aa182d58752a))
- Harden multi thread ptrace collection ([#2216](https://github.com/datadog/libdatadog/issues/2216)) - ([11748c6](https://github.com/datadog/libdatadog/commit/11748c6508bad368df5e9fb562d223c5e22ea56f))
- Remove frame count experimental field ([#2114](https://github.com/datadog/libdatadog/issues/2114)) - ([bdc0a62](https://github.com/datadog/libdatadog/commit/bdc0a629487976a99f956ab98ced6c7f14396146))
- Create errorsintake crash ping directly from telemetry ([#1963](https://github.com/datadog/libdatadog/issues/1963)) - ([4719738](https://github.com/datadog/libdatadog/commit/471973823822cb9cc3fed30bc46bb5b0783aba71))
- Bump libdatadog-libunwind to v1.0.2 ([#1942](https://github.com/datadog/libdatadog/issues/1942)) - ([0a70516](https://github.com/datadog/libdatadog/commit/0a70516d66314efdd7115644b0da4b3b3e0958e0))
- Default errors intake crash report upload to be on ([#1902](https://github.com/datadog/libdatadog/issues/1902)) - ([08f85eb](https://github.com/datadog/libdatadog/commit/08f85eb55ca13fdd9fa01501054d262775c6bb0b))
- Preserve errno for crashtracker ([#1767](https://github.com/datadog/libdatadog/issues/1767)) - ([b15cd20](https://github.com/datadog/libdatadog/commit/b15cd20992cd19a56d798b90b0faf0151102e729))
- Rename target triple to runtime platform ([#1747](https://github.com/datadog/libdatadog/issues/1747)) - ([5426a8b](https://github.com/datadog/libdatadog/commit/5426a8b5162c8ef4787dbc87b55d8722126e49e5))
- Add tag for target triple ([#1741](https://github.com/datadog/libdatadog/issues/1741)) - ([6a02f01](https://github.com/datadog/libdatadog/commit/6a02f0142a29d349b4f4ea53ef9d70949cf44e5d))
- Emit a best effort stacktrace for Mac ([#1645](https://github.com/datadog/libdatadog/issues/1645)) - ([f79e281](https://github.com/datadog/libdatadog/commit/f79e281ce8ec941603d3faec3f9a3d65d9d7fba0))
- Bump os_info crate to 3.14 ([#1507](https://github.com/datadog/libdatadog/issues/1507)) - ([aa61ebb](https://github.com/datadog/libdatadog/commit/aa61ebb81846ad737e6c38409fa4a425bb2af86e))
- Add minimal LD preload test for crashtracker collector ([#1428](https://github.com/datadog/libdatadog/issues/1428)) - ([488418a](https://github.com/datadog/libdatadog/commit/488418af8be2a817f7df40e7b199eced836bcaab))
- Add `is_crash_debug` tag to crashtracker receiver debug logs ([#1445](https://github.com/datadog/libdatadog/issues/1445)) - ([efe99d5](https://github.com/datadog/libdatadog/commit/efe99d5e2992ab029e6ad58c3a77b0f615447b95))
- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Remove path reference for libdd-libunwind-sys ([#1877](https://github.com/datadog/libdatadog/issues/1877)) - ([c684b2e](https://github.com/datadog/libdatadog/commit/c684b2e6a691c22814d8c11aa71e5f37c8dc2264))
- Avoid leaking Endpoint through the public API ([#1705](https://github.com/datadog/libdatadog/issues/1705)) - ([892b7bf](https://github.com/datadog/libdatadog/commit/892b7bf3f873905a9cfca1f2b4649154830be3bc))
- Avoid leaking libdd-common types in the public API ([#2152](https://github.com/datadog/libdatadog/issues/2152)) - ([b3144c6](https://github.com/datadog/libdatadog/commit/b3144c676b73e157f9d563903c01df016882e8c4))
- Avoid a dedicated socket for crashtracker ([#2179](https://github.com/datadog/libdatadog/issues/2179)) - ([e91210e](https://github.com/datadog/libdatadog/commit/e91210ef9100b3eedf9b4365fba821da0007fad8))
- Skip/shorten slow miri jobs ([#2331](https://github.com/datadog/libdatadog/issues/2331)) - ([52b616d](https://github.com/datadog/libdatadog/commit/52b616d3a865583c28b9e422c832a4118446ac72))
- Migrate to workspace dependencies, phase 4 ([#2296](https://github.com/datadog/libdatadog/issues/2296)) - ([3c4c095](https://github.com/datadog/libdatadog/commit/3c4c0952c016b3b156d8a82ec27eeb515079d286))
- Migrate to workspace dependencies, phase 3 ([#2283](https://github.com/datadog/libdatadog/issues/2283)) - ([f73e8ae](https://github.com/datadog/libdatadog/commit/f73e8ae5997d54860984ad8e155fa9fa257d9263))
- Moving to workspace-level dependencies, phase 2 ([#2270](https://github.com/datadog/libdatadog/issues/2270)) - ([caa732f](https://github.com/datadog/libdatadog/commit/caa732f3fe7c82a347813ba36686e039d29981a3))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))
- Stabilize flaky tests ([#2256](https://github.com/datadog/libdatadog/issues/2256)) - ([054402d](https://github.com/datadog/libdatadog/commit/054402d28cb03f0b99df938e5ac0b419da18423b))
- Update nightly in CI to 2026-02-08 ([#1539](https://github.com/datadog/libdatadog/issues/1539)) - ([5b504e5](https://github.com/datadog/libdatadog/commit/5b504e5938a2ed15f38902b0aa5f7fecf99a9f9b))
- Don't bail ([#1494](https://github.com/datadog/libdatadog/issues/1494)) - ([41025bb](https://github.com/datadog/libdatadog/commit/41025bbe73f51c421b859f32691cf996a2bddf59))
- Prepare libdd-telemetry-v2.0.0 ([#1457](https://github.com/datadog/libdatadog/issues/1457)) - ([753df4f](https://github.com/datadog/libdatadog/commit/753df4f235074cd3420a7e3cd8d2ff9bc964db0d))
- [crashtracker] Retrieve panic message when crashing ([#1361](https://github.com/datadog/libdatadog/issues/1361)) - ([65a5d9a](https://github.com/datadog/libdatadog/commit/65a5d9af8c9931f8ecbf2db8729fabbc3881fb07))
- [crashtracker] Log errors in crashtracker receiver ([#1395](https://github.com/datadog/libdatadog/issues/1395)) - ([73c675b](https://github.com/datadog/libdatadog/commit/73c675b79f81978ee1190be6af0c5abec997e3b0))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))
- Fix CI ([#1389](https://github.com/datadog/libdatadog/issues/1389)) - ([4219fa9](https://github.com/datadog/libdatadog/commit/4219fa9adf2080321e58a0c1239edf003ec7529f))
- [crashtracker] Set OS info in the crash info builder when receiving report ([#1388](https://github.com/datadog/libdatadog/issues/1388)) - ([e6671fc](https://github.com/datadog/libdatadog/commit/e6671fc694068d3f4500a02bdd4b33fff241da82))
- Support cxx bindings for crashinfo ([#1379](https://github.com/datadog/libdatadog/issues/1379)) - ([6b26318](https://github.com/datadog/libdatadog/commit/6b263189044f48cec6a67745036bd027b44f6daa))

### Fixed

- Use single threaded for all tests that mutate signal state and use non-fatal signal ([#1812](https://github.com/datadog/libdatadog/issues/1812)) - ([489884e](https://github.com/datadog/libdatadog/commit/489884e77c9e778f92dc75e8d9d079fc64b32037))
- Use single threaded to avoid race conditions for sa guard tests ([#1800](https://github.com/datadog/libdatadog/issues/1800)) - ([e8f9d68](https://github.com/datadog/libdatadog/commit/e8f9d68cc5cc588c0f7d5e43cea9482e559b394a))
- Increase test_waitall_nohang timeout to 500ms ([#2097](https://github.com/datadog/libdatadog/issues/2097)) - ([382a087](https://github.com/datadog/libdatadog/commit/382a08732c4f0061c55f890830b5206afc3e929f))
- Support socket based receiver for all thread collection ([#2080](https://github.com/datadog/libdatadog/issues/2080)) - ([a97e1d4](https://github.com/datadog/libdatadog/commit/a97e1d4bb5d19dbb85921ea5d7efd255b547d471))
- Set failed thread stack collection as incomplete empty stack ([#2079](https://github.com/datadog/libdatadog/issues/2079)) - ([1612ee9](https://github.com/datadog/libdatadog/commit/1612ee9b22ff255f6543ae44eea32699d16915d3))
- Move preload logger marking after recursive guard ([#2023](https://github.com/datadog/libdatadog/issues/2023)) - ([118d260](https://github.com/datadog/libdatadog/commit/118d26070b93c24260598fbe69feb1b6c81db19f))
- Fix bin_tests in gitlab ([#1832](https://github.com/datadog/libdatadog/issues/1832)) - ([84b9b32](https://github.com/datadog/libdatadog/commit/84b9b32ddb57b0dabe5fe38b94b361248de7484d))
- Check fields and exclude uuid for `has_data` ([#2322](https://github.com/datadog/libdatadog/issues/2322)) - ([1674a4a](https://github.com/datadog/libdatadog/commit/1674a4aa45d0d2a6d41d49da3d5d39dad589d3e4))
- Sanitize type and message for unhandled exceptions ([#2148](https://github.com/datadog/libdatadog/issues/2148)) - ([80407bf](https://github.com/datadog/libdatadog/commit/80407bf7d1d4f2ea4b19605a0e3a0f747f40368b))
- Multi thread collection centos flakes harden ([#2113](https://github.com/datadog/libdatadog/issues/2113)) - ([1ce0b30](https://github.com/datadog/libdatadog/commit/1ce0b30d55a476871bc0e2cdc054f1db34850b0d))
- Authenticate peer granted socket ptrace access ([#2098](https://github.com/datadog/libdatadog/issues/2098)) - ([e15985f](https://github.com/datadog/libdatadog/commit/e15985f3bbcf25c29a29e4527c266b1b10942c0a))
- Flatten all threads object into a list of `ThreadData` ([#2054](https://github.com/datadog/libdatadog/issues/2054)) - ([2a659a6](https://github.com/datadog/libdatadog/commit/2a659a6eb7171413105412330db991d4753ce9d1))
- Handle new lines in client submitted exception message ([#1836](https://github.com/datadog/libdatadog/issues/1836)) - ([02d95c0](https://github.com/datadog/libdatadog/commit/02d95c0e99722328d7601d92263cc125e2beb487))
- Fix SIGCHLD signal guarding while in CT signal handler ([#1807](https://github.com/datadog/libdatadog/issues/1807)) - ([9e2b7b9](https://github.com/datadog/libdatadog/commit/9e2b7b9f46d83424adc991dcdd20b1cdd8b37c0b))
- Guard sigchld and sigpipe during crashtracker signal handler execution ([#1771](https://github.com/datadog/libdatadog/issues/1771)) - ([adaeb4e](https://github.com/datadog/libdatadog/commit/adaeb4eca43bd5f85bf9975c8d2dc841af676a42))
- Use libunwind to unwind frames ([#1663](https://github.com/datadog/libdatadog/issues/1663)) - ([de888e2](https://github.com/datadog/libdatadog/commit/de888e2a7b41df44b141b041e595f80b02601f3d))
- Add process_tags to application field ([#1576](https://github.com/datadog/libdatadog/issues/1576)) - ([a0cef26](https://github.com/datadog/libdatadog/commit/a0cef26b0240f19dd994d471d5679e8c426adfc8))
- Restore previous Cargo.toml version ([#1993](https://github.com/datadog/libdatadog/issues/1993)) - ([500c147](https://github.com/datadog/libdatadog/commit/500c147ec07e9c768abdfaec074a84ab88885e2a))
- Don't double-encode file:// telemetry endpoints ([#2230](https://github.com/datadog/libdatadog/issues/2230)) - ([20c267e](https://github.com/datadog/libdatadog/commit/20c267e23caac58839a9ebe660601ee381117d22))
- AWS lambda also can return EACCESS for shm_open ([#1446](https://github.com/datadog/libdatadog/issues/1446)) - ([c65d768](https://github.com/datadog/libdatadog/commit/c65d7680109c92f49195b9a9314c9c301fc29f32))
- Fix logs payload format [APMSP-2590] ([#1498](https://github.com/datadog/libdatadog/issues/1498)) - ([b44bb77](https://github.com/datadog/libdatadog/commit/b44bb77dc7e7dcfd8e47d9e8c2bbe1d3cfa894f6))
- New clippy lints ([#2219](https://github.com/datadog/libdatadog/issues/2219)) - ([e026a3c](https://github.com/datadog/libdatadog/commit/e026a3c76cfdd1959e4e1e30b7d234eeffe830c6))

## 1.0.0 - 2025-11-28

Initial release.
