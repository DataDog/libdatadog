// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C-FFI bindings for the reqwest-based profiling exporter

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use function_name::named;
use libdd_common::tag::Tag;
use libdd_common_ffi::slice::{AsBytes, CharSlice, Slice};
use libdd_common_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};
use libdd_profiling::exporter::reqwest_exporter::ProfileExporter;
use libdd_profiling::internal::EncodedProfile;

type TokioCancellationToken = tokio_util::sync::CancellationToken;

// Re-export types from the main exporter module
pub use super::exporter::{File, HttpStatus, ProfilingEndpoint};

/// Creates a new reqwest-based exporter to be used to report profiling data.
///
/// This is a modern async exporter using reqwest instead of hyper directly.
///
/// # Arguments
/// * `profiling_library_name` - Profiling library name, usually dd-trace-something, e.g.
///   "dd-trace-rb". See
///   https://datadoghq.atlassian.net/wiki/spaces/PROF/pages/1538884229/Client#Header-values
///   (Datadog internal link)
///   for a list of common values.
/// * `profiling_library_version` - Version used when publishing the profiling library to a package
///   manager
/// * `family` - Profile family, e.g. "ruby"
/// * `tags` - Tags to include with every profile reported by this exporter. It's also possible to
///   include profile-specific tags, see `additional_tags` on `ddog_prof_ReqwestExporter_send`.
/// * `endpoint` - Configuration for reporting data
///
/// # Safety
/// All pointers must refer to valid objects of the correct types.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_ReqwestExporter_new(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&libdd_common_ffi::Vec<Tag>>,
    endpoint: ProfilingEndpoint,
) -> Result<Handle<ProfileExporter>> {
    wrap_with_ffi_result!({
        let library_name = profiling_library_name.to_utf8_lossy().into_owned();
        let library_version = profiling_library_version.to_utf8_lossy().into_owned();
        let family = family.to_utf8_lossy().into_owned();
        let converted_endpoint = unsafe { super::exporter::try_to_endpoint(endpoint)? };
        let tags = tags.map(|tags| tags.iter().cloned().collect()).unwrap_or_default();
        
        anyhow::Ok(
            ProfileExporter::new(
                &library_name,
                &library_version,
                &family,
                tags,
                converted_endpoint,
            )?
            .into(),
        )
    })
}

/// # Safety
/// The `exporter` may be null, but if non-null the pointer must point to a
/// valid `ddog_prof_ReqwestExporter` object made by the Rust Global
/// allocator that has not already been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ReqwestExporter_drop(
    mut exporter: *mut Handle<ProfileExporter>,
) {
    drop(exporter.take())
}

unsafe fn into_vec_files_reqwest<'a>(slice: Slice<'a, File>) -> Vec<libdd_profiling::exporter::reqwest_exporter::File<'a>> {
    // Convert the FFI File slice using the shared conversion function from exporter module
    let hyper_files = super::exporter::into_vec_files(slice);
    // Convert from hyper exporter::File to reqwest exporter::File
    hyper_files
        .into_iter()
        .map(|file| libdd_profiling::exporter::reqwest_exporter::File {
            name: file.name,
            bytes: file.bytes,
        })
        .collect()
}

unsafe fn parse_json(
    string_id: &str,
    json_string: Option<&CharSlice>,
) -> anyhow::Result<Option<serde_json::Value>> {
    match json_string {
        None => Ok(None),
        Some(json_string) => {
            let json = json_string.try_to_utf8()?;
            match serde_json::from_str(json) {
                Ok(parsed) => Ok(Some(parsed)),
                Err(error) => Err(anyhow::anyhow!(
                    "Failed to parse contents of {} json string (`{}`): {}.",
                    string_id,
                    json,
                    error
                )),
            }
        }
    }
}

/// Sends a profile asynchronously, returning the HTTP status code.
///
/// This combines building and sending the profile in one step.
///
/// For details on the `optional_internal_metadata_json`, please reference the Datadog-internal
/// "RFC: Attaching internal metadata to pprof profiles".
///
/// For details on the `optional_info_json`, please reference the Datadog-internal
/// "RFC: Pprof System Info Support".
///
/// # Arguments
/// * `exporter` - Borrows the exporter for sending the request.
/// * `profile` - Takes ownership of the profile.
/// * `additional_files` - Additional files to attach to the request.
/// * `optional_additional_tags` - Additional tags for this specific profile.
/// * `optional_internal_metadata_json` - Internal metadata as JSON string.
/// * `optional_info_json` - Info metadata as JSON string.
/// * `cancel` - Optional cancellation token.
///
/// # Safety
/// All non-null arguments MUST have been created by APIs in this module.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_ReqwestExporter_send(
    mut exporter: *mut Handle<ProfileExporter>,
    mut profile: *mut Handle<EncodedProfile>,
    additional_files: Slice<File>,
    optional_additional_tags: Option<&libdd_common_ffi::Vec<Tag>>,
    optional_internal_metadata_json: Option<&CharSlice>,
    optional_info_json: Option<&CharSlice>,
    mut cancel: *mut Handle<TokioCancellationToken>,
) -> Result<HttpStatus> {
    wrap_with_ffi_result!({
        let exporter = exporter.to_inner_mut()?;
        let profile = *profile.take().context("profile")?;
        let files = into_vec_files_reqwest(additional_files);
        let tags: Vec<Tag> = optional_additional_tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        
        let internal_metadata = parse_json("internal_metadata", optional_internal_metadata_json)?;
        let info = parse_json("info", optional_info_json)?;
        let cancel_token = cancel.to_inner_mut().ok();

        // Create a tokio runtime for the async operation
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let status = rt.block_on(async {
            exporter
                .send(
                    profile,
                    &files,
                    &tags,
                    internal_metadata,
                    info,
                    cancel_token.as_deref(),
                )
                .await
        })?;

        anyhow::Ok(HttpStatus(status.as_u16()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common::tag;
    use libdd_common_ffi::Slice;

    fn profiling_library_name() -> CharSlice<'static> {
        CharSlice::from("dd-trace-foo")
    }

    fn profiling_library_version() -> CharSlice<'static> {
        CharSlice::from("1.2.3")
    }

    fn family() -> CharSlice<'static> {
        CharSlice::from("native")
    }

    fn base_url() -> &'static str {
        "https://localhost:1337"
    }

    fn endpoint() -> CharSlice<'static> {
        CharSlice::from(base_url())
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reqwest_exporter_new_and_delete() {
        let tags = vec![tag!("host", "localhost")].into();

        let result = unsafe {
            ddog_prof_ReqwestExporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                Some(&tags),
                super::super::exporter::ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        match result {
            Result::Ok(mut exporter) => unsafe { ddog_prof_ReqwestExporter_drop(&mut exporter) },
            Result::Err(message) => {
                drop(message);
                panic!("Should not occur!")
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_file_endpoint() {
        let file_path = CharSlice::from("/tmp/test_profile.http");
        let result = unsafe {
            ddog_prof_ReqwestExporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                super::super::exporter::endpoint_file(file_path),
            )
        };

        match result {
            Result::Ok(mut exporter) => unsafe { ddog_prof_ReqwestExporter_drop(&mut exporter) },
            Result::Err(message) => {
                drop(message);
                panic!("Should not occur!")
            }
        }
    }
}

