// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    pub pid: libc::pid_t,
}
