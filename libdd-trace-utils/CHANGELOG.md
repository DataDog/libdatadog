# Changelog



## [9.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v8.0.0..libdd-trace-utils-v9.0.0) - 2026-07-06

### Added

- Add agentless export ([#2081](https://github.com/datadog/libdatadog/issues/2081)) - ([48b8243](https://github.com/datadog/libdatadog/commit/48b8243418426ed881173fa184a619962d7aa69f))
- Add stdout log trace exporter ([#2074](https://github.com/datadog/libdatadog/issues/2074)) - ([c2751ef](https://github.com/datadog/libdatadog/commit/c2751eff7036159127ec52c69130eebf7d9a5a97))
- OTLP HTTP/protobuf trace export ([#2115](https://github.com/datadog/libdatadog/issues/2115)) - ([4e8e6cc](https://github.com/datadog/libdatadog/commit/4e8e6cc8c0fe083089cc8e57f0fd26667f29941c))
- Export client-computed span stats as OTLP trace metrics ([#2067](https://github.com/datadog/libdatadog/issues/2067)) - ([cc2d696](https://github.com/datadog/libdatadog/commit/cc2d6963073a6f5f37c31c4429b805760e836906))
- CSS Trace Filters ([#1985](https://github.com/datadog/libdatadog/issues/1985)) - ([2842d90](https://github.com/datadog/libdatadog/commit/2842d906c6f6596fd589d85767038cec3f646d37))
- Change buffer foundation ([#2046](https://github.com/datadog/libdatadog/issues/2046)) - ([693406c](https://github.com/datadog/libdatadog/commit/693406cc108379b3923751f9c3c0267a24f00845))
- Export OTLP spans with attribute-level OTel compatibility ([#2091](https://github.com/datadog/libdatadog/issues/2091)) - ([c690b5e](https://github.com/datadog/libdatadog/commit/c690b5e43ccdf5ff84566db4447d416ac8c48ea8))
- Add v1 span and its encoder ([#2039](https://github.com/datadog/libdatadog/issues/2039)) - ([c7fbf3a](https://github.com/datadog/libdatadog/commit/c7fbf3aa843d3256d353e7aeec13dfb0a8c1f120))
- Change buffer implementation ([#2055](https://github.com/datadog/libdatadog/issues/2055)) - ([a21b946](https://github.com/datadog/libdatadog/commit/a21b946af7163968906f2730e93e6f56b9051257))

### Changed

- Replace slot index with span_id, fix segment isolation ([#2105](https://github.com/datadog/libdatadog/issues/2105)) - ([5bbf425](https://github.com/datadog/libdatadog/commit/5bbf425da51d69adf655ab5042a68bcedac8d047))
- Skip slow miri tests ([#2188](https://github.com/datadog/libdatadog/issues/2188)) - ([4b66bd6](https://github.com/datadog/libdatadog/commit/4b66bd62c4d39184c68a58d576d7955f1fb51aaa))
- Use VecMap for `meta`, `metrics` and `meta_struct` for v04 spans ([#2043](https://github.com/datadog/libdatadog/issues/2043)) - ([74284ca](https://github.com/datadog/libdatadog/commit/74284cac76e9e6f8e4085b0029c851ec8d47b2f4))
- Submit p0 telemetry in stats ([#2130](https://github.com/datadog/libdatadog/issues/2130)) - ([54bd386](https://github.com/datadog/libdatadog/commit/54bd38625350d27000653278cb2dd835005157da))
- Add V05 msgpack decode microbenchmark ([#2127](https://github.com/datadog/libdatadog/issues/2127)) - ([ade9d96](https://github.com/datadog/libdatadog/commit/ade9d96eb908b1bcfa224263efb695964130101c))
- Add VecMap microbenchmarks ([#2126](https://github.com/datadog/libdatadog/issues/2126)) - ([cd90e50](https://github.com/datadog/libdatadog/commit/cd90e50a5b067cf77a3e06641d838bc4c6b62aba))
- Update protobufs to be in sync with datadog-agent ([#2180](https://github.com/datadog/libdatadog/issues/2180)) - ([b02d454](https://github.com/datadog/libdatadog/commit/b02d454576034ea56becbd61411ff2f831a89562))
- Fix timeouts on heavily contended scenarios ([#2093](https://github.com/datadog/libdatadog/issues/2093)) - ([e780619](https://github.com/datadog/libdatadog/commit/e780619d5e0626a8cc161c4cb848057fbbe13533))

### Fixed

- Mark decoded span maps as deduped ([#2110](https://github.com/datadog/libdatadog/issues/2110)) - ([8907887](https://github.com/datadog/libdatadog/commit/8907887c56605bee49b2ee06040bda1aea99ec47))



## [8.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v7.0.0..libdd-trace-utils-v8.0.0) - 2026-06-08

### Changed

- Avoid Clone bound from dedup ([#2069](https://github.com/datadog/libdatadog/issues/2069)) - ([93e9723](https://github.com/datadog/libdatadog/commit/93e97238020b5cf1165b2583ec555d58307af3c1))



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v6.0.1..libdd-trace-utils-v7.0.0) - 2026-06-05

### Added

- Add dedup convenience to VecMap ([#2049](https://github.com/datadog/libdatadog/issues/2049)) - ([331b904](https://github.com/datadog/libdatadog/commit/331b90444aff0db70d37bc2d507056f19881633b))

### Changed

- Revert add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) ([#2073](https://github.com/datadog/libdatadog/issues/2073)) - ([a21e9d5](https://github.com/datadog/libdatadog/commit/a21e9d5eeeff0be4a1b9de8104a2cf2eae2be6a3))

### Fixed

- Follow max retries of the strategy ([#2047](https://github.com/datadog/libdatadog/issues/2047)) - ([0172960](https://github.com/datadog/libdatadog/commit/01729601279185fa921147959f4b5c401340b838))
- Match the Go trace agent when parsing `datadog-client-computed-*` bool headers ([#2071](https://github.com/datadog/libdatadog/issues/2071)) - ([48da0d8](https://github.com/datadog/libdatadog/commit/48da0d82cb32b43d4cdece35b794c9bcbc275a03))



## [6.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v6.0.0..libdd-trace-utils-v6.0.1) - 2026-06-01

### Fixed

- Propagate _dd.p.tid from chunk root to all spans ([#2014](https://github.com/datadog/libdatadog/issues/2014)) - ([42d9ab0](https://github.com/datadog/libdatadog/commit/42d9ab0438338516d2e8ef962de4f8ed158c519d))



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v5.0.0..libdd-trace-utils-v6.0.0) - 2026-05-29

### Added

- Introduce VecMap datastructure ([#2022](https://github.com/datadog/libdatadog/issues/2022)) - ([f7d471d](https://github.com/datadog/libdatadog/commit/f7d471dc51bb3f2131e9577adc9ea0e06ee417c7))
- Update test agent version ([#2038](https://github.com/datadog/libdatadog/issues/2038)) - ([670a5ad](https://github.com/datadog/libdatadog/commit/670a5ad9fe540d7f4f3eee0b1f5192f532bbc06d))

### Changed

- Replace use_v05_format bool and remove infallible expect ([#1946](https://github.com/datadog/libdatadog/issues/1946)) - ([54afa6f](https://github.com/datadog/libdatadog/commit/54afa6f73cb46a864a58100bbbc4027acd0b9a0b))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v4.0.0..libdd-trace-utils-v5.0.0) - 2026-05-22

### Added

- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) - ([ecdca7d](https://github.com/datadog/libdatadog/commit/ecdca7d4ef4e7f11c0194ed2f4e25173973404e7))
- Add encoder from v04 to v1 ([#1896](https://github.com/datadog/libdatadog/issues/1896)) - ([e2fb886](https://github.com/datadog/libdatadog/commit/e2fb8860d002d1b56d0dc8b0b185fca7954371df))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v3.0.1..libdd-trace-utils-v4.0.0) - 2026-05-18

### Added

- Trait architecture http ([#1555](https://github.com/datadog/libdatadog/issues/1555)) - ([b863364](https://github.com/datadog/libdatadog/commit/b863364bbb9cb4567b10c80cd11bc4a22b49fcf4))
- Sleep & spawn capabilities ([#1873](https://github.com/datadog/libdatadog/issues/1873)) - ([b419f6e](https://github.com/datadog/libdatadog/commit/b419f6e1edb7679c750a65713893c68fc697404c))
- Check for empty value in header datadog-client-computed-stats ([#1900](https://github.com/datadog/libdatadog/issues/1900)) - ([27aa92c](https://github.com/datadog/libdatadog/commit/27aa92cfeeca073d8730a8b4974bd3fdef7ddf3a))
- Add support for OTLP trace export ([#1641](https://github.com/datadog/libdatadog/issues/1641)) - ([ee83a45](https://github.com/datadog/libdatadog/commit/ee83a4522289af457263f83a2877916ad297b44c))
- Add shared runtime ([#1602](https://github.com/datadog/libdatadog/issues/1602)) - ([33896de](https://github.com/datadog/libdatadog/commit/33896def2418a9c0fc5bf74b05011210d333759f))
- Map DD span resource to OTLP resource.name attribute ([#1811](https://github.com/datadog/libdatadog/issues/1811)) - ([9b42048](https://github.com/datadog/libdatadog/commit/9b420483c2e9745be692d7ca4de7ba769f94a5e7))
- Search all spans to populate tracer payload fields ([#1954](https://github.com/datadog/libdatadog/issues/1954)) - ([0a3304c](https://github.com/datadog/libdatadog/commit/0a3304c6aaf84738786b670d706a01edc22dab81))

### Changed

- Add allocation size tracking allocator ([#1905](https://github.com/datadog/libdatadog/issues/1905)) - ([d29b8d2](https://github.com/datadog/libdatadog/commit/d29b8d22f33ee0bd2ca9baf40f1afee801550c73))
- Pre-compute string messagepack encoding ([#1948](https://github.com/datadog/libdatadog/issues/1948)) - ([c713122](https://github.com/datadog/libdatadog/commit/c7131222cb42dd0513821456a4071245c4a819f6))
- Compilation of libdd-data-pipeline to wasm32 ([#1830](https://github.com/datadog/libdatadog/issues/1830)) - ([32f9679](https://github.com/datadog/libdatadog/commit/32f96790350141f82ad78a4b53babe5b757ea345))

### Fixed

- Gate libdd-common TLS features in obfuscation and capabilities-impl ([#1872](https://github.com/datadog/libdatadog/issues/1872)) - ([986aab5](https://github.com/datadog/libdatadog/commit/986aab55cb7941d8453dffb59d35a70599d08665))
- Update cloud environment detection logic for Serverless [SVLS-8799] ([#1857](https://github.com/datadog/libdatadog/issues/1857)) - ([d60d0a4](https://github.com/datadog/libdatadog/commit/d60d0a4bc7df3841d91929f9b852c5d9ccecd637))
- Defer trampoline self-deletion to avoid Valgrind false positive ([#1844](https://github.com/datadog/libdatadog/issues/1844)) - ([fc86998](https://github.com/datadog/libdatadog/commit/fc869988ed4f3dc04a081c08d1fda352d4ee2650))



## [3.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v3.0.0..libdd-trace-utils-v3.0.1) - 2026-03-25

### Changed
- Fix previous version.



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v2.0.2..libdd-trace-utils-v3.0.0) - 2026-03-23

### Changed

- Change header name type to accept dynamic values ([#1722](https://github.com/datadog/libdatadog/issues/1722)) - ([4dd532f](https://github.com/datadog/libdatadog/commit/4dd532f2c15e928103fc441ab030bc8d94f070c0))

### Fixed

- Rename wrongly cased stats fields ([#1780](https://github.com/datadog/libdatadog/issues/1780)) - ([5ff99ff](https://github.com/datadog/libdatadog/commit/5ff99ff6c465a95a740a494f42cce258c0e80be8))


## [2.0.2](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v2.0.1..libdd-trace-utils-v2.0.2) - 2026-03-16

### Changed

- Update dependencies ([#1734](https://github.com/DataDog/libdatadog/issues/1734)) - ([38dd71b](https://github.com/DataDog/libdatadog/commit/38dd71bd6fdac45ecab3d74ce1b4a827abae794a))



## [2.0.1](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v2.0.0..libdd-trace-utils-v2.0.1) - 2026-03-16

### Added

- Add two fields to ClientGroupedStats [SVLS-8627] ([#1630](https://github.com/datadog/libdatadog/issues/1630)) - ([7e909c0](https://github.com/datadog/libdatadog/commit/7e909c0910a15303eb90fdb3399211a3517d70c8))

### Changed

- Update bytes to 1.11.1 to address RUSTSEC-2026-0007 ([#1628](https://github.com/datadog/libdatadog/issues/1628)) - ([0b0863b](https://github.com/datadog/libdatadog/commit/0b0863b2afb7302fe02ea0af77cb9f98550e2a62))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-trace-utils-v1.0.0..libdd-trace-utils-v2.0.0) - 2026-02-23

### Added

- Introduce TraceData to unify text and binary data ([#1247](https://github.com/datadog/libdatadog/issues/1247)) - ([d430cbd](https://github.com/datadog/libdatadog/commit/d430cbd912d5300d521131392b86fc36a599aa27))
- Allow sending trace stats using custom HTTP client ([#1345](https://github.com/datadog/libdatadog/issues/1345)) - ([c98467e](https://github.com/datadog/libdatadog/commit/c98467eb286c61b4483b5af5a33b268a55ccc6ff))
- Unify Azure tags ([#1553](https://github.com/datadog/libdatadog/issues/1553)) - ([aa58f2d](https://github.com/datadog/libdatadog/commit/aa58f2d7f6db9278f94d9a9034caf215b90ccbe0))

### Changed

- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Bump the test agent version used for integration tests ([#1417](https://github.com/datadog/libdatadog/issues/1417)) - ([e7c2ff8](https://github.com/datadog/libdatadog/commit/e7c2ff864ff3ecca090abe07291a2207c9e413c7))
- Remove manual changelog modifications ([#1472](https://github.com/datadog/libdatadog/issues/1472)) - ([d5f1bbf](https://github.com/datadog/libdatadog/commit/d5f1bbfac5850d1b4ecc9052772855fa33587459))
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
