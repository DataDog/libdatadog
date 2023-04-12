// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use hyper::{http, Body, Response, StatusCode};
use log::{error, info};
use serde_json::json;

/// Logs a message (at the info level) and returns the same message in the body of response with status code 200.
/// Response body:
/// {
///     "message": message
/// }
pub fn log_and_return_http_success_response(message: &str) -> http::Result<Response<Body>> {
    info!("{}", message);
    let body = json!({ "message": message }).to_string();
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(body))
}

/// Logs a message (at the error level) and returns the same message in the body of response with status code 500.
/// Response body:
/// {
///     "message": message
/// }
pub fn log_and_return_http_error_response(message: &str) -> http::Result<Response<Body>> {
    error!("{}", message);
    let body = json!({ "message": message }).to_string();
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(body))
}
