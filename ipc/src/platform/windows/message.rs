// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    // The handles are to be sent before via DuplicateHandle - post-transfer reassigns the correct handle
    pub handles: HashMap<u64, u64>,
}
