# Changelog



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v1.0.0..libdd-data-pipeline-v2.0.0) - 2026-02-20

### Added

- Include reason for chunks dropped telemetry ([#1449](https://github.com/datadog/libdatadog/issues/1449)) - ([99be5d7](https://github.com/datadog/libdatadog/commit/99be5d7d6c26940f0197290493b60e8ba603fbb1))
- Introduce TraceData to unify text and binary data ([#1247](https://github.com/datadog/libdatadog/issues/1247)) - ([d430cbd](https://github.com/datadog/libdatadog/commit/d430cbd912d5300d521131392b86fc36a599aa27))

### Changed

- Handle EINTR in test_health_metrics_disabled ([#1430](https://github.com/datadog/libdatadog/issues/1430)) - ([e13f239](https://github.com/datadog/libdatadog/commit/e13f2393185031757f493fcebdfe0e9e435b60e9))
- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Health metrics ([#1433](https://github.com/datadog/libdatadog/issues/1433)) - ([7f30d50](https://github.com/datadog/libdatadog/commit/7f30d50f45be5027b1fc67296d06720f8279efe5))
- Remove Proxy TraceExporter input mode ([#1583](https://github.com/datadog/libdatadog/issues/1583)) - ([2078f6f](https://github.com/datadog/libdatadog/commit/2078f6f051c90ed8e6af2e171d943dc6a117971c))
- Release libddcommon-v1.1.0 ([#1456](https://github.com/datadog/libdatadog/issues/1456)) - ([94cc701](https://github.com/datadog/libdatadog/commit/94cc701e24bbaacfdcc4b034419e72dea1816cc9))
- Prepare libdd-telemetry-v2.0.0 ([#1457](https://github.com/datadog/libdatadog/issues/1457)) - ([753df4f](https://github.com/datadog/libdatadog/commit/753df4f235074cd3420a7e3cd8d2ff9bc964db0d))
- Allow submitting Vec<Vec<Span>> asynchronously ([#1302](https://github.com/datadog/libdatadog/issues/1302)) - ([158b594](https://github.com/datadog/libdatadog/commit/158b59471f1132e3cb36023fa3c46ccb2dd0eda1))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))

## 1.0.0 - 2025-11-18

Initial release.
