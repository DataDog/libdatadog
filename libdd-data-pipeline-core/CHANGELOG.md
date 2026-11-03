# Changelog



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-core-v3.0.0..libdd-data-pipeline-core-v4.0.0) - 2026-10-23

### Added

- Add basic no_std to libdd-common ([#2638](https://github.com/datadog/libdatadog/issues/2638)) - ([d0dd0a2](https://github.com/datadog/libdatadog/commit/d0dd0a21ffca664d7c6a0798dd8c178234fd6750))
- Add v1-native span obfuscation ([#2565](https://github.com/datadog/libdatadog/issues/2565)) - ([75d4447](https://github.com/datadog/libdatadog/commit/75d4447eb19cac5b1f0337fccf85f34d08af6c36))
- Wire v1 payload to the trace exporter and add v1 pooling ([#2558](https://github.com/datadog/libdatadog/issues/2558)) - ([1967b81](https://github.com/datadog/libdatadog/commit/1967b8144736556a245d4f4b55eba41372752f45))

### Fixed

- Reject unfollowed redirects ([#2651](https://github.com/datadog/libdatadog/issues/2651)) - ([bbe81f7](https://github.com/datadog/libdatadog/commit/bbe81f75ae8dc58c607d6d5b1af1c3649d372d26))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-core-v2.0.0..libdd-data-pipeline-core-v3.0.0) - 2026-10-02

### Added

- Add mutable metadata ([#2545](https://github.com/datadog/libdatadog/issues/2545)) - ([823002e](https://github.com/datadog/libdatadog/commit/823002e431dff914aa7332d2647ec73d0537f9f3))

### Changed

- Update workspace to Rust 2024 edition ([#2575](https://github.com/datadog/libdatadog/issues/2575)) - ([620a212](https://github.com/datadog/libdatadog/commit/620a212fc91e128234f48b29d657c71d6ad5caed))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-core-v1.0.0..libdd-data-pipeline-core-v2.0.0) - 2026-09-24

### Added

- Generate agentless trace stats ([#2488](https://github.com/datadog/libdatadog/issues/2488)) - ([7d014fe](https://github.com/datadog/libdatadog/commit/7d014fe1695bc122925d6403e758c558059bb8c7))

### Changed

- Avoid parsing static entity header names ([#2457](https://github.com/datadog/libdatadog/issues/2457)) - ([3f833f8](https://github.com/datadog/libdatadog/commit/3f833f8b37ff2c40e35d88b8f7460f94462354de))
- Use pooled spans on the send path ([#2382](https://github.com/datadog/libdatadog/issues/2382)) - ([286813f](https://github.com/datadog/libdatadog/commit/286813fc377c1f7a8ff0fe2739ac3648d6f5ffa3))
- Apply small timeout pooling strategy to libdd-http-client as well ([#2449](https://github.com/datadog/libdatadog/issues/2449)) - ([16e10db](https://github.com/datadog/libdatadog/commit/16e10db927db4adcdf386cd09535e100bdf4f587))
- Move all remaining external deps to workspace-level dependencies ([#2476](https://github.com/datadog/libdatadog/issues/2476)) - ([ea4379c](https://github.com/datadog/libdatadog/commit/ea4379c9ca0dd024c7a3ed5c497fa8e117018d6a))


## 1.0.0 - 2026-09-08

Initial release.
