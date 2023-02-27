// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

// DEFAULT_SERVICE_NAME is the default name we assign a service if it's missing and we have no reasonable fallback
const DEFAULT_SERVICE_NAME: &str = "unnamed-service";

// MAX_NAME_LEN the maximum length a name can have
pub(crate) const MAX_NAME_LEN: usize = 100;
// MAX_SERVICE_LEN the maximum length a service can have
const MAX_SERVICE_LEN: usize = 100;
// MAX_SERVICE_LEN the maximum length a tag can have
const MAX_TAG_LEN: usize = 200;

// truncate_utf8 truncates the given string to make sure it uses less than limit bytes.
// If the last character is a utf8 character that would be split, it removes it
// entirely to make sure the resulting string is not broken.
pub(crate) fn truncate_utf8(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    let mut prev_index = 0;
    for i in 0..s.len() {
        if i > limit {
            return &s[0..prev_index];
        }
        if s.is_char_boundary(i) {
            prev_index = i;
        }
    }
    s
}

// fallback_service returns the fallback service name for a service
// belonging to language lang.
// In the go agent implementation, if a lang was specified in TagStats
// (extracted from the payload header) the fallback_service name would be "unnamed-{lang}-service".
pub(crate) fn fallback_service() -> String {
    DEFAULT_SERVICE_NAME.to_string()
}

// normalize_service normalizes a span service
pub(crate) fn normalize_service(svc: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!svc.is_empty(), "Normalizer Error: Empty service name.");

    let truncated_service = truncate_utf8(svc, MAX_SERVICE_LEN);

    normalize_tag(truncated_service)
}

// normalize_tag applies some normalization to ensure the tags match the backend requirements.
pub(crate) fn normalize_tag(tag: &str) -> anyhow::Result<String> {
    // Fast path: Check if the tag is valid and only contains ASCII characters,
    // if yes return it as-is right away. For most use-cases this reduces CPU usage.
    if is_normalized_ascii_tag(tag) {
        return Ok(tag.to_string());
    }

    anyhow::ensure!(!tag.is_empty(), "Normalizer Error: Empty tag name.");

    // given a dummy value
    let mut last_char: char = 'a';

    let mut result = String::with_capacity(tag.len());

    for cur_char in tag.chars() {
        if result.len() == MAX_TAG_LEN {
            break;
        }
        if cur_char.is_lowercase() {
            result.push(cur_char);
            last_char = cur_char;
            continue;
        }
        if cur_char.is_uppercase() {
            let mut iter = cur_char.to_lowercase();
            if let Some(c) = iter.next() {
                result.push(c);
                last_char = c;
            }
            continue;
        }
        if cur_char.is_alphabetic() {
            result.push(cur_char);
            last_char = cur_char;
            continue;
        }
        if cur_char == ':' {
            result.push(cur_char);
            last_char = cur_char;
            continue;
        }
        if !result.is_empty()
            && (cur_char.is_ascii_digit() || cur_char == '.' || cur_char == '/' || cur_char == '-')
        {
            result.push(cur_char);
            last_char = cur_char;
            continue;
        }
        if !result.is_empty() && last_char != '_' {
            result.push('_');
            last_char = '_';
        }
    }

    if last_char == '_' {
        result.remove(result.len() - 1);
    }

    Ok(result.to_string())
}

pub(crate) fn is_normalized_ascii_tag(tag: &str) -> bool {
    if tag.is_empty() {
        return true;
    }
    if tag.len() > MAX_TAG_LEN {
        return false;
    }

    let mut tag_iter = tag.chars();

    match tag_iter.next() {
        Some(c) => {
            if !is_valid_ascii_start_char(c) {
                return false;
            }
        }
        None => return false,
    }

    while let Some(cur_char) = tag_iter.next() {
        if is_valid_ascii_tag_char(cur_char) {
            continue;
        }
        if cur_char == '_' {
            // an underscore is only okay if followed by a valid non-underscore character
            match tag_iter.next() {
                Some(c) => {
                    if !is_valid_ascii_tag_char(c) {
                        return false;
                    }
                }
                None => return false,
            };
        } else {
            return false;
        }
    }
    true
}

pub(crate) fn is_valid_ascii_start_char(c: char) -> bool {
    ('a'..='z').contains(&c) || c == ':'
}

pub(crate) fn is_valid_ascii_tag_char(c: char) -> bool {
    is_valid_ascii_start_char(c) || ('0'..='9').contains(&c) || c == '.' || c == '/' || c == '-'
}

// normalize_name normalizes a span name or an error describing why normalization failed.
pub(crate) fn normalize_name(name: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!name.is_empty(), "Normalizer Error: Empty span name.");

    let truncated_name = if name.len() > MAX_NAME_LEN {
        truncate_utf8(name, MAX_NAME_LEN)
    } else {
        name
    };

    normalize_metric_names(truncated_name)
}

pub(crate) fn normalize_metric_names(name: &str) -> anyhow::Result<String> {
    let mut result = String::with_capacity(name.len());

    // given a dummy value
    let mut last_char: char = 'a';

    let char_vec: Vec<char> = name.chars().collect();

    // skip non-alphabetic characters
    let mut i = match name.chars().position(is_alpha) {
        Some(val) => val,
        None => {
            // if there were no alphabetic characters it wasn't valid
            anyhow::bail!("Normalizer Error: Name contains no alphabetic chars.")
        }
    };

    while i < name.len() {
        if is_alpha_num(char_vec[i]) {
            result.push(char_vec[i]);
            last_char = char_vec[i];
        } else if char_vec[i] == '.' {
            // we skipped all non-alpha chars up front so we have seen at least one
            if last_char == '_' {
                // overwrite underscores that happen before periods
                result.replace_range((result.len() - 1)..(result.len()), ".");
                last_char = '.'
            } else {
                result.push('.');
                last_char = '.';
            }
        } else {
            // we skipped all non-alpha chars up front so we have seen at least one
            if last_char != '.' && last_char != '_' {
                result.push('_');
                last_char = '_';
            }
        }
        i += 1;
    }

    if last_char == '_' {
        result.remove(result.len() - 1);
    }
    Ok(result)
}

pub(crate) fn is_alpha(c: char) -> bool {
    ('a'..='z').contains(&c) || ('A'..='Z').contains(&c)
}

pub(crate) fn is_alpha_num(c: char) -> bool {
    is_alpha(c) || ('0'..='9').contains(&c)
}

#[cfg(test)]
mod tests {

    use crate::normalize_utils;
    use duplicate::duplicate_item;

    #[duplicate_item(
        test_name                       input                               expected                    expected_err;
        [test_normalize_empty_string]   [""]                                [""]                        ["Normalizer Error: Empty span name."];
        [test_normalize_valid_string]   ["good"]                            ["good"]                    [""];
        [test_normalize_long_string]    ["Too-Long-.".repeat(20).as_str()]  ["Too_Long.".repeat(10)]    [""];
        [test_normalize_dash_string]    ["bad-name"]                        ["bad_name"]                [""];
        [test_normalize_invalid_string] ["&***"]                            [""]                        ["Normalizer Error: Name contains no alphabetic chars."];
        [test_normalize_invalid_prefix] ["&&&&&&&_test-name-"]              ["test_name"]               [""];
    )]
    #[test]
    fn test_name() {
        match normalize_utils::normalize_name(input) {
            Ok(val) => {
                assert_eq!(expected_err, "");
                assert_eq!(val, expected);
            }
            Err(err) => {
                assert_eq!(format!("{err}"), expected_err);
            }
        }
    }

    #[duplicate_item(
        test_name                       input                               expected                                expected_err;
        [test_normalize_empty_service]  [""]                                [normalize_utils::DEFAULT_SERVICE_NAME] ["Normalizer Error: Empty service name."];
        [test_normalize_valid_service]  ["good"]                            ["good"]                                [""];
        [test_normalize_long_service]   ["Too$Long$.".repeat(20).as_str()]  ["too_long_.".repeat(10)]               [""];
        [test_normalize_dash_service]   ["bad&service"]                     ["bad_service"]                         [""];
    )]
    #[test]
    fn test_name() {
        match normalize_utils::normalize_service(input) {
            Ok(val) => {
                assert_eq!(expected_err, "");
                assert_eq!(val, expected)
            }
            Err(err) => {
                assert_eq!(format!("{err}"), expected_err);
            }
        }
    }
    #[duplicate_item(
        test_name               input   expected    expected_err;
        [test_normalize_tag_1]  ["#test_starting_hash"] ["test_starting_hash"] [""];
        [test_normalize_tag_2]  ["TestCAPSandSuch"] ["testcapsandsuch"] [""];
        [test_normalize_tag_3]  ["Test Conversion Of Weird !@#$%^&**() Characters"] ["test_conversion_of_weird_characters"] [""];
        [test_normalize_tag_4]  ["$#weird_starting"] ["weird_starting"] [""];
        [test_normalize_tag_5]  ["allowed:c0l0ns"] ["allowed:c0l0ns"] [""];
        [test_normalize_tag_6]  ["1love"] ["love"] [""];
        [test_normalize_tag_7]  ["ünicöde"] ["ünicöde"] [""];
        [test_normalize_tag_8]  ["ünicöde:metäl"] ["ünicöde:metäl"] [""];
        [test_normalize_tag_9]  ["Data🐨dog🐶 繋がっ⛰てて"] ["data_dog_繋がっ_てて"] [""];
        [test_normalize_tag_10] [" spaces   "] ["spaces"] [""];
        [test_normalize_tag_11] [" #hashtag!@#spaces #__<>#  "] ["hashtag_spaces"] [""];
        [test_normalize_tag_12] [":testing"] [":testing"] [""];
        [test_normalize_tag_13] ["_foo"] ["foo"] [""];
        [test_normalize_tag_14] [":::test"] [":::test"] [""];
        [test_normalize_tag_15] ["contiguous_____underscores"] ["contiguous_underscores"] [""];
        [test_normalize_tag_16] ["foo_"] ["foo"] [""];
        [test_normalize_tag_17] ["\u{017F}odd_\u{017F}case\u{017F}"] ["\u{017F}odd_\u{017F}case\u{017F}"]  [""]; // edge-case
        [test_normalize_tag_18] [""] [""] [""];
        [test_normalize_tag_19] [" "] [""] [""];
        [test_normalize_tag_20] ["ok"] ["ok"] [""];
        [test_normalize_tag_21] ["™Ö™Ö™™Ö™"] ["ö_ö_ö"] [""];
        [test_normalize_tag_22] ["AlsO:ök"] ["also:ök"] [""];
        [test_normalize_tag_23] [":still_ok"] [":still_ok"] [""];
        [test_normalize_tag_24] ["___trim"] ["trim"] [""];
        [test_normalize_tag_25] ["12.:trim@"] [":trim"] [""];
        [test_normalize_tag_26] ["12.:trim@@"] [":trim"] [""];
        [test_normalize_tag_27] ["fun:ky__tag/1"] ["fun:ky_tag/1"] [""];
        [test_normalize_tag_28] ["fun:ky@tag/2"] ["fun:ky_tag/2"] [""];
        [test_normalize_tag_29] ["fun:ky@@@tag/3"] ["fun:ky_tag/3"] [""];
        [test_normalize_tag_30] ["tag:1/2.3"] ["tag:1/2.3"] [""];
        [test_normalize_tag_31] ["---fun:k####y_ta@#g/1_@@#"]["fun:k_y_ta_g/1"] [""];
        [test_normalize_tag_32] ["AlsO:œ#@ö))œk"] ["also:œ_ö_œk"] [""];
        [test_normalize_tag_33] ["a".repeat(888).as_str()] ["a".repeat(200)] [""];
        [test_normalize_tag_34] [("a".to_owned() + &"🐶".repeat(799)).as_str()] ["a"] [""];
        [test_normalize_tag_35] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string()).as_str()] ["a"] [""];
        [test_normalize_tag_36] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string()).as_str()] ["a"] [""];
        [test_normalize_tag_37] [("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string() + "b").as_str()] ["a_b"] [""];
        [test_normalize_tag_38]
            ["A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000"]
            ["a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000_0"]
            [""];
    )]
    #[test]
    fn test_name() {
        match normalize_utils::normalize_tag(input) {
            Ok(normalized_tag) => {
                assert_eq!(expected_err, "");
                assert_eq!(normalized_tag, expected)
            }
            Err(err) => {
                assert_eq!(format!("{err}"), expected_err);
            }
        }
    }
}
