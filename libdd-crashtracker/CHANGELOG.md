# Changelog



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-crashtracker-v1.0.0..libdd-crashtracker-v2.0.0) - 2026-03-17

### Added

- Report unhandled exceptions ([#1596](https://github.com/datadog/libdatadog/issues/1596)) - ([eb48c1a](https://github.com/datadog/libdatadog/commit/eb48c1a8c6b1f115e0cb1f357ca300e46c089e25))
- Include `Kind` in crash ping and clarify requirements ([#1595](https://github.com/datadog/libdatadog/issues/1595)) - ([27de9f3](https://github.com/datadog/libdatadog/commit/27de9f37d5ece4e0d737703efc879b88f7040540))
- Emit crashing thread name in crash report for linux crashes ([#1485](https://github.com/datadog/libdatadog/issues/1485)) - ([c9d6835](https://github.com/datadog/libdatadog/commit/c9d68358e2acab9461c2b6403f5e2426b823b756))

### Changed

- Bump to 29.0.0 ([#1702](https://github.com/datadog/libdatadog/issues/1702)) - ([001bd56](https://github.com/datadog/libdatadog/commit/001bd56fcbba34fa4ec3f9798a6c4fbcddeffa40))
- Give libdd-libunwind-sys its own version ([#1743](https://github.com/datadog/libdatadog/issues/1743)) - ([bb2b2bb](https://github.com/datadog/libdatadog/commit/bb2b2bb83decae7b71066c84c950caddd7f99dd2))
- Fix crashtracker receiver binary rpath setting ([#1652](https://github.com/datadog/libdatadog/issues/1652)) - ([b13e787](https://github.com/datadog/libdatadog/commit/b13e787309bad5636bbb64f56437a3cd8999af60))
- Use default-features=false for aws-lc-sys ([#1625](https://github.com/datadog/libdatadog/issues/1625)) - ([5bb62b1](https://github.com/datadog/libdatadog/commit/5bb62b1aecfb67ed22d14e834989aa182d58752a))
- Add tag for target triple ([#1741](https://github.com/datadog/libdatadog/issues/1741)) - ([6a02f01](https://github.com/datadog/libdatadog/commit/6a02f0142a29d349b4f4ea53ef9d70949cf44e5d))
- Emit a best effort stacktrace for Mac ([#1645](https://github.com/datadog/libdatadog/issues/1645)) - ([f79e281](https://github.com/datadog/libdatadog/commit/f79e281ce8ec941603d3faec3f9a3d65d9d7fba0))
- Bump os_info crate to 3.14 ([#1507](https://github.com/datadog/libdatadog/issues/1507)) - ([aa61ebb](https://github.com/datadog/libdatadog/commit/aa61ebb81846ad737e6c38409fa4a425bb2af86e))
- Add minimal LD preload test for crashtracker collector ([#1428](https://github.com/datadog/libdatadog/issues/1428)) - ([488418a](https://github.com/datadog/libdatadog/commit/488418af8be2a817f7df40e7b199eced836bcaab))
- Add `is_crash_debug` tag to crashtracker receiver debug logs ([#1445](https://github.com/datadog/libdatadog/issues/1445)) - ([efe99d5](https://github.com/datadog/libdatadog/commit/efe99d5e2992ab029e6ad58c3a77b0f615447b95))
- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Avoid leaking Endpoint through the public API ([#1705](https://github.com/datadog/libdatadog/issues/1705)) - ([892b7bf](https://github.com/datadog/libdatadog/commit/892b7bf3f873905a9cfca1f2b4649154830be3bc))
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

- Use libunwind to unwind frames ([#1663](https://github.com/datadog/libdatadog/issues/1663)) - ([de888e2](https://github.com/datadog/libdatadog/commit/de888e2a7b41df44b141b041e595f80b02601f3d))
- Add process_tags to application field ([#1576](https://github.com/datadog/libdatadog/issues/1576)) - ([a0cef26](https://github.com/datadog/libdatadog/commit/a0cef26b0240f19dd994d471d5679e8c426adfc8))
- AWS lambda also can return EACCESS for shm_open ([#1446](https://github.com/datadog/libdatadog/issues/1446)) - ([c65d768](https://github.com/datadog/libdatadog/commit/c65d7680109c92f49195b9a9314c9c301fc29f32))
- Fix logs payload format [APMSP-2590] ([#1498](https://github.com/datadog/libdatadog/issues/1498)) - ([b44bb77](https://github.com/datadog/libdatadog/commit/b44bb77dc7e7dcfd8e47d9e8c2bbe1d3cfa894f6))

## 1.0.0 - 2025-11-28

Initial release.
