// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use hyper::{http, Body, Response, StatusCode};
use log::{error, info};
use serde_json::json;

/// Does two things:
/// - Logs the given message. A success status code (within 200-299) will cause an info log to be written,
/// otherwise error will be written. Returns the given message in the body of JSON response with the given status code.
/// Response body format:
/// {
///     "message": message
/// }
pub fn log_and_create_http_response(
    message: &str,
    status: StatusCode,
) -> http::Result<Response<Body>> {
    if status.is_success() {
        info!("{message}");
    } else {
        error!("{message}");
    }
    let body = json!({ "message": message }).to_string();
    Response::builder().status(status).body(Body::from(body))
}
