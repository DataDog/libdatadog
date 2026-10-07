# Changelog



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-otel-thread-ctx-v1.1.0..libdd-otel-thread-ctx-v2.0.0) - 2026-10-01

### Added

- Add autoclean feature to avoid leaks ([#2566](https://github.com/datadog/libdatadog/issues/2566)) - ([89ee2a8](https://github.com/datadog/libdatadog/commit/89ee2a8b042db71e99017d5156facf5dd897c36b))
- Add shared thread context ([#2168](https://github.com/datadog/libdatadog/issues/2168)) - ([fed3d2e](https://github.com/datadog/libdatadog/commit/fed3d2effa94ff083833eb38df85d2e38ff65a6f))

### Changed

- Clarify ID representation ([#2581](https://github.com/datadog/libdatadog/issues/2581)) - ([679f332](https://github.com/datadog/libdatadog/commit/679f33240a213e97c47639c36ef9a827d35bb87e))
- Update workspace to Rust 2024 edition ([#2575](https://github.com/datadog/libdatadog/issues/2575)) - ([620a212](https://github.com/datadog/libdatadog/commit/620a212fc91e128234f48b29d657c71d6ad5caed))
- Run clippy with default features too ([#2590](https://github.com/datadog/libdatadog/issues/2590)) - ([2154280](https://github.com/datadog/libdatadog/commit/2154280ae6c70d9e98a031c00f443c80ab4e1834))
- Move all remaining external deps to workspace-level dependencies ([#2476](https://github.com/datadog/libdatadog/issues/2476)) - ([ea4379c](https://github.com/datadog/libdatadog/commit/ea4379c9ca0dd024c7a3ed5c497fa8e117018d6a))



## [1.1.0](https://github.com/datadog/libdatadog/compare/libdd-otel-thread-ctx-v1.0.0..libdd-otel-thread-ctx-v1.1.0) - 2026-09-08

### Added

- Add update-and-attach operation ([#2443](https://github.com/datadog/libdatadog/issues/2443)) - ([684f117](https://github.com/datadog/libdatadog/commit/684f1174fb17394be31746941e5096303226b1a1))

## Unreleased

- Add `ThreadContext::update_and_attach` and `ddog_otel_thread_ctx_update_and_attach` for updating named thread context before attaching it.

## 1.0.0 - 2026-04-10

Initial release.
