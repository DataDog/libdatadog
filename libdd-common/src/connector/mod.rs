// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod errors;
pub mod named_pipe;
#[cfg(unix)]
pub mod uds;

#[cfg(feature = "http-client")]
mod conn_stream;
#[cfg(feature = "http-client")]
mod http_client;
#[cfg(feature = "http-client")]
pub use http_client::Connector;
