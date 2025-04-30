// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use percent_encoding::percent_decode_str;
use url::Url;

pub fn obfuscate_url_string(
    url: &str,
    remove_query_string: bool,
    remove_path_digits: bool,
) -> String {
    let mut parsed_url = match Url::parse(url) {
        Ok(res) => res,
        Err(_) => return "?".to_string(),
    };

    // remove username & password
    parsed_url.set_username("").unwrap_or_default();
    parsed_url.set_password(Some("")).unwrap_or_default();

    if remove_query_string && parsed_url.query().is_some() {
        parsed_url.set_query(Some(""));
    }

    if !remove_path_digits {
        return parsed_url.to_string();
    }

    // remove path digits
    let mut split_url: Vec<&str> = parsed_url.path().split('/').collect();
    let mut changed = false;
    for segment in split_url.iter_mut() {
        // we don't want to redact any HTML encodings
        #[allow(clippy::unwrap_used)]
        let decoded = percent_decode_str(segment).decode_utf8().unwrap();
        if decoded.chars().any(|c| char::is_ascii_digit(&c)) {
            *segment = "/REDACTED/";
            changed = true;
        }
    }
    if changed {
        parsed_url.set_path(&split_url.join("/"));
    }

    parsed_url.to_string().replace("/REDACTED/", "?")
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::obfuscate_url_string;

    #[duplicate_item(
        [
            test_name           [remove_query_string_1]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/"]
            expected_output     ["http://foo.com/"];
        ]
        [
            test_name           [remove_query_string_2]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/123"]
            expected_output     ["http://foo.com/123"];
        ]
        [
            test_name           [remove_query_string_3]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/id/123/page/1?search=bar&page=2"]
            expected_output     ["http://foo.com/id/123/page/1?"];
        ]
        [
            test_name           [remove_query_string_4]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/id/123/page/1?search=bar&page=2#fragment"]
            expected_output     ["http://foo.com/id/123/page/1?#fragment"];
        ]
        [
            test_name           [remove_query_string_5]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/id/123/page/1?blabla"]
            expected_output     ["http://foo.com/id/123/page/1?"];
        ]
        [
            test_name           [remove_query_string_6]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://foo.com/id/123/pa%3Fge/1?blabla"]
            expected_output     ["http://foo.com/id/123/pa%3Fge/1?"];
        ]
        [
            test_name           [remove_query_string_7]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["http://user:password@foo.com/1/2/3?q=james"]
            expected_output     ["http://foo.com/1/2/3?"];
        ]
        [
            test_name           [remove_path_digits_1]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/"]
            expected_output     ["http://foo.com/"];
        ]
        [
            test_name           [remove_path_digits_2]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/name?query=search"]
            expected_output     ["http://foo.com/name?query=search"];
        ]
        [
            test_name           [remove_path_digits_3]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/id/123/page/1?search=bar&page=2"]
            expected_output     ["http://foo.com/id/?/page/??search=bar&page=2"];
        ]
        [
            test_name           [remove_path_digits_4]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/id/a1/page/1qwe233?search=bar&page=2#fragment-123"]
            expected_output     ["http://foo.com/id/?/page/??search=bar&page=2#fragment-123"];
        ]
        [
            test_name           [remove_path_digits_5]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/123"]
            expected_output     ["http://foo.com/?"];
        ]
        [
            test_name           [remove_path_digits_6]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/123/abcd9"]
            expected_output     ["http://foo.com/?/?"];
        ]
        [
            test_name           [remove_path_digits_7]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/123/name/abcd9"]
            expected_output     ["http://foo.com/?/name/?"];
        ]
        [
            test_name           [remove_path_digits_8]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://foo.com/1%3F3/nam%3Fe/abcd9"]
            expected_output     ["http://foo.com/?/nam%3Fe/?"];
        ]
        [
            test_name           [remove_path_digits_9]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["http://user:password@foo.com/1/2/3?q=james"]
            expected_output     ["http://foo.com/?/?/??q=james"];
        ]
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_url_string(input, remove_query_string, remove_path_digits);
        assert_eq!(result, expected_output);
    }
}
