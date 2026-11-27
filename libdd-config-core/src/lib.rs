// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Compile a tracer's effective configuration manifest and resolve ordered settings sources.

extern crate alloc;

mod resolve;
mod schema;
mod transforms;
mod value;
mod wire;

use std::path::PathBuf;

pub use wire::WireResolver;

pub use resolve::{
    AliasOrder, Diagnostic, DiagnosticKind, Entry, InputValue, Layer, Origin, PendingResolution,
    Resolution, ResolveInputs, ResolvedConfig, ResolvedValue, Setting, SourceEvent,
    TransformRequest,
};
pub use schema::ConfigSchema;
pub use transforms::{
    Transform, TransformContext, TransformFn, TransformRegistry, TransformResult,
};
pub use value::{
    BooleanSyntax, Infinity, MapSyntax, Number, NumberSyntax, ParsePolicy, Value, ValueKind,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read configuration manifest {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not decode configuration manifest")]
    Json(#[from] serde_json::Error),
    #[error("unsupported configuration manifest version {0}")]
    UnsupportedManifest(String),
    #[error("expected one effective definition for {0}")]
    AmbiguousDefinition(String),
    #[error("ambiguous configuration name {0}")]
    AmbiguousName(String),
    #[error("invalid default value for {0}")]
    InvalidDefault(String),
    #[error("could not compile allowed regexp for {key}")]
    Regex {
        key: String,
        #[source]
        source: libdd_regex::Error,
    },
    #[error("allowed regexp requires a scalar setting for {0}")]
    UnsupportedValidationKind(String),
    #[error("transform {0} is already registered for that input type")]
    DuplicateTransform(String),
    #[error("no compatible binding for transform {name} on {key}")]
    UnboundTransform { key: String, name: String },
    #[error("transform returned an incompatible value for {0}")]
    InvalidTransformResult(String),
    #[error("host returned {actual} transform results, expected {expected}")]
    TransformCount { expected: usize, actual: usize },
}
