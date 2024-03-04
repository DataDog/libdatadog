// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! The purpose of this module is to enable a profiler memory optimization.
//! Each `Sample` in the profile is associated with a list of `i64` values,
//! which are provided as a `Vec<i64>`.  This is wasteful, because all
//! observations for a Profile are of the same length.
//! If each map type stores its own length, then we can reduce this down to a
//! single pointer of overhead.

mod observations;
mod timestamped_observations;
mod trimmed_observation;

// We keep trimmed_observation private, to ensure that only maps can make and
// operate on trimmed objects, which helps ensure safety.
pub use observations::*;
