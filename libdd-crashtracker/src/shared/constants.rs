// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::time::Duration;

use super::defaults::DD_CRASHTRACK_DEFAULT_TIMEOUT_SECS;

pub use crate::protocol::*;

pub const DD_CRASHTRACK_DEFAULT_TIMEOUT: Duration =
    Duration::from_secs(DD_CRASHTRACK_DEFAULT_TIMEOUT_SECS as u64);
