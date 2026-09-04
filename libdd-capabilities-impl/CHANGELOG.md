# Changelog



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-capabilities-impl-v4.0.0..libdd-capabilities-impl-v5.0.0) - 2026-09-04

### Added

- Add runtime-independent agentless sending ([#2389](https://github.com/datadog/libdatadog/issues/2389)) - ([4cfd390](https://github.com/datadog/libdatadog/commit/4cfd390c26275689456a8bd7db26d37601d232d7))
- Do not entirely disable connection pooling for periodic connections ([#2440](https://github.com/datadog/libdatadog/issues/2440)) - ([a4df07e](https://github.com/datadog/libdatadog/commit/a4df07ed442e88e70d9e6248c79a1ab1319d2182))

### Changed

- Migrate HTTP & networking deps to workspace level (phase 4bis) ([#2350](https://github.com/datadog/libdatadog/issues/2350)) - ([55cdf67](https://github.com/datadog/libdatadog/commit/55cdf67b720b7df427b1febd147b0f72d486167f))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-capabilities-impl-v3.0.0..libdd-capabilities-impl-v4.0.0) - 2026-08-07

### Added

- Add streaming to http capabilities ([#2251](https://github.com/datadog/libdatadog/issues/2251)) - ([ef95bde](https://github.com/datadog/libdatadog/commit/ef95bdef216807c2e493c2fc2f9dd27a56fc255c))
- Add Installation signature and AppProduct changes payloads ([#2213](https://github.com/datadog/libdatadog/issues/2213)) - ([f3d3d80](https://github.com/datadog/libdatadog/commit/f3d3d80b807b82d2a49d57df99a0eb02a800a978))
- Make telemetry worker wasm-compatible for the TraceExporter ([#2172](https://github.com/datadog/libdatadog/issues/2172)) - ([73f23a2](https://github.com/datadog/libdatadog/commit/73f23a2c03be39971c966e444785d63b6fd52e81))
- Added file capability [APMSP-3780] ([#2240](https://github.com/datadog/libdatadog/issues/2240)) - ([3081603](https://github.com/datadog/libdatadog/commit/3081603d3c74f209be4e3be951f78a1a7469397f))
- Added environment capability [APMSP-3780] ([#2239](https://github.com/datadog/libdatadog/issues/2239)) - ([0c6e2a5](https://github.com/datadog/libdatadog/commit/0c6e2a5df2a163d34c4f385353ffc5d7257c72f4))

### Changed

- Migrate to workspace dependencies, phase 4 ([#2296](https://github.com/datadog/libdatadog/issues/2296)) - ([3c4c095](https://github.com/datadog/libdatadog/commit/3c4c0952c016b3b156d8a82ec27eeb515079d286))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))

### Fixed

- Expose HttpClientCapability in remote config ([#2252](https://github.com/datadog/libdatadog/issues/2252)) - ([43156bb](https://github.com/datadog/libdatadog/commit/43156bbe53c026fdeeaeb3777cb9d4054507a250))
- Stop sending Connection: close to the Agent ([#2286](https://github.com/datadog/libdatadog/issues/2286)) - ([8dfe721](https://github.com/datadog/libdatadog/commit/8dfe721a5b24fe225ed4cab66b2e07cb0f2ff6dd))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-capabilities-impl-v2.0.0..libdd-capabilities-impl-v3.0.0) - 2026-07-07

### Added

- Add stdout log trace exporter ([#2074](https://github.com/datadog/libdatadog/issues/2074)) - ([c2751ef](https://github.com/datadog/libdatadog/commit/c2751eff7036159127ec52c69130eebf7d9a5a97))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-capabilities-impl-v1.0.0..libdd-capabilities-impl-v2.0.0) - 2026-05-15

### Added

- Sleep & spawn capabilities ([#1873](https://github.com/datadog/libdatadog/issues/1873)) - ([b419f6e](https://github.com/datadog/libdatadog/commit/b419f6e1edb7679c750a65713893c68fc697404c))


## 1.0.0 - 2026-04-27

Initial release.
