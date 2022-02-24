// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::{Buffer, Slice, Timespec};
use ddprof_exporter as exporter;
use exporter::{Exporter, ProfileExporterV3};
use reqwest::header::HeaderMap;
use std::borrow::Cow;
use std::convert::TryInto;
use std::ffi::CStr;
use std::io::Write;
use std::os::raw::c_char;
use std::ptr::NonNull;
use std::time::Duration;

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

type ByteSlice<'a> = crate::Slice<'a, u8>;

#[repr(C)]
pub struct Field<'a> {
    name: *const c_char,
    value: ByteSlice<'a>,
}

/// Create a new Exporter, initializing the TLS stack.
#[export_name = "ddprof_ffi_Exporter_new"]
pub extern "C" fn exporter_new() -> Option<Box<Exporter>> {
    match Exporter::new() {
        Ok(exporter) => Some(Box::new(exporter)),
        Err(_) => None,
    }
}

/// # Safety
/// All pointers must point to valid objects for that type. If they are used as
/// arrays, such as in Slice, then they must be valid for the associated number
/// of elements. All pointers must be aligned.
#[export_name = "ddprof_ffi_Exporter_send"]
pub unsafe extern "C" fn exporter_send(
    exporter_ptr: Option<NonNull<Exporter>>,
    http_method: *const c_char,
    url: *const c_char,
    headers: Slice<Field>,
    body: ByteSlice,
    timeout_ms: u64,
) -> SendResult {
    if !crate::is_aligned_and_not_null(http_method) {
        let vec: &[u8] = b"Failed to export: http_method was null\0";
        return SendResult::Failure(Buffer::from_vec(Vec::from(vec)));
    };

    if !crate::is_aligned_and_not_null(url) {
        let vec: &[u8] = b"Failed to export: url was null\0";
        return SendResult::Failure(Buffer::from_vec(Vec::from(vec)));
    };

    match exporter_ptr {
        None => {
            let vec: &[u8] = b"Failed to export: exporter was null\0";
            SendResult::Failure(Buffer::from_vec(Vec::from(vec)))
        }
        Some(non_null_exporter) => {
            let exporter = non_null_exporter.as_ref();

            match || -> Result<reqwest::Response, Box<dyn std::error::Error>> {
                let mut headers_map = HeaderMap::with_capacity(headers.len);

                for field in headers.into_slice().iter() {
                    let name = CStr::from_ptr((*field).name);
                    let value = (*field).value.try_into()?;
                    let header = reqwest::header::HeaderValue::from_str(value)?;
                    headers_map.insert(name.to_str()?, header);
                }

                let method = reqwest::Method::from_bytes(CStr::from_ptr(http_method).to_bytes())?;
                let url_str = CStr::from_ptr(url).to_str()?;
                let body_slice: &[u8] = body.into();
                let timeout = Duration::from_millis(timeout_ms);

                exporter.send(method, url_str, headers_map, body_slice, timeout)
            }() {
                Ok(response) => SendResult::HttpResponse(HttpStatus(response.status().as_u16())),
                Err(err) => SendResult::Failure(error_into_buffer(err)),
            }
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

/// Used to convert "external" data (e.g. not originated in libddprof) into a libddprof buffer, for
/// instance, when you want to attach JSON data (such as code_provenance.json or metrics.json) to a
/// profile.
///
/// The resulting buffer should be treated by the caller as opaque data that can only be manipulated
/// and accessed by libddprof.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddprof_ffi_Buffer_from_byte_slice(buffer: ByteSlice) -> Box<Buffer> {
    let buffer_slice: &[u8] = buffer.into();
    Box::new(Buffer::from_vec(Vec::from(buffer_slice)))
}

/// Destroys the Exporter.
#[export_name = "ddprof_ffi_Exporter_delete"]
pub extern "C" fn exporter_delete(exporter: Option<Box<Exporter>>) {
    std::mem::drop(exporter)
}

#[repr(C)]
pub struct Tag<'a> {
    name: ByteSlice<'a>,
    value: ByteSlice<'a>,
}

#[repr(C)]
pub enum EndpointV3<'a> {
    Agent(ByteSlice<'a>),
    Agentless(ByteSlice<'a>, ByteSlice<'a>),
}

#[repr(C)]
pub struct File<'a> {
    name: ByteSlice<'a>,
    file: Option<NonNull<Buffer>>,
}

/// This type only exists to workaround a bug in cbindgen; may be removed in the
/// future.
pub struct Request(reqwest::Request);

#[repr(C)]
/// cbindgen:field-names=[code]
pub struct HttpStatus(u16);

/// Creates an endpoint that uses the agent.
/// # Arguments
/// * `base_url` - a ByteSlice which contains a URL with scheme, host, and port
///                e.g. "https://agent:8126/"
#[export_name = "ddprof_ffi_EndpointV3_agent"]
pub extern "C" fn endpoint_agent(base_url: ByteSlice) -> EndpointV3 {
    EndpointV3::Agent(base_url)
}

/// Creates an endpoint that uses the Datadog intake directly aka agentless.
/// # Arguments
/// * `site` - a ByteSlice which contains a host and port e.g.
///            "datadoghq.com"
/// * `api_key` - A ByteSlice which contains the Datadog API key.
#[export_name = "ddprof_ffi_EndpointV3_agentless"]
pub extern "C" fn endpoint_agentless<'a>(
    site: ByteSlice<'a>,
    api_key: ByteSlice<'a>,
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

fn try_to_url(slice: ByteSlice) -> Result<reqwest::Url, Box<dyn std::error::Error>> {
    let str = slice.try_into()?;
    match reqwest::Url::parse(str) {
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
    family: ByteSlice,
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
        let bytes: &[u8] = file.file.as_ref()?.as_ref().as_slice();
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

    #[test]
    fn exporter_new_and_delete() {
        let exporter = exporter_new();
        exporter_delete(exporter);
    }

    #[test]
    fn empty_tag_name() {
        let tag = Tag {
            name: Slice::new("".as_ptr(), 0),
            value: Slice::new("1".as_ptr(), 1),
        };
        let tags = Slice::new((&tag) as *const Tag, 1);
        let result = try_to_tags(tags);
        assert!(result.is_err());
    }

    #[test]
    fn profile_exporter_v3_new_and_delete() {
        let family = ByteSlice::new("native".as_ptr(), "native".len());

        let tags = [Tag {
            name: ByteSlice::new("host".as_ptr(), "host".len()),
            value: ByteSlice::new("localhost".as_ptr(), "localhost".len()),
        }];

        let base_url = "https://localhost:1337";
        let endpoint = endpoint_agent(ByteSlice::new(base_url.as_ptr(), base_url.len()));

        let result = profile_exporter_new(family, Slice::new(tags.as_ptr(), tags.len()), endpoint);

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
    fn byte_slice_to_buffer_conversion() {
        let raw: &mut [u8] = &mut [104, 101, 108, 108, 111]; // "hello"
        let slice = Slice::new(raw.as_ptr(), raw.len());

        let converted: Box<Buffer> = unsafe { ddprof_ffi_Buffer_from_byte_slice(slice) };
        let converted_contents: &[u8] = unsafe { (*converted).as_slice() };

        raw.reverse(); // mutate the original buffer, to validate that a copy is really being made

        assert_eq!(b"hello", converted_contents);
        assert_eq!(b"olleh", raw);
        std::mem::drop(converted);
    }
}
