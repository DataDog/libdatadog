# Changelog



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v5.0.0..libdd-trace-stats-v6.0.0) - 2026-07-07

### Added

- Export client-computed span stats as OTLP trace metrics ([#2067](https://github.com/datadog/libdatadog/issues/2067)) - ([cc2d696](https://github.com/datadog/libdatadog/commit/cc2d6963073a6f5f37c31c4429b805760e836906))
- SharedRuntime Borrowed & Owned mode ([#2061](https://github.com/datadog/libdatadog/issues/2061)) - ([4b79b7e](https://github.com/datadog/libdatadog/commit/4b79b7ed87113bea01db583d54e13fb0c2a19e74))
- Send telemetry for cardinality limits ([#2159](https://github.com/datadog/libdatadog/issues/2159)) - ([a4d4417](https://github.com/datadog/libdatadog/commit/a4d4417004bb0c2af4010575d56c729185d29000))
- Add whole key cardinality limit ([#2158](https://github.com/datadog/libdatadog/issues/2158)) - ([a38b630](https://github.com/datadog/libdatadog/commit/a38b6304dcd63c91a52a752f2baa04e7d21e374d))

### Changed

- Use VecMap for `meta`, `metrics` and `meta_struct` for v04 spans ([#2043](https://github.com/datadog/libdatadog/issues/2043)) - ([74284ca](https://github.com/datadog/libdatadog/commit/74284cac76e9e6f8e4085b0029c851ec8d47b2f4))
- Update protobufs to be in sync with datadog-agent ([#2180](https://github.com/datadog/libdatadog/issues/2180)) - ([b02d454](https://github.com/datadog/libdatadog/commit/b02d454576034ea56becbd61411ff2f831a89562))

### Fixed

- Add grpc_method to aggregation key ([#2151](https://github.com/datadog/libdatadog/issues/2151)) - ([53e20b5](https://github.com/datadog/libdatadog/commit/53e20b54ed79e04e3bf5636ce97519732bcdbfad))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v4.0.0..libdd-trace-stats-v5.0.0) - 2026-06-08

### Changed

- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) ([#2073](https://github.com/datadog/libdatadog/issues/2073)) - ([a21e9d5](https://github.com/datadog/libdatadog/commit/a21e9d5eeeff0be4a1b9de8104a2cf2eae2be6a3))
- Bump msrv to 1.87.0 ([#2017](https://github.com/datadog/libdatadog/issues/2017)) - ([276039d](https://github.com/datadog/libdatadog/commit/276039da8897a8e9e83ed3162912792f2241c5d7))

### Fixed

- Follow max retries of the strategy ([#2047](https://github.com/datadog/libdatadog/issues/2047)) - ([0172960](https://github.com/datadog/libdatadog/commit/01729601279185fa921147959f4b5c401340b838))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v3.0.0..libdd-trace-stats-v4.0.0) - 2026-05-22

### Added

- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) - ([ecdca7d](https://github.com/datadog/libdatadog/commit/ecdca7d4ef4e7f11c0194ed2f4e25173973404e7))
- Add encoder from v04 to v1 ([#1896](https://github.com/datadog/libdatadog/issues/1896)) - ([e2fb886](https://github.com/datadog/libdatadog/commit/e2fb8860d002d1b56d0dc8b0b185fca7954371df))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v2.0.0..libdd-trace-stats-v3.0.0) - 2026-05-18

### Added

- Sleep & spawn capabilities ([#1873](https://github.com/datadog/libdatadog/issues/1873)) - ([b419f6e](https://github.com/datadog/libdatadog/commit/b419f6e1edb7679c750a65713893c68fc697404c))
- Allow worker to be stopped after fork ([#1893](https://github.com/datadog/libdatadog/issues/1893)) - ([5b798ae](https://github.com/datadog/libdatadog/commit/5b798aee0be0b47ca3cec0dedda9becc0334e1dc))
- Add stats computation via SHM ([#1821](https://github.com/datadog/libdatadog/issues/1821)) - ([ff8e912](https://github.com/datadog/libdatadog/commit/ff8e9120c7fe1746f3b0cad5b5e7c1cefa4d99ef))
- Propagate service source from span meta to client stats payload ([#1803](https://github.com/datadog/libdatadog/issues/1803)) - ([5cfc694](https://github.com/datadog/libdatadog/commit/5cfc694173da07ff13b7bff967a46bddc903e3db))
- Integrate obfuscation to the stats exporter [APMSP-2764] ([#1819](https://github.com/datadog/libdatadog/issues/1819)) - ([540f186](https://github.com/datadog/libdatadog/commit/540f18646d58bd18984990fbed85254b3678ac7f))
- Use ip quantization when aggregating peer tags for trace stats ([#1944](https://github.com/datadog/libdatadog/issues/1944)) - ([4ae8ebe](https://github.com/datadog/libdatadog/commit/4ae8ebe252451374c292efd159ce254c3f5a72e0))

### Changed

- Pre-compute string messagepack encoding ([#1948](https://github.com/datadog/libdatadog/issues/1948)) - ([c713122](https://github.com/datadog/libdatadog/commit/c7131222cb42dd0513821456a4071245c4a819f6))

### Fixed

- Gate libdd-common TLS features in remaining internal crates + add CI guard ([#1943](https://github.com/datadog/libdatadog/issues/1943)) - ([db05e1f](https://github.com/datadog/libdatadog/commit/db05e1f8408a76075efb37ecec544d2e74217e57))
- Align with css spec ([#1790](https://github.com/datadog/libdatadog/issues/1790)) - ([b1d5bcf](https://github.com/datadog/libdatadog/commit/b1d5bcf7a2a006e2de95925cd8aa5ec13eec4b87))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v1.0.4..libdd-trace-stats-v2.0.0) - 2026-03-25

### Changed
- Fix previous version.



## [1.0.4](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v1.0.3..libdd-trace-stats-v1.0.4) - 2026-03-23

### Changed

- Update dependencies ([#1781](https://github.com/DataDog/libdatadog/issues/1781)) - ([6e2e7caf7](https://github.com/DataDog/libdatadog/commit/6e2e7caf7294f0046f731527b1f479fe7a864ea9))



## [1.0.3](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v1.0.2..libdd-trace-stats-v1.0.3) - 2026-03-16

### Changed

- Update dependencies ([#1734](https://github.com/DataDog/libdatadog/issues/1734)) - ([38dd71b](https://github.com/DataDog/libdatadog/commit/38dd71bd6fdac45ecab3d74ce1b4a827abae794a))



## [1.0.2](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v1.0.1..libdd-trace-stats-v1.0.2) - 2026-03-16

### Added

- Add two fields to ClientGroupedStats [SVLS-8627] ([#1630](https://github.com/datadog/libdatadog/issues/1630)) - ([7e909c0](https://github.com/datadog/libdatadog/commit/7e909c0910a15303eb90fdb3399211a3517d70c8))
- Add grpc status code in the stats bucket key ([#1701](https://github.com/datadog/libdatadog/issues/1701)) - ([8fae837](https://github.com/datadog/libdatadog/commit/8fae837c9a93bffe49c6f9d73df2d929d13063ed))



## [1.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-stats-v1.0.0..libdd-trace-stats-v1.0.1) - 2026-02-23

### Added

- Introduce TraceData to unify text and binary data ([#1247](https://github.com/datadog/libdatadog/issues/1247)) - ([d430cbd](https://github.com/datadog/libdatadog/commit/d430cbd912d5300d521131392b86fc36a599aa27))

### Changed

- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))

## 1.0.0 - 2025-11-18

Initial release.
