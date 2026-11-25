// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(missing_docs)]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! Blocking transports for allocation-free HTTP clients.
//!
//! The `rustix-tcp` feature provides a TCP stream for callers using reqwless
//! on Unix platforms. It does not allocate, resolve names, or start an async
//! runtime.

pub mod dns;
pub mod env;

#[cfg(feature = "libc_dns")]
pub mod libc_dns;

#[cfg(all(feature = "rustix-tcp", unix))]
pub mod rustix;

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "rustix-tcp", unix))]
    use super::rustix;

    #[cfg(all(feature = "rustix-tcp", unix))]
    #[test]
    fn rustix_stream_supports_sync_and_async_io() {
        fn assert_sync<T: embedded_io::Read + embedded_io::Write>() {}
        fn assert_async<T: embedded_io_async::Read + embedded_io_async::Write>() {}
        fn assert_connector<T: embedded_nal_async::TcpConnect>() {}

        assert_sync::<rustix::TcpStream>();
        assert_async::<rustix::TcpStream>();
        assert_connector::<rustix::TcpConnector>();
    }
}
