# Changelog

## 1.0.2 (unreleased)

### Patch changes
- Fixed memory safety bug in `ResolutionDetails` where borrowed pointers in `flag_metadata` and `extra_logging` were becoming dangling after the source data was moved.

## 1.0.1

### Patch changes
- Bundle datadog-ffe-ffi with the rest of libdatadog release. In preparation for this, we also fixed a couple of smaller issues and inconsistencies.

## 1.0.0

Initial release.
