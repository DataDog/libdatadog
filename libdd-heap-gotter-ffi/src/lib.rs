// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C FFI bindings for [`libdd_heap_gotter`]. Exposes install / update /
//! is-installed entry points as `extern "C"` functions so language
//! runtimes (Python, Ruby, …) can drive GOT-based heap profiling from
//! their own native extension code.

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
#![cfg_attr(not(test), deny(clippy::unreachable))]

// `wrap_with_void_ffi_result!` uses `function_name!()` below.
use function_name::named;
use libdd_common_ffi::{wrap_with_void_ffi_result, VoidResult};

// `libdd_heap_gotter` exposes the same public surface on every target.
// On non-Linux the underlying functions are no-ops, so callers that
// invoke these FFI entry points outside Linux observe a clean error
// from `ddog_heap_gotter_install` (nothing was overridden) without
// having to `#ifdef` their integration code.

/// Install GOT overrides for supported heap-allocation symbols in the current process.
///
/// Installation is permanent: there is no un-install (see [`libdd_heap_gotter`]
/// for why). GOT entries are patched to point at functions in this library, so
/// the library containing these hooks must remain loaded for the life of the
/// process; unloading it would leave dangling function pointers.
///
/// On non-Linux targets this returns an error indicating that nothing
/// could be installed; the rest of the API can still be called safely.
#[no_mangle]
#[must_use]
#[named]
pub extern "C" fn ddog_heap_gotter_install() -> VoidResult {
    wrap_with_void_ffi_result!({
        let installed = libdd_heap_gotter::install_heap_overrides();
        anyhow::ensure!(installed, "no heap GOT overrides could be installed");
    })
}

/// Re-scan loaded libraries and patch newly-introduced GOT entries.
///
/// This is normally called automatically by the installed `dlopen` hook, but language runtimes may
/// call it explicitly after unusual native-extension loading flows. No-op on non-Linux targets.
#[no_mangle]
#[must_use]
#[named]
pub extern "C" fn ddog_heap_gotter_update() -> VoidResult {
    wrap_with_void_ffi_result!({
        libdd_heap_gotter::update_heap_overrides();
    })
}

/// Return whether heap GOT overrides are currently installed in this process. Always `false` on
/// non-Linux targets.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_heap_gotter_is_installed() -> bool {
    libdd_heap_gotter::heap_overrides_are_installed()
}
