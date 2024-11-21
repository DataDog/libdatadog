// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::SystemTime;

// MAX_TYPE_LEN the maximum size for a span type
pub(crate) const MAX_TYPE_LEN: usize = 100;
/// an arbitrary cutoff to spot weird-looking values
/// nanoseconds since epoch on Jan 1, 2000
const YEAR_2000_NANOSEC_TS: i64 = 946684800000000000;
/// DEFAULT_SPAN_NAME is the default name we assign a span if it's missing and we have no reasonable
/// fallback
pub(crate) const DEFAULT_SPAN_NAME: &str = "unnamed_operation";
/// DEFAULT_SERVICE_NAME is the default name we assign a service if it's missing and we have no
/// reasonable fallback
pub(crate) const DEFAULT_SERVICE_NAME: &str = "unnamed-service";
/// MAX_NAME_LEN the maximum length a name can have
pub(crate) const MAX_NAME_LEN: usize = 100;
/// MAX_SERVICE_LEN the maximum length a service can have
const MAX_SERVICE_LEN: usize = 100;
/// MAX_SERVICE_LEN the maximum length a tag can have
const MAX_TAG_LEN: usize = 200;

// normalize_service normalizes a span service
pub fn normalize_service(svc: &mut String) {
    truncate_utf8(svc, MAX_SERVICE_LEN);
    normalize_tag(svc);
    if svc.is_empty() {
        DEFAULT_SERVICE_NAME.clone_into(svc);
    }
}

// normalize_name normalizes a span name or an error describing why normalization failed.
pub fn normalize_name(name: &mut String) {
    truncate_utf8(name, MAX_NAME_LEN);
    normalize_metric_name(name);
    if name.is_empty() {
        DEFAULT_SPAN_NAME.clone_into(name);
    }
}

#[allow(clippy::ptr_arg)]
pub fn normalize_resource(resource: &mut String, name: &str) {
    if resource.is_empty() {
        name.clone_into(resource);
    }
}

pub fn normalize_span_type(span_type: &mut String) {
    truncate_utf8(span_type, MAX_TYPE_LEN);
}

pub fn normalize_span_start_duration(start: &mut i64, duration: &mut i64) {
    // Start & Duration as nanoseconds timestamps
    // if s.Start is very little, less than year 2000 probably a unit issue so discard
    if *duration < 0 {
        *duration = 0;
    }
    if *duration > i64::MAX - *start {
        *duration = 0;
    }

    if *start < YEAR_2000_NANOSEC_TS {
        let now = SystemTime::UNIX_EPOCH.elapsed().map_or_else(
            |e| -(e.duration().as_nanos() as i64),
            |t| t.as_nanos() as i64,
        );
        *start = now - *duration;
        if *start < 0 {
            *start = now;
        }
    }
}

pub fn normalize_parent_id(parent_id: &mut u64, trace_id: u64, span_id: u64) {
    // ParentID, TraceID and SpanID set in the client could be the same
    // Supporting the ParentID == TraceID == SpanID for the root span, is compliant
    // with the Zipkin implementation. Furthermore, as described in the PR
    // https://github.com/openzipkin/zipkin/pull/851 the constraint that the
    // root span's ``trace id = span id`` has been removed
    if *parent_id == trace_id && *parent_id == span_id {
        *parent_id = 0;
    }
}

pub fn normalize_tag(tag: &mut String) {
    // Since we know that we're only going to write valid utf8 we can work with the Vec directly
    let bytes = unsafe { tag.as_mut_vec() };
    if bytes.is_empty() {
        return;
    }
    let mut read_cursor = 0;
    let mut write_cursor = 0;
    let mut is_in_illegal_span = true;
    let mut codepoints_written = 0;

    loop {
        if read_cursor >= bytes.len()
            || write_cursor >= 2 * MAX_TAG_LEN
            || codepoints_written >= MAX_TAG_LEN
        {
            break;
        }

        let b = bytes[read_cursor];
        // ascii fast-path
        match b {
            b'a'..=b'z' | b':' => {
                bytes[write_cursor] = b;
                is_in_illegal_span = false;
                write_cursor += 1;
                codepoints_written += 1;
                read_cursor += 1;
                continue;
            }
            b'A'..=b'Z' => {
                bytes[write_cursor] = b - b'A' + b'a';
                is_in_illegal_span = false;
                write_cursor += 1;
                codepoints_written += 1;
                read_cursor += 1;
                continue;
            }
            b'0'..=b'9' | b'.' | b'/' | b'-' => {
                if write_cursor != 0 {
                    bytes[write_cursor] = b;
                    is_in_illegal_span = false;
                    write_cursor += 1;
                    codepoints_written += 1;
                }
                read_cursor += 1;
                continue;
            }
            b'_' if !is_in_illegal_span => {
                if write_cursor != 0 {
                    bytes[write_cursor] = b;
                    is_in_illegal_span = true;
                    write_cursor += 1;
                    codepoints_written += 1;
                }
                read_cursor += 1;
                continue;
            }
            // ASCII range
            0x00..=0x7F if !is_in_illegal_span => {
                bytes[write_cursor] = b'_';
                is_in_illegal_span = true;
                write_cursor += 1;
                codepoints_written += 1;
                read_cursor += 1;
                continue;
            }
            0x00..=0x7F if is_in_illegal_span => {
                read_cursor += 1;
                continue;
            }
            _ => {}
        }

        // Grab current unicode codepoint
        let mut c = {
            let mut it = bytes[read_cursor..].iter();
            // This won't panic because we now bytes is a valid utf8 array, and next_code_point
            // returns and actual utf8 codepoint
            std::char::from_u32(crate::utf8_helpers::next_code_point(&mut it).unwrap()).unwrap()
        };
        let mut len_utf8 = c.len_utf8();
        read_cursor += len_utf8;

        if c.is_lowercase() {
            c.encode_utf8(&mut bytes[write_cursor..write_cursor + len_utf8]);
            is_in_illegal_span = false;
            write_cursor += len_utf8;
            codepoints_written += 1;
            continue;
        }
        if c.is_uppercase() {
            // Take only first codepoint of the lowercase conversion
            // Lowercase the current character if it has the same width as it's lower
            if let Some(lower) = c.to_lowercase().next() {
                if lower.len_utf8() <= len_utf8 {
                    c = lower;
                    len_utf8 = c.len_utf8();
                }
            }
        }

        // The method in the agent checks if the character is of a Letter unicode class,
        // which is not excatly the same. Alphabetics also contains Nl and Other_aplhabetics
        // unicode character classes https://www.unicode.org/reports/tr44/#Alphabetic , but
        // close enough
        if c.is_alphabetic() {
            c.encode_utf8(&mut bytes[write_cursor..write_cursor + len_utf8]);
            is_in_illegal_span = false;
            write_cursor += len_utf8;
            codepoints_written += 1;
        } else if c.is_numeric() {
            if write_cursor != 0 {
                c.encode_utf8(&mut bytes[write_cursor..write_cursor + len_utf8]);
                is_in_illegal_span = false;
                write_cursor += len_utf8;
                codepoints_written += 1;
            }
        } else if !is_in_illegal_span {
            bytes[write_cursor] = b'_';
            is_in_illegal_span = true;
            write_cursor += 1;
            codepoints_written += 1;
        }
    }
    // If we end up in an illegal span, remove the last written _
    if is_in_illegal_span && write_cursor > 0 {
        write_cursor -= 1;
    }
    bytes.truncate(write_cursor);
}

fn normalize_metric_name(name: &mut String) {
    // Since we know that we're only going to write valid utf8 we can work with the Vec directly
    let bytes = unsafe { name.as_mut_vec() };
    if bytes.is_empty() {
        return;
    }

    // Find first alpha character, if none is found the metric name is empty
    let Some((mut read_cursor, _)) = bytes
        .iter()
        .enumerate()
        .find(|(_, c)| c.is_ascii_alphabetic())
    else {
        *name = String::new();
        return;
    };
    let mut write_cursor = 0;
    let mut last_written_char = 0;
    loop {
        if read_cursor >= bytes.len() {
            break;
        }
        match (bytes[read_cursor], last_written_char) {
            (b @ (b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'), _) => {
                bytes[write_cursor] = b;
                last_written_char = b;
            }
            // If we've written a _ last, replace it with a .
            (b'.', b'_') => {
                // This safe because the first character is alpha so
                // we don't go back to the beginning
                write_cursor -= 1;
                bytes[write_cursor] = b'.';
                last_written_char = b'.'
            }
            // If we've written a _ or a . last, do nothing
            (_, b'_' | b'.') => {}
            (b @ (b'_' | b'.'), _) => {
                bytes[write_cursor] = b;
                last_written_char = b;
            }
            // Otherwise write _ instead of any non conforming char
            (_, _) => {
                bytes[write_cursor] = b'_';
                last_written_char = b'_';
            }
        }
        write_cursor += 1;
        read_cursor += 1;
    }
    if last_written_char == b'_' {
        write_cursor -= 1;
    }
    bytes.truncate(write_cursor);
}

// truncate_utf8 truncates the given string to make sure it uses less than limit bytes.
// If the last character is a utf8 character that would be split, it removes it
// entirely to make sure the resulting string is not broken.
pub(crate) fn truncate_utf8(s: &mut String, limit: usize) {
    let boundary = crate::utf8_helpers::floor_char_boundary(s, limit);
    s.truncate(boundary);
}

#[cfg(test)]
mod tests {

    use super::*;
    use duplicate::duplicate_item;

    #[duplicate_item(
        test_name                       input                               expected;
        [test_normalize_empty_string]   [""]                                ["unnamed_operation"];
        [test_normalize_valid_string]   ["good"]                            ["good"];
        [test_normalize_long_string]    ["Too-Long-.".repeat(20).as_str()]  ["Too_Long.".repeat(10)];
        [test_normalize_dash_string]    ["bad-name"]                        ["bad_name"];
        [test_normalize_invalid_string] ["&***"]                            ["unnamed_operation"];
        [test_normalize_invalid_prefix] ["&&&&&&&_test-name-"]              ["test_name"];
    )]
    #[test]
    fn test_name() {
        let mut val = input.to_owned();
        normalize_name(&mut val);
        assert_eq!(val, expected);
    }

    #[duplicate_item(
        test_name                       input                               expected;
        [test_normalize_empty_service]  [""]                                ["unnamed-service"];
        [test_normalize_valid_service]  ["good"]                            ["good"];
        [test_normalize_long_service]   ["Too$Long$.".repeat(20).as_str()]  ["too_long_.".repeat(10)];
        [test_normalize_dash_service]   ["bad&service"]                     ["bad_service"];
    )]
    #[test]
    fn test_name() {
        let mut val = input.to_owned();
        normalize_service(&mut val);
        assert_eq!(val, expected);
    }

    #[duplicate_item(
        test_name               input   expected;
        [test_normalize_tag_1]  ["#test_starting_hash"] ["test_starting_hash"];
        [test_normalize_tag_2]  ["TestCAPSandSuch"] ["testcapsandsuch"];
        [test_normalize_tag_3]  ["Test Conversion Of Weird !@#$%^&**() Characters"] ["test_conversion_of_weird_characters"];
        [test_normalize_tag_4]  ["$#weird_starting"] ["weird_starting"];
        [test_normalize_tag_5]  ["allowed:c0l0ns"] ["allowed:c0l0ns"];
        [test_normalize_tag_6]  ["1love"] ["love"];
        [test_normalize_tag_7]  ["ünicöde"] ["ünicöde"];
        [test_normalize_tag_8]  ["ünicöde:metäl"] ["ünicöde:metäl"];
        [test_normalize_tag_9]  ["Data🐨dog🐶 繋がっ⛰てて"] ["data_dog_繋がっ_てて"];
        [test_normalize_tag_10] [" spaces   "] ["spaces"];
        [test_normalize_tag_11] [" #hashtag!@#spaces #__<>#  "] ["hashtag_spaces"];
        [test_normalize_tag_12] [":testing"] [":testing"];
        [test_normalize_tag_13] ["_foo"] ["foo"];
        [test_normalize_tag_14] [":::test"] [":::test"];
        [test_normalize_tag_15] ["contiguous_____underscores"] ["contiguous_underscores"];
        [test_normalize_tag_16] ["foo_"] ["foo"];
        [test_normalize_tag_17] ["\u{017F}odd_\u{017F}case\u{017F}"] ["\u{017F}odd_\u{017F}case\u{017F}"] ; // edge-case
        [test_normalize_tag_18] [""] [""];
        [test_normalize_tag_19] [" "] [""];
        [test_normalize_tag_20] ["ok"] ["ok"];
        [test_normalize_tag_21] ["™Ö™Ö™™Ö™"] ["ö_ö_ö"];
        [test_normalize_tag_22] ["AlsO:ök"] ["also:ök"];
        [test_normalize_tag_23] [":still_ok"] [":still_ok"];
        [test_normalize_tag_24] ["___trim"] ["trim"];
        [test_normalize_tag_25] ["12.:trim@"] [":trim"];
        [test_normalize_tag_26] ["12.:trim@@"] [":trim"];
        [test_normalize_tag_27] ["fun:ky__tag/1"] ["fun:ky_tag/1"];
        [test_normalize_tag_28] ["fun:ky@tag/2"] ["fun:ky_tag/2"];
        [test_normalize_tag_29] ["fun:ky@@@tag/3"] ["fun:ky_tag/3"];
        [test_normalize_tag_30] ["tag:1/2.3"] ["tag:1/2.3"];
        [test_normalize_tag_31] ["---fun:k####y_ta@#g/1_@@#"]["fun:k_y_ta_g/1"];
        [test_normalize_tag_32] ["AlsO:œ#@ö))œk"] ["also:œ_ö_œk"];
        [test_normalize_tag_33] ["a".repeat(888).as_str()] ["a".repeat(200)];
        [test_normalize_tag_34] [("a".to_owned() + &"🐶".repeat(799)).as_str()] ["a"];
        [test_normalize_tag_35] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string()).as_str()] ["a"];
        [test_normalize_tag_36] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string()).as_str()] ["a"];
        [test_normalize_tag_37] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string() + "b").as_str()] ["a_b"];
        [test_normalize_tag_38]
            ["A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000"]
            ["a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000_0"]
           ;
    )]
    #[test]
    fn test_name() {
        let mut v = input.to_owned();
        normalize_tag(&mut v);
        assert_eq!(v, expected)
    }
}
