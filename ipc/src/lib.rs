// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#[cfg(unix)]
pub mod handles;
pub mod platform;

#[cfg(unix)]
pub mod transport;

#[cfg(unix)]
pub mod example_interface;

pub use tarpc;
