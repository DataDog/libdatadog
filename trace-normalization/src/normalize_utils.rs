// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

// MAX_NAME_LEN the maximum length a name can have
pub const MAX_NAME_LEN: usize = 100;
// MAX_SERVICE_LEN the maximum length a service can have
pub const MAX_SERVICE_LEN: usize = 100;

pub const MAX_TAG_LEN: usize = 200;

// TruncateUTF8 truncates the given string to make sure it uses less than limit bytes.
// If the last character is a utf8 character that would be split, it removes it
// entirely to make sure the resulting string is not broken.
pub fn truncate_utf8(s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut prev_index = 0;
    for i in 0..s.len() {
        if i > limit {
            return s[0..prev_index].to_string();
        }
        prev_index = i;
    }
    s
}

// NormalizeService normalizes a span service and returns an error describing the reason
// (if any) why the name was modified.
// TODO: Implement this in a future PR
// pub fn normalize_service(svc: String, lang: String) -> (String, Option<errors::NormalizeErrors>) {
// if svc == "" {
//     return (fallback_service(lang), errors::NormalizeErrors::ErrorEmpty);
// }
// if svc.len() > MAX_SERVICE_LEN {
//     return (truncate_utf8(svc, MAX_SERVICE_LEN), errors::NormalizeErrors::ErrorTooLong.into());
// }
// TODO: implement tag normalization
// let s: String = normalize_tag(svc);
// if s == "" {
//     return (fallbackService(lang), errors::NormalizeErrors::ErrorInvalid)
// }
// return (s, err)
// (svc, None)
// }

// normalize_name normalizes a span name and returns an error describing the reason
// (if any) why the name was modified.
// pub fn normalize_name(name: String) -> (String, Option<errors::NormalizeErrors>) {
pub fn normalize_name(name: String) -> anyhow::Result<String> {
    anyhow::ensure!(!name.is_empty(), "Normalizer Error: Empty");

    let mut truncated_name = name.clone();

    if name.len() > MAX_NAME_LEN {
        truncated_name = truncate_utf8(name, MAX_NAME_LEN);
    }

    let normalized_name = normalize_metric_names(truncated_name)?;
    Ok(normalized_name)
}

// TODO: Implement this in a future PR
// NormalizeTag applies some normalization to ensure the tags match the backend requirements.
// pub fn normalize_tag(v: String) -> String {
// Fast path: Check if the tag is valid and only contains ASCII characters,
// if yes return it as-is right away. For most use-cases this reduces CPU usage.
// 	if is_normalized_ascii_tag(v.clone()) {
// 		return v;
// 	}

//     if v.is_empty() {
//         return "".to_string();
//     }

//     "".to_string()
// }

// pub fn is_normalized_ascii_tag(tag: String) -> bool {
//     if tag.is_empty() {
//         return true;
//     }
//     if tag.len() > MAX_TAG_LEN {
//         return false;
//     }
//     if !is_valid_ascii_start_char(tag.chars().next().unwrap()) {
//         return false;
//     }
//     for mut i in 0..tag.len() {
//         let b: char = tag.chars().nth(i).unwrap();
//         if is_valid_ascii_tag_char(b) {
//             continue;
//         }
//         if b == '_' {
//             // an underscore is only okay if followed by a valid non-underscore character
// 			i+=1;
// 			if i == tag.len() || !is_valid_ascii_tag_char(tag.chars().nth(i).unwrap()) {
// 				return false;
// 			}
//         } else {
//             return false;
//         }
//     }
//     true
// }

// pub fn is_valid_ascii_start_char(c: char) -> bool {
//     ('a'..='z').contains(&c) || c == ':'
// }

// pub fn is_valid_ascii_tag_char(c: char) -> bool {
//     is_valid_ascii_start_char(c) || ('0'..='9').contains(&c) || c == '.' || c == '/' || c == '-'
// }

pub fn normalize_metric_names(name: String) -> anyhow::Result<String> {
    anyhow::ensure!(!name.is_empty(), "Normalizer Error: Empty");

    // rust efficient ways to build strings, see here:
    // https://github.com/hoodie/concatenation_benchmarks-rs
    let mut result = String::with_capacity(name.len());

    // given a dummy value
    let mut last_char: char = 'a';

    let char_vec: Vec<char> = name.chars().collect();

    let mut i = 0;

    // skip non-alphabetic characters
    while i < name.len() && !is_alpha(char_vec[0]) {
        i += 1;
    }

    // if there were no alphabetic characters it wasn't valid
    anyhow::ensure!(i != name.len(), "Normalizer Error: Invalid");

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

pub fn is_alpha(c: char) -> bool {
    ('a'..='z').contains(&c) || ('A'..='Z').contains(&c)
}

pub fn is_alpha_num(c: char) -> bool {
    is_alpha(c) || ('0'..='9').contains(&c)
}

#[cfg(test)]
mod tests {

    use crate::normalize_utils;

    #[test]
    fn test_normalize_name() {
        let test_tuples: [(&str, &str, &str); 5] = [
            (
                "",
                "",
                "Normalizer Error: Empty"
            ),
            (
                "good",
                "good",
                "",
            ),
            (
                "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
                "Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.",
                "",
            ),
            (
                "bad-name",
                "bad_name",
                "",
            ),
            (
                "&",
                "",
                "Normalizer Error: Invalid"
            )
        ];

        for tuple in test_tuples.iter() {
            let input = tuple.0;
            let expected = tuple.1;
            let expected_err = tuple.2;

            match normalize_utils::normalize_name(input.to_string()) {
                Ok(val) => {
                    assert_eq!(expected_err, "");
                    assert_eq!(val, expected);
                }
                Err(err) => {
                    assert_eq!(format!("{}", err), expected_err);
                }
            }
        }
    }
}
