// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use tools::headers::dedup_headers;

/// Usage:
/// ./dedup_headers <base_header> <child_headers>...
///
/// All type definitions will be removed from the child_headers, and moved to the base_header
/// if they are not already defined in the parent_header
fn main() {
    let args: Vec<_> = std::env::args_os()
        .flat_map(|arg| arg.into_string())
        .collect();
    dedup_headers(&args[1], &args[2..]);
}
