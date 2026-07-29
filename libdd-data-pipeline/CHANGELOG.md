# Changelog



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v6.0.0..libdd-data-pipeline-v7.0.0) - 2026-07-07

### Added

- Add agentless export ([#2081](https://github.com/datadog/libdatadog/issues/2081)) - ([48b8243](https://github.com/datadog/libdatadog/commit/48b8243418426ed881173fa184a619962d7aa69f))
- Add stdout log trace exporter ([#2074](https://github.com/datadog/libdatadog/issues/2074)) - ([c2751ef](https://github.com/datadog/libdatadog/commit/c2751eff7036159127ec52c69130eebf7d9a5a97))
- OTLP HTTP/protobuf trace export ([#2115](https://github.com/datadog/libdatadog/issues/2115)) - ([4e8e6cc](https://github.com/datadog/libdatadog/commit/4e8e6cc8c0fe083089cc8e57f0fd26667f29941c))
- Export client-computed span stats as OTLP trace metrics ([#2067](https://github.com/datadog/libdatadog/issues/2067)) - ([cc2d696](https://github.com/datadog/libdatadog/commit/cc2d6963073a6f5f37c31c4429b805760e836906))
- CSS Trace Filters ([#1985](https://github.com/datadog/libdatadog/issues/1985)) - ([2842d90](https://github.com/datadog/libdatadog/commit/2842d906c6f6596fd589d85767038cec3f646d37))
- Export OTLP spans with attribute-level OTel compatibility ([#2091](https://github.com/datadog/libdatadog/issues/2091)) - ([c690b5e](https://github.com/datadog/libdatadog/commit/c690b5e43ccdf5ff84566db4447d416ac8c48ea8))
- SharedRuntime Borrowed & Owned mode ([#2061](https://github.com/datadog/libdatadog/issues/2061)) - ([4b79b7e](https://github.com/datadog/libdatadog/commit/4b79b7ed87113bea01db583d54e13fb0c2a19e74))
- Use weak waker in trigger [APMSP-3371] ([#2050](https://github.com/datadog/libdatadog/issues/2050)) - ([da8cbcb](https://github.com/datadog/libdatadog/commit/da8cbcb8b81b5b46d8d06da494157d6c74eabf0e))
- Emit canonical gRPC status name for OTLP rpc.response.status_code ([#2183](https://github.com/datadog/libdatadog/issues/2183)) - ([5e66eb6](https://github.com/datadog/libdatadog/commit/5e66eb6d84f37cdb3806d10aa35822665a0c5b77))
- Send telemetry for cardinality limits ([#2159](https://github.com/datadog/libdatadog/issues/2159)) - ([a4d4417](https://github.com/datadog/libdatadog/commit/a4d4417004bb0c2af4010575d56c729185d29000))
- Add whole key cardinality limit ([#2158](https://github.com/datadog/libdatadog/issues/2158)) - ([a38b630](https://github.com/datadog/libdatadog/commit/a38b6304dcd63c91a52a752f2baa04e7d21e374d))
- Add endpoint gating to client-side stats [APMSP-3361] ([#2040](https://github.com/datadog/libdatadog/issues/2040)) - ([cde8f3a](https://github.com/datadog/libdatadog/commit/cde8f3ad7300a5c8ecdcd26c4b76ebd6c2250b36))
- Enable telemetry in stats exporter ([#2160](https://github.com/datadog/libdatadog/issues/2160)) - ([d7b2aad](https://github.com/datadog/libdatadog/commit/d7b2aad37e2c45e44ba54473c9dd5ef5e3c94669))

### Changed

- Avoid leaking libdd-common types in the public API ([#2152](https://github.com/datadog/libdatadog/issues/2152)) - ([b3144c6](https://github.com/datadog/libdatadog/commit/b3144c676b73e157f9d563903c01df016882e8c4))
- Use VecMap for `meta`, `metrics` and `meta_struct` for v04 spans ([#2043](https://github.com/datadog/libdatadog/issues/2043)) - ([74284ca](https://github.com/datadog/libdatadog/commit/74284cac76e9e6f8e4085b0029c851ec8d47b2f4))
- Submit p0 telemetry in stats ([#2130](https://github.com/datadog/libdatadog/issues/2130)) - ([54bd386](https://github.com/datadog/libdatadog/commit/54bd38625350d27000653278cb2dd835005157da))

### Fixed

- Add grpc_method to aggregation key ([#2151](https://github.com/datadog/libdatadog/issues/2151)) - ([53e20b5](https://github.com/datadog/libdatadog/commit/53e20b54ed79e04e3bf5636ce97519732bcdbfad))



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v5.0.0..libdd-data-pipeline-v6.0.0) - 2026-06-08

### Added

- Add fork safety hooks and cancellation token for trace exporter FFI ([#2051](https://github.com/datadog/libdatadog/issues/2051)) - ([2a6c295](https://github.com/datadog/libdatadog/commit/2a6c295615eee10150f668f013ef34aba05f4d9e))
- Move the async boundary up ([#2064](https://github.com/datadog/libdatadog/issues/2064)) - ([43a5c6b](https://github.com/datadog/libdatadog/commit/43a5c6b87ea4f384f56608656a08b2ba3d59604e))
- Add fail-closed fallback to v04 ([#2037](https://github.com/datadog/libdatadog/issues/2037)) - ([a84923e](https://github.com/datadog/libdatadog/commit/a84923e5ec124efb59e413adac98afb9546a490b))

### Changed

- Replace use_v05_format bool and remove infallible expect ([#1946](https://github.com/datadog/libdatadog/issues/1946)) - ([54afa6f](https://github.com/datadog/libdatadog/commit/54afa6f73cb46a864a58100bbbc4027acd0b9a0b))
- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) ([#2073](https://github.com/datadog/libdatadog/issues/2073)) - ([a21e9d5](https://github.com/datadog/libdatadog/commit/a21e9d5eeeff0be4a1b9de8104a2cf2eae2be6a3))
- Bump msrv to 1.87.0 ([#2017](https://github.com/datadog/libdatadog/issues/2017)) - ([276039d](https://github.com/datadog/libdatadog/commit/276039da8897a8e9e83ed3162912792f2241c5d7))

### Fixed

- Follow max retries of the strategy ([#2047](https://github.com/datadog/libdatadog/issues/2047)) - ([0172960](https://github.com/datadog/libdatadog/commit/01729601279185fa921147959f4b5c401340b838))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v4.0.0..libdd-data-pipeline-v5.0.0) - 2026-05-22

### Added

- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) - ([ecdca7d](https://github.com/datadog/libdatadog/commit/ecdca7d4ef4e7f11c0194ed2f4e25173973404e7))
- Flush based on size of chunks in bytes ([#1953](https://github.com/datadog/libdatadog/issues/1953)) - ([bc8f375](https://github.com/datadog/libdatadog/commit/bc8f37585deb16c873fdb126cb3033d7757dd426))
- Add encoder from v04 to v1 ([#1896](https://github.com/datadog/libdatadog/issues/1896)) - ([e2fb886](https://github.com/datadog/libdatadog/commit/e2fb8860d002d1b56d0dc8b0b185fca7954371df))

### Fixed

- Allow old PascalCase fields in obfuscation config scheme ([#2008](https://github.com/datadog/libdatadog/issues/2008)) - ([cea1e44](https://github.com/datadog/libdatadog/commit/cea1e44edddd9124f75d5095f31026904a1f58d8))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v3.0.1..libdd-data-pipeline-v4.0.0) - 2026-05-18

### Added

- Trait architecture http ([#1555](https://github.com/datadog/libdatadog/issues/1555)) - ([b863364](https://github.com/datadog/libdatadog/commit/b863364bbb9cb4567b10c80cd11bc4a22b49fcf4))
- Sleep & spawn capabilities ([#1873](https://github.com/datadog/libdatadog/issues/1873)) - ([b419f6e](https://github.com/datadog/libdatadog/commit/b419f6e1edb7679c750a65713893c68fc697404c))
- Port dd-trace-rs trace buffer implementation ([#1826](https://github.com/datadog/libdatadog/issues/1826)) - ([555a22e](https://github.com/datadog/libdatadog/commit/555a22e85b1dabed6dee8987839efe0f541e22c6))
- Add timeout to info fetcher ([#1890](https://github.com/datadog/libdatadog/issues/1890)) - ([c7c315b](https://github.com/datadog/libdatadog/commit/c7c315baf9a5baff11d94d0af1c99ce086049bfa))
- Add support for OTLP trace export ([#1641](https://github.com/datadog/libdatadog/issues/1641)) - ([ee83a45](https://github.com/datadog/libdatadog/commit/ee83a4522289af457263f83a2877916ad297b44c))
- Add shared runtime ([#1602](https://github.com/datadog/libdatadog/issues/1602)) - ([33896de](https://github.com/datadog/libdatadog/commit/33896def2418a9c0fc5bf74b05011210d333759f))
- Allow worker to be stopped after fork ([#1893](https://github.com/datadog/libdatadog/issues/1893)) - ([5b798ae](https://github.com/datadog/libdatadog/commit/5b798aee0be0b47ca3cec0dedda9becc0334e1dc))
- Add stats computation via SHM ([#1821](https://github.com/datadog/libdatadog/issues/1821)) - ([ff8e912](https://github.com/datadog/libdatadog/commit/ff8e9120c7fe1746f3b0cad5b5e7c1cefa4d99ef))
- Include dependencies and integrations in app-extended-heartbeat ([#1962](https://github.com/datadog/libdatadog/issues/1962)) - ([91fd13c](https://github.com/datadog/libdatadog/commit/91fd13c8a0ca5335fe39940f8764cd825bbef7e8))
- Add session id support to trace export ([#1822](https://github.com/datadog/libdatadog/issues/1822)) - ([b1b58fc](https://github.com/datadog/libdatadog/commit/b1b58fc389f1f078a063e8beffbd312f930065b4))
- Integrate obfuscation to the stats exporter [APMSP-2764] ([#1819](https://github.com/datadog/libdatadog/issues/1819)) - ([540f186](https://github.com/datadog/libdatadog/commit/540f18646d58bd18984990fbed85254b3678ac7f))
- Added regex-lite feature ([#1939](https://github.com/datadog/libdatadog/issues/1939)) - ([58b86d5](https://github.com/datadog/libdatadog/commit/58b86d5a1b2dc43be98eb9568ec734c259a430a7))

### Changed

- Downgrade version so publish workflow succeeds ([#1870](https://github.com/datadog/libdatadog/issues/1870)) - ([730c122](https://github.com/datadog/libdatadog/commit/730c1221f9f73ecadcdcc90681f54730fe8e92f2))
- Pre-allocate serialization buffer ([#1949](https://github.com/datadog/libdatadog/issues/1949)) - ([d700bb0](https://github.com/datadog/libdatadog/commit/d700bb0de476a0e2f273f71c3be87227ee58027b))
- Pre-compute string messagepack encoding ([#1948](https://github.com/datadog/libdatadog/issues/1948)) - ([c713122](https://github.com/datadog/libdatadog/commit/c7131222cb42dd0513821456a4071245c4a819f6))
- Compilation of libdd-data-pipeline to wasm32 ([#1830](https://github.com/datadog/libdatadog/issues/1830)) - ([32f9679](https://github.com/datadog/libdatadog/commit/32f96790350141f82ad78a4b53babe5b757ea345))

### Fixed

- Gate libdd-common TLS features in remaining internal crates + add CI guard ([#1943](https://github.com/datadog/libdatadog/issues/1943)) - ([db05e1f](https://github.com/datadog/libdatadog/commit/db05e1f8408a76075efb37ecec544d2e74217e57))
- Remove default-features from of trace-obfuscation ([#1981](https://github.com/datadog/libdatadog/issues/1981)) - ([12b7b09](https://github.com/datadog/libdatadog/commit/12b7b09379215c96751fe204e0a598e8f805fc61))
- Restore previous Cargo.toml version ([#1993](https://github.com/datadog/libdatadog/issues/1993)) - ([500c147](https://github.com/datadog/libdatadog/commit/500c147ec07e9c768abdfaec074a84ab88885e2a))
- Missing bench path in data-pipeline ([#1907](https://github.com/datadog/libdatadog/issues/1907)) - ([530cd96](https://github.com/datadog/libdatadog/commit/530cd96349e50dce032a450e88c019e4b47a39fe))
- Align with css spec ([#1790](https://github.com/datadog/libdatadog/issues/1790)) - ([b1d5bcf](https://github.com/datadog/libdatadog/commit/b1d5bcf7a2a006e2de95925cd8aa5ec13eec4b87))
- Avoid trigger loop in telemetry worker ([#1950](https://github.com/datadog/libdatadog/issues/1950)) - ([7a24f53](https://github.com/datadog/libdatadog/commit/7a24f534a46367bc2b2007994dd3a3d2d62ad663))
- Unwrap_or being eager is not good ([#1983](https://github.com/datadog/libdatadog/issues/1983)) - ([68c6519](https://github.com/datadog/libdatadog/commit/68c65192b0d3de960a7b0eb648e26da6952d796c))



## [3.0.1](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v3.0.0..libdd-data-pipeline-v3.0.1) - 2026-03-25

### Added
  - Fix previous version.



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v2.0.1..libdd-data-pipeline-v3.0.0) - 2026-03-23

### Added

- Retrieve container tags hash from /info endpoint ([#1700](https://github.com/datadog/libdatadog/issues/1700)) - ([cc4a550](https://github.com/datadog/libdatadog/commit/cc4a550bf6063f80e969332485df806e2c420ebf))

### Changed

- Change header name type to accept dynamic values ([#1722](https://github.com/datadog/libdatadog/issues/1722)) - ([4dd532f](https://github.com/datadog/libdatadog/commit/4dd532f2c15e928103fc441ab030bc8d94f070c0))



## [2.0.1](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v2.0.0..libdd-data-pipeline-v2.0.1) - 2026-03-16

### Changed

- Update dependencies ([#1734](https://github.com/DataDog/libdatadog/issues/1734)) - ([38dd71b](https://github.com/DataDog/libdatadog/commit/38dd71bd6fdac45ecab3d74ce1b4a827abae794a))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-data-pipeline-v1.0.0..libdd-data-pipeline-v2.0.0) - 2026-02-23

### Added

- Include reason for chunks dropped telemetry ([#1449](https://github.com/datadog/libdatadog/issues/1449)) - ([99be5d7](https://github.com/datadog/libdatadog/commit/99be5d7d6c26940f0197290493b60e8ba603fbb1))
- Introduce TraceData to unify text and binary data ([#1247](https://github.com/datadog/libdatadog/issues/1247)) - ([d430cbd](https://github.com/datadog/libdatadog/commit/d430cbd912d5300d521131392b86fc36a599aa27))

### Changed

- Handle EINTR in test_health_metrics_disabled ([#1430](https://github.com/datadog/libdatadog/issues/1430)) - ([e13f239](https://github.com/datadog/libdatadog/commit/e13f2393185031757f493fcebdfe0e9e435b60e9))
- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Health metrics ([#1433](https://github.com/datadog/libdatadog/issues/1433)) - ([7f30d50](https://github.com/datadog/libdatadog/commit/7f30d50f45be5027b1fc67296d06720f8279efe5))
- Remove Proxy TraceExporter input mode ([#1583](https://github.com/datadog/libdatadog/issues/1583)) - ([2078f6f](https://github.com/datadog/libdatadog/commit/2078f6f051c90ed8e6af2e171d943dc6a117971c))
- Prepare libdd-telemetry-v2.0.0 ([#1457](https://github.com/datadog/libdatadog/issues/1457)) - ([753df4f](https://github.com/datadog/libdatadog/commit/753df4f235074cd3420a7e3cd8d2ff9bc964db0d))
- Allow submitting Vec<Vec<Span>> asynchronously ([#1302](https://github.com/datadog/libdatadog/issues/1302)) - ([158b594](https://github.com/datadog/libdatadog/commit/158b59471f1132e3cb36023fa3c46ccb2dd0eda1))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))

## 1.0.0 - 2025-11-18

Initial release.
