// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() -> anyhow::Result<()> {
    datadog_crashtracker::receiver_entry_point_stdin()
}
