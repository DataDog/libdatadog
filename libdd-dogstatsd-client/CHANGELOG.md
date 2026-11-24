# Changelog



## [10.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v9.0.0..libdd-dogstatsd-client-v10.0.0) - 2026-10-28

### Changed

- Bump MSRV from 1.87.0 to 1.91.1 ([#2734](https://github.com/datadog/libdatadog/issues/2734)) - ([1c6b398](https://github.com/datadog/libdatadog/commit/1c6b398b6caf0cfa2deaf1f493c08248719f2dc4))



## [9.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v8.0.0..libdd-dogstatsd-client-v9.0.0) - 2026-10-23

### Added

- Add basic no_std to libdd-common ([#2638](https://github.com/datadog/libdatadog/issues/2638)) - ([d0dd0a2](https://github.com/datadog/libdatadog/commit/d0dd0a21ffca664d7c6a0798dd8c178234fd6750))

### Changed

- Fix last doc lints ([#2694](https://github.com/datadog/libdatadog/issues/2694)) - ([3e4a453](https://github.com/datadog/libdatadog/commit/3e4a4532fb7f79eebfb6daef022a2d0c6772a560))

### Fixed

- Fix --no-default-features tests ([#2620](https://github.com/datadog/libdatadog/issues/2620)) - ([ecd0de1](https://github.com/datadog/libdatadog/commit/ecd0de12d4ceae14cffc8fb9fa89e3664ca9ad1c))
- Most doc warnings - semantic changes ([#2547](https://github.com/datadog/libdatadog/issues/2547)) - ([a887c16](https://github.com/datadog/libdatadog/commit/a887c1644be7b7b44829fcf71ed5e6058ae8d67e))
- Most doc warnings ([#2289](https://github.com/datadog/libdatadog/issues/2289)) - ([f52ab62](https://github.com/datadog/libdatadog/commit/f52ab62fdaa1acc0ca9e8812a9e9468cc59af6b5))



## [8.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v7.0.0..libdd-dogstatsd-client-v8.0.0) - 2026-10-02

### Added

- Authenticate sidecar connections and shared memory ([#2551](https://github.com/datadog/libdatadog/issues/2551)) - ([d120c10](https://github.com/datadog/libdatadog/commit/d120c1090a03fa7274fed4121344d25660081895))

### Changed

- Update workspace to Rust 2024 edition ([#2575](https://github.com/datadog/libdatadog/issues/2575)) - ([620a212](https://github.com/datadog/libdatadog/commit/620a212fc91e128234f48b29d657c71d6ad5caed))



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v6.0.0..libdd-dogstatsd-client-v7.0.0) - 2026-09-24

### Changed

- Move all remaining external deps to workspace-level dependencies ([#2476](https://github.com/datadog/libdatadog/issues/2476)) - ([ea4379c](https://github.com/datadog/libdatadog/commit/ea4379c9ca0dd024c7a3ed5c497fa8e117018d6a))



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-dogstatsd-client-v5.0.0..libdd-dogstatsd-client-v6.0.0) - 2026-09-08

### Changed

- Migrate HTTP & networking deps to workspace level (phase 4bis) ([#2350](https://github.com/datadog/libdatadog/issues/2350)) - ([55cdf67](https://github.com/datadog/libdatadog/commit/55cdf67b720b7df427b1febd147b0f72d486167f))



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
