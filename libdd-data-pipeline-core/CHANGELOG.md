# Changelog



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
