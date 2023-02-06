// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

// DEFAULT_SERVICE_NAME is the default name we assign a service if it's missing and we have no reasonable fallback
pub(crate) const DEFAULT_SERVICE_NAME: &str = "unnamed-service";

// MAX_NAME_LEN the maximum length a name can have
pub(crate) const MAX_NAME_LEN: usize = 100;
// MAX_SERVICE_LEN the maximum length a service can have
pub(crate) const MAX_SERVICE_LEN: usize = 100;
// MAX_SERVICE_LEN the maximum length a tag can have
pub(crate) const MAX_TAG_LEN: usize = 200;

// TruncateUTF8 truncates the given string to make sure it uses less than limit bytes.
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

// fallbackService returns the fallback service name for a service
// belonging to language lang.
pub(crate) fn fallback_service(lang: &str) -> String {
    if lang.is_empty() {
        return DEFAULT_SERVICE_NAME.to_string();
    }
    let mut service_name = String::new();
    service_name.push_str("unnamed-");
    service_name.push_str(lang);
    service_name.push_str("-service");
    // TODO: the original golang implementation uses a map to cache previously created
    // service names. Implement that here.
    service_name
}

// NormalizeService normalizes a span service and returns an error describing the reason
// (if any) why the name was modified.
pub(crate) fn normalize_service(svc: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!svc.is_empty(), "Normalizer Error: Empty service name.");

    let truncated_service = if svc.len() > MAX_SERVICE_LEN {
        truncate_utf8(svc, MAX_SERVICE_LEN)
    } else {
        svc
    };

    normalize_tag(truncated_service)
}

// NormalizeTag applies some normalization to ensure the tags match the backend requirements.
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

    let char_vec: Vec<char> = tag.chars().collect();

    for cur_char in char_vec {
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
            if iter.len() == 1 {
                let c: char = iter.next().unwrap();
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
    if !is_valid_ascii_start_char(tag.chars().next().unwrap()) {
        return false;
    }
    for mut i in 0..tag.len() {
        let b: char = tag.chars().nth(i).unwrap();
        if is_valid_ascii_tag_char(b) {
            continue;
        }
        if b == '_' {
            // an underscore is only okay if followed by a valid non-underscore character
            i += 1;
            if i == tag.len() || !is_valid_ascii_tag_char(tag.chars().nth(i).unwrap()) {
                return false;
            }
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
        test_name                       input                               expected                    expected_err;
        [test_normalize_empty_service]   [""]                                [normalize_utils::DEFAULT_SERVICE_NAME]      ["Normalizer Error: Empty service name."];
        [test_normalize_valid_service]   ["good"]                            ["good"]                    [""];
        [test_normalize_long_service]    ["Too$Long$.".repeat(20).as_str()]  ["too_long_.".repeat(10)]    [""];
        [test_normalize_dash_service]    ["bad&service"]                        ["bad_service"]                [""];
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
}
