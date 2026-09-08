# Changelog



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v4.0.0..libdd-dogstatsd-client-v5.0.0) - 2026-08-17

### Added

- Add shared_runtime buffered sink ([#2224](https://github.com/datadog/libdatadog/issues/2224)) - ([919c275](https://github.com/datadog/libdatadog/commit/919c275cdabdb5c1a300d605793908e219d1a22c))

### Changed

- Make client clonable ([#2222](https://github.com/datadog/libdatadog/issues/2222)) - ([b9fae6d](https://github.com/datadog/libdatadog/commit/b9fae6d5365be2ddce0d11d6d771481de5c47c27))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v3.0.0..libdd-dogstatsd-client-v4.0.0) - 2026-07-07

### Changed

- Bump `libdd-common` to a new major version (`^4.1.0` → `^5.1.0`)

## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v2.0.0..libdd-dogstatsd-client-v3.0.0) - 2026-05-18

### Fixed

- Gate libdd-common TLS features in remaining internal crates + add CI guard ([#1943](https://github.com/datadog/libdatadog/issues/1943)) - ([db05e1f](https://github.com/datadog/libdatadog/commit/db05e1f8408a76075efb37ecec544d2e74217e57))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v1.0.2..libdd-dogstatsd-client-v2.0.0) - 2026-03-25

### Changed

  - Fix previous version.



## [1.0.2](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v1.0.1..libdd-dogstatsd-client-v1.0.2) - 2026-03-23

### Changed

- Update dependencies ([#1781](https://github.com/DataDog/libdatadog/issues/1781)) - ([557c06d](https://github.com/DataDog/libdatadog/commit/557c06da7a9171e452a128b419767b75ba7d78db))



## [1.0.1](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v1.0.0..libdd-dogstatsd-client-v1.0.1) - 2026-02-23

### Changed

- Remove direct dependency on hyper client everywhere in common ([#1604](https://github.com/datadog/libdatadog/issues/1604)) - ([497e324](https://github.com/datadog/libdatadog/commit/497e324438614d0214e7991438062ca5de9f0a1f))
- Add changelog for every published crate ([#1396](https://github.com/datadog/libdatadog/issues/1396)) - ([5c4a024](https://github.com/datadog/libdatadog/commit/5c4a024598d6fe6cbd93a3e3dc9882848912064f))
- Fix recent clippy warnings ([#1346](https://github.com/datadog/libdatadog/issues/1346)) - ([516ed31](https://github.com/datadog/libdatadog/commit/516ed31146c5b7d611481973060bafc694cc0eb6))

## 1.0.0

Initial release.
