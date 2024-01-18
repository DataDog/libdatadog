// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
#![cfg(unix)]

use datadog_profiling::crashtracker::receiver_entry_point;

pub fn main() -> anyhow::Result<()> {
    receiver_entry_point()
}
