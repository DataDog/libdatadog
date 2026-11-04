// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The `error.type` of a failed provider operation, in one bounded
//! vocabulary, so that a failure is labeled the same wherever it was
//! observed: from an HTTP status, from the error type or code a provider put
//! in its body or stream, or from the name of an SDK or transport exception.

pub const ERROR_TYPE_BAD_REQUEST: &str = "bad_request_error";
pub const ERROR_TYPE_AUTHENTICATION: &str = "authentication_error";
pub const ERROR_TYPE_PERMISSION_DENIED: &str = "permission_denied_error";
pub const ERROR_TYPE_NOT_FOUND: &str = "not_found_error";
pub const ERROR_TYPE_TIMEOUT: &str = "timeout_error";
pub const ERROR_TYPE_CONFLICT: &str = "conflict_error";
pub const ERROR_TYPE_UNPROCESSABLE_ENTITY: &str = "unprocessable_entity_error";
pub const ERROR_TYPE_RATE_LIMIT: &str = "rate_limit_error";
pub const ERROR_TYPE_INTERNAL_SERVER: &str = "internal_server_error";
pub const ERROR_TYPE_API_CONNECTION: &str = "api_connection_error";
pub const ERROR_TYPE_CANCELLED: &str = "cancelled";
/// A failure nothing more specific names.
pub const ERROR_TYPE_API: &str = "api_error";

/// The vocabulary, in no particular order.
pub const ERROR_TYPES: [&str; 12] = [
    ERROR_TYPE_BAD_REQUEST,
    ERROR_TYPE_AUTHENTICATION,
    ERROR_TYPE_PERMISSION_DENIED,
    ERROR_TYPE_NOT_FOUND,
    ERROR_TYPE_TIMEOUT,
    ERROR_TYPE_CONFLICT,
    ERROR_TYPE_UNPROCESSABLE_ENTITY,
    ERROR_TYPE_RATE_LIMIT,
    ERROR_TYPE_INTERNAL_SERVER,
    ERROR_TYPE_API_CONNECTION,
    ERROR_TYPE_CANCELLED,
    ERROR_TYPE_API,
];

/// The error names providers and SDKs report, other than the vocabulary's
/// own values, which map to themselves.
const ERROR_NAMES: [(&str, &str); 35] = [
    // Anthropic and OpenAI error types and codes.
    ("invalid_request_error", ERROR_TYPE_BAD_REQUEST),
    // Anthropic's type for HTTP 413, which has no `error.type` of its own:
    // `api_error`, as the status and an SDK's generic status error read.
    ("request_too_large", ERROR_TYPE_API),
    ("permission_error", ERROR_TYPE_PERMISSION_DENIED),
    ("overloaded_error", ERROR_TYPE_INTERNAL_SERVER),
    // SDK exception classes (OpenAI, Anthropic, LiteLLM).
    ("BadRequestError", ERROR_TYPE_BAD_REQUEST),
    ("AuthenticationError", ERROR_TYPE_AUTHENTICATION),
    ("PermissionDeniedError", ERROR_TYPE_PERMISSION_DENIED),
    ("NotFoundError", ERROR_TYPE_NOT_FOUND),
    ("ConflictError", ERROR_TYPE_CONFLICT),
    ("UnprocessableEntityError", ERROR_TYPE_UNPROCESSABLE_ENTITY),
    ("RateLimitError", ERROR_TYPE_RATE_LIMIT),
    ("InternalServerError", ERROR_TYPE_INTERNAL_SERVER),
    ("ServiceUnavailableError", ERROR_TYPE_INTERNAL_SERVER),
    ("BadGatewayError", ERROR_TYPE_INTERNAL_SERVER),
    ("APITimeoutError", ERROR_TYPE_TIMEOUT),
    ("Timeout", ERROR_TYPE_TIMEOUT),
    ("TimeoutError", ERROR_TYPE_TIMEOUT),
    ("TimeoutException", ERROR_TYPE_TIMEOUT),
    ("APIConnectionError", ERROR_TYPE_API_CONNECTION),
    // Transport failures.
    ("ConnectionError", ERROR_TYPE_API_CONNECTION),
    ("ProxyError", ERROR_TYPE_API_CONNECTION),
    ("NetworkError", ERROR_TYPE_API_CONNECTION),
    ("ProtocolError", ERROR_TYPE_API_CONNECTION),
    ("EndpointConnectionError", ERROR_TYPE_API_CONNECTION),
    ("CancelledError", ERROR_TYPE_CANCELLED),
    ("GeneratorExit", ERROR_TYPE_CANCELLED),
    ("canceled", ERROR_TYPE_CANCELLED),
    // How a gateway sees a stream end.
    ("missing_message_stop", ERROR_TYPE_API_CONNECTION),
    ("missing_terminal_event", ERROR_TYPE_API_CONNECTION),
    ("upstream_error", ERROR_TYPE_API_CONNECTION),
    ("stream_read_error", ERROR_TYPE_API_CONNECTION),
    ("early_close", ERROR_TYPE_API_CONNECTION),
    ("incomplete_stream", ERROR_TYPE_API_CONNECTION),
    ("local_error", ERROR_TYPE_API),
    ("provider_error", ERROR_TYPE_API),
];

/// The `error.type` of a failed HTTP status, or `None` below 400. A 5xx
/// without a name of its own is `internal_server_error`, any other 4xx
/// `api_error`.
pub fn error_type_for_status(status: u16) -> Option<&'static str> {
    Some(match status {
        0..=399 => return None,
        400 => ERROR_TYPE_BAD_REQUEST,
        401 => ERROR_TYPE_AUTHENTICATION,
        403 => ERROR_TYPE_PERMISSION_DENIED,
        404 => ERROR_TYPE_NOT_FOUND,
        408 | 504 => ERROR_TYPE_TIMEOUT,
        409 => ERROR_TYPE_CONFLICT,
        422 => ERROR_TYPE_UNPROCESSABLE_ENTITY,
        429 => ERROR_TYPE_RATE_LIMIT,
        500.. => ERROR_TYPE_INTERNAL_SERVER,
        _ => ERROR_TYPE_API,
    })
}

/// The `error.type` of an error name a provider, an SDK or a transport
/// reported, or `None` for a name it does not know. A value of the
/// vocabulary is itself.
pub fn error_type_for_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    ERROR_TYPES
        .iter()
        .find(|known| **known == name)
        .copied()
        .or_else(|| {
            ERROR_NAMES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, error_type)| *error_type)
        })
}

/// An exception class name as a low-cardinality identifier:
/// `RateLimitError` is `rate_limit_error`, `APIConnectionError` is
/// `api_connection_error`. For an integration that records an unknown
/// class by its own name rather than as `api_error`. `None` when the name
/// does not start with a letter.
pub fn error_type_identifier(class_name: &str) -> Option<String> {
    let chars: Vec<char> = class_name.trim().chars().collect();
    let mut identifier = String::with_capacity(chars.len() + 4);
    for (at, c) in chars.iter().enumerate() {
        let previous = at.checked_sub(1).and_then(|p| chars.get(p));
        let next = chars.get(at + 1);
        // A word starts at an upper-case letter after a lower-case letter or
        // digit (`rateLimit`), or at the last capital of an acronym followed
        // by a lower-case letter (`APIConnection`).
        let boundary = c.is_ascii_uppercase()
            && previous.is_some_and(|p| {
                p.is_ascii_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_ascii_uppercase() && next.is_some_and(char::is_ascii_lowercase))
            });
        if boundary {
            identifier.push('_');
        }
        let c = c.to_ascii_lowercase();
        identifier.push(
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            },
        );
    }
    identifier
        .starts_with(|c: char| c.is_ascii_lowercase())
        .then_some(identifier)
}

/// The `error.type` of a failure: its name when the vocabulary knows it
/// (a three-digit name is read as a status), else its HTTP status, else
/// `api_error` for a named failure. `None` when neither says it failed.
pub fn classify_error(name: Option<&str>, status: Option<u16>) -> Option<&'static str> {
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = name {
        if let Some(error_type) = error_type_for_name(name) {
            return Some(error_type);
        }
        if name.len() == 3
            && let Ok(code) = name.parse::<u16>()
            && let Some(error_type) = error_type_for_status(code)
        {
            return Some(error_type);
        }
    }
    status
        .and_then(error_type_for_status)
        .or(name.map(|_| ERROR_TYPE_API))
}
