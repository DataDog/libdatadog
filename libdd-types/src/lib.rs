// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

pub mod tag;

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::{header::HeaderName, HeaderValue};

    pub const APPLICATION_MSGPACK_STR: &str = "application/msgpack";
    pub const APPLICATION_PROTOBUF_STR: &str = "application/x-protobuf";

    pub const DATADOG_CONTAINER_ID: HeaderName = HeaderName::from_static("datadog-container-id");
    pub const DATADOG_ENTITY_ID: HeaderName = HeaderName::from_static("datadog-entity-id");
    pub const DATADOG_EXTERNAL_ENV: HeaderName = HeaderName::from_static("datadog-external-env");
    pub const DATADOG_TRACE_COUNT: HeaderName = HeaderName::from_static("x-datadog-trace-count");
    /// Signal to the agent to send 429 responses when a payload is dropped
    /// If this is not set then the agent will always return a 200 regardless if the payload is
    /// dropped.
    pub const DATADOG_SEND_REAL_HTTP_STATUS: HeaderName =
        HeaderName::from_static("datadog-send-real-http-status");
    pub const DATADOG_API_KEY: HeaderName = HeaderName::from_static("dd-api-key");
    pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
    pub const APPLICATION_MSGPACK: HeaderValue = HeaderValue::from_static(APPLICATION_MSGPACK_STR);
    pub const APPLICATION_PROTOBUF: HeaderValue =
        HeaderValue::from_static(APPLICATION_PROTOBUF_STR);
    pub const X_DATADOG_TEST_SESSION_TOKEN: HeaderName =
        HeaderName::from_static("x-datadog-test-session-token");
}

// Used by tag! macro
pub use const_format;
