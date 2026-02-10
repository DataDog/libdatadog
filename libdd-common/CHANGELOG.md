# Changelog



## [1.2.0](https://github.com/datadog/libdatadog/compare/libdd-common-v1.1.0..libdd-common-v1.2.0) - 2026-02-10

### Added

- Single source of truth for headers (fixes issue in profiling with missing headers) ([#1493](https://github.com/datadog/libdatadog/issues/1493)) - ([9f2417e](https://github.com/datadog/libdatadog/commit/9f2417e1a472d433eddc2adeeb0c19ec2cb8b53a))

### Changed

- [**breaking**] Make reqwest available in common ([#1504](https://github.com/datadog/libdatadog/issues/1504)) - ([7986270](https://github.com/datadog/libdatadog/commit/7986270b124c313a71ae28ae415201ec3ccd794b))


## [1.1.0](https://github.com/datadog/libdatadog/compare/libdd-common-v1.0.0..libdd-common-v1.1.0) - 2026-01-20

### Added

- *(profiling)* Simpler API for profile exporter ([#1423](https://github.com/datadog/libdatadog/issues/1423)) - ([0d4ebbe](https://github.com/datadog/libdatadog/commit/0d4ebbe55ab841c2af8db41da74597c007375f0e))

### Changed

- *(profiling)* [**breaking**] Use reqwest instead of hyper for exporter ([#1444](https://github.com/datadog/libdatadog/issues/1444)) - ([39c7829](https://github.com/datadog/libdatadog/commit/39c7829592142d8fc8e8988b3631208e2d9ad1cc))
- Don't panic if CryptoProvider already installed  ([#1391](https://github.com/datadog/libdatadog/issues/1391)) - ([2f641ea](https://github.com/datadog/libdatadog/commit/2f641eae3708c34e4adfe62c9d477e665da4f12e))
- Support cxx bindings for crashinfo ([#1379](https://github.com/datadog/libdatadog/issues/1379)) - ([6b26318](https://github.com/datadog/libdatadog/commit/6b263189044f48cec6a67745036bd027b44f6daa))

## 1.0.0 - 2025-11-14

Initial release.
