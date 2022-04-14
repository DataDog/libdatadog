// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use crate::{AsBytes, ByteSlice, CharSlice, Slice, Timespec};
use ddprof_exporter as exporter;
use ddprof_exporter::Tag;
use exporter::ProfileExporterV3;
use std::borrow::Cow;
use std::error::Error;
use std::ptr::NonNull;
use std::str::FromStr;

#[repr(C)]
pub enum SendResult {
    HttpResponse(HttpStatus),
    Failure(crate::Vec<u8>),
}

#[repr(C)]
pub enum NewProfileExporterV3Result {
    Ok(*mut ProfileExporterV3),
    Err(crate::Vec<u8>),
}

#[export_name = "ddprof_ffi_NewProfileExporterV3Result_drop"]
pub unsafe extern "C" fn new_profile_exporter_v3_result_drop(result: NewProfileExporterV3Result) {
    match result {
        NewProfileExporterV3Result::Ok(ptr) => {
            let exporter = Box::from_raw(ptr);
            std::mem::drop(exporter);
        }
        NewProfileExporterV3Result::Err(message) => {
            std::mem::drop(message);
        }
    }
}

#[repr(C)]
pub enum EndpointV3<'a> {
    Agent(CharSlice<'a>),
    Agentless(CharSlice<'a>, CharSlice<'a>),
}

#[repr(C)]
pub struct File<'a> {
    name: CharSlice<'a>,
    file: ByteSlice<'a>,
}

/// This type only exists to workaround a bug in cbindgen; may be removed in the
/// future.
pub struct Request(exporter::Request);

// This type exists only to force cbindgen to expose an CancellationToken as an opaque type.
pub struct CancellationToken(tokio_util::sync::CancellationToken);

#[repr(C)]
/// cbindgen:field-names=[code]
pub struct HttpStatus(u16);

/// Creates an endpoint that uses the agent.
/// # Arguments
/// * `base_url` - Contains a URL with scheme, host, and port e.g. "https://agent:8126/".
#[export_name = "ddprof_ffi_EndpointV3_agent"]
pub extern "C" fn endpoint_agent(base_url: CharSlice) -> EndpointV3 {
    EndpointV3::Agent(base_url)
}

/// Creates an endpoint that uses the Datadog intake directly aka agentless.
/// # Arguments
/// * `site` - Contains a host and port e.g. "datadoghq.com".
/// * `api_key` - Contains the Datadog API key.
#[export_name = "ddprof_ffi_EndpointV3_agentless"]
pub extern "C" fn endpoint_agentless<'a>(
    site: CharSlice<'a>,
    api_key: CharSlice<'a>,
) -> EndpointV3<'a> {
    EndpointV3::Agentless(site, api_key)
}

unsafe fn try_to_url(slice: CharSlice) -> Result<hyper::Uri, Box<dyn std::error::Error>> {
    let str: &str = slice.try_to_utf8()?;
    #[cfg(unix)]
    if let Some(path) = str.strip_prefix("unix://") {
        return ddprof_exporter::socket_path_to_uri(path.as_ref());
    }
    match hyper::Uri::from_str(str) {
        Ok(url) => Ok(url),
        Err(err) => Err(Box::new(err)),
    }
}

unsafe fn try_to_endpoint(
    endpoint: EndpointV3,
) -> Result<ddprof_exporter::Endpoint, Box<dyn std::error::Error>> {
    // convert to utf8 losslessly -- URLs and API keys should all be ASCII, so
    // a failed result is likely to be an error.
    match endpoint {
        EndpointV3::Agent(url) => {
            let base_url = try_to_url(url)?;
            ddprof_exporter::Endpoint::agent(base_url)
        }
        EndpointV3::Agentless(site, api_key) => {
            let site_str = site.try_to_utf8()?;
            let api_key_str = api_key.try_to_utf8()?;
            ddprof_exporter::Endpoint::agentless(
                Cow::Owned(site_str.to_owned()),
                Cow::Owned(api_key_str.to_owned()),
            )
        }
    }
}

#[must_use]
#[export_name = "ddprof_ffi_ProfileExporterV3_new"]
pub extern "C" fn profile_exporter_new(
    family: CharSlice,
    tags: Option<&crate::Vec<Tag>>,
    endpoint: EndpointV3,
) -> NewProfileExporterV3Result {
    match || -> Result<ProfileExporterV3, Box<dyn Error>> {
        let family = unsafe { family.to_utf8_lossy() }.into_owned();
        let converted_endpoint = unsafe { try_to_endpoint(endpoint)? };
        let tags = tags.map(|tags| tags.iter().map(|tag| tag.clone().into_owned()).collect());
        ProfileExporterV3::new(family, tags, converted_endpoint)
    }() {
        Ok(exporter) => NewProfileExporterV3Result::Ok(Box::into_raw(Box::new(exporter))),
        Err(err) => NewProfileExporterV3Result::Err(err.into()),
    }
}

#[export_name = "ddprof_ffi_ProfileExporterV3_delete"]
pub extern "C" fn profile_exporter_delete(exporter: Option<Box<ProfileExporterV3>>) {
    std::mem::drop(exporter)
}

unsafe fn into_vec_files<'a>(slice: Slice<'a, File>) -> Vec<ddprof_exporter::File<'a>> {
    slice
        .into_slice()
        .iter()
        .map(|file| {
            let name = file.name.try_to_utf8().unwrap_or("{invalid utf-8}");
            let bytes = file.file.as_slice();
            ddprof_exporter::File { name, bytes }
        })
        .collect()
}

/// Builds a Request object based on the profile data supplied.
///
/// # Safety
/// The `exporter` and the files inside of the `files` slice need to have been
/// created by this module.
#[export_name = "ddprof_ffi_ProfileExporterV3_build"]
pub unsafe extern "C" fn profile_exporter_build(
    exporter: Option<NonNull<ProfileExporterV3>>,
    start: Timespec,
    end: Timespec,
    files: Slice<File>,
    additional_tags: Option<&crate::Vec<Tag>>,
    timeout_ms: u64,
) -> Option<Box<Request>> {
    match exporter {
        None => None,
        Some(exporter) => {
            let timeout = std::time::Duration::from_millis(timeout_ms);
            let converted_files = into_vec_files(files);
            let tags = additional_tags.map(|tags| tags.iter().map(Tag::clone).collect());
            match exporter.as_ref().build(
                start.into(),
                end.into(),
                converted_files.as_slice(),
                tags.as_ref(),
                timeout,
            ) {
                Ok(request) => Some(Box::new(Request(request))),
                Err(_) => None,
            }
        }
    }
}

/// Sends the request, returning the HttpStatus.
///
/// # Arguments
/// * `exporter` - borrows the exporter for sending the request
/// * `request` - takes ownership of the request
/// * `cancel` - borrows the cancel, if any
///
/// # Safety
/// All non-null arguments MUST have been created by created by apis in this module.
#[must_use]
#[export_name = "ddprof_ffi_ProfileExporterV3_send"]
pub unsafe extern "C" fn profile_exporter_send(
    exporter: Option<NonNull<ProfileExporterV3>>,
    request: Option<Box<Request>>,
    cancel: Option<NonNull<CancellationToken>>,
) -> SendResult {
    let exp_ptr = match exporter {
        None => {
            let buf: &[u8] = b"Failed to export: exporter was null";
            return SendResult::Failure(crate::Vec::from(Vec::from(buf)));
        }
        Some(e) => e,
    };

    let request_ptr = match request {
        None => {
            let buf: &[u8] = b"Failed to export: request was null";
            return SendResult::Failure(crate::Vec::from(Vec::from(buf)));
        }
        Some(req) => req,
    };

    let cancel_option = unwrap_cancellation_token(cancel);

    match || -> Result<HttpStatus, Box<dyn std::error::Error>> {
        let response = exp_ptr.as_ref().send((*request_ptr).0, cancel_option)?;

        Ok(HttpStatus(response.status().as_u16()))
    }() {
        Ok(code) => SendResult::HttpResponse(code),
        Err(err) => SendResult::Failure(err.into()),
    }
}

fn unwrap_cancellation_token<'a>(
    cancel: Option<NonNull<CancellationToken>>,
) -> Option<&'a tokio_util::sync::CancellationToken> {
    cancel.map(|c| {
        let wrapped_reference: &CancellationToken = unsafe { c.as_ref() };
        let unwrapped_reference: &tokio_util::sync::CancellationToken = &(*wrapped_reference).0;

        unwrapped_reference
    })
}

/// Can be passed as an argument to send and then be used to asynchronously cancel it from a different thread.
#[no_mangle]
#[must_use]
pub extern "C" fn ddprof_ffi_CancellationToken_new() -> *mut CancellationToken {
    Box::into_raw(Box::new(CancellationToken(
        tokio_util::sync::CancellationToken::new(),
    )))
}

#[no_mangle]
#[must_use]
/// A cloned CancellationToken is connected to the CancellationToken it was created from.
/// Either the cloned or the original token can be used to cancel or provided as arguments to send.
/// The useful part is that they have independent lifetimes and can be dropped separately.
///
/// Thus, it's possible to do something like:
/// ```c
/// cancel_t1 = ddprof_ffi_CancellationToken_new();
/// cancel_t2 = ddprof_ffi_CancellationToken_clone(cancel_t1);
///
/// // On thread t1:
///     ddprof_ffi_ProfileExporterV3_send(..., cancel_t1);
///     ddprof_ffi_CancellationToken_drop(cancel_t1);
///
/// // On thread t2:
///     ddprof_ffi_CancellationToken_cancel(cancel_t2);
///     ddprof_ffi_CancellationToken_drop(cancel_t2);
/// ```
///
/// Without clone, both t1 and t2 would need to synchronize to make sure neither was using the cancel
/// before it could be dropped. With clone, there is no need for such synchronization, both threads
/// have their own cancel and should drop that cancel after they are done with it.
pub extern "C" fn ddprof_ffi_CancellationToken_clone(
    cancel: Option<NonNull<CancellationToken>>,
) -> *mut CancellationToken {
    match unwrap_cancellation_token(cancel) {
        Some(reference) => Box::into_raw(Box::new(CancellationToken(reference.clone()))),
        None => std::ptr::null_mut(),
    }
}

/// Cancel send that is being called in another thread with the given token.
/// Note that cancellation is a terminal state; cancelling a token more than once does nothing.
/// Returns `true` if token was successfully cancelled.
#[no_mangle]
pub extern "C" fn ddprof_ffi_CancellationToken_cancel(
    cancel: Option<NonNull<CancellationToken>>,
) -> bool {
    let cancel_reference = match unwrap_cancellation_token(cancel) {
        Some(reference) => reference,
        None => return false,
    };

    if cancel_reference.is_cancelled() {
        return false;
    }
    cancel_reference.cancel();

    true
}

#[no_mangle]
pub extern "C" fn ddprof_ffi_CancellationToken_drop(_cancel: Option<Box<CancellationToken>>) {
    // _cancel implicitly dropped because we've turned it into a Box
}

#[export_name = "ddprof_ffi_SendResult_drop"]
pub unsafe extern "C" fn send_result_drop(result: SendResult) {
    std::mem::drop(result)
}

#[cfg(test)]
mod test {
    use crate::exporter::*;
    use crate::Slice;

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
    fn profile_exporter_v3_new_and_delete() {
        let mut tags = crate::Vec::default();
        let host = Tag::new("host", "localhost").expect("static tags to be valid");
        tags.push(host);

        let result = profile_exporter_new(family(), Some(&tags), endpoint_agent(endpoint()));

        match result {
            NewProfileExporterV3Result::Ok(exporter) => unsafe {
                profile_exporter_delete(Some(Box::from_raw(exporter)))
            },
            NewProfileExporterV3Result::Err(message) => {
                std::mem::drop(message);
                panic!("Should not occur!")
            }
        }
    }

    #[test]
    fn profile_exporter_v3_build() {
        let exporter_result = profile_exporter_new(family(), None, endpoint_agent(endpoint()));

        let exporter = match exporter_result {
            NewProfileExporterV3Result::Ok(exporter) => unsafe {
                Some(NonNull::new_unchecked(exporter))
            },
            NewProfileExporterV3Result::Err(message) => {
                std::mem::drop(message);
                panic!("Should not occur!")
            }
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

        let maybe_request = unsafe {
            profile_exporter_build(
                exporter,
                start,
                finish,
                Slice::from(files),
                None,
                timeout_milliseconds,
            )
        };

        assert!(maybe_request.is_some());

        // TODO: Currently, we're only testing that a request was built (building did not fail), but
        //     we have no coverage for the request actually being correct.
        //     It'd be nice to actually perform the request, capture its contents, and assert that
        //     they are as expected.
    }
}
