# Changelog



## [1.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v1.0.0..libdd-trace-utils-v1.0.1) - 2026-02-17

### Added

- Introduce TraceData to unify text and binary data ([#1247](https://github.com/datadog/libdatadog/issues/1247)) - ([d430cbd](https://github.com/datadog/libdatadog/commit/d430cbd912d5300d521131392b86fc36a599aa27))
- Allow sending trace stats using custom HTTP client ([#1345](https://github.com/datadog/libdatadog/issues/1345)) - ([c98467e](https://github.com/datadog/libdatadog/commit/c98467eb286c61b4483b5af5a33b268a55ccc6ff))

### Changed

- Bump the test agent version used for integration tests ([#1417](https://github.com/datadog/libdatadog/issues/1417)) - ([e7c2ff8](https://github.com/datadog/libdatadog/commit/e7c2ff864ff3ecca090abe07291a2207c9e413c7))
- Merge remote-tracking branch 'origin/main' into release - ([03f0e30](https://github.com/datadog/libdatadog/commit/03f0e304a7da0954b0379d87af18dabe66d8b858))
- Remove manual changelog modifications ([#1472](https://github.com/datadog/libdatadog/issues/1472)) - ([d5f1bbf](https://github.com/datadog/libdatadog/commit/d5f1bbfac5850d1b4ecc9052772855fa33587459))
- Release libddcommon-v1.1.0 ([#1456](https://github.com/datadog/libdatadog/issues/1456)) - ([94cc701](https://github.com/datadog/libdatadog/commit/94cc701e24bbaacfdcc4b034419e72dea1816cc9))
- [SLES-2652] Log error details when trace request fails (2) ([#1441](https://github.com/datadog/libdatadog/issues/1441)) - ([8c830bf](https://github.com/datadog/libdatadog/commit/8c830bfe5164e6346de8d6c35fd97fdbaee9a16e))
- Update `prost` crates ([#1426](https://github.com/datadog/libdatadog/issues/1426)) - ([14bab86](https://github.com/datadog/libdatadog/commit/14bab865cfab5151fd399c594ab8f67e8bc7dcf1))
- [Serverless] Skip AAS metadata tagging when span is from API Management ([#1409](https://github.com/datadog/libdatadog/issues/1409)) - ([660c550](https://github.com/datadog/libdatadog/commit/660c550b6311a209d9cf7de762e54b6b7109bcdb))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))
- Handle null span tag values ([#1394](https://github.com/datadog/libdatadog/issues/1394)) - ([3abff86](https://github.com/datadog/libdatadog/commit/3abff8639a2dfdaf8b81842d6e927f2ee37e895b))
- [SVLS-7934] Log error details when trace request fails ([#1392](https://github.com/datadog/libdatadog/issues/1392)) - ([928e65f](https://github.com/datadog/libdatadog/commit/928e65f28db1174cabf9fd75efaaa94de661a8c5))
- Fix trace utils clippy warning ([#1397](https://github.com/datadog/libdatadog/issues/1397)) - ([c9ff30b](https://github.com/datadog/libdatadog/commit/c9ff30b24f94447ead139f64066ffae9f095ebb3))

### Fixed

- Set hostname on stats from tracer to empty string ([#1530](https://github.com/datadog/libdatadog/issues/1530)) - ([52d45ca](https://github.com/datadog/libdatadog/commit/52d45ca907504fd72e6b416a00e1dfeaa2b61f74))
- Undo commenting arg in docker cmd ([#1439](https://github.com/datadog/libdatadog/issues/1439)) - ([033991d](https://github.com/datadog/libdatadog/commit/033991d5beb9d17e82eadf0a98fdf489edc384da))

## 1.0.0 - 2025-11-18

Initial release.
