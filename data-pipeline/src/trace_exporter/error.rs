// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_exporter::msgpack_decoder::v04::error::DecodeError;
use hyper::http::StatusCode;
use hyper::Error as HyperError;
use serde_json::error::Error as SerdeError;
use std::error::Error;
use std::fmt::{Debug, Display};

#[derive(Debug, PartialEq)]
pub enum BuilderErrorKind {
    InvalidUri,
}

impl Display for BuilderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuilderErrorKind::InvalidUri => write!(f, "Invalid URI"),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum NetworkErrorKind {
    Body,
    Canceled,
    ConnectionClosed,
    MessageTooLarge,
    Parse,
    TimedOut,
    Unknown,
    WrongStatus,
}

#[derive(Debug)]
pub struct NetworkError {
    kind: NetworkErrorKind,
    source: HyperError,
}

impl Error for NetworkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

impl NetworkError {
    fn new(kind: NetworkErrorKind, source: HyperError) -> Self {
        Self { kind, source }
    }

    pub fn kind(&self) -> NetworkErrorKind {
        self.kind
    }
}

impl Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.source().unwrap(), f)
    }
}

#[derive(Debug, PartialEq)]
pub struct RequestError {
    code: StatusCode,
    msg: String,
}

impl Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            format_args!("Error code: {}, Response: {}", self.code, self.msg)
        )
    }
}

impl RequestError {
    pub fn new(code: StatusCode, msg: &str) -> Self {
        Self {
            code,
            msg: msg.to_owned(),
        }
    }

    pub fn status(&self) -> StatusCode {
        self.code
    }
}

/// TraceExporterError holds different types of errors occurred when handling traces.
#[derive(Debug)]
pub enum TraceExporterError {
    Builder(BuilderErrorKind),
    Deserialization(DecodeError),
    Io(std::io::Error),
    Network(NetworkError),
    Request(RequestError),
    Serde(SerdeError),
}

impl From<serde_json::error::Error> for TraceExporterError {
    fn from(value: SerdeError) -> Self {
        TraceExporterError::Serde(value)
    }
}

impl From<hyper::http::uri::InvalidUri> for TraceExporterError {
    fn from(_value: hyper::http::uri::InvalidUri) -> Self {
        TraceExporterError::Builder(BuilderErrorKind::InvalidUri)
    }
}

impl From<HyperError> for TraceExporterError {
    fn from(err: HyperError) -> Self {
        if err.is_parse() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::Parse, err))
        } else if err.is_canceled() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::Canceled, err))
        } else if err.is_connect() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::ConnectionClosed, err))
        } else if err.is_parse_too_large() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::MessageTooLarge, err))
        } else if err.is_incomplete_message() || err.is_body_write_aborted() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::Body, err))
        } else if err.is_parse_status() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::WrongStatus, err))
        } else if err.is_timeout() {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::TimedOut, err))
        } else {
            TraceExporterError::Network(NetworkError::new(NetworkErrorKind::Unknown, err))
        }
    }
}

impl From<DecodeError> for TraceExporterError {
    fn from(err: DecodeError) -> Self {
        TraceExporterError::Deserialization(err)
    }
}

impl From<std::io::Error> for TraceExporterError {
    fn from(err: std::io::Error) -> Self {
        TraceExporterError::Io(err)
    }
}

impl Display for TraceExporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceExporterError::Builder(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Deserialization(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Io(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Network(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Request(e) => std::fmt::Display::fmt(e, f),
            TraceExporterError::Serde(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl Error for TraceExporterError {}
