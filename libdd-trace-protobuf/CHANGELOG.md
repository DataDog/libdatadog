# Changelog



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v3.0.2..libdd-trace-protobuf-v4.0.0) - 2026-07-07

### Added

- OTLP HTTP/protobuf trace export ([#2115](https://github.com/datadog/libdatadog/issues/2115)) - ([4e8e6cc](https://github.com/datadog/libdatadog/commit/4e8e6cc8c0fe083089cc8e57f0fd26667f29941c))
- Use the proto file from the agent ([#2165](https://github.com/datadog/libdatadog/issues/2165)) - ([3ff0006](https://github.com/datadog/libdatadog/commit/3ff0006718c3e4fea7e0ed1ae7c8a4cacf0268ff))
- Add whole key cardinality limit ([#2158](https://github.com/datadog/libdatadog/issues/2158)) - ([a38b630](https://github.com/datadog/libdatadog/commit/a38b6304dcd63c91a52a752f2baa04e7d21e374d))

### Changed

- Update protobufs to be in sync with datadog-agent ([#2180](https://github.com/datadog/libdatadog/issues/2180)) - ([b02d454](https://github.com/datadog/libdatadog/commit/b02d454576034ea56becbd61411ff2f831a89562))



## [3.0.2](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v3.0.1..libdd-trace-protobuf-v3.0.2) - 2026-05-18

### Added

- Feature parity on span obfuscation [APMSP-2671] ([#1788](https://github.com/datadog/libdatadog/issues/1788)) - ([102231d](https://github.com/datadog/libdatadog/commit/102231d7f0f35a4e57b18452f0dfbf5d3d97517d))



## [3.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v3.0.0..libdd-trace-protobuf-v3.0.1) - 2026-03-25

### Changed
- Fix previous version.



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v2.0.0..libdd-trace-protobuf-v3.0.0) - 2026-03-23

### Added

- Add process_tags to remote config Target ([#1586](https://github.com/datadog/libdatadog/issues/1586)) - ([e44af12](https://github.com/datadog/libdatadog/commit/e44af12593051510ca7b4ff3430b8ae668389cc8))

### Fixed

- Rename wrongly cased stats fields ([#1780](https://github.com/datadog/libdatadog/issues/1780)) - ([5ff99ff](https://github.com/datadog/libdatadog/commit/5ff99ff6c465a95a740a494f42cce258c0e80be8))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v1.1.0..libdd-trace-protobuf-v2.0.0) - 2026-03-13

### Added

- Add two fields to ClientGroupedStats [SVLS-8627] ([#1630](https://github.com/datadog/libdatadog/issues/1630)) - ([7e909c0](https://github.com/datadog/libdatadog/commit/7e909c0910a15303eb90fdb3399211a3517d70c8))
- Otel process ctxt protobuf encoding ([#1651](https://github.com/datadog/libdatadog/issues/1651)) - ([412ae10](https://github.com/datadog/libdatadog/commit/412ae10fdacc06e1cbffa8cc2051caad0d02f64f))



## [1.1.0](https://github.com/datadog/libdatadog/compare/libdd-trace-protobuf-v1.0.0..libdd-trace-protobuf-v1.1.0) - 2026-02-23

### Changed

- Remove manual changelog modifications ([#1472](https://github.com/datadog/libdatadog/issues/1472)) - ([d5f1bbf](https://github.com/datadog/libdatadog/commit/d5f1bbfac5850d1b4ecc9052772855fa33587459))
- Update `prost` crates ([#1426](https://github.com/datadog/libdatadog/issues/1426)) - ([14bab86](https://github.com/datadog/libdatadog/commit/14bab865cfab5151fd399c594ab8f67e8bc7dcf1))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))
- Handle null span tag values ([#1394](https://github.com/datadog/libdatadog/issues/1394)) - ([3abff86](https://github.com/datadog/libdatadog/commit/3abff8639a2dfdaf8b81842d6e927f2ee37e895b))

## 1.0.0 - 2025-11-17

Initial release.
