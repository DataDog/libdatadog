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

/// Why a normalizer changed a value, as the agent's `ErrEmpty`, `ErrTooLong` and `ErrInvalid`.
///
/// The `_with_reason` functions return it. The value is normalized in any case: the reason only
/// tells why it changed, for telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NormalizeError {
    /// The value was empty, and was replaced with a fallback.
    Empty,
    /// The value was longer than its limit, and was truncated.
    TooLong,
    /// Nothing valid was left after normalization, so the value was replaced with a fallback.
    Invalid,
}

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
    let _ = normalize_service_with_reason(svc, lang);
}

/// Like [`normalize_service_with_lang`], and returns why the service changed, as the agent's
/// `NormalizeService` does.
///
/// Character repairs alone return `None`. An empty service returns [`NormalizeError::Empty`], and
/// one with nothing valid left returns [`NormalizeError::Invalid`], even if it was truncated.
pub fn normalize_service_with_reason(svc: &mut String, lang: &str) -> Option<NormalizeError> {
    let reason = if svc.is_empty() {
        Some(NormalizeError::Empty)
    } else {
        // A non-empty service that normalizes to nothing returns `Invalid`.
        normalize_peer_service_with_reason(svc)
    };
    if svc.is_empty() {
        if lang.is_empty() {
            DEFAULT_SERVICE_NAME.clone_into(svc);
        } else {
            *svc = format!("unnamed-{lang}-service");
        }
    }
    reason
}

/// Normalizes a `peer.service` or `_dd.base_service` value, as the agent's `NormalizePeerService`
/// does: like [`normalize_service`], but a value that normalizes to nothing stays empty.
pub fn normalize_peer_service(svc: &mut String) {
    let _ = normalize_peer_service_with_reason(svc);
}

/// Like [`normalize_peer_service`], and returns why the value changed, as the agent's
/// `NormalizePeerService` does.
///
/// An empty value returns `None`. A value with nothing valid left returns
/// [`NormalizeError::Invalid`], even if it was truncated.
pub fn normalize_peer_service_with_reason(svc: &mut String) -> Option<NormalizeError> {
    if svc.is_empty() {
        return None;
    }
    let too_long = svc.len() > MAX_SERVICE_LEN;
    truncate_utf8(svc, MAX_SERVICE_LEN);
    normalize_tag_value(svc);
    if svc.is_empty() {
        Some(NormalizeError::Invalid)
    } else {
        too_long.then_some(NormalizeError::TooLong)
    }
}

// normalize_name normalizes a span name or an error describing why normalization failed.
pub fn normalize_name(name: &mut String) {
    let _ = normalize_name_with_reason(name);
}

/// Like [`normalize_name`], and returns why the name changed, as the agent's `NormalizeName`
/// does.
///
/// Character repairs alone return `None`. An empty name returns [`NormalizeError::Empty`], and one
/// without an ASCII letter returns [`NormalizeError::Invalid`], even if it was truncated.
pub fn normalize_name_with_reason(name: &mut String) -> Option<NormalizeError> {
    if name.is_empty() {
        DEFAULT_SPAN_NAME.clone_into(name);
        return Some(NormalizeError::Empty);
    }
    let too_long = name.len() > MAX_NAME_LEN;
    truncate_utf8(name, MAX_NAME_LEN);
    normalize_metric_name(name);
    if name.is_empty() {
        DEFAULT_SPAN_NAME.clone_into(name);
        return Some(NormalizeError::Invalid);
    }
    too_long.then_some(NormalizeError::TooLong)
}

#[allow(clippy::ptr_arg)]
pub fn normalize_resource(resource: &mut String, name: &str) {
    if resource.is_empty() {
        name.clone_into(resource);
    }
}

pub fn normalize_span_type(span_type: &mut String) {
    let _ = normalize_span_type_with_reason(span_type);
}

/// Like [`normalize_span_type`], and returns [`NormalizeError::TooLong`] if the type was
/// truncated.
pub fn normalize_span_type_with_reason(span_type: &mut String) -> Option<NormalizeError> {
    if span_type.len() <= MAX_TYPE_LEN {
        return None;
    }
    truncate_utf8(span_type, MAX_TYPE_LEN);
    Some(NormalizeError::TooLong)
}

pub fn normalize_span_start_duration(start: &mut i64, duration: &mut i64) {
    // Start & Duration as nanoseconds timestamps
    // if s.Start is very little, less than year 2000 probably a unit issue so discard
    if let Some(fixed) = fixed_duration(*start, *duration) {
        *duration = fixed;
    }
    if let Some(fixed) = fixed_start(*start, *duration) {
        *start = fixed;
    }
}

/// Returns 0 if `duration` is invalid: negative, or so large that `start + duration` overflows.
/// Returns `None` if it is valid.
pub(crate) fn fixed_duration(start: i64, duration: i64) -> Option<i64> {
    (duration < 0 || start.checked_add(duration).is_none()).then_some(0)
}

/// Returns the start to use if `start` is before the year 2000, probably a unit issue: now minus
/// `duration`, or now if that is negative. Returns `None` if `start` is valid.
pub(crate) fn fixed_start(start: i64, duration: i64) -> Option<i64> {
    if start >= YEAR_2000_NANOSEC_TS {
        return None;
    }
    let now = SystemTime::UNIX_EPOCH.elapsed().map_or_else(
        |e| -(e.duration().as_nanos() as i64),
        |t| t.as_nanos() as i64,
    );
    let start = now - duration;
    Some(if start < 0 { now } else { start })
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
/// - Uppercase characters are lowercased, if the lowercase character has the same UTF-8 width.
/// - Letters, `:`, digits, `.`, `/` and `-` are kept.
/// - If `remove_digit_start_char` is set, characters up to the first letter or `:` are removed.
/// - Each run of other characters, `_` included, becomes one `_`. A run at the end is removed.
/// - Processing stops after [`MAX_TAG_LEN`] characters, or once it reaches `2 * MAX_TAG_LEN` bytes
///   of the input. Removed leading characters don't count toward the first limit.
fn normalize(tag: &mut String, remove_digit_start_char: bool) {
    if is_normalized_ascii_tag(tag, remove_digit_start_char) {
        return;
    }
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
                // Lowercase only if the width is the same, as the agent does: some lowercase
                // characters are narrower, such as `i` for `İ`.
                if c.is_uppercase()
                    && let Some(lower) = c.to_lowercase().next()
                    && lower.len_utf8() == len
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

/// Whether `tag` is ASCII and already normalized, as the agent's `isNormalizedASCIITag` checks:
/// then [`normalize`] doesn't change it.
///
/// `tag` has at most [`MAX_TAG_LEN`] bytes, made of lowercase ASCII letters, `:`, digits, `.`, `/`
/// and `-`, and `_` followed by one of them. If `check_valid_start_char` is set, it starts with a
/// lowercase ASCII letter or `:`.
pub(crate) fn is_normalized_ascii_tag(tag: &str, check_valid_start_char: bool) -> bool {
    fn is_valid_start_char(b: u8) -> bool {
        b.is_ascii_lowercase() || b == b':'
    }
    fn is_valid_char(b: u8) -> bool {
        is_valid_start_char(b) || b.is_ascii_digit() || matches!(b, b'.' | b'/' | b'-')
    }

    let bytes = tag.as_bytes();
    if bytes.len() > MAX_TAG_LEN {
        return false;
    }
    let mut i = 0;
    if check_valid_start_char && let Some(&first) = bytes.first() {
        if !is_valid_start_char(first) {
            return false;
        }
        i = 1;
    }
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'_' {
            // An `_` is only valid if a valid character other than `_` follows it.
            i += 1;
            if !bytes.get(i).is_some_and(|&b| is_valid_char(b)) {
                return false;
            }
        } else if !is_valid_char(b) {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether [`normalize_service`] doesn't change `svc`.
pub(crate) fn is_normalized_service(svc: &str) -> bool {
    !svc.is_empty() && is_normalized_peer_service(svc)
}

/// Whether [`normalize_peer_service`] doesn't change `svc`.
pub(crate) fn is_normalized_peer_service(svc: &str) -> bool {
    svc.len() <= MAX_SERVICE_LEN && is_normalized_ascii_tag(svc, false)
}

/// Whether [`normalize_tag_value`] doesn't change `value`, as the agent's
/// `isNormalizedASCIITag(v, false)` checks: `value` is ASCII, has at most 200 bytes, and is made of
/// lowercase letters, `:`, digits, `.`, `/` and `-`, and `_` followed by one of them.
///
/// It doesn't allocate: use it to skip copying a value that is already normalized. A `false`
/// result doesn't mean that [`normalize_tag_value`] changes the value, for example if it isn't
/// ASCII.
pub fn is_normalized_tag_value(value: &str) -> bool {
    is_normalized_ascii_tag(value, false)
}

/// Whether [`normalize_name`] doesn't change `name`.
pub(crate) fn is_normalized_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && is_valid_normalized_metric_name(name.as_bytes())
        && !name.ends_with('_')
}

/// Whether [`normalize_span_type`] doesn't change `span_type`.
pub(crate) fn is_normalized_span_type(span_type: &str) -> bool {
    span_type.len() <= MAX_TYPE_LEN
}

/// Whether `name` is a normalized metric name, as the agent's `isValidNormalizedMetricName`
/// checks: an ASCII letter, then ASCII letters, digits, `.` and `_`, with no `__` and no `_.`.
fn is_valid_normalized_metric_name(name: &[u8]) -> bool {
    let Some((first, rest)) = name.split_first() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let mut prev = *first;
    for &b in rest {
        let valid = b.is_ascii_alphanumeric() || (matches!(b, b'.' | b'_') && prev != b'_');
        if !valid {
            return false;
        }
        prev = b;
    }
    true
}

/// Normalizes a span name, as the agent's `normMetricNameParse` does.
///
/// Characters up to the first ASCII letter are removed: if there is none, the name becomes
/// empty. Then each run of characters other than ASCII letters, digits and `.` becomes one `_`, an
/// `_` before a `.` is removed, and no `_` follows a `.`. Consecutive periods are kept. A trailing
/// `_` is removed.
fn normalize_metric_name(name: &mut String) {
    // SAFETY: only ASCII bytes are written, so the buffer stays valid UTF-8. Non-ASCII bytes are
    // either removed or replaced as a whole run, as no ASCII byte is part of a multi-byte
    // character.
    let bytes = unsafe { name.as_mut_vec() };
    let Some(start) = bytes.iter().position(u8::is_ascii_alphabetic) else {
        bytes.clear();
        return;
    };
    if is_valid_normalized_metric_name(&bytes[start..]) {
        bytes.drain(..start);
        if bytes.last() == Some(&b'_') {
            bytes.pop();
        }
        return;
    }

    // `bytes[start]` is a letter, so `write` is at least 1 after the first iteration, and the write
    // position never passes the read position.
    let mut write = 0;
    for read in start..bytes.len() {
        let b = bytes[read];
        if b.is_ascii_alphanumeric() {
            bytes[write] = b;
            write += 1;
        } else if b == b'.' {
            if bytes[write - 1] == b'_' {
                // An `_` before a `.` is replaced by the `.`.
                bytes[write - 1] = b'.';
            } else {
                bytes[write] = b'.';
                write += 1;
            }
        } else if !matches!(bytes[write - 1], b'.' | b'_') {
            bytes[write] = b'_';
            write += 1;
        }
    }
    if bytes[write - 1] == b'_' {
        write -= 1;
    }
    bytes.truncate(write);
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
        // From TestNormalizeName in the agent's normalize_test.go.
        [test_normalize_last_underscore] ["last.underscore_trunc_"]         ["last.underscore_trunc"];
        [test_normalize_last_double_underscore] ["last.double_underscore_trunc__"] ["last.double_underscore_trunc"];
        [test_normalize_double_point]   ["double..point"]                   ["double..point"];
        [test_normalize_other_char]     ["other_^.character^^_than_underscore"] ["other.character_than_underscore"];
        [test_normalize_non_alpha_prefix] ["^^_.non_alpha.prefix"]          ["non_alpha.prefix"];
        [test_normalize_underscore]     ["_"]                               ["unnamed_operation"];
        // Consecutive periods are kept, as in the agent.
        [test_normalize_double_point_repaired] ["http..request!"]           ["http..request"];
        [test_normalize_underscore_double_point] ["a_..b!"]                 ["a..b"];
        // A valid name is kept as is, but a repaired one has no `_` after a `.`.
        [test_normalize_point_underscore_valid] ["a._b"]                    ["a._b"];
        [test_normalize_point_underscore_repaired] ["a._b!"]                ["a.b"];
        [test_normalize_non_ascii]      ["a\u{e9}b"]                        ["a_b"];
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
    fn test_normalize_tag_lowercases_only_with_the_same_width() {
        // `İ` (2 bytes) lowercases to `i` (1 byte), the Ohm and Kelvin signs (3 bytes) to `ω`
        // (2 bytes) and `k` (1 byte): the agent keeps them as they are.
        for (input, expected) in [
            ("İstanbul", "İstanbul"),
            ("\u{2126}x", "\u{2126}x"),
            ("a\u{212A}b", "a\u{212A}b"),
            ("ΩMEGA ÀÉÎ", "ωmega_àéî"),
        ] {
            let mut v = input.to_owned();
            normalize_tag(&mut v);
            assert_eq!(v, expected, "{input}");
        }
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

    #[test]
    fn test_is_normalized_ascii_tag() {
        // (input, as a tag value, as a tag)
        let long = "a".repeat(200);
        let too_long = "a".repeat(201);
        let cases = [
            ("", true, true),
            ("good:tag/1.2-3", true, true),
            ("a_b", true, true),
            ("_a", true, false),
            ("1a", true, false),
            (":a", true, true),
            ("a__b", false, false),
            ("a_", false, false),
            ("_", false, false),
            ("A", false, false),
            ("a b", false, false),
            ("é", false, false),
            (long.as_str(), true, true),
            (too_long.as_str(), false, false),
        ];
        for (input, value, tag) in cases {
            assert_eq!(
                (
                    is_normalized_ascii_tag(input, false),
                    is_normalized_ascii_tag(input, true)
                ),
                (value, tag),
                "{input:?}"
            );
        }
    }

    #[test]
    fn test_is_normalized_matches_normalize() {
        // Each `is_normalized_*` check must imply that its normalizer keeps the value.
        let inputs = [
            "",
            "a",
            "A",
            "_",
            "a_",
            "_a",
            "a__b",
            "a_b",
            "a.b",
            "a..b",
            "a._b",
            "a_.b",
            "1a",
            ":a",
            "a-b/c",
            "a b",
            "é",
            "a!",
            "web",
            "http..request",
            "unnamed_operation",
        ];
        let long = [
            "a".repeat(99),
            "a".repeat(100),
            "a".repeat(101),
            "a".repeat(201),
        ];
        for input in inputs
            .iter()
            .copied()
            .chain(long.iter().map(String::as_str))
        {
            let check = |name: &str, is_normalized: fn(&str) -> bool, f: fn(&mut String)| {
                let mut v = input.to_owned();
                f(&mut v);
                if is_normalized(input) {
                    assert_eq!(v, input, "{name}({input:?})");
                }
            };
            check("service", is_normalized_service, normalize_service);
            check(
                "peer_service",
                is_normalized_peer_service,
                normalize_peer_service,
            );
            check("tag_value", is_normalized_tag_value, normalize_tag_value);
            check("tag", |t| is_normalized_ascii_tag(t, true), normalize_tag);
            check("name", is_normalized_name, normalize_name);
            check("span_type", is_normalized_span_type, normalize_span_type);
        }
    }

    #[test]
    fn test_normalize_name_with_reason() {
        // TestNormalizeName in the agent's normalize_test.go.
        let cases = [
            ("", "unnamed_operation", Some(NormalizeError::Empty)),
            ("good", "good", None),
            ("last.underscore_trunc_", "last.underscore_trunc", None),
            (
                "last.double_underscore_trunc__",
                "last.double_underscore_trunc",
                None,
            ),
            (
                "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
                "Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.",
                Some(NormalizeError::TooLong),
            ),
            ("double..point", "double..point", None),
            (
                "other_^.character^^_than_underscore",
                "other.character_than_underscore",
                None,
            ),
            ("bad-name", "bad_name", None),
            ("^^_.non_alpha.prefix", "non_alpha.prefix", None),
            ("_", "unnamed_operation", Some(NormalizeError::Invalid)),
        ];
        for (input, expected, reason) in cases {
            let mut v = input.to_owned();
            assert_eq!(normalize_name_with_reason(&mut v), reason, "{input}");
            assert_eq!(v, expected, "{input}");
        }
        // Invalid wins over a truncation.
        let mut v = "1".repeat(150);
        assert_eq!(
            normalize_name_with_reason(&mut v),
            Some(NormalizeError::Invalid)
        );
    }

    #[test]
    fn test_normalize_service_and_peer_service_with_reason() {
        // TestNormalizeService and TestNormalizePeerService in the agent's normalize_test.go.
        for (input, expected) in SERVICE_CASES {
            let reason = (input.len() > MAX_SERVICE_LEN).then_some(NormalizeError::TooLong);
            let mut v = (*input).to_owned();
            assert_eq!(normalize_service_with_reason(&mut v, ""), reason, "{input}");
            assert_eq!(v, *expected, "{input}");
            let mut v = (*input).to_owned();
            assert_eq!(
                normalize_peer_service_with_reason(&mut v),
                reason,
                "{input}"
            );
            assert_eq!(v, *expected, "{input}");
        }

        let mut v = String::new();
        assert_eq!(
            normalize_service_with_reason(&mut v, "java"),
            Some(NormalizeError::Empty)
        );
        assert_eq!(v, "unnamed-java-service");
        let mut v = "!!!".to_owned();
        assert_eq!(
            normalize_service_with_reason(&mut v, ""),
            Some(NormalizeError::Invalid)
        );
        assert_eq!(v, "unnamed-service");
        let mut v = "!".repeat(150);
        assert_eq!(
            normalize_service_with_reason(&mut v, ""),
            Some(NormalizeError::Invalid)
        );

        let mut v = String::new();
        assert_eq!(normalize_peer_service_with_reason(&mut v), None);
        assert_eq!(v, "");
        let mut v = "!!!".to_owned();
        assert_eq!(
            normalize_peer_service_with_reason(&mut v),
            Some(NormalizeError::Invalid)
        );
        assert_eq!(v, "");
    }

    #[test]
    fn test_normalize_span_type_with_reason() {
        let mut v = "sql".repeat(1000);
        assert_eq!(
            normalize_span_type_with_reason(&mut v),
            Some(NormalizeError::TooLong)
        );
        assert_eq!(v.len(), MAX_TYPE_LEN);
        let mut v = "web".to_owned();
        assert_eq!(normalize_span_type_with_reason(&mut v), None);
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
