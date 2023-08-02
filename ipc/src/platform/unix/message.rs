// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::os::unix::prelude::RawFd;

use serde::{Deserialize, Serialize};

use crate::handles::{HandlesTransport, TransferHandles};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
pub const MAX_FDS: usize = 20;

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    pub acked_handles: Vec<RawFd>,
    pub pid: libc::pid_t,
}
