// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use function_name::named;
use libdd_common::tag::Tag;
use libdd_common_ffi::slice::{AsBytes, ByteSlice, CharSlice, Slice};
use libdd_common_ffi::{wrap_with_ffi_result, Handle, Result, ToInner};
use libdd_profiling::exporter;
use libdd_profiling::exporter::ProfileExporter;
use libdd_profiling::internal::EncodedProfile;
use std::borrow::Cow;
use std::str::FromStr;

type TokioCancellationToken = tokio_util::sync::CancellationToken;

#[allow(dead_code)]
#[repr(C)]
pub enum ProfilingEndpoint<'a> {
    Agent(CharSlice<'a>),
    Agentless(CharSlice<'a>, CharSlice<'a>),
    File(CharSlice<'a>),
}

#[allow(dead_code)]
#[repr(C)]
pub struct File<'a> {
    name: CharSlice<'a>,
    file: ByteSlice<'a>,
}

#[must_use]
#[no_mangle]
pub extern "C" fn ddog_prof_Exporter_Slice_File_empty() -> Slice<'static, File<'static>> {
    Slice::empty()
}

#[derive(Debug)]
#[repr(C)]
/// cbindgen:field-names=[code]
pub struct HttpStatus(u16);

/// Creates an endpoint that uses the agent.
/// # Arguments
/// * `base_url` - Contains a URL with scheme, host, and port e.g. "https://agent:8126/".
#[no_mangle]
pub extern "C" fn ddog_prof_Endpoint_agent(base_url: CharSlice) -> ProfilingEndpoint {
    ProfilingEndpoint::Agent(base_url)
}

/// Creates an endpoint that uses the Datadog intake directly aka agentless.
/// # Arguments
/// * `site` - Contains a host and port e.g. "datadoghq.com".
/// * `api_key` - Contains the Datadog API key.
#[no_mangle]
pub extern "C" fn ddog_prof_Endpoint_agentless<'a>(
    site: CharSlice<'a>,
    api_key: CharSlice<'a>,
) -> ProfilingEndpoint<'a> {
    ProfilingEndpoint::Agentless(site, api_key)
}

/// Creates an endpoint that writes to a file.
/// Useful for local debugging.
/// Currently only supported by the crashtracker.
/// # Arguments
/// * `filename` - Path to the output file "/tmp/file.txt".
#[export_name = "ddog_Endpoint_file"]
pub extern "C" fn endpoint_file(filename: CharSlice) -> ProfilingEndpoint {
    ProfilingEndpoint::File(filename)
}
unsafe fn try_to_url(slice: CharSlice) -> anyhow::Result<hyper::Uri> {
    let str: &str = slice.try_to_utf8()?;
    #[cfg(unix)]
    if let Some(path) = str.strip_prefix("unix://") {
        use libdd_common::connector::uds::socket_path_to_uri;
        return Ok(socket_path_to_uri(path.as_ref())?);
    }
    #[cfg(windows)]
    if let Some(path) = str.strip_prefix("windows:") {
        return Ok(exporter::named_pipe_path_to_uri(path.as_ref())?);
    }
    Ok(hyper::Uri::from_str(str)?)
}

pub unsafe fn try_to_endpoint(
    endpoint: ProfilingEndpoint,
) -> anyhow::Result<libdd_common::Endpoint> {
    // convert to utf8 losslessly -- URLs and API keys should all be ASCII, so
    // a failed result is likely to be an error.
    match endpoint {
        ProfilingEndpoint::Agent(url) => {
            let base_url = try_to_url(url)?;
            exporter::config::agent(base_url)
        }
        ProfilingEndpoint::Agentless(site, api_key) => {
            let site_str = site.try_to_utf8()?;
            let api_key_str = api_key.try_to_utf8()?;
            exporter::config::agentless(
                Cow::Owned(site_str.to_owned()),
                Cow::Owned(api_key_str.to_owned()),
            )
        }
        ProfilingEndpoint::File(filename) => {
            let filename = filename.try_to_utf8()?;
            exporter::config::file(filename)
        }
    }
}

/// Creates a new exporter to be used to report profiling data.
/// # Arguments
/// * `profiling_library_name` - Profiling library name, usually dd-trace-something, e.g.
///   "dd-trace-rb". See
///   https://datadoghq.atlassian.net/wiki/spaces/PROF/pages/1538884229/Client#Header-values
///   (Datadog internal link)
///   for a list of common values.
/// * `profliling_library_version` - Version used when publishing the profiling library to a package
///   manager
/// * `family` - Profile family, e.g. "ruby"
/// * `tags` - Tags to include with every profile reported by this exporter. It's also possible to
///   include profile-specific tags, see `additional_tags` on `profile_exporter_build`.
/// * `endpoint` - Configuration for reporting data
/// # Safety
/// All pointers must refer to valid objects of the correct types.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Exporter_new(
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
        let converted_endpoint = unsafe { try_to_endpoint(endpoint)? };
        let tags: Vec<Tag> = tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
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
/// valid `ddog_prof_Exporter_Request` object made by the Rust Global
/// allocator that has not already been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Exporter_drop(mut exporter: *mut Handle<ProfileExporter>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    drop(exporter.take())
}

unsafe fn into_vec_files<'a>(slice: Slice<'a, File>) -> Vec<exporter::File<'a>> {
    slice
        .into_slice()
        .iter()
        .map(|file| {
            let name = file.name.try_to_utf8().unwrap_or("{invalid utf-8}");
            let bytes = file.file.as_slice();
            exporter::File { name, bytes }
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

/// Sends a profile to Datadog, returning the HttpStatus.
///
/// # Arguments
/// * `exporter` - Borrows the exporter for sending.
/// * `profile` - Takes ownership of the profile (will be consumed).
/// * `files_to_compress_and_export` - Files to compress and attach.
/// * `optional_additional_tags` - Per-profile tags.
/// * `optional_process_tags` - Process-level tags as comma-separated string.
/// * `optional_internal_metadata_json` - Internal metadata as JSON string.
/// * `optional_info_json` - System info as JSON string.
/// * `cancel` - Optional cancellation token.
///
/// # Safety
/// All non-null arguments MUST have been created by APIs in this module.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Exporter_send(
    mut exporter: *mut Handle<ProfileExporter>,
    mut profile: *mut Handle<EncodedProfile>,
    files_to_compress_and_export: Slice<File>,
    optional_additional_tags: Option<&libdd_common_ffi::Vec<Tag>>,
    optional_process_tags: Option<&CharSlice>,
    optional_internal_metadata_json: Option<&CharSlice>,
    optional_info_json: Option<&CharSlice>,
    mut cancel: *mut Handle<TokioCancellationToken>,
) -> Result<HttpStatus> {
    wrap_with_ffi_result!({
        let exporter = exporter.to_inner_mut()?;
        let profile = *profile.take()?;
        let files = into_vec_files(files_to_compress_and_export);
        let tags: Vec<Tag> = optional_additional_tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        let process_tags_str = optional_process_tags
            .map(|cs| cs.try_to_utf8())
            .transpose()?;
        let internal_metadata = parse_json("internal_metadata", optional_internal_metadata_json)?;
        let info = parse_json("info", optional_info_json)?;
        let cancel = cancel.to_inner_mut().ok();

        let status = exporter.send_blocking(
            profile,
            files.as_slice(),
            &tags,
            internal_metadata,
            info,
            process_tags_str,
            cancel.as_deref(),
        )?;

        anyhow::Ok(HttpStatus(status.as_u16()))
    })
}

/// Can be passed as an argument to send and then be used to asynchronously cancel it from a
/// different thread.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_CancellationToken_new() -> Handle<TokioCancellationToken> {
    TokioCancellationToken::new().into()
}

/// A cloned TokioCancellationToken is connected to the TokioCancellationToken it was created from.
/// Either the cloned or the original token can be used to cancel or provided as arguments to send.
/// The useful part is that they have independent lifetimes and can be dropped separately.
///
/// Thus, it's possible to do something like:
/// ```c
/// cancel_t1 = ddog_CancellationToken_new();
/// cancel_t2 = ddog_CancellationToken_clone(cancel_t1);
///
/// // On thread t1:
///     ddog_prof_Exporter_send(..., cancel_t1);
///     ddog_CancellationToken_drop(cancel_t1);
///
/// // On thread t2:
///     ddog_CancellationToken_cancel(cancel_t2);
///     ddog_CancellationToken_drop(cancel_t2);
/// ```
///
/// Without clone, both t1 and t2 would need to synchronize to make sure neither was using the
/// cancel before it could be dropped. With clone, there is no need for such synchronization, both
/// threads have their own cancel and should drop that cancel after they are done with it.
///
/// # Safety
/// If the `token` is non-null, it must point to a valid object.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_CancellationToken_clone(
    mut token: *mut Handle<TokioCancellationToken>,
) -> Handle<TokioCancellationToken> {
    if let Ok(token) = token.to_inner_mut() {
        token.clone().into()
    } else {
        Handle::empty()
    }
}

/// Cancel send that is being called in another thread with the given token.
/// Note that cancellation is a terminal state; cancelling a token more than once does nothing.
/// Returns `true` if token was successfully cancelled.
#[no_mangle]
pub unsafe extern "C" fn ddog_CancellationToken_cancel(
    mut cancel: *mut Handle<TokioCancellationToken>,
) -> bool {
    if let Ok(token) = cancel.to_inner_mut() {
        let will_cancel = !token.is_cancelled();
        if will_cancel {
            token.cancel();
        }
        will_cancel
    } else {
        false
    }
}

/// # Safety
/// The `token` can be null, but non-null values must be created by the Rust
/// Global allocator and must have not been dropped already.
#[no_mangle]
pub unsafe extern "C" fn ddog_CancellationToken_drop(
    mut token: *mut Handle<TokioCancellationToken>,
) {
    drop(token.take())
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
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn profile_exporter_new_and_delete() {
        let tags = vec![tag!("host", "localhost")].into();

        let result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                Some(&tags),
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        match result {
            Result::Ok(mut exporter) => unsafe { ddog_prof_Exporter_drop(&mut exporter) },
            Result::Err(message) => {
                drop(message);
                panic!("Should not occur!")
            }
        }
    }

    #[test]
    fn send_fails_with_null() {
        let exporter = &mut Handle::empty();
        let profile = &mut Handle::empty();
        let files = Slice::empty();
        let tags = None;
        let process_tags = None;
        let metadata = None;
        let info = None;
        let cancel = &mut Handle::empty();
        unsafe {
            let error = ddog_prof_Exporter_send(
                exporter,
                profile,
                files,
                tags,
                process_tags,
                metadata,
                info,
                cancel,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("ddog_prof_Exporter_send failed"));
        }
    }
}
