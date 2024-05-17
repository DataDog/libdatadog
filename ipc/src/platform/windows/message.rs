// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    // The handles are to be sent before via DuplicateHandle - post-transfer reassigns the correct
    // handle
    pub handles: HashMap<u64, u64>,
}
