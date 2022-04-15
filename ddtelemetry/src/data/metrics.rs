// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct CounterGauge {
    metric: String,
    points: Vec<(u64, f64)>,
    tags: Vec<String>,
    common: bool,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum Metric {
    #[serde(rename = "gauge")]
    Gauge(CounterGauge),
    #[serde(rename = "gauge")]
    Counter(CounterGauge),
}
