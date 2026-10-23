// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(not(unix), not(feature = "receiver")))]
fn main() {}

#[cfg(unix)]
#[cfg(feature = "receiver")]
fn main() -> anyhow::Result<()> {
    libdd_crashtracker::receiver_entry_point_stdin()
}
