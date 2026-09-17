// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sidecar-style crashtracker receiver that listens on a Unix socket.
//! Used by integration tests to verify the socket-based receiver path

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    let socket_path = std::env::args()
        .nth(1)
        .expect("usage: crashtracker_unix_socket_receiver <socket_path>");
    libdd_crashtracker::receiver_entry_point_unix_socket(&socket_path)
}
