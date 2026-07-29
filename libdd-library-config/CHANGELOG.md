# Changelog



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-library-config-v2.0.0..libdd-library-config-v3.0.0) - 2026-07-07

### Added

- Caller-supplied threadlocal schema and extra process-context attributes ([#2162](https://github.com/datadog/libdatadog/issues/2162)) - ([7cdeb78](https://github.com/datadog/libdatadog/commit/7cdeb7896e92d1ba38bde495934e112dac2eda25))

### Fixed

- Put the threadlocal attributes at the right place in the context ([#2167](https://github.com/datadog/libdatadog/issues/2167)) - ([3630553](https://github.com/datadog/libdatadog/commit/36305534667a75c3125ad92c092829449439b324))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-library-config-v1.1.0..libdd-library-config-v2.0.0) - 2026-05-26

### Added

- Thread-level ctx publication ([#1791](https://github.com/datadog/libdatadog/issues/1791)) - ([660b8a8](https://github.com/datadog/libdatadog/commit/660b8a8ae71eb5bc2cdd286a206870fbcb04a62a))
- Add Hash trait to TracerMetadata ([#1931](https://github.com/datadog/libdatadog/issues/1931)) - ([d7eef80](https://github.com/datadog/libdatadog/commit/d7eef8031192d0ee79ba64cd824804c5a57abacf))
- Add PartialEq and Eq traits to TracerMetadata ([#1922](https://github.com/datadog/libdatadog/issues/1922)) - ([971c407](https://github.com/datadog/libdatadog/commit/971c407d856db58baf1078bd7802abe13bac4f9f))
- Root_span_id handling in otel thread ctx ([#1834](https://github.com/datadog/libdatadog/issues/1834)) - ([4be1fcc](https://github.com/datadog/libdatadog/commit/4be1fccc01264b1f48f4423460c64f6140580153))
- Extend tracer metadata with thread ctx attrbutes ([#1831](https://github.com/datadog/libdatadog/issues/1831)) - ([a1d45fc](https://github.com/datadog/libdatadog/commit/a1d45fc69308e330d04420be626f7c165f269ead))

### Changed

- Migrate from rustix to libc ([#1859](https://github.com/datadog/libdatadog/issues/1859)) - ([68822c5](https://github.com/datadog/libdatadog/commit/68822c55446efe8d6654d2449d696f5ff2f28d31))
- Move otel thread ctx in dedicated crate ([#1855](https://github.com/datadog/libdatadog/issues/1855)) - ([252c693](https://github.com/datadog/libdatadog/commit/252c693e68df9fa598119dd8cff26a2881bd8140))
- Gate behind feature ([#1843](https://github.com/datadog/libdatadog/issues/1843)) - ([11d4111](https://github.com/datadog/libdatadog/commit/11d4111c934d9af49d8124b8266dbbdda5857cb4))



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
