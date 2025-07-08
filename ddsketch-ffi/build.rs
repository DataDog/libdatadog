// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use build_common::generate_and_configure_header;

fn main() {
    generate_and_configure_header("datadog_ddsketch.h");
}
