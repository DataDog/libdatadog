# libdd-library-config-ffi

C-compatible FFI bindings for libdd-library-config.

## Overview

`libdd-library-config-ffi` provides Foreign Function Interface (FFI) bindings that expose the functionality of `libdd-library-config` to C and other non-Rust languages.

The `config-core` feature exposes a startup resolver that accepts the same
[configuration requests](../libdd-config-core/README.md) as the WASI reactor.
Enable the `config-core` feature on the release builder to include this API
in the C distribution. The release builder defines `DDOG_HAS_CONFIG_CORE`
in the installed header. If you build the FFI crate directly with
`config-core`, define `DDOG_HAS_CONFIG_CORE` when compiling the C caller.
The caller owns the resolver and releases it with `ddog_config_core_drop`.
Each request borrows its input and returns an owned string or error. Use the
corresponding `common.h` drop function after consuming the result.
