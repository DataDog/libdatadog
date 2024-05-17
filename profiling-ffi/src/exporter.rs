// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use crate::Timespec;
use datadog_profiling::exporter;
use datadog_profiling::exporter::{ProfileExporter, Request};
use datadog_profiling::internal::ProfiledEndpointsStats;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, ByteSlice, CharSlice, Slice};
use ddcommon_ffi::Error;
use std::borrow::Cow;
use std::ptr::NonNull;
use std::str::FromStr;

#[repr(C)]
pub enum ExporterNewResult {
    Ok(NonNull<ProfileExporter>),
    Err(Error),
}

#[repr(C)]
pub enum RequestBuildResult {
    Ok(NonNull<Request>),
    Err(Error),
}

#[repr(C)]
pub enum SendResult {
    HttpResponse(HttpStatus),
    Err(Error),
}

#[repr(C)]
pub enum ProfilingEndpoint<'a> {
    Agent(CharSlice<'a>),
    Agentless(CharSlice<'a>, CharSlice<'a>),
    File(CharSlice<'a>),
}

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

// This type exists only to force cbindgen to expose an CancellationToken as an opaque type.
pub struct CancellationToken(tokio_util::sync::CancellationToken);

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
pub unsafe extern "C" fn ddog_prof_Exporter_new(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: ProfilingEndpoint,
) -> ExporterNewResult {
    // Use a helper function so we can use the ? operator.
    match ddog_prof_exporter_new_impl(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
    ) {
        Ok(exporter) => {
            // Safety: Box::into_raw will always be non-null.
            let ptr = NonNull::new_unchecked(Box::into_raw(Box::new(exporter)));
            ExporterNewResult::Ok(ptr)
        }
        Err(err) => ExporterNewResult::Err(err.into()),
    }
}

fn ddog_prof_exporter_new_impl(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: ProfilingEndpoint,
) -> anyhow::Result<ProfileExporter> {
    let library_name = profiling_library_name.to_utf8_lossy().into_owned();
    let library_version = profiling_library_version.to_utf8_lossy().into_owned();
    let family = family.to_utf8_lossy().into_owned();
    let converted_endpoint = unsafe { try_to_endpoint(endpoint)? };
    let tags = tags.map(|tags| tags.iter().cloned().collect());
    ProfileExporter::new(
        library_name,
        library_version,
        family,
        tags,
        converted_endpoint,
    )
}

/// # Safety
/// The `exporter` may be null, but if non-null the pointer must point to a
/// valid `ddog_prof_Exporter_Request` object made by the Rust Global
/// allocator that has not already been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Exporter_drop(exporter: Option<&mut ProfileExporter>) {
    if let Some(reference) = exporter {
        // Safety: ProfileExporter's are opaque and therefore Boxed.
        drop(Box::from_raw(reference as *mut _))
    }
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

#[cfg(test)]
impl From<RequestBuildResult> for Result<Box<Request>, String> {
    fn from(result: RequestBuildResult) -> Self {
        match result {
            // Safety: Request is opaque, can only be built from Rust.
            RequestBuildResult::Ok(ok) => Ok(unsafe { Box::from_raw(ok.as_ptr()) }),
            RequestBuildResult::Err(err) => Err(err.to_string()),
        }
    }
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
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Exporter_Request_build(
    exporter: Option<&mut ProfileExporter>,
    start: Timespec,
    end: Timespec,
    files_to_compress_and_export: Slice<File>,
    files_to_export_unmodified: Slice<File>,
    optional_additional_tags: Option<&ddcommon_ffi::Vec<Tag>>,
    optional_endpoints_stats: Option<&ProfiledEndpointsStats>,
    optional_internal_metadata_json: Option<&CharSlice>,
    optional_info_json: Option<&CharSlice>,
    timeout_ms: u64,
) -> RequestBuildResult {
    match exporter {
        None => RequestBuildResult::Err(anyhow::anyhow!("exporter was null").into()),
        Some(exporter) => {
            let timeout = std::time::Duration::from_millis(timeout_ms);
            let files_to_compress_and_export = into_vec_files(files_to_compress_and_export);
            let files_to_export_unmodified = into_vec_files(files_to_export_unmodified);
            let tags = optional_additional_tags.map(|tags| tags.iter().cloned().collect());

            let internal_metadata =
                match parse_json("internal_metadata", optional_internal_metadata_json) {
                    Ok(parsed) => parsed,
                    Err(err) => return RequestBuildResult::Err(err.into()),
                };

            let info = match parse_json("info", optional_info_json) {
                Ok(parsed) => parsed,
                Err(err) => return RequestBuildResult::Err(err.into()),
            };

            match exporter.build(
                start.into(),
                end.into(),
                files_to_compress_and_export.as_slice(),
                files_to_export_unmodified.as_slice(),
                tags.as_ref(),
                optional_endpoints_stats,
                internal_metadata,
                info,
                timeout,
            ) {
                Ok(request) => {
                    RequestBuildResult::Ok(NonNull::new_unchecked(Box::into_raw(Box::new(request))))
                }
                Err(err) => RequestBuildResult::Err(err.into()),
            }
        }
    }
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
pub unsafe extern "C" fn ddog_prof_Exporter_Request_drop(
    request: Option<&mut Option<&mut Request>>,
) {
    drop(rebox_request(request))
}

/// Replace the inner `*mut Request` with a nullptr to reduce chance of
/// double-free in caller.
unsafe fn rebox_request(request: Option<&mut Option<&mut Request>>) -> Option<Box<Request>> {
    if let Some(ref_ptr) = request {
        let mut tmp = None;
        std::mem::swap(ref_ptr, &mut tmp);
        tmp.map(|ptr| Box::from_raw(ptr as *mut _))
    } else {
        None
    }
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
pub unsafe extern "C" fn ddog_prof_Exporter_send(
    exporter: Option<&mut ProfileExporter>,
    request: Option<&mut Option<&mut Request>>,
    cancel: Option<&CancellationToken>,
) -> SendResult {
    match ddog_prof_exporter_send_impl(exporter, request, cancel) {
        Ok(code) => SendResult::HttpResponse(code),
        Err(err) => SendResult::Err(Error::from(err.context("failed ddog_prof_Exporter_send"))),
    }
}

unsafe fn ddog_prof_exporter_send_impl(
    exporter: Option<&mut ProfileExporter>,
    request: Option<&mut Option<&mut Request>>,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<HttpStatus> {
    // Re-box the request first, to avoid leaks on other errors.
    let request = match rebox_request(request) {
        Some(boxed) => boxed,
        None => anyhow::bail!("request was null"),
    };

    let exporter = match exporter {
        Some(exporter) => exporter,
        None => anyhow::bail!("exporter was null"),
    };

    let cancel = cancel.map(|ptr| &ptr.0);
    let response = exporter.send(*request, cancel)?;

    Ok(HttpStatus(response.status().as_u16()))
}

/// Can be passed as an argument to send and then be used to asynchronously cancel it from a
/// different thread.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_CancellationToken_new() -> NonNull<CancellationToken> {
    let token = CancellationToken(tokio_util::sync::CancellationToken::new());
    let ptr = Box::into_raw(Box::new(token));
    // Safety: Box::into_raw will be non-null.
    unsafe { NonNull::new_unchecked(ptr) }
}

/// A cloned CancellationToken is connected to the CancellationToken it was created from.
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
    token: Option<&CancellationToken>,
) -> *mut CancellationToken {
    match token {
        Some(ptr) => {
            let new_token = ptr.0.clone();
            Box::into_raw(Box::new(CancellationToken(new_token)))
        }
        None => std::ptr::null_mut(),
    }
}

/// Cancel send that is being called in another thread with the given token.
/// Note that cancellation is a terminal state; cancelling a token more than once does nothing.
/// Returns `true` if token was successfully cancelled.
#[no_mangle]
pub extern "C" fn ddog_CancellationToken_cancel(cancel: Option<&CancellationToken>) -> bool {
    match cancel {
        Some(ptr) => {
            let token = &ptr.0;
            let will_cancel = !token.is_cancelled();
            if will_cancel {
                token.cancel();
            }
            will_cancel
        }
        None => false,
    }
}

/// # Safety
/// The `token` can be null, but non-null values must be created by the Rust
/// Global allocator and must have not been dropped already.
#[no_mangle]
pub unsafe extern "C" fn ddog_CancellationToken_drop(token: Option<&mut CancellationToken>) {
    if let Some(reference) = token {
        // Safety: the token is not repr(C), so it is boxed.
        drop(Box::from_raw(reference as *mut _))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddcommon_ffi::Slice;
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

    fn parsed_event_json(request: RequestBuildResult) -> serde_json::Value {
        let request = Result::from(request).unwrap();

        // Really hacky way of getting the event.json file contents, because I didn't want to
        // implement a full multipart parser and didn't find a particularly good
        // alternative. If you do figure out a better way, there's another copy of this code
        // in the profiling tests, please update there too :)
        let body = request.body();
        let body_bytes: String = String::from_utf8_lossy(
            &futures::executor::block_on(hyper::body::to_bytes(body)).unwrap(),
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
        let mut tags = ddcommon_ffi::Vec::default();
        let host = Tag::new("host", "localhost").expect("static tags to be valid");
        tags.push(host);

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
            ExporterNewResult::Ok(mut exporter) => unsafe {
                ddog_prof_Exporter_drop(Some(exporter.as_mut()))
            },
            ExporterNewResult::Err(message) => {
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

        let mut exporter = match exporter_result {
            ExporterNewResult::Ok(e) => e,
            ExporterNewResult::Err(_) => panic!("Should not occur!"),
        };

        let files_to_compress_and_export: &[File] = &[File {
            name: CharSlice::from("foo.pprof"),
            file: ByteSlice::from(b"dummy contents" as &[u8]),
        }];

        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files_to_compress_and_export),
                Slice::empty(),
                None,
                None,
                None,
                None,
                timeout_milliseconds,
            )
        };

        let parsed_event_json = parsed_event_json(build_result);

        assert_eq!(parsed_event_json["attachments"], json!(["foo.pprof"]));
        assert_eq!(parsed_event_json["endpoint_counts"], json!(null));
        assert_eq!(
            parsed_event_json["start"],
            json!("1970-01-01T00:00:12.000000034Z")
        );
        assert_eq!(
            parsed_event_json["end"],
            json!("1970-01-01T00:00:56.000000078Z")
        );
        assert_eq!(parsed_event_json["family"], json!("native"));
        assert_eq!(parsed_event_json["internal"], json!({}));
        assert_eq!(parsed_event_json["tags_profiler"], json!(""));
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

        let mut exporter = match exporter_result {
            ExporterNewResult::Ok(e) => e,
            ExporterNewResult::Err(_) => panic!("Should not occur!"),
        };

        let files: &[File] = &[File {
            name: CharSlice::from("foo.pprof"),
            file: ByteSlice::from(b"dummy contents" as &[u8]),
        }];

        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

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
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files),
                Slice::empty(),
                None,
                None,
                Some(&raw_internal_metadata),
                None,
                timeout_milliseconds,
            )
        };

        let parsed_event_json = parsed_event_json(build_result);

        assert_eq!(
            parsed_event_json["internal"],
            json!({
                "no_signals_workaround_enabled": "true",
                "execution_trace_enabled": "false",
                "extra object": {"key": [1, 2, true]}
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

        let mut exporter = match exporter_result {
            ExporterNewResult::Ok(e) => e,
            ExporterNewResult::Err(_) => panic!("Should not occur!"),
        };

        let files: &[File] = &[File {
            name: CharSlice::from("foo.pprof"),
            file: ByteSlice::from(b"dummy contents" as &[u8]),
        }];

        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

        let raw_internal_metadata = CharSlice::from("this is not a valid json string");

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files),
                Slice::empty(),
                None,
                None,
                Some(&raw_internal_metadata),
                None,
                timeout_milliseconds,
            )
        };

        match build_result {
            RequestBuildResult::Ok(_) => panic!("Should not happen!"),
            RequestBuildResult::Err(message) => assert!(String::from(message).starts_with(
                r#"Failed to parse contents of internal_metadata json string (`this is not a valid json string`)"#
            )),
        }
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

        let mut exporter = match exporter_result {
            ExporterNewResult::Ok(e) => e,
            ExporterNewResult::Err(_) => panic!("Should not occur!"),
        };

        let files: &[File] = &[File {
            name: CharSlice::from("foo.pprof"),
            file: ByteSlice::from(b"dummy contents" as &[u8]),
        }];

        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

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
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files),
                Slice::empty(),
                None,
                None,
                None,
                Some(&raw_info),
                timeout_milliseconds,
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

        let mut exporter = match exporter_result {
            ExporterNewResult::Ok(e) => e,
            ExporterNewResult::Err(_) => panic!("Should not occur!"),
        };

        let files: &[File] = &[File {
            name: CharSlice::from("foo.pprof"),
            file: ByteSlice::from(b"dummy contents" as &[u8]),
        }];

        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

        let raw_info = CharSlice::from("this is not a valid json string");

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files),
                Slice::empty(),
                None,
                None,
                None,
                Some(&raw_info),
                timeout_milliseconds,
            )
        };

        match build_result {
            RequestBuildResult::Ok(_) => panic!("Should not happen!"),
            RequestBuildResult::Err(message) => assert!(String::from(message).starts_with(
                r#"Failed to parse contents of info json string (`this is not a valid json string`)"#
            )),
        }
    }

    #[test]
    fn test_build_failure() {
        let start = Timespec {
            seconds: 12,
            nanoseconds: 34,
        };
        let finish = Timespec {
            seconds: 56,
            nanoseconds: 78,
        };
        let timeout_milliseconds = 90;

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                None, // No exporter, will fail
                start,
                finish,
                Slice::empty(),
                Slice::empty(),
                None,
                None,
                None,
                None,
                timeout_milliseconds,
            )
        };

        let build_result = Result::from(build_result);
        build_result.unwrap_err();
    }

    #[test]
    fn send_fails_with_null() {
        unsafe {
            match ddog_prof_Exporter_send(None, None, None) {
                SendResult::HttpResponse(http_status) => {
                    panic!("Expected test to fail, got {http_status:?}")
                }
                SendResult::Err(error) => {
                    let actual_error = error.to_string();
                    assert_eq!(
                        "failed ddog_prof_Exporter_send: request was null",
                        actual_error
                    );
                }
            }
        }
    }
}
