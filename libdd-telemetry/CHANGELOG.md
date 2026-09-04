# Changelog



## [8.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v7.0.0..libdd-telemetry-v8.0.0) - 2026-09-04

### Changed

- Avoid doing two separate http requests in stop telemetry ([#2435](https://github.com/datadog/libdatadog/issues/2435)) - ([c5f3b82](https://github.com/datadog/libdatadog/commit/c5f3b821220bd65d7fd6046446d8848cc972e6c4))
- Migrate HTTP & networking deps to workspace level (phase 4bis) ([#2350](https://github.com/datadog/libdatadog/issues/2350)) - ([55cdf67](https://github.com/datadog/libdatadog/commit/55cdf67b720b7df427b1febd147b0f72d486167f))



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v6.0.0..libdd-telemetry-v7.0.0) - 2026-08-17

### Added

- Add Installation signature and AppProduct changes payloads ([#2213](https://github.com/datadog/libdatadog/issues/2213)) - ([f3d3d80](https://github.com/datadog/libdatadog/commit/f3d3d80b807b82d2a49d57df99a0eb02a800a978))
- Make telemetry worker wasm-compatible for the TraceExporter ([#2172](https://github.com/datadog/libdatadog/issues/2172)) - ([73f23a2](https://github.com/datadog/libdatadog/commit/73f23a2c03be39971c966e444785d63b6fd52e81))

### Changed

- Make conversion from RemoteConfigProduct back and forth generally available ([#2325](https://github.com/datadog/libdatadog/issues/2325)) - ([ea75b04](https://github.com/datadog/libdatadog/commit/ea75b04c3547037937730a14cf8a72a5ebf702d7))
- Migrate to workspace dependencies, phase 3 ([#2283](https://github.com/datadog/libdatadog/issues/2283)) - ([f73e8ae](https://github.com/datadog/libdatadog/commit/f73e8ae5997d54860984ad8e155fa9fa257d9263))
- Moving to workspace-level dependencies, phase 2 ([#2270](https://github.com/datadog/libdatadog/issues/2270)) - ([caa732f](https://github.com/datadog/libdatadog/commit/caa732f3fe7c82a347813ba36686e039d29981a3))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))

### Fixed

- Don't double-encode file:// telemetry endpoints ([#2230](https://github.com/datadog/libdatadog/issues/2230)) - ([20c267e](https://github.com/datadog/libdatadog/commit/20c267e23caac58839a9ebe660601ee381117d22))
- Stop sending Connection: close to the Agent ([#2286](https://github.com/datadog/libdatadog/issues/2286)) - ([8dfe721](https://github.com/datadog/libdatadog/commit/8dfe721a5b24fe225ed4cab66b2e07cb0f2ff6dd))
- Drain the mailbox before stopping ([#2258](https://github.com/datadog/libdatadog/issues/2258)) - ([0c78ffc](https://github.com/datadog/libdatadog/commit/0c78ffcfab27d9a3ce5197b6cf56dfa017d15b72))



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v5.0.1..libdd-telemetry-v6.0.0) - 2026-07-07

### Added

- SharedRuntime Borrowed & Owned mode ([#2061](https://github.com/datadog/libdatadog/issues/2061)) - ([4b79b7e](https://github.com/datadog/libdatadog/commit/4b79b7ed87113bea01db583d54e13fb0c2a19e74))

### Changed

- Avoid leaking libdd-common types in the public API ([#2152](https://github.com/datadog/libdatadog/issues/2152)) - ([b3144c6](https://github.com/datadog/libdatadog/commit/b3144c676b73e157f9d563903c01df016882e8c4))
- Skip slow miri tests ([#2188](https://github.com/datadog/libdatadog/issues/2188)) - ([4b66bd6](https://github.com/datadog/libdatadog/commit/4b66bd62c4d39184c68a58d576d7955f1fb51aaa))



## [5.0.1](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v5.0.0..libdd-telemetry-v5.0.1) - 2026-06-08

### Fixed

- Serialize Method::Other as "*" per OpenAPI spec ([#1998](https://github.com/datadog/libdatadog/issues/1998)) - ([b2ef19f](https://github.com/datadog/libdatadog/commit/b2ef19f622350443fca3c23811fabb7b898a933e))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v4.0.0..libdd-telemetry-v5.0.0) - 2026-05-15

### Added

- Trait architecture http ([#1555](https://github.com/datadog/libdatadog/issues/1555)) - ([b863364](https://github.com/datadog/libdatadog/commit/b863364bbb9cb4567b10c80cd11bc4a22b49fcf4))
- Sleep & spawn capabilities ([#1873](https://github.com/datadog/libdatadog/issues/1873)) - ([b419f6e](https://github.com/datadog/libdatadog/commit/b419f6e1edb7679c750a65713893c68fc697404c))
- Add shared runtime ([#1602](https://github.com/datadog/libdatadog/issues/1602)) - ([33896de](https://github.com/datadog/libdatadog/commit/33896def2418a9c0fc5bf74b05011210d333759f))
- Wire telemetry_extended_heartbeat_interval through SessionConfig ([#1882](https://github.com/datadog/libdatadog/issues/1882)) - ([a41b623](https://github.com/datadog/libdatadog/commit/a41b623f09bf41909fa394e78b3c316da27239c0))
- Include dependencies and integrations in app-extended-heartbeat ([#1962](https://github.com/datadog/libdatadog/issues/1962)) - ([91fd13c](https://github.com/datadog/libdatadog/commit/91fd13c8a0ca5335fe39940f8764cd825bbef7e8))

### Changed

- Downgrade version so publish workflow succeeds ([#1870](https://github.com/datadog/libdatadog/issues/1870)) - ([730c122](https://github.com/datadog/libdatadog/commit/730c1221f9f73ecadcdcc90681f54730fe8e92f2))
- Batch ack sending & consumption ([#1835](https://github.com/datadog/libdatadog/issues/1835)) - ([eff9d8a](https://github.com/datadog/libdatadog/commit/eff9d8a4421aa727fad6ce874f5c0f02820b3e6d))
- Add session id support ([#1817](https://github.com/datadog/libdatadog/issues/1817)) - ([802f06a](https://github.com/datadog/libdatadog/commit/802f06a842848ba81b4fed9587a0ba7904cb7830))
- Use weaker mem ordering for SEQ_ID ([#1749](https://github.com/datadog/libdatadog/issues/1749)) - ([8d2029d](https://github.com/datadog/libdatadog/commit/8d2029d2fad5129fc36a7b3b68d3148d68b48b79))

### Fixed

- Gate libdd-common TLS features in remaining internal crates + add CI guard ([#1943](https://github.com/datadog/libdatadog/issues/1943)) - ([db05e1f](https://github.com/datadog/libdatadog/commit/db05e1f8408a76075efb37ecec544d2e74217e57))
- Restore previous Cargo.toml version ([#1993](https://github.com/datadog/libdatadog/issues/1993)) - ([500c147](https://github.com/datadog/libdatadog/commit/500c147ec07e9c768abdfaec074a84ab88885e2a))
- Avoid trigger loop in telemetry worker ([#1950](https://github.com/datadog/libdatadog/issues/1950)) - ([7a24f53](https://github.com/datadog/libdatadog/commit/7a24f534a46367bc2b2007994dd3a3d2d62ad663))
- Schedule ExtendedHeartbeat on worker start ([#1910](https://github.com/datadog/libdatadog/issues/1910)) - ([650c804](https://github.com/datadog/libdatadog/commit/650c804b170f9bb47ace9a0e8e672851f818b5d7))
- Skip sending empty payloads ([#1894](https://github.com/datadog/libdatadog/issues/1894)) - ([ca7a74b](https://github.com/datadog/libdatadog/commit/ca7a74be123f34b5ac6982705c8f3abef4ed2977))
- Wire up DD_TELEMETRY_EXTENDED_HEARTBEAT_INTERVAL to scheduler ([#1824](https://github.com/datadog/libdatadog/issues/1824)) - ([f1f0df1](https://github.com/datadog/libdatadog/commit/f1f0df1b5d9066a7fbff14524c04ca3636d778d6))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v3.1.0..libdd-telemetry-v4.0.0) - 2026-03-25

### Changed
- Fix previous version.



## [3.1.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v3.0.0..libdd-telemetry-v3.1.0) - 2026-03-23

### Changed

- Refactor tarpc away ([#1742](https://github.com/datadog/libdatadog/issues/1742)) - ([c722b20](https://github.com/datadog/libdatadog/commit/c722b209ece89f245da4d5c1f35e01914b27f315))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v2.0.0..libdd-telemetry-v3.0.0) - 2026-02-23

### Added

- Add endpoints collection ([#1182](https://github.com/datadog/libdatadog/issues/1182)) - ([44cabf1](https://github.com/datadog/libdatadog/commit/44cabf193fd0bde789b53be2a91bcce7ebce3fe7))
- Add process_tags to Application in telemetry ([#1459](https://github.com/datadog/libdatadog/issues/1459)) - ([b09abfb](https://github.com/datadog/libdatadog/commit/b09abfb6ad12f139899e445b7034a6fdb85e3314))

### Changed

- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))

### Fixed

- Fix logs payload format [APMSP-2590] ([#1498](https://github.com/datadog/libdatadog/issues/1498)) - ([b44bb77](https://github.com/datadog/libdatadog/commit/b44bb77dc7e7dcfd8e47d9e8c2bbe1d3cfa894f6))


## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-telemetry-v1.0.0..libdd-telemetry-v2.0.0) - 2026-01-20

### Added

- *(config_visibility)* [APMAPI-1693] Telemetry for enhanced config reporting ([#1385](https://github.com/datadog/libdatadog/issues/1385)) - ([435107c](https://github.com/datadog/libdatadog/commit/435107c245112397914935c0f7148a18b91cafc6))

### Changed

- *(telemetry)* Flush metrics with heartbeats if the interval is small ([#1418](https://github.com/datadog/libdatadog/issues/1418)) - ([40a1ad6](https://github.com/datadog/libdatadog/commit/40a1ad6bc8fe903b67af0c95ce530fd7efe28329))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))
- Support cxx bindings for crashinfo ([#1379](https://github.com/datadog/libdatadog/issues/1379)) - ([6b26318](https://github.com/datadog/libdatadog/commit/6b263189044f48cec6a67745036bd027b44f6daa))

## 1.0.0 - 2025-11-17

Initial release.
