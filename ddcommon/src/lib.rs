// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub mod azure_app_services;
#[cfg(feature = "agent-connector")]
pub mod connector;
pub mod container_id;
#[macro_use]
pub mod cstr;
pub mod tag;

#[cfg(feature = "agent-http")]
mod http;
#[cfg(feature = "agent-http")]
pub use http::*;
