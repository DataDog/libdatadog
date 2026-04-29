# Changelog



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-common-v3.0.2..libdd-common-v4.0.0) - 2026-04-27

### Added

- Trait architecture http ([#1555](https://github.com/datadog/libdatadog/issues/1555)) - ([b863364](https://github.com/datadog/libdatadog/commit/b863364bbb9cb4567b10c80cd11bc4a22b49fcf4))
- Add shared runtime ([#1602](https://github.com/datadog/libdatadog/issues/1602)) - ([33896de](https://github.com/datadog/libdatadog/commit/33896def2418a9c0fc5bf74b05011210d333759f))
- Implement HTTP common component ([#1624](https://github.com/datadog/libdatadog/issues/1624)) - ([29678bd](https://github.com/datadog/libdatadog/commit/29678bd0434bbe61dda64b90e99fbb36037f79d2))

### Changed

- Add allocation size tracking allocator ([#1905](https://github.com/datadog/libdatadog/issues/1905)) - ([d29b8d2](https://github.com/datadog/libdatadog/commit/d29b8d22f33ee0bd2ca9baf40f1afee801550c73))
- Mock now function for rate limiter in tests to make them deterministic ([#1842](https://github.com/datadog/libdatadog/issues/1842)) - ([eb3c39b](https://github.com/datadog/libdatadog/commit/eb3c39b03521962ddedb2fd2c5990fdacea0a135))
- Remove transitive dependency ([#1895](https://github.com/datadog/libdatadog/issues/1895)) - ([bdb0ad5](https://github.com/datadog/libdatadog/commit/bdb0ad556a6abeb17d2f31a037e149ec05cb5e8b))
- Skip reqwest test that takes 10mn ([#1784](https://github.com/datadog/libdatadog/issues/1784)) - ([c929cdb](https://github.com/datadog/libdatadog/commit/c929cdb78d84f753f19ccacbee045e77dd5c5688))

### Fixed

- Skip thread counting test ([#1841](https://github.com/datadog/libdatadog/issues/1841)) - ([4360dbb](https://github.com/datadog/libdatadog/commit/4360dbb14e39d00d8a4fc40b6e66d1301f79acff))
- Don't use reqwest http proxies ([#1810](https://github.com/datadog/libdatadog/issues/1810)) - ([3fc2961](https://github.com/datadog/libdatadog/commit/3fc29617a905dea8cda45300656896f482d7278c))
- Use `ring` for non-fips builds ([#1816](https://github.com/datadog/libdatadog/issues/1816)) - ([5b6dffc](https://github.com/datadog/libdatadog/commit/5b6dffc5101a48706fe9c06f91e6c5afaf5e0ab5))
- Handle Podman cgroupns=host cgroup path ([#1828](https://github.com/datadog/libdatadog/issues/1828)) - ([e5de518](https://github.com/datadog/libdatadog/commit/e5de518b54dfdc649a87dcf57a09680ca3859a53))
- Fix condition so testing with --all-features works ([#1919](https://github.com/datadog/libdatadog/issues/1919)) - ([243aec1](https://github.com/datadog/libdatadog/commit/243aec1b5450b7edb546b5a59e5f80ab79abed08))



## [3.0.2](https://github.com/datadog/libdatadog/compare/libdd-common-v3.0.1..libdd-common-v3.0.2) - 2026-03-25

### Changed
- Fix previous release.



## [3.0.1](https://github.com/datadog/libdatadog/compare/libdd-common-v3.0.0..libdd-common-v3.0.1) - 2026-03-23

### Changed

- Update reqwest and quinn-proto dependency for dependabot alert ([#1774](https://github.com/datadog/libdatadog/issues/1774)) - ([1cd2791](https://github.com/datadog/libdatadog/commit/1cd2791f5e94ab3197e8e68bf6d670cc715d80a0))
- Ekump/APMSP-2718 update aws-lc dependencies ([#1751](https://github.com/datadog/libdatadog/issues/1751)) - ([5d5a596](https://github.com/datadog/libdatadog/commit/5d5a596b54b4bc3729063c30393e9706cf2d4eba))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-common-v2.0.1..libdd-common-v3.0.0) - 2026-03-18

### Changed

- Change header name type to accept dynamic values ([#1722](https://github.com/datadog/libdatadog/issues/1722)) - ([4dd532f](https://github.com/datadog/libdatadog/commit/4dd532f2c15e928103fc441ab030bc8d94f070c0))



## [2.0.1](https://github.com/datadog/libdatadog/compare/libdd-common-v2.0.0..libdd-common-v2.0.1) - 2026-03-16

### Changed

- Run thread count test as single threaded ([#1626](https://github.com/datadog/libdatadog/issues/1626)) - ([b0296aa](https://github.com/datadog/libdatadog/commit/b0296aa173211c81ba1349f2e2812a79938f3153))
- Run thread count test in own process ([#1693](https://github.com/datadog/libdatadog/issues/1693)) - ([3f3efef](https://github.com/datadog/libdatadog/commit/3f3efefb2ff45d7a5491b770480396d001b87631))
- Update bytes to 1.11.1 to address RUSTSEC-2026-0007 ([#1628](https://github.com/datadog/libdatadog/issues/1628)) - ([0b0863b](https://github.com/datadog/libdatadog/commit/0b0863b2afb7302fe02ea0af77cb9f98550e2a62))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-common-v1.1.0..libdd-common-v2.0.0) - 2026-02-23

### Added

- Add current thread id API ([#1569](https://github.com/datadog/libdatadog/issues/1569)) - ([367c8b2](https://github.com/datadog/libdatadog/commit/367c8b24f8c4b75fdbe431ad572ae71cb94fdfa5))
- Enable non-blocking DNS for reqwest ([#1558](https://github.com/datadog/libdatadog/issues/1558)) - ([bf953c0](https://github.com/datadog/libdatadog/commit/bf953c082825de2500f7fdf0c8ebf8ae7f946ff0))
- Unify Azure tags ([#1553](https://github.com/datadog/libdatadog/issues/1553)) - ([aa58f2d](https://github.com/datadog/libdatadog/commit/aa58f2d7f6db9278f94d9a9034caf215b90ccbe0))
- Single source of truth for headers (fixes issue in profiling with missing headers) ([#1493](https://github.com/datadog/libdatadog/issues/1493)) - ([9f2417e](https://github.com/datadog/libdatadog/commit/9f2417e1a472d433eddc2adeeb0c19ec2cb8b53a))

### Changed

- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Switch from multipart to multer to resolve deprecation warnings and dependabot alerts ([#1540](https://github.com/datadog/libdatadog/issues/1540)) - ([0d804b3](https://github.com/datadog/libdatadog/commit/0d804b39c0bfb7315f59f097a3702f1b70aa191a))
- Make reqwest available in common ([#1504](https://github.com/datadog/libdatadog/issues/1504)) - ([7986270](https://github.com/datadog/libdatadog/commit/7986270b124c313a71ae28ae415201ec3ccd794b))


## [1.1.0](https://github.com/datadog/libdatadog/compare/libdd-common-v1.0.0..libdd-common-v1.1.0) - 2026-01-20

### Added

- *(profiling)* Simpler API for profile exporter ([#1423](https://github.com/datadog/libdatadog/issues/1423)) - ([0d4ebbe](https://github.com/datadog/libdatadog/commit/0d4ebbe55ab841c2af8db41da74597c007375f0e))

### Changed

- *(profiling)* [**breaking**] Use reqwest instead of hyper for exporter ([#1444](https://github.com/datadog/libdatadog/issues/1444)) - ([39c7829](https://github.com/datadog/libdatadog/commit/39c7829592142d8fc8e8988b3631208e2d9ad1cc))
- Don't panic if CryptoProvider already installed  ([#1391](https://github.com/datadog/libdatadog/issues/1391)) - ([2f641ea](https://github.com/datadog/libdatadog/commit/2f641eae3708c34e4adfe62c9d477e665da4f12e))
- Support cxx bindings for crashinfo ([#1379](https://github.com/datadog/libdatadog/issues/1379)) - ([6b26318](https://github.com/datadog/libdatadog/commit/6b263189044f48cec6a67745036bd027b44f6daa))

## 1.0.0 - 2025-11-14

Initial release.
