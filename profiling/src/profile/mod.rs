// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub mod api;
pub mod internal;
pub mod pprof;
pub mod profiled_endpoints;

pub type Timestamp = std::num::NonZeroI64;
pub type TimestampedObservation = (Timestamp, Box<[i64]>);
