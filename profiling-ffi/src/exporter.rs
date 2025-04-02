// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use datadog_profiling::exporter;
use datadog_profiling::exporter::{ProfileExporter, Request};
use datadog_profiling::internal::EncodedProfile;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, ByteSlice, CharSlice, Slice};
use ddcommon_ffi::{
    wrap_with_ffi_result, wrap_with_void_ffi_result, Handle, Result, ToInner, VoidResult,
};
use function_name::named;
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
        return Ok(exporter::socket_path_to_uri(path.as_ref())?);
    }
    #[cfg(windows)]
    if let Some(path) = str.strip_prefix("windows:") {
        return Ok(exporter::named_pipe_path_to_uri(path.as_ref())?);
    }
    Ok(hyper::Uri::from_str(str)?)
}

pub unsafe fn try_to_endpoint(endpoint: ProfilingEndpoint) -> anyhow::Result<ddcommon::Endpoint> {
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
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: ProfilingEndpoint,
) -> Result<Handle<ProfileExporter>> {
    wrap_with_ffi_result!({
        let library_name = profiling_library_name.to_utf8_lossy().into_owned();
        let library_version = profiling_library_version.to_utf8_lossy().into_owned();
        let family = family.to_utf8_lossy().into_owned();
        let converted_endpoint = unsafe { try_to_endpoint(endpoint)? };
        let tags = tags.map(|tags| tags.iter().cloned().collect());
        anyhow::Ok(
            ProfileExporter::new(
                library_name,
                library_version,
                family,
                tags,
                converted_endpoint,
            )?
            .into(),
        )
    })
}

/// Sets the value for the exporter's timeout.
/// # Arguments
/// * `exporter` - ProfileExporter instance.
/// * `timeout_ms` - timeout in milliseconds.
#[no_mangle]
#[named]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Exporter_set_timeout(
    mut exporter: *mut Handle<ProfileExporter>,
    timeout_ms: u64,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        exporter.to_inner_mut()?.set_timeout(timeout_ms);
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

/// If successful, builds a `ddog_prof_Exporter_Request` object based on the
/// profile data supplied. If unsuccessful, it returns an error message.
///
/// For details on the `optional_internal_metadata_json`, please reference the Datadog-internal
/// "RFC: Attaching internal metadata to pprof profiles".
/// If you use this parameter, please update the RFC with your use-case, so we can keep track of how
/// this is getting used.
///
/// For details on the `optional_info_json`, please reference the Datadog-internal
/// "RFC: Pprof System Info Support".
///
/// # Safety
/// The `exporter`, `optional_additional_stats`, and `optional_endpoint_stats` args should be
/// valid objects created by this module.
/// NULL is allowed for `optional_additional_tags`, `optional_endpoints_stats`,
/// `optional_internal_metadata_json` and `optional_info_json`.
/// Consumes the `SerializedProfile`
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Exporter_Request_build(
    mut exporter: *mut Handle<ProfileExporter>,
    mut profile: *mut Handle<EncodedProfile>,
    files_to_compress_and_export: Slice<File>,
    files_to_export_unmodified: Slice<File>,
    optional_additional_tags: Option<&ddcommon_ffi::Vec<Tag>>,
    optional_internal_metadata_json: Option<&CharSlice>,
    optional_info_json: Option<&CharSlice>,
) -> Result<Handle<Request>> {
    wrap_with_ffi_result!({
        let exporter = exporter.to_inner_mut()?;
        let profile = *profile.take()?;
        let files_to_compress_and_export = into_vec_files(files_to_compress_and_export);
        let files_to_export_unmodified = into_vec_files(files_to_export_unmodified);
        let tags = optional_additional_tags.map(|tags| tags.iter().cloned().collect());

        let internal_metadata = parse_json("internal_metadata", optional_internal_metadata_json)?;
        let info = parse_json("info", optional_info_json)?;

        let request = exporter.build(
            profile,
            files_to_compress_and_export.as_slice(),
            files_to_export_unmodified.as_slice(),
            tags.as_ref(),
            internal_metadata,
            info,
        )?;
        anyhow::Ok(request.into())
    })
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

/// # Safety
/// Each pointer of `request` may be null, but if non-null the inner-most
/// pointer must point to a valid `ddog_prof_Exporter_Request` object made by
/// the Rust Global allocator.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Exporter_Request_drop(mut request: *mut Handle<Request>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    drop(request.take())
}

/// Sends the request, returning the HttpStatus.
///
/// # Arguments
/// * `exporter` - Borrows the exporter for sending the request.
/// * `request` - Takes ownership of the request, replacing it with a null pointer. This is why it
///   takes a double-pointer, rather than a single one.
/// * `cancel` - Borrows the cancel, if any.
///
/// # Safety
/// All non-null arguments MUST have been created by created by apis in this module.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_prof_Exporter_send(
    mut exporter: *mut Handle<ProfileExporter>,
    mut request: *mut Handle<Request>,
    mut cancel: *mut Handle<TokioCancellationToken>,
) -> Result<HttpStatus> {
    wrap_with_ffi_result!({
        let request = *request.take().context("request")?;
        let exporter = exporter.to_inner_mut()?;
        let cancel = cancel.to_inner_mut().ok();
        let response = exporter.send(request, cancel.as_deref())?;

        anyhow::Ok(HttpStatus(response.status().as_u16()))
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
    use ddcommon::tag;
    use ddcommon_ffi::Slice;
    use http_body_util::BodyExt;
    use serde_json::json;

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

    fn parsed_event_json(request: ddcommon_ffi::Result<Handle<Request>>) -> serde_json::Value {
        // Safety: This is a test
        let request = unsafe { request.unwrap().take().unwrap() };
        // Really hacky way of getting the event.json file contents, because I didn't want to
        // implement a full multipart parser and didn't find a particularly good
        // alternative. If you do figure out a better way, there's another copy of this code
        // in the profiling tests, please update there too :)
        let body = request.body();
        let body_bytes: String = String::from_utf8_lossy(
            &futures::executor::block_on(body.collect())
                .unwrap()
                .to_bytes(),
        )
        .to_string();
        let event_json = body_bytes
            .lines()
            .skip_while(|line| !line.contains(r#"filename="event.json""#))
            .nth(2)
            .unwrap();

        serde_json::from_str(event_json).unwrap()
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
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn test_build() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        let mut exporter = exporter_result.unwrap();

        let profile = &mut EncodedProfile::test_instance().unwrap().into();
        let timeout_milliseconds = 90;
        unsafe {
            ddog_prof_Exporter_set_timeout(&mut exporter, timeout_milliseconds).unwrap();
        }

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                &mut exporter,
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                None,
                None,
            )
        };

        let parsed_event_json = parsed_event_json(build_result);

        assert_eq!(parsed_event_json["attachments"], json!(["profile.pprof"]));
        assert_eq!(parsed_event_json["endpoint_counts"], json!(null));
        #[cfg(not(windows))]
        {
            assert_eq!(
                parsed_event_json["start"],
                json!("1970-01-01T00:00:12.000000034Z")
            );
            assert_eq!(
                parsed_event_json["end"],
                json!("1970-01-01T00:00:56.000000078Z")
            );
        }
        // Windows is less accurate on timestamps
        #[cfg(windows)]
        {
            assert_eq!(
                parsed_event_json["start"],
                json!("1970-01-01T00:00:12.000000000Z")
            );
            assert_eq!(
                parsed_event_json["end"],
                json!("1970-01-01T00:00:56.000000000Z")
            );
        }

        assert_eq!(parsed_event_json["family"], json!("native"));
        assert_eq!(
            parsed_event_json["internal"],
            json!({"libdatadog_version": env!("CARGO_PKG_VERSION")})
        );
        assert_eq!(parsed_event_json["version"], json!("4"));

        // TODO: Assert on contents of attachments, as well as on the headers/configuration for the
        // exporter
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn test_build_with_internal_metadata() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        let mut exporter = exporter_result.unwrap();

        let profile = &mut EncodedProfile::test_instance().unwrap().into();
        let timeout_milliseconds = 90;
        unsafe {
            ddog_prof_Exporter_set_timeout(&mut exporter, timeout_milliseconds).unwrap();
        }

        let raw_internal_metadata = CharSlice::from(
            r#"
            {
                "no_signals_workaround_enabled": "true",
                "execution_trace_enabled": "false",
                "extra object": {"key": [1, 2, true]}
            }
        "#,
        );

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                &mut exporter,
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                Some(&raw_internal_metadata),
                None,
            )
        };

        let parsed_event_json = parsed_event_json(build_result);

        assert_eq!(
            parsed_event_json["internal"],
            json!({
                "no_signals_workaround_enabled": "true",
                "execution_trace_enabled": "false",
                "extra object": {"key": [1, 2, true]},
                "libdatadog_version": env!("CARGO_PKG_VERSION"),
            })
        );
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn test_build_with_invalid_internal_metadata() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        let mut exporter = exporter_result.unwrap();

        let profile = &mut EncodedProfile::test_instance().unwrap().into();

        let timeout_milliseconds = 90;
        unsafe {
            ddog_prof_Exporter_set_timeout(&mut exporter, timeout_milliseconds).unwrap();
        }

        let raw_internal_metadata = CharSlice::from("this is not a valid json string");

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                &mut exporter,
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                Some(&raw_internal_metadata),
                None,
            )
        };

        let message = build_result.unwrap_err();
        assert!(String::from(message).starts_with(
                 r#"ddog_prof_Exporter_Request_build failed: Failed to parse contents of internal_metadata json string (`this is not a valid json string`)"#
  ));
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn test_build_with_info() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        let mut exporter = exporter_result.unwrap();

        let profile = &mut EncodedProfile::test_instance().unwrap().into();
        let timeout_milliseconds = 90;
        unsafe {
            ddog_prof_Exporter_set_timeout(&mut exporter, timeout_milliseconds).unwrap();
        }

        let raw_info = CharSlice::from(
            r#"
            {
                "application": {
                  "start_time": "2024-01-24T11:17:22+0000",
                  "env": "test"
                },
                "platform": {
                  "kernel": "Darwin Kernel Version 22.5.0",
                  "hostname": "COMP-XSDF"
                },
                "runtime": {
                  "engine": "ruby",
                  "version": "3.2.0"
                },
                "profiler": {
                  "version": "1.32.0",
                  "libdatadog": "1.2.3-darwin",
                  "settings": {
                    "profiling": {
                      "advanced": {
                        "allocation": true,
                        "heap": true
                      }
                    }
                  }
                }
            }
        "#,
        );

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                &mut exporter,
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                None,
                Some(&raw_info),
            )
        };

        let parsed_event_json = parsed_event_json(build_result);

        assert_eq!(
            parsed_event_json["info"],
            json!({
                "application": {
                  "start_time": "2024-01-24T11:17:22+0000",
                  "env": "test",
                },
                "platform": {
                  "kernel": "Darwin Kernel Version 22.5.0",
                  "hostname": "COMP-XSDF"
                },
                "runtime": {
                  "engine": "ruby",
                  "version": "3.2.0"
                },
                "profiler": {
                  "version": "1.32.0",
                  "libdatadog": "1.2.3-darwin",
                  "settings": {
                      "profiling": {
                          "advanced": {
                              "allocation": true,
                              "heap": true
                          }
                      }
                  }
                }
            })
        );
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn test_build_with_invalid_info() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                ddog_prof_Endpoint_agent(endpoint()),
            )
        };

        let exporter = &mut exporter_result.unwrap();

        let profile = &mut EncodedProfile::test_instance().unwrap().into();
        let timeout_milliseconds = 90;
        unsafe {
            ddog_prof_Exporter_set_timeout(exporter, timeout_milliseconds).unwrap();
        }

        let raw_info = CharSlice::from("this is not a valid json string");

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                exporter,
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                None,
                Some(&raw_info),
            )
        };

        let message = build_result.unwrap_err();
        assert!(String::from(message).starts_with(
            r#"ddog_prof_Exporter_Request_build failed: Failed to parse contents of info json string (`this is not a valid json string`)"#
        ));
    }

    #[test]
    fn test_build_failure() {
        let profile = &mut EncodedProfile::test_instance().unwrap().into();
        let exporter = &mut Handle::empty();
        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                exporter, // No exporter, will fail
                profile,
                Slice::empty(),
                Slice::empty(),
                None,
                None,
                None,
            )
        };

        build_result.unwrap_err();
    }

    #[test]
    fn send_fails_with_null() {
        let exporter = &mut Handle::empty();
        let request = &mut Handle::empty();
        let cancel = &mut Handle::empty();
        unsafe {
            let error = ddog_prof_Exporter_send(exporter, request, cancel)
                .unwrap_err()
                .to_string();
            assert_eq!(
                        "ddog_prof_Exporter_send failed: request: inner pointer was null, indicates use after free",
                        error
                    );
        }
    }
}
