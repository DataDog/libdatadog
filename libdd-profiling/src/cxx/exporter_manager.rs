// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::errors::ExporterManagerResult;
use super::ffi;
use super::profile::{EncodedProfile, Profile};
use super::profile_exporter::{prepare_export_args, prepare_profile_for_export, ProfileExporter};
use crate::exporter;
use crate::internal;

pub struct ExporterManager {
    pub(crate) inner: exporter::ExporterManager,
}

impl ExporterManager {
    pub fn create(exporter: Box<ProfileExporter>) -> Box<ExporterManagerResult> {
        ExporterManagerResult::from_result(
            "ExporterManager::create",
            (|| -> anyhow::Result<Box<ExporterManager>> {
                let inner = exporter::ExporterManager::new(exporter.inner)?;
                Ok(Box::new(ExporterManager { inner }))
            })(),
        )
    }

    /// Queue a profile to be sent asynchronously by the background worker thread.
    ///
    /// Resets the profile and queues the previous profile data for sending. This allows
    /// continuous profiling where you keep adding samples to the current profile while the
    /// previous period's data is being sent asynchronously.
    pub fn queue_profile(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        let result = prepare_profile_for_export(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )
        .and_then(
            |(
                encoded,
                files_to_compress_vec,
                additional_tags_vec,
                process_tags_opt,
                internal_metadata_json,
                info_json,
            )| {
                let EncodedProfile { inner: encoded } = *encoded;
                self.queue_encoded_profile_prepared(
                    encoded,
                    &files_to_compress_vec,
                    &additional_tags_vec,
                    process_tags_opt,
                    internal_metadata_json,
                    info_json,
                )
            },
        );
        ffi::Status::from_result("ExporterManager::queue_profile", result)
    }

    #[allow(clippy::boxed_local)]
    pub fn queue_encoded_profile(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        let result = prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )
        .and_then(
            |(
                files_to_compress_vec,
                additional_tags_vec,
                process_tags_opt,
                internal_metadata_json,
                info_json,
            )| {
                let EncodedProfile { inner: encoded } = *encoded;
                self.queue_encoded_profile_prepared(
                    encoded,
                    &files_to_compress_vec,
                    &additional_tags_vec,
                    process_tags_opt,
                    internal_metadata_json,
                    info_json,
                )
            },
        );
        ffi::Status::from_result("ExporterManager::queue_encoded_profile", result)
    }

    fn queue_encoded_profile_prepared(
        &self,
        encoded: internal::EncodedProfile,
        files_to_compress: &[exporter::File<'_>],
        additional_tags: &[libdd_common::tag::Tag],
        process_tags: Option<&str>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.inner.queue(
            encoded,
            files_to_compress,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;

        Ok(())
    }

    pub fn abort(&mut self) -> ffi::Status {
        ffi::Status::from_result("ExporterManager::abort", self.inner.abort())
    }

    pub fn prefork(&mut self) -> ffi::Status {
        ffi::Status::from_result("ExporterManager::prefork", self.inner.prefork())
    }

    pub fn postfork_child(&mut self) -> ffi::Status {
        ffi::Status::from_result(
            "ExporterManager::postfork_child",
            self.inner.postfork_child(),
        )
    }

    pub fn postfork_parent(&mut self) -> ffi::Status {
        ffi::Status::from_result(
            "ExporterManager::postfork_parent",
            self.inner.postfork_parent(),
        )
    }
}
