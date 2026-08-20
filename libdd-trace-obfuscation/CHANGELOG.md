# Changelog



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v6.0.0..libdd-trace-obfuscation-v7.0.0) - 2026-08-20

### Changed

- Bump `libdd-trace-protobuf` to a new major version (`^4.0.1` → `^5.0.0`)

## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v5.0.0..libdd-trace-obfuscation-v6.0.0) - 2026-08-17

### Changed

- Update nightly ([#2281](https://github.com/datadog/libdatadog/issues/2281)) - ([ddef629](https://github.com/datadog/libdatadog/commit/ddef629242c5288137987a1babfee0c588fe2533))
- Migrate to workspace dependencies, phase 4 ([#2296](https://github.com/datadog/libdatadog/issues/2296)) - ([3c4c095](https://github.com/datadog/libdatadog/commit/3c4c0952c016b3b156d8a82ec27eeb515079d286))
- Moving to workspace-level dependencies, phase 2 ([#2270](https://github.com/datadog/libdatadog/issues/2270)) - ([caa732f](https://github.com/datadog/libdatadog/commit/caa732f3fe7c82a347813ba36686e039d29981a3))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))

### Fixed

- New clippy lints ([#2219](https://github.com/datadog/libdatadog/issues/2219)) - ([e026a3c](https://github.com/datadog/libdatadog/commit/e026a3c76cfdd1959e4e1e30b7d234eeffe830c6))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v4.0.0..libdd-trace-obfuscation-v5.0.0) - 2026-07-07

### Changed

- Skip slow miri tests ([#2188](https://github.com/datadog/libdatadog/issues/2188)) - ([4b66bd6](https://github.com/datadog/libdatadog/commit/4b66bd62c4d39184c68a58d576d7955f1fb51aaa))

### Fixed

- Update anyhow for unsoundness ([#2186](https://github.com/datadog/libdatadog/issues/2186)) - ([f8b9cc1](https://github.com/datadog/libdatadog/commit/f8b9cc1d8db5cf69a070588fa6b728a75842653a))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v3.1.0..libdd-trace-obfuscation-v4.0.0) - 2026-06-08

### Changed

- Bump msrv to 1.87.0 ([#2017](https://github.com/datadog/libdatadog/issues/2017)) - ([276039d](https://github.com/datadog/libdatadog/commit/276039da8897a8e9e83ed3162912792f2241c5d7))



## [3.1.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v3.0.0..libdd-trace-obfuscation-v3.1.0) - 2026-05-22

### Fixed

- Cargo clippy fix with all lints ([#1947](https://github.com/datadog/libdatadog/issues/1947)) - ([ec55449](https://github.com/datadog/libdatadog/commit/ec55449ab4fad3fb6b224ff9d4235f42cfa3cc28))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v2.0.0..libdd-trace-obfuscation-v3.0.0) - 2026-05-18

### Added

- Feature parity on span obfuscation [APMSP-2671] ([#1788](https://github.com/datadog/libdatadog/issues/1788)) - ([102231d](https://github.com/datadog/libdatadog/commit/102231d7f0f35a4e57b18452f0dfbf5d3d97517d))
- Feature parity on sql obfuscation [APMSP-2667] ([#1708](https://github.com/datadog/libdatadog/issues/1708)) - ([c664ed7](https://github.com/datadog/libdatadog/commit/c664ed7c8230c4249151e0a133f1988ccbfb454f))
- Integrate obfuscation to the stats exporter [APMSP-2764] ([#1819](https://github.com/datadog/libdatadog/issues/1819)) - ([540f186](https://github.com/datadog/libdatadog/commit/540f18646d58bd18984990fbed85254b3678ac7f))
- Added regex-lite feature ([#1939](https://github.com/datadog/libdatadog/issues/1939)) - ([58b86d5](https://github.com/datadog/libdatadog/commit/58b86d5a1b2dc43be98eb9568ec734c259a430a7))

### Changed

- Clippy ([#1889](https://github.com/datadog/libdatadog/issues/1889)) - ([2d2bc4f](https://github.com/datadog/libdatadog/commit/2d2bc4fb7de47792b907b6cf279ea7c39604456a))

### Fixed

- Gate libdd-common TLS features in obfuscation and capabilities-impl ([#1872](https://github.com/datadog/libdatadog/issues/1872)) - ([986aab5](https://github.com/datadog/libdatadog/commit/986aab55cb7941d8453dffb59d35a70599d08665))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v1.0.1..libdd-trace-obfuscation-v2.0.0) - 2026-03-25

### Changed
- Fix previous version.


## [1.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-obfuscation-v1.0.0..libdd-trace-obfuscation-v1.0.1) - 2026-03-16

### Changed

- Update dependencies ([#1734](https://github.com/DataDog/libdatadog/issues/1734)) - ([38dd71b](https://github.com/DataDog/libdatadog/commit/38dd71bd6fdac45ecab3d74ce1b4a827abae794a))


## 1.0.0 - 2025-12-09

Initial release.
