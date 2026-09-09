# Changelog



## [2.0.0](https://github.com/datadog/libdatadog/compare/libdd-ffe-v1.0.0..libdd-ffe-v2.0.0) - 2026-09-08

### Added

- Support arbitrary semver core parts ([#2413](https://github.com/datadog/libdatadog/issues/2413)) - ([5eb40d6](https://github.com/datadog/libdatadog/commit/5eb40d6d2ae69dac6b7a21182aee7b273d535a36))
- Send the split serial id on exposure events [EX-3425] ([#2402](https://github.com/datadog/libdatadog/issues/2402)) - ([0c0c60b](https://github.com/datadog/libdatadog/commit/0c0c60b968e7f4f2f6e43fffc3f7a1710c1eade5))
- Expose observeFullEvaluationData config-level FFI getter ([#2373](https://github.com/datadog/libdatadog/issues/2373)) - ([378be45](https://github.com/datadog/libdatadog/commit/378be45c30e9c62a1203c4cc2069aaaf8d1f4673))

### Changed

- Migrate HTTP & networking deps to workspace level (phase 4bis) ([#2350](https://github.com/datadog/libdatadog/issues/2350)) - ([55cdf67](https://github.com/datadog/libdatadog/commit/55cdf67b720b7df427b1febd147b0f72d486167f))
- Skip/shorten slow miri jobs ([#2331](https://github.com/datadog/libdatadog/issues/2331)) - ([52b616d](https://github.com/datadog/libdatadog/commit/52b616d3a865583c28b9e422c832a4118446ac72))

### Fixed

- Report rejected flags as parse errors ([#2339](https://github.com/datadog/libdatadog/issues/2339)) - ([b748d66](https://github.com/datadog/libdatadog/commit/b748d66e319b7757a2953f4a7857cee67d29c87c))

## 1.0.0

Initial release.
