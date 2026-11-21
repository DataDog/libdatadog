// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP requests ready to send: Datadog intake (pprof) or OTLP/HTTP.
//!
//! Nothing here does any networking: the caller's HTTP client sends the
//! [`Request`] to its own base URL and adds its own credentials and entity
//! headers (API key, container id, ...).
//!
//! [`Compression::Auto`] picks per format. For the Datadog intake it means
//! zstd when built in, else gzip when built in, else none: `profile.pprof` and
//! each attachment are compressed (the intake detects compression from each
//! part's bytes; the multipart body and `event.json` are not). For OTLP it
//! means none, with `Content-Encoding` set for an explicit gzip or zstd.

use crate::collections::alloc::Global;
use crate::collections::vec::TryVec;
pub use crate::compress::Compression;
#[cfg(feature = "otel")]
use crate::encode::Resource as OtlpResource;
#[cfg(feature = "otel")]
use crate::encode::otlp::{self, Scope};
#[cfg(any(feature = "pprof", feature = "otel"))]
use crate::encode::{EncodeError, EncodeOptions};
#[cfg(feature = "pprof")]
use crate::encode::{Resource, pprof};
#[cfg(any(feature = "pprof", feature = "otel"))]
use crate::profile::{Function, Profile, Resolve, Stacks, Str};
#[cfg(feature = "pprof")]
use core::hash::BuildHasher;

/// One request header.
#[derive(Debug)]
pub struct Header {
    pub name: &'static str,
    pub value: TryVec<u8, Global>,
}

/// An HTTP request to send. `path` is relative to the destination's base
/// URL (the agent, the intake, or the collector).
#[derive(Debug)]
pub struct Request {
    pub method: &'static str,
    pub path: &'static str,
    pub headers: TryVec<Header, Global>,
    pub body: TryVec<u8, Global>,
}

#[cfg(feature = "pprof")]
/// Extra inputs of a Datadog intake request. Empty strings are omitted.
#[derive(Clone, Copy, Debug, Default)]
pub struct DatadogOptions<'a> {
    /// `DD-EVP-ORIGIN`, such as `dd-trace-py`.
    pub library_name: &'a str,
    /// `DD-EVP-ORIGIN-VERSION`.
    pub library_version: &'a str,
    /// The profiler family, such as `python`.
    pub family: &'a str,
    /// A JSON object for `internal`; `libdatadog_version` is added to it.
    pub internal_json: &'a str,
    /// A JSON object for `info`.
    pub info_json: &'a str,
    pub process_tags: &'a str,
    /// Further attachments, such as `code-provenance.json`: (name, bytes).
    pub attachments: &'a [(&'a str, &'a [u8])],
    /// The pprof's sampling period, if any.
    pub period: Option<crate::encode::Period<'a>>,
}

#[cfg(feature = "pprof")]
/// The Datadog intake path, relative to the agent or intake base URL.
pub const DATADOG_PATH: &str = "/profiling/v1/input";
#[cfg(feature = "otel")]
/// The OTLP/HTTP profiles path, relative to the collector's base URL.
pub const OTLP_PATH: &str = "/v1development/profiles";

#[cfg(feature = "pprof")]
/// A Datadog intake request: a multipart form with `event.json` and
/// `profile.pprof`.
///
/// # Errors
///
/// See [`EncodeError`].
pub fn datadog<S, F, K>(
    profile: &Profile<S, F, K>,
    end_ns: i64,
    resource: &Resource<'_>,
    options: &DatadogOptions<'_>,
    encode: EncodeOptions,
    compression: Compression,
) -> Result<Request, EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let compression = compression.for_pprof();
    let pprof = crate::compress::compress(
        pprof::encode(profile, end_ns, options.period, encode)?,
        compression,
    )?;
    let event = event_json(profile, end_ns, resource, options)?;
    // Attachments are compressed like the profile.
    let mut files: TryVec<TryVec<u8, Global>, Global> =
        TryVec::try_with_capacity_in(options.attachments.len(), Global)
            .map_err(|_| EncodeError::OutOfMemory)?;
    for &(_, bytes) in options.attachments {
        let mut copy = TryVec::try_with_capacity_in(bytes.len(), Global)
            .map_err(|_| EncodeError::OutOfMemory)?;
        copy.try_extend_from_slice(bytes)
            .map_err(|_| EncodeError::OutOfMemory)?;
        let _ = files.push_within_capacity(crate::compress::compress(copy, compression)?);
    }

    let mut parts: TryVec<Part<'_>, Global> = TryVec::new_in(Global);
    let mut push = |part| parts.try_push(part).map_err(|_| EncodeError::OutOfMemory);
    push(Part {
        name: "event",
        filename: "event.json",
        content_type: "application/json",
        bytes: &event,
    })?;
    for (&(name, _), bytes) in options.attachments.iter().zip(files.iter()) {
        push(Part {
            name,
            filename: name,
            content_type: "",
            bytes,
        })?;
    }
    push(Part {
        name: "profile.pprof",
        filename: "profile.pprof",
        content_type: "",
        bytes: &pprof,
    })?;

    let boundary = boundary(&parts);
    let mut body = Out::new();
    for part in &parts {
        body.str("--")?;
        body.bytes(&boundary)?;
        body.str("\r\nContent-Disposition: form-data; name=\"")?;
        body.str(part.name)?;
        body.str("\"; filename=\"")?;
        body.str(part.filename)?;
        body.str("\"\r\n")?;
        if !part.content_type.is_empty() {
            body.str("Content-Type: ")?;
            body.str(part.content_type)?;
            body.str("\r\n")?;
        }
        body.str("\r\n")?;
        body.bytes(part.bytes)?;
        body.str("\r\n")?;
    }
    body.str("--")?;
    body.bytes(&boundary)?;
    body.str("--\r\n")?;

    let mut content_type = Out::new();
    content_type.str("multipart/form-data; boundary=")?;
    content_type.bytes(&boundary)?;
    let mut headers = TryVec::new_in(Global);
    add_header(&mut headers, "Content-Type", content_type.0)?;
    if !options.library_name.is_empty() {
        add_header(
            &mut headers,
            "DD-EVP-ORIGIN",
            Out::from(options.library_name)?,
        )?;
    }
    if !options.library_version.is_empty() {
        add_header(
            &mut headers,
            "DD-EVP-ORIGIN-VERSION",
            Out::from(options.library_version)?,
        )?;
    }
    Ok(Request {
        method: "POST",
        path: DATADOG_PATH,
        headers,
        body: body.0,
    })
}

#[cfg(feature = "otel")]
/// An OTLP/HTTP profiles request, with a protobuf body compressed as
/// `compression` says.
///
/// # Errors
///
/// See [`EncodeError`]; [`EncodeError::NothingToEncode`] when the profile has
/// no CPU or memory sample type.
pub fn otlp<S, F, K>(
    profile: &Profile<S, F, K>,
    end_ns: i64,
    resource: &OtlpResource<'_>,
    scope: &Scope<'_>,
    specs: &[otlp::ProfileSpec<'_>],
    encode: EncodeOptions,
    compression: Compression,
) -> Result<Request, EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let compression = compression.for_otlp();
    let body = otlp::encode(profile, end_ns, resource, scope, specs, encode)?;
    let body = crate::compress::compress(body, compression)?;
    let mut headers = TryVec::new_in(Global);
    add_header(
        &mut headers,
        "Content-Type",
        Out::from("application/x-protobuf")?,
    )?;
    if let Some(encoding) = compression.content_encoding() {
        add_header(&mut headers, "Content-Encoding", Out::from(encoding)?)?;
    }
    Ok(Request {
        method: "POST",
        path: OTLP_PATH,
        headers,
        body,
    })
}

#[cfg(any(feature = "pprof", feature = "otel"))]
fn add_header(
    headers: &mut TryVec<Header, Global>,
    name: &'static str,
    value: TryVec<u8, Global>,
) -> Result<(), EncodeError> {
    headers
        .try_push(Header { name, value })
        .map_err(|_| EncodeError::OutOfMemory)
}

#[cfg(feature = "pprof")]
struct Part<'a> {
    name: &'a str,
    filename: &'a str,
    content_type: &'a str,
    bytes: &'a [u8],
}

#[cfg(feature = "pprof")]
/// A multipart boundary that occurs in no part: 40 hex digits derived from
/// the parts' contents, re-derived in the (vanishingly rare) case of a clash.
fn boundary(parts: &[Part<'_>]) -> [u8; 40] {
    let mut seed = 0_u64;
    loop {
        let mut out = *b"ddprof-boundary-000000000000000000000000";
        let mut state = seed;
        for part in parts {
            state ^= foldhash::fast::FixedState::with_seed(state).hash_one(part.bytes);
        }
        for (slot, nibble) in out.iter_mut().skip(24).zip(0..) {
            let [digit, ..] = ((state >> (nibble * 4)) & 0xf).to_le_bytes();
            *slot = b"0123456789abcdef"
                .get(usize::from(digit))
                .copied()
                .unwrap_or(b'0');
        }
        if !parts.iter().any(|part| contains(part.bytes, &out)) {
            return out;
        }
        seed = seed.wrapping_add(1);
    }
}

#[cfg(feature = "pprof")]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(feature = "pprof")]
/// The `event.json` attachment.
fn event_json<S, F, K>(
    profile: &Profile<S, F, K>,
    end_ns: i64,
    resource: &Resource<'_>,
    options: &DatadogOptions<'_>,
) -> Result<TryVec<u8, Global>, EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let mut out = Out::new();
    out.str("{\"attachments\":[")?;
    for &(name, _) in options.attachments {
        out.json_string(name)?;
        out.str(",")?;
    }
    out.str("\"profile.pprof\"],\"tags_profiler\":")?;
    let tags = tags_profiler(resource)?;
    out.json_string_bytes(&tags)?;
    out.str(",\"start\":\"")?;
    out.rfc3339(profile.start_ns())?;
    out.str("\",\"end\":\"")?;
    out.rfc3339(end_ns)?;
    out.str("\",\"family\":")?;
    out.json_string(options.family)?;
    out.str(",\"version\":\"4\"")?;

    let mut counts = profile.endpoint_counts().peekable();
    if counts.peek().is_some() {
        out.str(",\"endpoint_counts\":{")?;
        for (i, (endpoint, count)) in counts.enumerate() {
            if i > 0 {
                out.str(",")?;
            }
            out.json_string(endpoint)?;
            out.str(":")?;
            out.i64(count)?;
        }
        out.str("}")?;
    }
    if !options.process_tags.is_empty() {
        out.str(",\"process_tags\":")?;
        out.json_string(options.process_tags)?;
    }

    // `internal` gets `libdatadog_version` merged into the caller's object.
    out.str(",\"internal\":{\"libdatadog_version\":\"")?;
    out.str(env!("CARGO_PKG_VERSION"))?;
    out.str("\"")?;
    let members = object_members(options.internal_json);
    if !members.is_empty() {
        out.str(",")?;
        out.str(members)?;
    }
    out.str("},\"info\":")?;
    let info = options.info_json.trim();
    out.str(if info.is_empty() { "{}" } else { info })?;
    out.str("}")?;
    Ok(out.0)
}

#[cfg(feature = "pprof")]
/// The members of a JSON object, without its braces: `{"a":1}` gives
/// `"a":1`. Anything that is not an object gives nothing.
fn object_members(object: &str) -> &str {
    object
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .map_or("", str::trim)
}

#[cfg(feature = "pprof")]
/// The resource as Datadog's comma-separated `key:value` tags.
fn tags_profiler(resource: &Resource<'_>) -> Result<TryVec<u8, Global>, EncodeError> {
    let mut out = Out::new();
    let mut first = true;
    let mut tag = |out: &mut Out, key: &str, value: &str| -> Result<(), EncodeError> {
        if value.is_empty() {
            return Ok(());
        }
        if !first {
            out.str(",")?;
        }
        first = false;
        out.str(key)?;
        out.str(":")?;
        out.str(value)
    };
    for (key, _, value) in resource.fields() {
        tag(&mut out, key, value)?;
    }
    if resource.process_id != 0 {
        let mut digits = [0_u8; 20];
        let digits = decimal(u64::from(resource.process_id), &mut digits);
        tag(
            &mut out,
            "process_id",
            core::str::from_utf8(digits).unwrap_or(""),
        )?;
    }
    for &(key, value) in resource.tags {
        tag(&mut out, key, value)?;
    }
    Ok(out.0)
}

#[cfg(feature = "pprof")]
/// The decimal digits of `value`, in `buf`.
fn decimal(mut value: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut start = buf.len();
    loop {
        start -= 1;
        let [digit, ..] = (value % 10).to_le_bytes();
        if let Some(slot) = buf.get_mut(start) {
            *slot = b'0' + digit;
        }
        value /= 10;
        if value == 0 || start == 0 {
            break;
        }
    }
    buf.get(start..).unwrap_or(&[])
}

/// A fallible byte buffer for building text.
#[cfg(any(feature = "pprof", feature = "otel"))]
struct Out(TryVec<u8, Global>);

#[cfg(any(feature = "pprof", feature = "otel"))]
impl Out {
    const fn new() -> Self {
        Self(TryVec::new_in(Global))
    }

    fn from(s: &str) -> Result<TryVec<u8, Global>, EncodeError> {
        let mut out = Self::new();
        out.str(s)?;
        Ok(out.0)
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.0
            .try_extend_from_slice(bytes)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    fn str(&mut self, s: &str) -> Result<(), EncodeError> {
        self.bytes(s.as_bytes())
    }

    #[cfg(feature = "pprof")]
    fn i64(&mut self, value: i64) -> Result<(), EncodeError> {
        if value < 0 {
            self.str("-")?;
        }
        let mut digits = [0_u8; 20];
        self.bytes(decimal(value.unsigned_abs(), &mut digits))
    }

    #[cfg(feature = "pprof")]
    fn json_string(&mut self, s: &str) -> Result<(), EncodeError> {
        self.json_string_bytes(s.as_bytes())
    }

    #[cfg(feature = "pprof")]
    /// A JSON string of UTF-8 `bytes`, escaped.
    fn json_string_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.str("\"")?;
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let escape: Option<&[u8]> = match b {
                b'"' => Some(b"\\\""),
                b'\\' => Some(b"\\\\"),
                b'\n' => Some(b"\\n"),
                b'\r' => Some(b"\\r"),
                b'\t' => Some(b"\\t"),
                0..0x20 => None,
                _ => continue,
            };
            self.bytes(bytes.get(start..i).unwrap_or(&[]))?;
            if let Some(escape) = escape {
                self.bytes(escape)?;
            } else {
                let hex = b"0123456789abcdef";
                let hi = hex.get(usize::from(b >> 4)).copied().unwrap_or(b'0');
                let lo = hex.get(usize::from(b & 0xf)).copied().unwrap_or(b'0');
                self.bytes(&[b'\\', b'u', b'0', b'0', hi, lo])?;
            }
            start = i + 1;
        }
        self.bytes(bytes.get(start..).unwrap_or(&[]))?;
        self.str("\"")
    }

    #[cfg(feature = "pprof")]
    /// `ns` (Unix nanoseconds) as `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`.
    fn rfc3339(&mut self, ns: i64) -> Result<(), EncodeError> {
        let secs = ns.div_euclid(1_000_000_000);
        let nanos = ns.rem_euclid(1_000_000_000).unsigned_abs();
        let days = secs.div_euclid(86_400);
        let second_of_day = secs.rem_euclid(86_400).unsigned_abs();
        let (year, month, day) = civil_from_days(days);
        let mut text = *b"0000-00-00T00:00:00.000000000Z";
        put(&mut text, 0, year.clamp(0, 9999).unsigned_abs(), 4);
        put(&mut text, 5, u64::from(month), 2);
        put(&mut text, 8, u64::from(day), 2);
        put(&mut text, 11, second_of_day / 3600, 2);
        put(&mut text, 14, second_of_day % 3600 / 60, 2);
        put(&mut text, 17, second_of_day % 60, 2);
        put(&mut text, 20, nanos, 9);
        self.bytes(&text)
    }
}

#[cfg(feature = "pprof")]
/// Writes `value` as `width` zero-padded decimal digits at `at`.
fn put(text: &mut [u8], at: usize, mut value: u64, width: usize) {
    for i in (0..width).rev() {
        if let Some(slot) = text.get_mut(at + i) {
            let [digit, ..] = (value % 10).to_le_bytes();
            *slot = b'0' + digit;
        }
        value /= 10;
    }
}

#[cfg(feature = "pprof")]
/// The proleptic Gregorian date of a day count from 1970-01-01, after
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // `day` is 1..=31 and `month` 1..=12, so both fit.
    (year, day_or_month(month), day_or_month(day))
}

#[cfg(feature = "pprof")]
fn day_or_month(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

#[cfg(test)]
#[cfg(feature = "pprof")]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_unix_nanoseconds() {
        let mut out = Out::new();
        out.rfc3339(0).unwrap();
        out.str(" ").unwrap();
        out.rfc3339(1_700_000_000_123_456_789).unwrap();
        out.str(" ").unwrap();
        out.rfc3339(951_782_400_000_000_000).unwrap(); // 2000-02-29
        assert_eq!(
            core::str::from_utf8(&out.0).unwrap(),
            "1970-01-01T00:00:00.000000000Z 2023-11-14T22:13:20.123456789Z \
             2000-02-29T00:00:00.000000000Z"
        );
    }

    #[test]
    fn json_strings_are_escaped() {
        let mut out = Out::new();
        out.json_string("a\"b\\c\n\u{1}é").unwrap();
        assert_eq!(
            core::str::from_utf8(&out.0).unwrap(),
            "\"a\\\"b\\\\c\\n\\u0001é\""
        );
    }

    #[test]
    fn object_members_strip_braces() {
        assert_eq!(object_members(" {\"a\": 1} "), "\"a\": 1");
        assert_eq!(object_members("{}"), "");
        assert_eq!(object_members("[1]"), "");
        assert_eq!(object_members(""), "");
    }

    #[test]
    fn decimals() {
        let mut buf = [0; 20];
        assert_eq!(decimal(0, &mut buf), b"0");
        assert_eq!(decimal(u64::MAX, &mut buf), b"18446744073709551615");
    }
}
