// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Unified exporter supporting both Hyper and Reqwest backends

use super::{HyperProfileExporter, HyperRequest};
use anyhow::Result;
use libdd_common::tag::Tag;
use libdd_common::{hyper_migration, Endpoint, HttpResponse};
use std::borrow::Cow;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::internal::EncodedProfile;

/// Backend implementation selector
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Hyper,
    Reqwest,
}

impl Default for BackendType {
    fn default() -> Self {
        Self::Hyper
    }
}

/// Unified exporter supporting multiple backends
pub enum ProfileExporter {
    Hyper(Box<HyperProfileExporter>),
    Reqwest(Box<ReqwestExporterData>),
}

/// cbindgen:ignore
pub struct ReqwestExporterData {
    inner: super::reqwest_exporter::ProfileExporter,
    runtime: Runtime,
    timeout_ms: u64,
}

/// Unified request type
pub enum Request {
    Hyper(HyperRequest),
    Reqwest(ReqwestRequestData),
}

/// Captured data for reqwest send (since reqwest doesn't have separate build/send)
/// cbindgen:ignore
pub struct ReqwestRequestData {
    profile: EncodedProfile,
    files: Vec<(String, Vec<u8>)>,
    tags: Vec<Tag>,
    internal_metadata: Option<serde_json::Value>,
    info: Option<serde_json::Value>,
}

impl ProfileExporter {
    /// Creates a new exporter with the specified backend
    pub fn new<F, N, V>(
        profiling_library_name: N,
        profiling_library_version: V,
        family: F,
        tags: Option<Vec<Tag>>,
        endpoint: Endpoint,
        backend: BackendType,
    ) -> Result<Self>
    where
        F: Into<Cow<'static, str>>,
        N: Into<Cow<'static, str>>,
        V: Into<Cow<'static, str>>,
    {
        match backend {
            BackendType::Hyper => Ok(Self::Hyper(Box::new(HyperProfileExporter::new(
                profiling_library_name,
                profiling_library_version,
                family,
                tags,
                endpoint,
            )?))),
            BackendType::Reqwest => {
                let name = profiling_library_name.into();
                let version = profiling_library_version.into();
                let fam = family.into();
                let timeout_ms = endpoint.timeout_ms;

                Ok(Self::Reqwest(Box::new(ReqwestExporterData {
                    inner: super::reqwest_exporter::ProfileExporter::new(
                        &name,
                        &version,
                        &fam,
                        tags.unwrap_or_default(),
                        endpoint,
                    )?,
                    runtime: tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?,
                    timeout_ms,
                })))
            }
        }
    }

    pub fn build(
        &self,
        profile: EncodedProfile,
        files: &[super::File],
        additional_tags: Option<&Vec<Tag>>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
    ) -> Result<Request> {
        match self {
            Self::Hyper(exporter) => Ok(Request::Hyper(exporter.build(
                profile,
                files,
                additional_tags,
                internal_metadata,
                info,
            )?)),
            Self::Reqwest(_) => {
                // Capture data for later sending
                Ok(Request::Reqwest(ReqwestRequestData {
                    profile,
                    files: files
                        .iter()
                        .map(|f| (f.name.to_string(), f.bytes.to_vec()))
                        .collect(),
                    tags: additional_tags.cloned().unwrap_or_default(),
                    internal_metadata,
                    info,
                }))
            }
        }
    }

    pub fn send(
        &self,
        request: Request,
        cancel: Option<&CancellationToken>,
    ) -> Result<HttpResponse> {
        match (self, request) {
            (Self::Hyper(exporter), Request::Hyper(req)) => exporter.send(req, cancel),
            (Self::Reqwest(exporter_data), Request::Reqwest(req_data)) => {
                // Convert captured data back to borrowed slices
                let files: Vec<super::reqwest_exporter::File> = req_data
                    .files
                    .iter()
                    .map(|(name, bytes)| super::reqwest_exporter::File { name, bytes })
                    .collect();

                let status = exporter_data.runtime.block_on(exporter_data.inner.send(
                    req_data.profile,
                    &files,
                    &req_data.tags,
                    req_data.internal_metadata,
                    req_data.info,
                    cancel,
                ))?;

                // Convert status to HttpResponse
                Ok(hyper::Response::builder()
                    .status(status.as_u16())
                    .body(hyper_migration::Body::empty())?)
            }
            _ => anyhow::bail!("Backend and request type mismatch"),
        }
    }

    pub fn set_timeout(&mut self, timeout_ms: u64) {
        match self {
            Self::Hyper(exporter) => exporter.set_timeout(timeout_ms),
            Self::Reqwest(data) => {
                data.timeout_ms = timeout_ms;
            }
        }
    }

    pub fn backend_type(&self) -> BackendType {
        match self {
            Self::Hyper(_) => BackendType::Hyper,
            Self::Reqwest(_) => BackendType::Reqwest,
        }
    }
}


// Test helper methods for inspecting the unified Request enum
// These only work with Hyper requests and will panic for Reqwest requests
impl Request {
    #[doc(hidden)]
    #[allow(clippy::panic)] // These are test-only helpers
    pub fn timeout(&self) -> &Option<std::time::Duration> {
        match self {
            Request::Hyper(req) => req.timeout(),
            Request::Reqwest(_) => &None, // Timeout handled in exporter
        }
    }

    #[doc(hidden)]
    #[allow(clippy::panic)]
    pub fn uri(&self) -> &hyper::Uri {
        match self {
            Request::Hyper(req) => req.uri(),
            Request::Reqwest(_) => panic!("uri() is not supported for Reqwest requests"),
        }
    }

    #[doc(hidden)]
    #[allow(clippy::panic)]
    pub fn headers(&self) -> &hyper::HeaderMap {
        match self {
            Request::Hyper(req) => req.headers(),
            Request::Reqwest(_) => panic!("headers() is not supported for Reqwest requests"),
        }
    }

    #[doc(hidden)]
    #[allow(clippy::panic)]
    pub fn body(self) -> crate::exporter::hyper_migration::Body {
        match self {
            Request::Hyper(req) => req.body(),
            Request::Reqwest(_) => panic!("body() is not supported for Reqwest requests"),
        }
    }
}

