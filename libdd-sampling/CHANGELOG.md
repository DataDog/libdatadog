# Changelog



## [7.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v6.0.0..libdd-sampling-v7.0.0) - 2026-09-24

### Added

- Add from owned to SpanText ([#2403](https://github.com/datadog/libdatadog/issues/2403)) - ([39590c6](https://github.com/datadog/libdatadog/commit/39590c6fb599919eb56eaf829af78c10c7346474))

### Changed

- Remove batched-loop sampling noise ([#2498](https://github.com/datadog/libdatadog/issues/2498)) - ([e0a0134](https://github.com/datadog/libdatadog/commit/e0a0134bbf0896603db615344e9b2950e7a2b2b9))
- Make rate_limiter thread-safety test deterministic ([#2354](https://github.com/datadog/libdatadog/issues/2354)) - ([4678752](https://github.com/datadog/libdatadog/commit/4678752b4be5bec7e55112598eb1b9491ddd9cc6))
- Move all remaining external deps to workspace-level dependencies ([#2476](https://github.com/datadog/libdatadog/issues/2476)) - ([ea4379c](https://github.com/datadog/libdatadog/commit/ea4379c9ca0dd024c7a3ed5c497fa8e117018d6a))

### Fixed

- Benchmark was doing many samples for deterministic heap usage ([#2465](https://github.com/datadog/libdatadog/issues/2465)) - ([441cdcf](https://github.com/datadog/libdatadog/commit/441cdcf2fa14ac2b0a75df3f901a219eda2cc705))



## [6.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v5.0.0..libdd-sampling-v6.0.0) - 2026-08-17

### Added

- OTel consistent-probability rv/th derivation (APMAPI-2181) ([#2276](https://github.com/datadog/libdatadog/issues/2276)) - ([ed5af0e](https://github.com/datadog/libdatadog/commit/ed5af0e21d0e4f2f5ccf85bd4d5eb9266054b15e))

### Changed

- Migrate to workspace dependencies, phase 4 ([#2296](https://github.com/datadog/libdatadog/issues/2296)) - ([3c4c095](https://github.com/datadog/libdatadog/commit/3c4c0952c016b3b156d8a82ec27eeb515079d286))
- Moving to workspace-level dependencies, phase 2 ([#2270](https://github.com/datadog/libdatadog/issues/2270)) - ([caa732f](https://github.com/datadog/libdatadog/commit/caa732f3fe7c82a347813ba36686e039d29981a3))
- Consolidate core dependencies at workspace level (phase 1) ([#2253](https://github.com/datadog/libdatadog/issues/2253)) - ([15899df](https://github.com/datadog/libdatadog/commit/15899dfe754d12186ce7db72f0ff41c1920d52ec))

### Fixed

- Record rate limiter's effective rate on allow, not just drop ([#2288](https://github.com/datadog/libdatadog/issues/2288)) - ([ef1bfe4](https://github.com/datadog/libdatadog/commit/ef1bfe4d2391d08b9c0cd05b264db0adb75fa6c2))



## [5.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v4.0.0..libdd-sampling-v5.0.0) - 2026-07-07

### Changed

- Bump `libdd-common` to a new major version (`^4.2.0` → `^5.1.0`)
- Bump `libdd-trace-utils` to a new major version (`^8.0.0` → `^9.0.0`)

## [4.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v3.0.0..libdd-sampling-v4.0.0) - 2026-06-08

### Changed

- Update dependencies ([#2090](https://github.com/DataDog/libdatadog/issues/2090)) - ([9479a9a](https://github.com/datadog/libdatadog/commit/9479a9a0faf2d70b4d54045c02911d9b339b98ed))



## [3.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v2.1.0..libdd-sampling-v3.0.0) - 2026-06-05

### Changed

- Revert add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) ([#2073](https://github.com/datadog/libdatadog/issues/2073)) - ([a21e9d5](https://github.com/datadog/libdatadog/commit/a21e9d5eeeff0be4a1b9de8104a2cf2eae2be6a3))

### Fixed

- Format _dd.p.ksr to 6 decimal places, not 6 significant digits ([#2086](https://github.com/datadog/libdatadog/issues/2086)) - ([37891f1](https://github.com/datadog/libdatadog/commit/37891f1536f344cc339f2d97184038fc75a92c6b))



## [2.1.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v2.0.0..libdd-sampling-v2.1.0) - 2026-06-01

### Added

- Accept Remote Config list-shape tags natively ([#2033](https://github.com/datadog/libdatadog/issues/2033)) - ([b36be02](https://github.com/datadog/libdatadog/commit/b36be023f8c4ae03732f3455ba6f84b4d24d110c))



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-sampling-v1.0.0..libdd-sampling-v2.0.0) - 2026-05-29

### Added

- Add from_string to span text ([#2011](https://github.com/datadog/libdatadog/issues/2011)) - ([ecdca7d](https://github.com/datadog/libdatadog/commit/ecdca7d4ef4e7f11c0194ed2f4e25173973404e7))

### Fixed

- Disable libdd-common default features ([#2057](https://github.com/datadog/libdatadog/issues/2057)) - ([a75be44](https://github.com/datadog/libdatadog/commit/a75be440e8633cf64a06239dacce99bc9c234c9a))


## 1.0.0 - 2026-05-18

Initial release.
