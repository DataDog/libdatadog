// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use core::fmt::Debug;
use serde::{Deserialize, Serialize};
use std::fmt::Formatter;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    name: String,
    value: f64,
}

impl Debug for Metric {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metric")
            .field("name", &self.name)
            .field("value", &self.value)
            .finish()
    }
}

impl Metric {
    pub fn new(name: String, value: f64) -> Result<Self, &'static str> {
        if name.is_empty() {
            Err("empty metric name is not valid")
        } else {
            Ok(Metric { name, value })
        }
    }
}
