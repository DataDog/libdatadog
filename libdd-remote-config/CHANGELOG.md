# Changelog



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-remote-config-v4.0.0..libdd-remote-config-v5.0.0) - 2026-09-04

### Changed

- Migrate HTTP & networking deps to workspace level (phase 4bis) ([#2350](https://github.com/datadog/libdatadog/issues/2350)) - ([55cdf67](https://github.com/datadog/libdatadog/commit/55cdf67b720b7df427b1febd147b0f72d486167f))

### Fixed

- Reuse injected sleep capability ([#2429](https://github.com/datadog/libdatadog/issues/2429)) - ([ad3606f](https://github.com/datadog/libdatadog/commit/ad3606f700b77cc19387b3cfc14f0751b5689f9b))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-remote-config-v3.0.0..libdd-remote-config-v4.0.0) - 2026-08-25

### Added

- Vendor rust-tuf crate in libdatadog for release ([#2365](https://github.com/datadog/libdatadog/issues/2365)) - ([9ea9268](https://github.com/datadog/libdatadog/commit/9ea926846c4795aa6730e8a1f06c205db7d295aa))
- Add overrides for RC config/director roots ([#2404](https://github.com/datadog/libdatadog/issues/2404)) - ([966f921](https://github.com/datadog/libdatadog/commit/966f921c226b63a1ba84c2e53e3d0f4625f90c75))
- Agentless RC fetcher ([#2112](https://github.com/datadog/libdatadog/issues/2112)) - ([20a3f4d](https://github.com/datadog/libdatadog/commit/20a3f4d67bba02004554be4c4e32a27ebe574fa7))
- Revert vendoring of tuf-rust ([#2365](https://github.com/datadog/libdatadog/issues/2365)), use new crates.io package ([#2374](https://github.com/datadog/libdatadog/issues/2374)) - ([b56b21c](https://github.com/datadog/libdatadog/commit/b56b21c490ef11bc733c6fe7dc3da838ef5b1d28))

### Changed

- Add DEBUG product ([#2306](https://github.com/datadog/libdatadog/issues/2306)) - ([7be7ac7](https://github.com/datadog/libdatadog/commit/7be7ac7f383deea0af32bce895cfa1b44e6c8b94))

### Fixed

- Avoid depending on the crypto nodejs API for remote config ([#2407](https://github.com/datadog/libdatadog/issues/2407)) - ([15453cd](https://github.com/datadog/libdatadog/commit/15453cdf0c42417d21118f5e762d5c69f0fde4f5))
- Fixup rc to be wasm compatible ([#2393](https://github.com/datadog/libdatadog/issues/2393)) - ([542a34e](https://github.com/datadog/libdatadog/commit/542a34effabf23ac9ac1a9a81793f1f0a604cfd9))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-remote-config-v2.0.0..libdd-remote-config-v3.0.0) - 2026-08-07

### Added

- Handle expired config status ([#2274](https://github.com/datadog/libdatadog/issues/2274)) - ([4a0e5bc](https://github.com/datadog/libdatadog/commit/4a0e5bc70cb6c011936eb46648839a2f469f1c66))
- Add AsmRawResponseBody capability ([#2278](https://github.com/datadog/libdatadog/issues/2278)) - ([95610de](https://github.com/datadog/libdatadog/commit/95610de06a776b8d645fe77ad8b8e1848ecd53b7))

### Changed

- Make conversion from RemoteConfigProduct back and forth generally available ([#2325](https://github.com/datadog/libdatadog/issues/2325)) - ([ea75b04](https://github.com/datadog/libdatadog/commit/ea75b04c3547037937730a14cf8a72a5ebf702d7))
- Migrate to workspace dependencies, phase 3 ([#2283](https://github.com/datadog/libdatadog/issues/2283)) - ([f73e8ae](https://github.com/datadog/libdatadog/commit/f73e8ae5997d54860984ad8e155fa9fa257d9263))
- Moving to workspace-level dependencies, phase 2 ([#2270](https://github.com/datadog/libdatadog/issues/2270)) - ([caa732f](https://github.com/datadog/libdatadog/commit/caa732f3fe7c82a347813ba36686e039d29981a3))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))

### Fixed

- Make Target fields available again after eaf5ad06 ([#2232](https://github.com/datadog/libdatadog/issues/2232)) - ([85ce322](https://github.com/datadog/libdatadog/commit/85ce322a1dcb1eda7df9bcc021223b2d1a236783))
- Expose HttpClientCapability in remote config ([#2252](https://github.com/datadog/libdatadog/issues/2252)) - ([43156bb](https://github.com/datadog/libdatadog/commit/43156bbe53c026fdeeaeb3777cb9d4054507a250))
- Finish the WASM port of remote-config ([#2315](https://github.com/datadog/libdatadog/issues/2315)) - ([44f705f](https://github.com/datadog/libdatadog/commit/44f705ff1c228e56148710be2520958500001487))
- New clippy lints ([#2219](https://github.com/datadog/libdatadog/issues/2219)) - ([e026a3c](https://github.com/datadog/libdatadog/commit/e026a3c76cfdd1959e4e1e30b7d234eeffe830c6))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-remote-config-v1.0.0..libdd-remote-config-v2.0.0) - 2026-07-07

### Added

- Use the proto file from the agent ([#2165](https://github.com/datadog/libdatadog/issues/2165)) - ([3ff0006](https://github.com/datadog/libdatadog/commit/3ff0006718c3e4fea7e0ed1ae7c8a4cacf0268ff))

### Changed

- Hide Target inner properties so they are not leaked ([#2182](https://github.com/datadog/libdatadog/issues/2182)) - ([eaf5ad0](https://github.com/datadog/libdatadog/commit/eaf5ad066b2cb73438e99cea81a854489534d067))
- Reexport Endpoint and Tag common types ([#2147](https://github.com/datadog/libdatadog/issues/2147)) - ([9697e87](https://github.com/datadog/libdatadog/commit/9697e87527b67e489333a94b799ca5e22c376a67))


## 1.0.0 - 2026-06-19

Initial release.
