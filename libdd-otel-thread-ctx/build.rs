// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;

/// Select the ownership mode (`otel_ctx_shared` cfg) and warn when both modes are requested.
///
/// `owned-context` and `shared-context` are mutually exclusive, and `owned-context` silently wins
/// (see `src/linux/mod.rs`). We report that through a build script rather than a `#[deprecated]`
/// trick or a lint, because a build-script warning doesn't turn into an error under
/// `clippy -- -D warnings`, which CI runs with `--all-features`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(otel_ctx_shared)");

    let owned = env::var_os("CARGO_FEATURE_OWNED_CONTEXT").is_some();
    let shared = env::var_os("CARGO_FEATURE_SHARED_CONTEXT").is_some();

    // `shared-context` is used only when asked for alone; otherwise (owned, both or neither) the
    // crate falls back to `owned-context`, so every feature combination builds.
    if shared && !owned {
        println!("cargo::rustc-cfg=otel_ctx_shared");
    }

    if owned && shared {
        println!(
            "cargo:warning=libdd-otel-thread-ctx: features `owned-context` and `shared-context` \
             are mutually exclusive; falling back to `owned-context` only. Build with \
             `--no-default-features --features shared-context` to get `SharedThreadContext`."
        );
    }
}
