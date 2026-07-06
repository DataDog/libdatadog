// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

pub use crate::protocol::*;

pub const DD_CRASHTRACK_DEFAULT_TIMEOUT: Duration = Duration::from_millis(5_000);
