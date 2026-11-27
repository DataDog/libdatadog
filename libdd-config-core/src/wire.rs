// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};

use crate::{
    ConfigSchema, Diagnostic, Error, ParsePolicy, PendingResolution, Resolution, ResolveInputs,
    ResolvedConfig, SourceEvent, Transform, TransformRegistry, TransformRequest, TransformResult,
    ValueKind,
};

#[derive(Default)]
pub struct WireResolver {
    schema: Option<ConfigSchema>,
    pending: Option<PendingResolution>,
}

#[derive(Deserialize)]
struct Request {
    version: u32,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation {
    Initialize {
        manifest: Manifest,
        #[serde(default)]
        policy: ParsePolicy,
        #[serde(default)]
        host_transforms: Vec<HostTransform>,
        #[serde(default)]
        inputs: ResolveInputs,
    },
    Resolve {
        inputs: ResolveInputs,
    },
    Resume {
        outcomes: Vec<TransformResult>,
    },
    Cancel,
}

#[derive(Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Manifest {
    Path(PathBuf),
    Json(String),
}

#[derive(Deserialize)]
struct HostTransform {
    name: String,
    input: ValueKind,
    output: ValueKind,
    slot: u32,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply<'a> {
    Complete {
        settings: Settings<'a>,
        diagnostics: &'a [Diagnostic],
        sources: &'a [SourceEvent],
    },
    Pending {
        requests: &'a [TransformRequest],
    },
    Cancelled,
    Error {
        message: String,
    },
}

struct Settings<'a>(&'a ResolvedConfig);

impl Serialize for Settings<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let settings = self.0.settings();
        let mut sequence = serializer.serialize_seq(Some(settings.len()))?;
        for setting in settings {
            sequence.serialize_element(&setting)?;
        }
        sequence.end()
    }
}

#[derive(Debug, thiserror::Error)]
enum WireError {
    #[error("invalid configuration request")]
    InvalidRequest(#[from] serde_json::Error),
    #[error("unsupported configuration protocol version {0}")]
    Version(u32),
    #[error("start by initializing a configuration manifest")]
    MissingSchema,
    #[error("there is no pending configuration")]
    MissingPending,
    #[error("resume or cancel the pending configuration first")]
    Busy,
    #[error(transparent)]
    Configuration(#[from] Error),
}

impl WireResolver {
    /// Send a version 1 request and receive one JSON reply.
    ///
    /// # Errors
    /// Returns an error if the reply cannot be serialized.
    pub fn request(&mut self, request: &[u8]) -> Result<Vec<u8>, serde_json::Error> {
        match self.handle(request) {
            Ok(response) => Ok(response),
            Err(error) => serde_json::to_vec(&Reply::Error {
                message: error.to_string(),
            }),
        }
    }

    fn handle(&mut self, request: &[u8]) -> Result<Vec<u8>, WireError> {
        let request: Request = serde_json::from_slice(request)?;
        if request.version != 1 {
            return Err(WireError::Version(request.version));
        }
        if self.pending.is_some()
            && !matches!(
                request.operation,
                Operation::Resume { .. } | Operation::Cancel
            )
        {
            return Err(WireError::Busy);
        }

        match request.operation {
            Operation::Initialize {
                manifest,
                policy,
                host_transforms,
                inputs,
            } => {
                let mut transforms = TransformRegistry::with_builtins();
                for transform in host_transforms {
                    transforms.register(
                        transform.name,
                        Transform::host(transform.input, transform.output, transform.slot),
                    )?;
                }
                let schema = match manifest {
                    Manifest::Path(path) => ConfigSchema::from_path(path, policy, &transforms)?,
                    Manifest::Json(json) => {
                        ConfigSchema::from_bytes(json.as_bytes(), policy, &transforms)?
                    }
                };
                let result = schema.resolve(inputs)?;
                let response = self.reply(result)?;
                self.schema = Some(schema);
                Ok(response)
            }
            Operation::Resolve { inputs } => {
                let schema = self.schema.as_ref().ok_or(WireError::MissingSchema)?;
                self.reply(schema.resolve(inputs)?).map_err(WireError::from)
            }
            Operation::Resume { outcomes } => {
                let pending = self.pending.take().ok_or(WireError::MissingPending)?;
                let snapshot = pending.resume(outcomes)?;
                Ok(snapshot_reply(&snapshot)?)
            }
            Operation::Cancel => {
                self.pending = None;
                Ok(serde_json::to_vec(&Reply::Cancelled)?)
            }
        }
    }

    fn reply(&mut self, resolution: Resolution) -> Result<Vec<u8>, serde_json::Error> {
        match resolution {
            Resolution::Complete(snapshot) => snapshot_reply(&snapshot),
            Resolution::Pending(pending) => {
                let response = serde_json::to_vec(&Reply::Pending {
                    requests: pending.requests(),
                })?;
                self.pending = Some(pending);
                Ok(response)
            }
        }
    }
}

fn snapshot_reply(snapshot: &ResolvedConfig) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&Reply::Complete {
        settings: Settings(snapshot),
        diagnostics: snapshot.diagnostics(),
        sources: snapshot.source_events(),
    })
}
