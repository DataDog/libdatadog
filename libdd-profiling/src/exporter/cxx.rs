// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for profiling exporter - provides a safe and idiomatic C++ API

use super::config;
use super::reqwest_exporter::ProfileExporter;
use crate::internal::EncodedProfile;
use libdd_common::Endpoint;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

// ============================================================================
// CXX Bridge - C++ Bindings
// ============================================================================

#[cxx::bridge(namespace = "datadog::profiling")]
pub mod ffi {
    // Shared structs
    struct ExporterConfig {
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<String>,
        endpoint_url: String,
        api_key: String,
        timeout_ms: u64,
    }

    struct ExporterFile {
        name: String,
        bytes: Vec<u8>,
    }

    // Opaque Rust types
    extern "Rust" {
        type ProfileExporter;
        type EncodedProfile;
        type CancellationToken;

        // CancellationToken methods
        #[Self = "CancellationToken"]
        fn create() -> Box<CancellationToken>;
        
        fn cancel(self: &CancellationToken);
        fn is_cancelled(self: &CancellationToken) -> bool;
        fn clone_token(self: &CancellationToken) -> Box<CancellationToken>;

        // Static factory methods
        #[Self = "ProfileExporter"]
        fn create(config: ExporterConfig) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        fn create_agent(
            profiling_library_name: String,
            profiling_library_version: String,
            family: String,
            tags: Vec<String>,
            agent_url: String,
        ) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        fn create_agentless(
            profiling_library_name: String,
            profiling_library_version: String,
            family: String,
            tags: Vec<String>,
            site: String,
            api_key: String,
        ) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        fn create_file(
            profiling_library_name: String,
            profiling_library_version: String,
            family: String,
            tags: Vec<String>,
            file_path: String,
        ) -> Result<Box<ProfileExporter>>;

        // ProfileExporter async method - blocking wrapper for C++
        fn send_blocking(
            self: &ProfileExporter,
            profile: Box<EncodedProfile>,
            additional_files: Vec<ExporterFile>,
            additional_tags: Vec<String>,
        ) -> Result<u16>;

        // ProfileExporter with cancellation support
        fn send_blocking_with_cancel(
            self: &ProfileExporter,
            profile: Box<EncodedProfile>,
            additional_files: Vec<ExporterFile>,
            additional_tags: Vec<String>,
            cancel_token: &CancellationToken,
        ) -> Result<u16>;

        // EncodedProfile factory (separate function to avoid name collision)
        fn create_test_profile() -> Result<Box<EncodedProfile>>;
    }
}

// ============================================================================
// Static Factory Methods
// ============================================================================

impl ProfileExporter {
    pub fn create(config: ffi::ExporterConfig) -> anyhow::Result<Box<ProfileExporter>> {
        // Parse the endpoint URL
        let endpoint = if config.endpoint_url.starts_with("file://") {
            let path = config.endpoint_url.strip_prefix("file://").unwrap();
            config::file(path)?
        } else {
            let url = config.endpoint_url.parse()?;
            Endpoint {
                url,
                api_key: if config.api_key.is_empty() {
                    None
                } else {
                    Some(config.api_key.into())
                },
                timeout_ms: config.timeout_ms,
                test_token: None,
            }
        };

        // Parse tags using parse_tags function
        let tags_str = config.tags.join(",");
        let (tags, parse_error) = libdd_common::tag::parse_tags(&tags_str);
        if let Some(err) = parse_error {
            anyhow::bail!("Tag parsing error: {}", err);
        }

        let exporter = ProfileExporter::new(
            &config.profiling_library_name,
            &config.profiling_library_version,
            &config.family,
            tags,
            endpoint,
        )?;

        Ok(Box::new(exporter))
    }

    pub fn create_agent(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<String>,
        agent_url: String,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let url = agent_url.parse()?;
        let endpoint = config::agent(url)?;

        let tags_str = tags.join(",");
        let (tag_vec, parse_error) = libdd_common::tag::parse_tags(&tags_str);
        if let Some(err) = parse_error {
            anyhow::bail!("Tag parsing error: {}", err);
        }

        let exporter = ProfileExporter::new(
            &profiling_library_name,
            &profiling_library_version,
            &family,
            tag_vec,
            endpoint,
        )?;

        Ok(Box::new(exporter))
    }

    pub fn create_agentless(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<String>,
        site: String,
        api_key: String,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let endpoint = config::agentless(site, api_key)?;

        let tags_str = tags.join(",");
        let (tag_vec, parse_error) = libdd_common::tag::parse_tags(&tags_str);
        if let Some(err) = parse_error {
            anyhow::bail!("Tag parsing error: {}", err);
        }

        let exporter = ProfileExporter::new(
            &profiling_library_name,
            &profiling_library_version,
            &family,
            tag_vec,
            endpoint,
        )?;

        Ok(Box::new(exporter))
    }

    pub fn create_file(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<String>,
        file_path: String,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let endpoint = config::file(file_path)?;

        let tags_str = tags.join(",");
        let (tag_vec, parse_error) = libdd_common::tag::parse_tags(&tags_str);
        if let Some(err) = parse_error {
            anyhow::bail!("Tag parsing error: {}", err);
        }

        let exporter = ProfileExporter::new(
            &profiling_library_name,
            &profiling_library_version,
            &family,
            tag_vec,
            endpoint,
        )?;

        Ok(Box::new(exporter))
    }

    /// Helper function to implement blocking send with optional cancellation
    fn send_blocking_impl(
        &self,
        profile: Box<EncodedProfile>,
        additional_files: Vec<ffi::ExporterFile>,
        additional_tags: Vec<String>,
        cancel_token: Option<&TokioCancellationToken>,
    ) -> anyhow::Result<u16> {
        // Convert ExporterFile to the internal File type
        let files: Vec<super::reqwest_exporter::File> = additional_files
            .iter()
            .map(|f| super::reqwest_exporter::File {
                name: f.name.as_str(),
                bytes: &f.bytes,
            })
            .collect();

        // Convert tags
        let tags_str = additional_tags.join(",");
        let (tags, parse_error) = libdd_common::tag::parse_tags(&tags_str);
        if let Some(err) = parse_error {
            anyhow::bail!("Tag parsing error: {}", err);
        }

        // Create a tokio runtime for this blocking call
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        // Block on the async send
        let status = rt.block_on(async {
            self.send(*profile, &files, &tags, None, None, cancel_token).await
        })?;

        Ok(status.as_u16())
    }

    /// Blocking wrapper for the async send method
    pub fn send_blocking(
        &self,
        profile: Box<EncodedProfile>,
        additional_files: Vec<ffi::ExporterFile>,
        additional_tags: Vec<String>,
    ) -> anyhow::Result<u16> {
        self.send_blocking_impl(profile, additional_files, additional_tags, None)
    }

    /// Blocking wrapper for the async send method with cancellation support
    pub fn send_blocking_with_cancel(
        &self,
        profile: Box<EncodedProfile>,
        additional_files: Vec<ffi::ExporterFile>,
        additional_tags: Vec<String>,
        cancel_token: &CancellationToken,
    ) -> anyhow::Result<u16> {
        self.send_blocking_impl(profile, additional_files, additional_tags, Some(&cancel_token.0))
    }
}

// ============================================================================
// CancellationToken Wrapper
// ============================================================================

/// Wrapper around Tokio's CancellationToken for CXX
#[derive(Clone)]
pub struct CancellationToken(TokioCancellationToken);

impl CancellationToken {
    pub fn create() -> Box<CancellationToken> {
        Box::new(CancellationToken(TokioCancellationToken::new()))
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    pub fn clone_token(&self) -> Box<CancellationToken> {
        Box::new(CancellationToken(self.0.clone()))
    }
}

// Free function for creating test profile to avoid name collision
pub fn create_test_profile() -> anyhow::Result<Box<EncodedProfile>> {
    Ok(Box::new(EncodedProfile::test_instance()?))
}

