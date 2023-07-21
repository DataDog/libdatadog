// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#[cfg(not(windows))]
pub mod config;
#[cfg(not(windows))]
pub mod interface;
#[cfg(not(windows))]
pub mod setup;

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::*;
