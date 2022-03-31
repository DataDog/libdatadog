// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#![allow(renamed_and_removed_lints)]
#![allow(clippy::box_vec)]

use crate::{ByteSlice, CharSlice, Slice, Timespec};
use ddprof_exporter as exporter;
use exporter::ProfileExporterV3;
use std::borrow::Cow;
use std::convert::TryInto;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
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
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tag<'a> {
    name: CharSlice<'a>,
    value: CharSlice<'a>,
}

impl<'a> Tag<'a> {
    fn new(key: &'a str, value: &'a str) -> Tag<'a> {
        Tag {
            name: CharSlice::from(key),
            value: CharSlice::from(value),
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

struct TagsError {
    message: String,
}

impl Debug for TagsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tag Error: {:?}.", self.message)
    }
}

impl Display for TagsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tag Error: {}.", self.message)
    }
}

impl Error for TagsError {}

fn try_to_tags(tags: Slice<Tag>) -> Result<Vec<ddprof_exporter::Tag>, Box<dyn std::error::Error>> {
    let mut converted_tags = Vec::with_capacity(tags.len);
    for tag in tags.into_slice().iter() {
        let name: &str = tag.name.try_into()?;
        let value: &str = tag.value.try_into()?;

        // If a tag name is empty, that's an error
        if name.is_empty() {
            return Err(Box::new(TagsError {
                message: "tag name must not be empty".to_string(),
            }));
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

#[must_use]
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
        Err(err) => NewProfileExporterV3Result::Err(err.into()),
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
    additional_tags: Slice<Tag>,
    timeout_ms: u64,
) -> Option<Box<Request>> {
    match exporter {
        None => None,
        Some(exporter) => {
            let timeout = std::time::Duration::from_millis(timeout_ms);
            let converted_files = try_into_vec_files(files)?;
            let converted_tags = try_to_tags(additional_tags).ok()?;
            match exporter.as_ref().build(
                start.into(),
                end.into(),
                converted_files.as_slice(),
                converted_tags.as_slice(),
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
#[must_use]
#[export_name = "ddprof_ffi_ProfileExporterV3_send"]
pub unsafe extern "C" fn profile_exporter_send(
    exporter: Option<NonNull<ProfileExporterV3>>,
    request: Option<Box<Request>>,
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

    match || -> Result<HttpStatus, Box<dyn std::error::Error>> {
        let response = exp_ptr.as_ref().send((*request_ptr).0)?;

        Ok(HttpStatus(response.status().as_u16()))
    }() {
        Ok(code) => SendResult::HttpResponse(code),
        Err(err) => SendResult::Failure(err.into()),
    }
}

#[export_name = "ddprof_ffi_SendResult_drop"]
pub unsafe extern "C" fn send_result_drop(result: SendResult) {
    std::mem::drop(result)
}

#[must_use]
#[export_name = "ddprof_ffi_Vec_tag_new"]
pub extern "C" fn vec_tag_new<'a>() -> crate::Vec<Tag<'a>> {
    crate::Vec::default()
}

/// Pushes the tag into the vec.
#[export_name = "ddprof_ffi_Vec_tag_push"]
pub unsafe extern "C" fn vec_tag_push<'a>(vec: &mut crate::Vec<Tag<'a>>, tag: Tag<'a>) {
    vec.push(tag)
}

#[allow(clippy::ptr_arg)]
#[export_name = "ddprof_ffi_Vec_tag_as_slice"]
pub extern "C" fn vec_tag_as_slice<'a>(vec: &'a crate::Vec<Tag<'a>>) -> Slice<'a, Tag<'a>> {
    vec.as_slice()
}

#[export_name = "ddprof_ffi_Vec_tag_drop"]
pub extern "C" fn vec_tag_drop(vec: crate::Vec<Tag>) {
    std::mem::drop(vec)
}

fn parse_tag_chunk(chunk: &str) -> Result<Tag, TagsError> {
    if let Some(first_colon_position) = chunk.find(':') {
        if first_colon_position == 0 {
            return Err(TagsError {
                message: format!("tag cannot start with a colon: \"{}\"", chunk),
            });
        }

        if chunk.ends_with(':') {
            return Err(TagsError {
                message: format!("tag cannot end with a colon: \"{}\"", chunk),
            });
        }
        let name = &chunk[..first_colon_position];
        let value = &chunk[(first_colon_position + 1)..];
        Ok(Tag::new(name, value))
    } else {
        Ok(Tag::new(chunk, ""))
    }
}

/// Parse a string of tags typically provided by environment variables
/// The tags are expected to be either space or comma separated:
///     "key1:value1,key2:value2"
///     "key1:value1 key2:value2"
/// Tag names and values are required and may not be empty.
fn parse_tags(str: &str) -> Result<crate::Vec<Tag>, TagsError> {
    let vec: Vec<_> = str
        .split(&[',', ' '][..])
        .flat_map(parse_tag_chunk)
        .collect();
    Ok(vec.into())
}

#[repr(C)]
pub enum VecTagResult<'a> {
    Ok(crate::Vec<Tag<'a>>),
    Err(crate::Vec<u8>),
}

#[export_name = "ddprof_ffi_VecTagResult_drop"]
pub extern "C" fn vec_tag_result_drop(result: VecTagResult) {
    std::mem::drop(result)
}

#[must_use]
#[export_name = "ddprof_ffi_Vec_tag_parse"]
pub extern "C" fn vec_tag_parse(string: CharSlice) -> VecTagResult {
    match string.try_into() {
        Ok(str) => match parse_tags(str) {
            Ok(vec) => VecTagResult::Ok(vec),
            Err(err) => VecTagResult::Err(crate::Vec::from(&err as &dyn Error)),
        },

        Err(err) => VecTagResult::Err(crate::Vec::from(&err as &dyn Error)),
    }
}

#[must_use]
#[allow(clippy::ptr_arg)]
#[export_name = "ddprof_ffi_Vec_tag_clone"]
pub extern "C" fn vec_tag_clone<'a>(vec: &'a crate::Vec<Tag<'a>>) -> VecTagResult {
    let mut clone = Vec::new();
    for tag in vec.into_iter() {
        clone.push(tag.to_owned())
    }
    VecTagResult::Ok(crate::Vec::from(clone))
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
                Slice::default(),
                timeout_milliseconds,
            )
        };

        assert!(maybe_request.is_some());

        // TODO: Currently, we're only testing that a request was built (building did not fail), but
        //     we have no coverage for the request actually being correct.
        //     It'd be nice to actually perform the request, capture its contents, and assert that
        //     they are as expected.
    }

    #[test]
    fn test_parse_tags() {
        // See the docs for what we convey to users about tags:
        // https://docs.datadoghq.com/getting_started/tagging/

        let cases = [
            ("env:staging:east", vec![Tag::new("env", "staging:east")]),
            ("value", vec![Tag::new("value", "")]),
            (
                "state:utah,state:idaho",
                vec![Tag::new("state", "utah"), Tag::new("state", "idaho")],
            ),
            (
                "key1:value1 key2:value2 key3:value3",
                vec![
                    Tag::new("key1", "value1"),
                    Tag::new("key2", "value2"),
                    Tag::new("key3", "value3"),
                ],
            ),
            ("key1:", vec![]),
        ];

        for case in cases {
            let expected = case.1;
            let actual = parse_tags(case.0).unwrap();
            assert_eq!(expected, std::vec::Vec::from(actual));
        }
    }
}
