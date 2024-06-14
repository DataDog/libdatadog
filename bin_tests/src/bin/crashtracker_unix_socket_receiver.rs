// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 2,
        "Usage: crashtracker_unix_socket_receiver path_to_unix_socket"
    );
    datadog_crashtracker::reciever_entry_point_unix_socket(&args[1])
}
