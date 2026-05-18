// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// FIXME: once obfuscation feature parity is reached with the agent, change both modules to be more
// restrictive on the accepted forms of urls so that this module can be greatly simplified.
// One idea for now is to match the url to a regex on both side to validate it

use fluent_uri::UriRef;
use percent_encoding::percent_decode_str;
use std::fmt::Write;

/// Returns true for Go net/url's "category 1" characters:
/// ASCII bytes that always trigger escaping in URLs (plus space and quote).
fn is_go_url_escape_cat1(c: char) -> bool {
    matches!(
        c,
        '\\' | '^' | '{' | '}' | '|' | '<' | '>' | '`' | ' ' | '"'
    )
}

/// Returns true for Go net/url's "category 2" characters for PATH contexts:
/// characters Go may escape in paths when Cat1 is present or non-ASCII exists.
fn is_go_url_escape_cat2_path(c: char) -> bool {
    matches!(c, '!' | '\'' | '(' | ')' | '*' | '[' | ']')
}

/// Returns true for Go net/url's "category 2" characters for FRAGMENT contexts:
/// characters Go may escape in fragments when non-ASCII exists.
fn is_go_url_escape_cat2_fragment(c: char) -> bool {
    matches!(c, '\'' | '[' | ']')
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => b - b'A' + 10,
    }
}

/// Decode %XX for unreserved chars (A-Za-z0-9-._~) in path, matching Go's url.Parse behavior.
fn normalize_pct_encoded_unreserved(path: &str) -> String {
    let b = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let v = (hex_val(b[i + 1]) << 4) | hex_val(b[i + 2]);
            if v.is_ascii_alphanumeric() || matches!(v, b'.' | b'_' | b'~') {
                out.push(v as char);
            } else {
                out.push_str(&path[i..i + 3]);
            }
            i += 3;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

fn encode_char(out: &mut String, c: char) {
    let mut buf = [0u8; 4];
    for &b in c.encode_utf8(&mut buf).as_bytes() {
        let _ = write!(out, "%{b:02X}");
    }
}

fn redact_path_digits(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if percent_decode_str(seg)
                .decode_utf8_lossy()
                .chars()
                .any(|c| c.is_ascii_digit())
            {
                "?"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn obfuscate_url_string(
    url: &str,
    remove_query_string: bool,
    remove_path_digits: bool,
) -> String {
    if url.is_empty() {
        return String::new();
    }

    let frag_pos = url.find('#');
    let path_query_end = frag_pos.unwrap_or(url.len());
    let path_end = url[..path_query_end].find('?').unwrap_or(path_query_end);

    // Control chars in path/query — Go rejects these
    if url[..path_query_end].bytes().any(|b| b < 0x20 || b == 0x7F) {
        return if remove_query_string || remove_path_digits {
            "?".to_string()
        } else {
            url.to_string()
        };
    }

    // Determine Go's escape() trigger: Cat1 or non-ASCII in path causes Cat2 encoding too
    let path = &url[..path_end];
    let needs_full_path = path.bytes().any(|b| b > 127) || path.chars().any(is_go_url_escape_cat1);
    let frag_has_non_ascii = frag_pos.is_some_and(|i| url[i + 1..].bytes().any(|b| b > 127));

    // Pre-encode chars that UriRef (strict RFC 3986) rejects.
    // We encode ALL non-ASCII chars (not just Cat1/Cat2) so that characters outside
    // RFC 3987 ucschar ranges (e.g. U+10EF4F, U+10FFFF) don't cause parse failures.
    // Exclude the query — Go doesn't validate query percent-encoding, so we pass
    // only path + fragment to UriRef and restore the original query afterward.
    let mut pre = String::with_capacity(url.len() * 4);
    for c in url[..path_end].chars() {
        if !c.is_ascii() {
            encode_char(&mut pre, c);
        } else if is_go_url_escape_cat1(c) || (needs_full_path && is_go_url_escape_cat2_path(c)) {
            let _ = write!(pre, "%{:02X}", c as u8);
        } else {
            pre.push(c);
        }
    }
    if let Some(fi) = frag_pos {
        pre.push('#');
        for c in url[fi + 1..].chars() {
            if !c.is_ascii()
                || (c as u32) < 0x20
                || c as u32 == 0x7F
                || c == '#'
                || is_go_url_escape_cat1(c)
                || (frag_has_non_ascii && is_go_url_escape_cat2_fragment(c))
            {
                encode_char(&mut pre, c);
            } else {
                pre.push(c);
            }
        }
    }

    let uri = match UriRef::parse(pre.as_str()) {
        Ok(u) => u,
        Err(_) => {
            return if remove_query_string || remove_path_digits {
                "?".to_string()
            } else {
                url.to_string()
            };
        }
    };

    let mut out = String::new();

    if let Some(scheme) = uri.scheme() {
        out.push_str(&scheme.as_str().to_lowercase());
        out.push(':');
    }

    if let Some(auth) = uri.authority() {
        out.push_str("//");
        // Strip userinfo — emit only host[:port]
        out.push_str(auth.host());
        if let Some(port) = auth.port() {
            out.push(':');
            out.push_str(port.as_str());
        }
        let path_str = normalize_pct_encoded_unreserved(uri.path().as_str());
        if remove_path_digits {
            out.push_str(&redact_path_digits(&path_str));
        } else {
            out.push_str(&path_str);
        }
    } else if let Some(scheme) = uri.scheme() {
        // This is a really weird case because there is a scheme but no authority.
        // For example: http:#
        // Length of "http:"
        let scheme_end = scheme.as_str().len() + 1;
        // http://example.com/?query -> //example.com/
        out.push_str(&url[scheme_end..path_end]);
    } else {
        // Relative reference: use pre-encoded path
        let path_str = normalize_pct_encoded_unreserved(uri.path().as_str());
        if remove_path_digits {
            out.push_str(&redact_path_digits(&path_str));
        } else {
            out.push_str(&path_str);
        }
    }

    // Use original URL positions to detect query — uri.query() is always None since we
    // excluded the query from the string we passed to UriRef.
    if remove_query_string {
        if path_end < path_query_end {
            out.push('?');
        }
    } else if path_end < path_query_end {
        // Restore original raw query (Go's url.RawQuery is kept verbatim)
        out.push_str(&url[path_end..path_query_end]);
    }

    if let Some(frag) = uri.fragment() {
        if !frag.as_str().is_empty() {
            out.push('#');
            out.push_str(frag.as_str());
        }
    }

    out
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
            test_name           [empty_input]
            remove_query_string [false]
            remove_path_digits  [false]
            input               [""]
            expected_output     [""];
        ]
        [
            test_name           [non_printable_chars]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["\u{10}"]
            // When both options false, Go returns original (obfuscateUserInfo passthrough)
            expected_output     ["\u{10}"];
        ]
        [
            test_name           [non_printable_chars_and_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["\u{10}ჸ"]
            expected_output     ["?"];
        ]
        [
            test_name           [hashtag]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#"]
            expected_output     [""];
        ]
        [
            test_name           [fuzzing_1050521893]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ"]
            expected_output     ["%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_594901251]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["%"]
            expected_output     ["?"];
        ]
        [
            test_name           [fuzzing_3638045804]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["."]
            expected_output     ["."];
        ]
        [
            test_name           [fuzzing_1928485962]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["0"]
            expected_output     ["?"];
        ]
        [
            test_name           [fuzzing_4273565798]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["!ჸ"]
            expected_output     ["%21%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_1457007156]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["!"]
            expected_output     ["!"];
        ]
        [
            test_name           [fuzzing_3119724369]
            remove_query_string [true]
            remove_path_digits  [true]
            input               [":"]
            expected_output     ["?"];
        ]
        [
            test_name           [fuzzing_1092426409]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#ჸ"]
            expected_output     ["#%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_1323831861]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#\u{01}"]
            expected_output     ["#%01"];
        ]
        [
            test_name           [fuzzing_35626170]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#\u{01}ჸ"]
            expected_output     ["#%01%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_618280270]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["\\"]
            expected_output     ["%5C"];
        ]
        [
            test_name           [fuzzing_1505427946]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["[ჸ"]
            expected_output     ["%5B%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_backslash_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["\\ჸ"]
            expected_output     ["%5C%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_2438023093]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#"]
            expected_output     ["%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_2729083127]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["!#ჸ"]
            expected_output     ["!#%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_slash_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["/ჸ"]
            expected_output     ["/%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_3710129001]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["##"]
            expected_output     ["#%23"];
        ]
        [
            test_name           [fuzzing_1009954227]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#\u{10}"]
            expected_output     ["%E1%83%B8#%10"];
        ]
        [
            test_name           [fuzzing_hash_exclamation]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#!"]
            expected_output     ["%E1%83%B8#!"];
        ]
        [
            test_name           [fuzzing_578834728]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#%"]
            expected_output     ["?"];
        ]
        [
            test_name           [fuzzing_3991369296]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#'ჸ"]
            expected_output     ["#%27%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_path_frag_quote]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#'ჸ"]
            expected_output     ["%E1%83%B8#%27%E1%83%B8"];
        ]
        [
            test_name           [fuzzing_hash_excl_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["#!ჸ"]
            expected_output     ["#!%E1%83%B8"];
        ]
        [
            // Cat1 char (<) triggers full escape(), which also encodes Cat2 char (!)
            test_name           [fuzzing_2455396347_cat1_triggers_cat2]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["<!"]
            expected_output     ["%3C%21"];
        ]
        [
            // Fragment has invalid percent-encoding (%\u{1}) AND control char — Go rejects
            test_name           [fuzzing_3886417401]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#%\u{1}"]
            expected_output     ["?"];
        ]
        [
            test_name           [parity_double_quote_cat1]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["\"!"]
            expected_output     ["%22%21"];
        ]
        [
            test_name           [parity_dot_hash_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               [".#ჸ"]
            expected_output     [".#%E1%83%B8"];
        ]
        [
            test_name           [parity_dot_hash]
            remove_query_string [true]
            remove_path_digits  [true]
            input               [".#"]
            expected_output     ["."];
        ]
        [
            test_name           [parity_unicode_hash_digit]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#0"]
            expected_output     ["%E1%83%B8#0"];
        ]
        [
            test_name           [parity_scheme_empty_frag]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["C:#"]
            expected_output     ["c:"];
        ]
        [
            test_name           [parity_relative_dotdot_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["../ჸ"]
            expected_output     ["../%E1%83%B8"];
        ]
        [
            test_name           [parity_query_hash_unicode_both]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["?#ჸ"]
            expected_output     ["?#%E1%83%B8"];
        ]
        [
            test_name           [parity_query_hash_unicode_digits]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["?#ჸ"]
            expected_output     ["?#%E1%83%B8"];
        ]
        [
            test_name           [parity_excl_query_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["!?ჸ"]
            expected_output     ["!?"];
        ]
        [
            test_name           [parity_query_unicode_keep]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["?ჸ"]
            expected_output     ["?ჸ"];
        ]
        [
            test_name           [parity_space_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               [" ჸ"]
            expected_output     ["%20%E1%83%B8"];
        ]
        [
            test_name           [parity_unicode_query_unicode_keep]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["ჸ?ჸ"]
            expected_output     ["%E1%83%B8?ჸ"];
        ]
        [
            test_name           [parity_unicode_query_hash_both]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["?ჸ#ჸ"]
            expected_output     ["?#%E1%83%B8"];
        ]
        [
            test_name           [parity_unicode_query_empty_hash]
            remove_query_string [false]
            remove_path_digits  [true]
            input               ["ჸ?#"]
            expected_output     ["%E1%83%B8?"];
        ]
        [
            test_name           [parity_pct_unreserved_normalize]
            remove_query_string [true]
            remove_path_digits  [false]
            input               ["%30ჸ"]
            expected_output     ["0%E1%83%B8"];
        ]
        [
            test_name           [parity_unicode_query_invalid_pct]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ?%"]
            expected_output     ["%E1%83%B8?"];
        ]
        [
            test_name           [parity_not_a_url_both_false]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["this is not a valid url"]
            expected_output     ["this%20is%20not%20a%20valid%20url"];
        ]
        [
            test_name           [parity_not_a_url_both_true]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["this is not a valid url"]
            expected_output     ["this%20is%20not%20a%20valid%20url"];
        ]
        [
            test_name           [parity_disabled_userinfo]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["http://user:password@foo.com/1/2/3?q=james"]
            expected_output     ["http://foo.com/1/2/3?q=james"];
        ]
        [
            test_name           [parity_colon_both_false]
            remove_query_string [false]
            remove_path_digits  [false]
            input               [":"]
            expected_output     [":"];
        ]
        [
            test_name           [parity_pct_both_false]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["%"]
            expected_output     ["%"];
        ]
        [
            test_name           [parity_ctrl_in_scheme_both_false]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["C:\u{1}"]
            expected_output     ["C:\u{1}"];
        ]
        [
            test_name           [parity_ctrl_both_false]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["\u{1}"]
            expected_output     ["\u{1}"];
        ]
        [
            test_name           [parity_frag_curly_brace]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["ჸ#{ჸ"]
            expected_output     ["%E1%83%B8#%7B%E1%83%B8"];
        ]
        [
            // Opaque URL: Go keeps the opaque part verbatim (not percent-encoded)
            test_name           [parity_opaque_url_unicode]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["A:ჸ"]
            expected_output     ["a:ჸ"];
        ]
        [
            test_name           [no_decode_dash]
            remove_query_string [false]
            remove_path_digits  [false]
            input               ["http://foo.com/foo%20bar/"]
            expected_output     ["http://foo.com/foo%20bar/"];
        ]
        [
            // Fragment with chars outside RFC 3987 ucschar ranges (U+10EF4F, U+10FFFF, etc.)
            // These must be percent-encoded, not cause a parse failure returning "?"
            test_name           [parity_fuzzing_supp_unicode_frag]
            remove_query_string [true]
            remove_path_digits  [true]
            input               ["\u{91cb8}\u{9232f}झ\u{44db0}#\u{3}\n\u{5bb50}\u{925d9}\u{925d5}\u{925d5}\u{925d5}\u{925d5}䕞\u{9a70d}\u{3d2ff}\u{10ef4f}\u{87307}\u{6}\u{10ef0a}\u{10ffff}\u{ad7e5}\u{33f}筚\u{361}➑\u{2}{\u{10de13}\u{10ffff}\u{10ffff}'"]
            expected_output     ["%F2%91%B2%B8%F2%92%8C%AF%E0%A4%9D%F1%84%B6%B0#%03%0A%F1%9B%AD%90%F2%92%97%99%F2%92%97%95%F2%92%97%95%F2%92%97%95%F2%92%97%95%E4%95%9E%F2%9A%9C%8D%F0%BD%8B%BF%F4%8E%BD%8F%F2%87%8C%87%06%F4%8E%BC%8A%F4%8F%BF%BF%F2%AD%9F%A5%CC%BF%E7%AD%9A%CD%A1%E2%9E%91%02%7B%F4%8D%B8%93%F4%8F%BF%BF%F4%8F%BF%BF%27"];
        ]
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_url_string(input, remove_query_string, remove_path_digits);
        assert_eq!(result, expected_output);
    }
}
