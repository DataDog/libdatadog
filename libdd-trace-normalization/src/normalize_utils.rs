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
pub(crate) const MAX_SERVICE_LEN: usize = 100;
/// MAX_TAG_LEN the maximum length a tag can have
const MAX_TAG_LEN: usize = 200;

// normalize_service normalizes a span service
pub fn normalize_service(svc: &mut String) {
    normalize_service_with_lang(svc, "");
}

/// Like [`normalize_service`], but falls back to `unnamed-<lang>-service` as the agent does when
/// `lang` is not empty.
///
/// The service is normalized as a tag value (see [`normalize_tag_value`]), as the agent's
/// `NormalizeService` does.
pub fn normalize_service_with_lang(svc: &mut String, lang: &str) {
    normalize_peer_service(svc);
    if svc.is_empty() {
        if lang.is_empty() {
            DEFAULT_SERVICE_NAME.clone_into(svc);
        } else {
            *svc = format!("unnamed-{lang}-service");
        }
    }
}

/// Normalizes a `peer.service` or `_dd.base_service` value, as the agent's `NormalizePeerService`
/// does: like [`normalize_service`], but a value that normalizes to nothing stays empty.
pub fn normalize_peer_service(svc: &mut String) {
    truncate_utf8(svc, MAX_SERVICE_LEN);
    normalize_tag_value(svc);
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
    if (*start).checked_add(*duration).is_none() {
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

pub fn normalize_tag_cloned(tag: &str) -> String {
    let mut tag = tag.to_owned();
    normalize_tag(&mut tag);
    tag
}

/// Normalizes a full `key:value` tag, as the agent's `NormalizeTag` does.
///
/// Characters that can't start a tag, such as digits, `.`, `/` and `-`, are removed from the start.
/// Use [`normalize_tag_value`] for a value without its key.
pub fn normalize_tag(tag: &mut String) {
    normalize(tag, true);
}

/// Normalizes a tag value, as the agent's `NormalizeTagValue` does.
///
/// Unlike [`normalize_tag`], it keeps leading digits, `.`, `/` and `-`: `1password` stays
/// `1password`. The agent uses it for the service, `peer.service`, `_dd.base_service` and `env`.
pub fn normalize_tag_value(value: &mut String) {
    normalize(value, false);
}

/// Normalizes `tag` in place, as the agent's `normalize` (`pkg/trace/traceutil/normalize`) does.
///
/// - ASCII letters are lowercased. Other uppercase characters are lowercased too.
/// - Letters, `:`, digits, `.`, `/` and `-` are kept.
/// - If `remove_digit_start_char` is set, characters up to the first letter or `:` are removed.
/// - Each run of other characters, `_` included, becomes one `_`. A run at the end is removed.
/// - Processing stops after [`MAX_TAG_LEN`] characters, or once it reaches `2 * MAX_TAG_LEN` bytes
///   of the input. Removed leading characters don't count toward the first limit.
fn normalize(tag: &mut String, remove_digit_start_char: bool) {
    // SAFETY: the loop below only writes complete UTF-8 encoded characters and `_` bytes, at a
    // write position that is never ahead of the read position, so the buffer stays valid UTF-8.
    let bytes = unsafe { tag.as_mut_vec() };
    let mut read = 0;
    let mut write = 0;
    // Number of characters processed, removed leading characters excluded.
    let mut chars = 0;
    // Whether the characters since the last written one were illegal.
    let mut pending_underscore = false;

    while read < bytes.len() {
        let b = bytes[read];
        let (c, len) = if b.is_ascii() {
            (char::from(b), 1)
        } else {
            let mut it = bytes[read..].iter();
            // The input is valid UTF-8 from `read` on, as nothing was written there yet.
            #[allow(clippy::unwrap_used)]
            let c = char::from_u32(crate::utf8_helpers::next_code_point(&mut it).unwrap()).unwrap();
            (c, c.len_utf8())
        };
        read += len;

        let keep = match c {
            'a'..='z' | ':' => {
                chars += 1;
                Some(c)
            }
            'A'..='Z' => {
                chars += 1;
                Some(c.to_ascii_lowercase())
            }
            _ => {
                let mut c = c;
                if c.is_uppercase()
                    && let Some(lower) = c.to_lowercase().next()
                    && lower.len_utf8() <= len
                {
                    c = lower;
                }
                if c.is_alphabetic() {
                    chars += 1;
                    Some(c)
                } else if remove_digit_start_char && chars == 0 {
                    None
                } else if c.is_numeric() || matches!(c, '.' | '/' | '-') {
                    chars += 1;
                    Some(c)
                } else {
                    chars += 1;
                    pending_underscore = true;
                    None
                }
            }
        };

        if let Some(c) = keep {
            if pending_underscore {
                bytes[write] = b'_';
                write += 1;
                pending_underscore = false;
            }
            write += c.encode_utf8(&mut bytes[write..]).len();
        }

        if read >= 2 * MAX_TAG_LEN || chars >= MAX_TAG_LEN {
            break;
        }
    }
    // A pending `_` at the end is dropped, as the agent removes trailing illegal characters.
    bytes.truncate(write);
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

    #[test]
    fn test_normalize_span_start_duration_handles_min_start() {
        let mut start = i64::MIN;
        let mut duration = 1;
        normalize_span_start_duration(&mut start, &mut duration);
        assert_eq!(duration, 1);
        assert!(start > YEAR_2000_NANOSEC_TS);
    }

    #[test]
    fn test_normalize_span_start_duration_handles_max_start() {
        let mut start = i64::MAX;
        let mut duration = 1;
        normalize_span_start_duration(&mut start, &mut duration);
        assert_eq!(duration, 0);
    }

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

    #[test]
    fn test_normalize_tag_counts_illegal_characters() {
        // As in the agent, each illegal character counts toward the limit of 200 characters, even
        // though a run of them only writes one `_`.
        let mut v = format!("a{}b", "!".repeat(250));
        normalize_tag(&mut v);
        assert_eq!(v, "a");
        // Removed leading characters don't count toward the character limit, but do count toward
        // the byte limit of 400.
        let mut v = format!("{}{}", "1".repeat(150), "a".repeat(300));
        normalize_tag(&mut v);
        assert_eq!(v, "a".repeat(200));
        let mut v = format!("{}{}", "1".repeat(350), "a".repeat(300));
        normalize_tag(&mut v);
        assert_eq!(v, "a".repeat(50));
    }

    // `svcNormCases` of the agent's `normalize_test.go`.
    const SERVICE_CASES: &[(&str, &str)] = &[
        ("good", "good"),
        ("127.0.0.1", "127.0.0.1"),
        (
            "127.site.platform-db-replica1",
            "127.site.platform-db-replica1",
        ),
        ("hyphenated-service-name", "hyphenated-service-name"),
        ("🐨animal-db🐶", "_animal-db"),
        ("🐨1animal-db🐶", "_1animal-db"),
        ("1🐨1animal-db🐶", "1_1animal-db"),
        (
            "Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.",
            "too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.",
        ),
        ("bad$service", "bad_service"),
    ];

    #[test]
    fn test_normalize_service_as_tag_value() {
        for (input, expected) in SERVICE_CASES {
            let mut v = (*input).to_owned();
            normalize_service(&mut v);
            assert_eq!(v, *expected, "{input}");
        }
        let mut v = "!!!".to_owned();
        normalize_service_with_lang(&mut v, "go");
        assert_eq!(v, "unnamed-go-service");
    }

    #[test]
    fn test_normalize_peer_service() {
        for (input, expected) in SERVICE_CASES.iter().chain(&[("", ""), ("!!!", "")]) {
            let mut v = (*input).to_owned();
            normalize_peer_service(&mut v);
            assert_eq!(v, *expected, "{input}");
        }
    }

    #[duplicate_item(
        test_name                     input                expected;
        [test_normalize_tag_value_1]  ["1love"]            ["1love"];
        [test_normalize_tag_value_2]  ["12.:trim@"]        ["12.:trim"];
        [test_normalize_tag_value_3]  ["---fun:k####y"]    ["---fun:k_y"];
        [test_normalize_tag_value_4]  ["_foo"]             ["_foo"];
        [test_normalize_tag_value_5]  ["___trim"]          ["_trim"];
        [test_normalize_tag_value_6]  ["#test"]            ["_test"];
        [test_normalize_tag_value_7]  ["123DEVELOPMENT"]   ["123development"];
        [test_normalize_tag_value_8]  [" spaces   "]       ["_spaces"];
        [test_normalize_tag_value_9]  [""]                 [""];
        [test_normalize_tag_value_10] ["/"]                ["/"];
    )]
    #[test]
    fn test_name() {
        let mut v = input.to_owned();
        normalize_tag_value(&mut v);
        assert_eq!(v, expected)
    }
}
