# Changelog



## [1.1.0](https://github.com/datadog/libdatadog/compare/libdd-library-config-v1.0.0..libdd-library-config-v1.1.0) - 2026-03-13

### Added

- Publish tracer metadata as OTel process ctx ([#1658](https://github.com/datadog/libdatadog/issues/1658)) - ([79f879e](https://github.com/datadog/libdatadog/commit/79f879ef22f7310a2bd50b9d4bd683f8c9e0779c))
- Otel process ctxt protobuf encoding ([#1651](https://github.com/datadog/libdatadog/issues/1651)) - ([412ae10](https://github.com/datadog/libdatadog/commit/412ae10fdacc06e1cbffa8cc2051caad0d02f64f))
- Process context publication ([#1585](https://github.com/datadog/libdatadog/issues/1585)) - ([8fb3175](https://github.com/datadog/libdatadog/commit/8fb31754e04a278f1554be128372f8734582f828))

### Changed

- Update otel process ctx protocol ([#1713](https://github.com/datadog/libdatadog/issues/1713)) - ([0e8c2c6](https://github.com/datadog/libdatadog/commit/0e8c2c6f7c7dc856784c559340c17cc7d53a4bd5))
- Implement otel process ctx update ([#1640](https://github.com/datadog/libdatadog/issues/1640)) - ([36383f2](https://github.com/datadog/libdatadog/commit/36383f2721377b989d5d3ab96a3c34af0a5f2112))
- Update nightly in CI to 2026-02-08 ([#1539](https://github.com/datadog/libdatadog/issues/1539)) - ([5b504e5](https://github.com/datadog/libdatadog/commit/5b504e5938a2ed15f38902b0aa5f7fecf99a9f9b))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))

### Fixed

- [APMAPI-1690] add >100mb check for stable config files ([#1432](https://github.com/datadog/libdatadog/issues/1432)) - ([51c8cb4](https://github.com/datadog/libdatadog/commit/51c8cb4eebe9fe245fc739033b7a3494311e520f))
- Handle fork in otel process ctx ([#1650](https://github.com/datadog/libdatadog/issues/1650)) - ([eed7965](https://github.com/datadog/libdatadog/commit/eed796547adec71dc85be73a679a742b5f959fd7))

## 1.0.0 - 2025-11-17

Initial release.
