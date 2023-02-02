// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use crate::Timespec;
use datadog_profiling::exporter;
use datadog_profiling::exporter::{ProfileExporter, Request};
use datadog_profiling::profile::profiled_endpoints;
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
pub enum Endpoint<'a> {
    Agent(CharSlice<'a>),
    Agentless(CharSlice<'a>, CharSlice<'a>),
}

#[repr(C)]
pub struct File<'a> {
    name: CharSlice<'a>,
    file: ByteSlice<'a>,
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
#[export_name = "ddog_Endpoint_agent"]
pub extern "C" fn endpoint_agent(base_url: CharSlice) -> Endpoint {
    Endpoint::Agent(base_url)
}

/// Creates an endpoint that uses the Datadog intake directly aka agentless.
/// # Arguments
/// * `site` - Contains a host and port e.g. "datadoghq.com".
/// * `api_key` - Contains the Datadog API key.
#[export_name = "ddog_Endpoint_agentless"]
pub extern "C" fn endpoint_agentless<'a>(
    site: CharSlice<'a>,
    api_key: CharSlice<'a>,
) -> Endpoint<'a> {
    Endpoint::Agentless(site, api_key)
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

unsafe fn try_to_endpoint(endpoint: Endpoint) -> anyhow::Result<exporter::Endpoint> {
    // convert to utf8 losslessly -- URLs and API keys should all be ASCII, so
    // a failed result is likely to be an error.
    match endpoint {
        Endpoint::Agent(url) => {
            let base_url = try_to_url(url)?;
            exporter::config::agent(base_url)
        }
        Endpoint::Agentless(site, api_key) => {
            let site_str = site.try_to_utf8()?;
            let api_key_str = api_key.try_to_utf8()?;
            exporter::config::agentless(
                Cow::Owned(site_str.to_owned()),
                Cow::Owned(api_key_str.to_owned()),
            )
        }
    }
}

/// Creates a new exporter to be used to report profiling data.
/// # Arguments
/// * `profiling_library_name` - Profiling library name, usually dd-trace-something, e.g. "dd-trace-rb". See
///   https://datadoghq.atlassian.net/wiki/spaces/PROF/pages/1538884229/Client#Header-values (Datadog internal link)
///   for a list of common values.
/// * `profliling_library_version` - Version used when publishing the profiling library to a package manager
/// * `family` - Profile family, e.g. "ruby"
/// * `tags` - Tags to include with every profile reported by this exporter. It's also possible to include
///   profile-specific tags, see `additional_tags` on `profile_exporter_build`.
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
    endpoint: Endpoint,
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
    endpoint: Endpoint,
) -> anyhow::Result<ProfileExporter> {
    let library_name = unsafe { profiling_library_name.to_utf8_lossy() }.into_owned();
    let library_version = unsafe { profiling_library_version.to_utf8_lossy() }.into_owned();
    let family = unsafe { family.to_utf8_lossy() }.into_owned();
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
/// # Safety
/// The `exporter`, `additional_stats`, and `endpoint_stats` args should be
/// valid objects created by this module, except NULL is allowed.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Exporter_Request_build(
    exporter: Option<&mut ProfileExporter>,
    start: Timespec,
    end: Timespec,
    files: Slice<File>,
    additional_tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoints_stats: Option<&profiled_endpoints::ProfiledEndpointsStats>,
    timeout_ms: u64,
) -> RequestBuildResult {
    match exporter {
        None => RequestBuildResult::Err(anyhow::anyhow!("exporter was null").into()),
        Some(exporter) => {
            let timeout = std::time::Duration::from_millis(timeout_ms);
            let converted_files = into_vec_files(files);
            let tags = additional_tags.map(|tags| tags.iter().cloned().collect());

            match exporter.build(
                start.into(),
                end.into(),
                converted_files.as_slice(),
                tags.as_ref(),
                endpoints_stats,
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
/// * `request` - Takes ownership of the request, replacing it with a null
///               pointer. This is why it takes a double-pointer, rather than
///               a single one.
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

/// Can be passed as an argument to send and then be used to asynchronously cancel it from a different thread.
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
/// Without clone, both t1 and t2 would need to synchronize to make sure neither was using the cancel
/// before it could be dropped. With clone, there is no need for such synchronization, both threads
/// have their own cancel and should drop that cancel after they are done with it.
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
mod test {
    use super::*;
    use ddcommon_ffi::Slice;

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
                endpoint_agent(endpoint()),
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
    fn test_build() {
        let exporter_result = unsafe {
            ddog_prof_Exporter_new(
                profiling_library_name(),
                profiling_library_version(),
                family(),
                None,
                endpoint_agent(endpoint()),
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

        let build_result = unsafe {
            ddog_prof_Exporter_Request_build(
                Some(exporter.as_mut()),
                start,
                finish,
                Slice::from(files),
                None,
                None,
                timeout_milliseconds,
            )
        };

        let build_result = Result::from(build_result);
        build_result.unwrap();

        // TODO: Currently, we're only testing that a request was built (building did not fail), but
        //     we have no coverage for the request actually being correct.
        //     It'd be nice to actually perform the request, capture its contents, and assert that
        //     they are as expected.
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
                Slice::default(),
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
