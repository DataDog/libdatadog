// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Provides _some_ migration paths from the ddcommon
pub mod compat;

/// The http module has types and functions for working with HTTP requests
/// through hyper and tokio. Generally, we do not need asynchronous execution,
/// but we do need features like HTTP over UNIX Domain Sockets (UDS) and
/// Windows Named Pipes. This aims to provide a simple API for doing blocking,
/// synchronous HTTP calls with all the different connectors we support.
pub mod http;

/// This module exports some dependencies so that crates depending on this
/// one do not also have to directly depend on and manage the versions.
pub mod dep {
    pub use hex;
    pub use http;
    pub use hyper;
    pub use tokio;
}

/// Holds a function to create a Tokio Runtime for the current thread.
/// Note that currently it will still use a thread pool for certain operations
/// which block.
pub mod rt {
    use std::io;
    use tokio::runtime;
    use tokio_util::sync::CancellationToken;

    /// Creates a tokio runtime for the current thread. This is the expected
    /// way to create a runtime used by this crate.
    pub fn create_current_thread_runtime() -> io::Result<runtime::Runtime> {
        runtime::Builder::new_current_thread().enable_all().build()
    }

    pub fn create_cancellation_token() -> CancellationToken {
        CancellationToken::new()
    }
}
