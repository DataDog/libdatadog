// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::{Buffer, ByteSlice, CharSlice, Slice, Timespec};
use ddprof_exporter as exporter;
use exporter::ProfileExporterV3;
use std::borrow::Cow;
use std::convert::TryInto;
use std::io::Write;
use std::ptr::NonNull;
use std::str::FromStr;

#[repr(C)]
pub enum SendResult {
    HttpResponse(HttpStatus),
    Failure(Buffer),
}

#[repr(C)]
pub enum NewProfileExporterV3Result {
    Ok(*mut ProfileExporterV3),
    Err(Buffer),
}

#[export_name = "ddprof_ffi_NewProfileExporterV3Result_dtor"]
pub unsafe extern "C" fn new_profile_exporter_v3_result_dtor(result: NewProfileExporterV3Result) {
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

/// Clears the contents of the Buffer, leaving length and capacity of 0.
/// # Safety
/// The `buffer` must be created by Rust, or null.
#[export_name = "ddprof_ffi_Buffer_reset"]
pub unsafe extern "C" fn buffer_reset(buffer: *mut Buffer) {
    match buffer.as_mut() {
        None => {}
        Some(buff) => buff.reset(),
    }
}

#[repr(C)]
pub struct Tag<'a> {
    name: CharSlice<'a>,
    value: CharSlice<'a>,
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

struct EmptyTagError {}

impl std::fmt::Display for EmptyTagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A tag name must not be empty.")
    }
}

impl std::fmt::Debug for EmptyTagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A tag name must not be empty.")
    }
}

impl std::error::Error for EmptyTagError {}

fn try_to_tags(tags: Slice<Tag>) -> Result<Vec<ddprof_exporter::Tag>, Box<dyn std::error::Error>> {
    let mut converted_tags = Vec::with_capacity(tags.len);
    for tag in unsafe { tags.into_slice() }.iter() {
        let name: &str = tag.name.try_into()?;
        let value: &str = tag.value.try_into()?;

        // If a tag name is empty, that's an error
        if name.is_empty() {
            return Err(Box::new(EmptyTagError {}));
        }

        /* However, empty tag values are treated as if the tag was not sent;
         * this makes it easier for the calling code to send a statically sized
         * tags slice.
         */
        if !value.is_empty() {
            converted_tags.push(ddprof_exporter::Tag {
                name: Cow::Owned(String::from(name)),
                value: Cow::Owned(String::from(value)),
            });
        }
    }
    Ok(converted_tags)
}

fn try_to_url(slice: CharSlice) -> Result<hyper::Uri, Box<dyn std::error::Error>> {
    let str: &str = slice.try_into()?;
    #[cfg(unix)]
    if let Some(path) = str.strip_prefix("unix://") {
        return ddprof_exporter::socket_path_to_uri(path.as_ref());
    }
    match hyper::Uri::from_str(str) {
        Ok(url) => Ok(url),
        Err(err) => Err(Box::new(err)),
    }
}

fn try_to_endpoint(
    endpoint: EndpointV3,
) -> Result<ddprof_exporter::Endpoint, Box<dyn std::error::Error>> {
    match endpoint {
        EndpointV3::Agent(url) => {
            let base_url = try_to_url(url)?;
            ddprof_exporter::Endpoint::agent(base_url)
        }
        EndpointV3::Agentless(site, api_key) => {
            let site_str: &str = site.try_into()?;
            let api_key_str: &str = api_key.try_into()?;
            ddprof_exporter::Endpoint::agentless(site_str, api_key_str)
        }
    }
}

fn error_into_buffer(err: Box<dyn std::error::Error>) -> Buffer {
    let mut vec = Vec::new();
    /* Ignore the possible but highly unlikely write failure into a
     * Vec. In case this happens, it will be an empty message, which
     * will be confusing but safe, and I'm not sure how else to handle
     * it. */
    let _ = write!(vec, "{}", err);
    Buffer::from_vec(vec)
}

#[export_name = "ddprof_ffi_ProfileExporterV3_new"]
pub extern "C" fn profile_exporter_new(
    family: CharSlice,
    tags: Slice<Tag>,
    endpoint: EndpointV3,
) -> NewProfileExporterV3Result {
    match || -> Result<ProfileExporterV3, Box<dyn std::error::Error>> {
        let converted_family: &str = family.try_into()?;
        let converted_tags = try_to_tags(tags)?;
        let converted_endpoint = try_to_endpoint(endpoint)?;
        ProfileExporterV3::new(converted_family, converted_tags, converted_endpoint)
    }() {
        Ok(exporter) => NewProfileExporterV3Result::Ok(Box::into_raw(Box::new(exporter))),
        Err(err) => NewProfileExporterV3Result::Err(error_into_buffer(err)),
    }
}

#[export_name = "ddprof_ffi_ProfileExporterV3_delete"]
pub extern "C" fn profile_exporter_delete(exporter: Option<Box<ProfileExporterV3>>) {
    std::mem::drop(exporter)
}

unsafe fn try_into_vec_files<'a>(slice: Slice<'a, File>) -> Option<Vec<ddprof_exporter::File<'a>>> {
    let mut vec = Vec::with_capacity(slice.len);

    for file in slice.into_slice().iter() {
        let name = file.name.try_into().ok()?;
        let bytes: &[u8] = file.file.into_slice();
        vec.push(ddprof_exporter::File { name, bytes });
    }
    Some(vec)
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
    timeout_ms: u64,
) -> Option<Box<Request>> {
    match exporter {
        None => None,
        Some(exporter) => {
            let timeout = std::time::Duration::from_millis(timeout_ms);
            let converted_files = try_into_vec_files(files)?;
            match exporter.as_ref().build(
                start.into(),
                end.into(),
                converted_files.as_slice(),
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
///
/// # Safety
/// If the `exporter` and `request` are non-null, then they need to have been
/// created by apis in this module.
#[export_name = "ddprof_ffi_ProfileExporterV3_send"]
pub unsafe extern "C" fn profile_exporter_send(
    exporter: Option<NonNull<ProfileExporterV3>>,
    request: Option<Box<Request>>,
) -> SendResult {
    let exp_ptr = match exporter {
        None => {
            let buf: &[u8] = b"Failed to export: exporter was null";
            return SendResult::Failure(Buffer::from_vec(Vec::from(buf)));
        }
        Some(e) => e,
    };

    let request_ptr = match request {
        None => {
            let buf: &[u8] = b"Failed to export: request was null";
            return SendResult::Failure(Buffer::from_vec(Vec::from(buf)));
        }
        Some(req) => req,
    };

    match || -> Result<HttpStatus, Box<dyn std::error::Error>> {
        let response = exp_ptr.as_ref().send((*request_ptr).0)?;

        Ok(HttpStatus(response.status().as_u16()))
    }() {
        Ok(code) => SendResult::HttpResponse(code),
        Err(err) => SendResult::Failure(error_into_buffer(err)),
    }
}

#[cfg(test)]
mod test {
    use crate::exporter::*;
    use crate::Slice;
    use std::os::raw::c_char;

    fn family() -> CharSlice<'static> {
        CharSlice::new("native".as_ptr() as *const c_char, "native".len())
    }

    fn base_url() -> &'static str {
        "https://localhost:1337"
    }

    fn endpoint() -> CharSlice<'static> {
        CharSlice::new(base_url().as_ptr() as *const c_char, base_url().len())
    }

    #[test]
    fn empty_tag_name() {
        let tag = Tag {
            name: Slice::new("".as_ptr() as *const c_char, 0),
            value: Slice::new("1".as_ptr() as *const c_char, 1),
        };
        let tags = Slice::new((&tag) as *const Tag, 1);
        let result = try_to_tags(tags);
        assert!(result.is_err());
    }

    #[test]
    fn profile_exporter_v3_new_and_delete() {
        let tags = [Tag {
            name: CharSlice::new("host".as_ptr() as *const c_char, "host".len()),
            value: CharSlice::new("localhost".as_ptr() as *const c_char, "localhost".len()),
        }];

        let result = profile_exporter_new(
            family(),
            Slice::new(tags.as_ptr(), tags.len()),
            endpoint_agent(endpoint()),
        );

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
        let exporter_result =
            profile_exporter_new(family(), Slice::default(), endpoint_agent(endpoint()));

        let exporter = match exporter_result {
            NewProfileExporterV3Result::Ok(exporter) => unsafe {
                Some(NonNull::new_unchecked(exporter))
            },
            NewProfileExporterV3Result::Err(message) => {
                std::mem::drop(message);
                panic!("Should not occur!")
            }
        };

        let files = [File {
            name: CharSlice::new("foo.pprof".as_ptr() as *const c_char, "foo.pprof".len()),
            file: ByteSlice::new("dummy contents".as_ptr(), "dummy contents".len()),
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
                Slice::new(files.as_ptr(), files.len()),
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
