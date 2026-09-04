# Changelog



## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-shared-runtime-v3.0.0..libdd-shared-runtime-v4.0.0) - 2026-09-04

### Fixed

- Allow disabling worker fork restart ([#2464](https://github.com/datadog/libdatadog/issues/2464)) - ([48e990d](https://github.com/datadog/libdatadog/commit/48e990d1554aabec43e139e27ebb5514df6dab89))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-shared-runtime-v2.0.0..libdd-shared-runtime-v3.0.0) - 2026-08-17

### Added

- Add block_on_with_timeout to BlockingRuntime ([#2333](https://github.com/datadog/libdatadog/issues/2333)) - ([a21b01f](https://github.com/datadog/libdatadog/commit/a21b01f602f6020654fbeef86040d1ffa762e89d))

### Changed

- Restart all workers on invalid state ([#2262](https://github.com/datadog/libdatadog/issues/2262)) - ([485a0a1](https://github.com/datadog/libdatadog/commit/485a0a151ea9c02b6005d63dd62ed24e52f2d5a9))
- Migrate to workspace dependencies, phase 3 ([#2283](https://github.com/datadog/libdatadog/issues/2283)) - ([f73e8ae](https://github.com/datadog/libdatadog/commit/f73e8ae5997d54860984ad8e155fa9fa257d9263))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-shared-runtime-v1.0.0..libdd-shared-runtime-v2.0.0) - 2026-07-07

### Added

- SharedRuntime Borrowed & Owned mode ([#2061](https://github.com/datadog/libdatadog/issues/2061)) - ([4b79b7e](https://github.com/datadog/libdatadog/commit/4b79b7ed87113bea01db583d54e13fb0c2a19e74))
- Use weak waker in trigger [APMSP-3371] ([#2050](https://github.com/datadog/libdatadog/issues/2050)) - ([da8cbcb](https://github.com/datadog/libdatadog/commit/da8cbcb8b81b5b46d8d06da494157d6c74eabf0e))


## 1.0.0 - 2026-05-15

Initial release.
