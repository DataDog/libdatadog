// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// FIXME: once obfuscation feature parity is reached with the agent, change both modules to be more
// restrictive on the accepted forms of urls so that this module can be greatly simplified.
// One idea for now is to match the url to a regex on both side to validate it

use core::fmt::Write;
use fluent_uri::UriRef;
use percent_encoding::percent_decode_str;

/// Returns true for Go net/url's "category 1" characters:
/// ASCII bytes that always trigger escaping in URLs (plus space and quote).
const fn is_go_url_escape_cat1(c: char) -> bool {
    matches!(
        c,
        '\\' | '^' | '{' | '}' | '|' | '<' | '>' | '`' | ' ' | '"'
    )
}

/// Returns true for Go net/url's "category 2" characters for PATH contexts:
/// characters Go may escape in paths when Cat1 is present or non-ASCII exists.
const fn is_go_url_escape_cat2_path(c: char) -> bool {
    matches!(c, '!' | '\'' | '(' | ')' | '*' | '[' | ']')
}

/// Returns true for Go net/url's "category 2" characters for FRAGMENT contexts:
/// characters Go may escape in fragments when non-ASCII exists.
const fn is_go_url_escape_cat2_fragment(c: char) -> bool {
    matches!(c, '\'' | '[' | ']')
}

const fn hex_val(b: u8) -> u8 {
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

/// Best-effort userinfo stripping for URLs that fail strict RFC 3986 parsing (or contain control
/// chars) and would otherwise be returned unchanged: credentials must never survive obfuscation
/// even when we can't fully parse the rest of the URL.
///
/// `prefix_end` bounds the scan to the path portion (before query/fragment) so a `@` inside the
/// query or fragment is never mistaken for userinfo.
///
/// This is a heuristic, not a parser: it may occasionally strip more than necessary on
/// non-URL-like malformed input, but it never leaves real credentials in the output.
fn strip_userinfo_best_effort(url: &str, prefix_end: usize) -> String {
    let prefix = &url[..prefix_end];
    let Some(marker) = prefix.find("//") else {
        return url.to_string();
    };
    let authority_start = marker + 2;
    let authority_end = prefix[authority_start..]
        .find('/')
        .map_or(prefix.len(), |i| authority_start + i);
    let authority = &prefix[authority_start..authority_end];
    let Some(at_pos) = authority.rfind('@') else {
        return url.to_string();
    };
    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..authority_start]);
    out.push_str(&authority[at_pos + 1..]);
    out.push_str(&url[authority_end..]);
    out
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

/// Returns whether [`obfuscate_url_string`] could change `url`, letting callers skip building the
/// obfuscated `String` when there is nothing to do.
///
/// Conservative superset of the transform's triggers: it may return `true` for an input the
/// transform leaves unchanged, but never `false` for one it would modify.
#[must_use]
pub fn should_obfuscate_url(
    url: &str,
    remove_query_string: bool,
    remove_path_digits: bool,
) -> bool {
    // Every trigger is single-byte ASCII, and non-ASCII bytes are caught by the range guard, so we
    // scan bytes directly and avoid UTF-8 decoding. Path and query need different checks, so we
    // split the scan at the first '?'.
    let bytes = url.as_bytes();

    // Path region.
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // A fragment is always normalized and userinfo always stripped. Returning on the first
            // '#' guarantees any '@' seen here precedes the fragment.
            b'#' | b'@' => return true,
            b'?' => {
                if remove_query_string {
                    return true;
                }
                i += 1;
                break;
            }
            b if (remove_path_digits && b.is_ascii_digit())
                    // non-ASCII (>= 0x80) or control char (< 0x20)
                    || !(0x20..0x80).contains(&b)
                    || b == 0x7F
                    || b == b'%'
                    // uppercase scheme gets lowercased
                    || b.is_ascii_uppercase()
                    || is_go_url_escape_cat1(b as char)
                    || is_go_url_escape_cat2_path(b as char) =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }

    // Query region: reached only when remove_query_string is false, so a control char still
    // forces "?" output only under remove_path_digits (obfuscate_url_string rejects control
    // chars in path+query when either flag is set).
    while i < bytes.len() {
        match bytes[i] {
            b'#' | b'@' => return true,
            b if remove_path_digits && (b < 0x20 || b == 0x7F) => return true,
            _ => {}
        }
        i += 1;
    }

    false
}

/// Obfuscates an HTTP URL, returning `None` when nothing needs to change.
///
/// Runs [`should_obfuscate_url`] first so no `String` is allocated when there is nothing to do.
#[must_use]
pub fn obfuscate_url(
    url: &str,
    remove_query_string: bool,
    remove_path_digits: bool,
) -> Option<String> {
    if !should_obfuscate_url(url, remove_query_string, remove_path_digits) {
        return None;
    }
    Some(obfuscate_url_string(
        url,
        remove_query_string,
        remove_path_digits,
    ))
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
            strip_userinfo_best_effort(url, path_end)
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

    let Ok(parsed) = UriRef::parse(pre.as_str()) else {
        return if remove_query_string || remove_path_digits {
            "?".to_string()
        } else {
            strip_userinfo_best_effort(url, path_end)
        };
    };

    let mut out = String::new();

    if let Some(scheme) = parsed.scheme() {
        out.push_str(&scheme.as_str().to_lowercase());
        out.push(':');
    }

    if let Some(auth) = parsed.authority() {
        out.push_str("//");
        // Strip userinfo — emit only host[:port]
        out.push_str(auth.host());
        if let Some(port) = auth.port() {
            out.push(':');
            out.push_str(port.as_str());
        }
        let path_str = normalize_pct_encoded_unreserved(parsed.path().as_str());
        if remove_path_digits {
            out.push_str(&redact_path_digits(&path_str));
        } else {
            out.push_str(&path_str);
        }
    } else if let Some(scheme) = parsed.scheme() {
        // This is a really weird case because there is a scheme but no authority.
        // For example: http:#
        // Length of "http:"
        let scheme_end = scheme.as_str().len() + 1;
        // http://example.com/?query -> //example.com/
        out.push_str(&url[scheme_end..path_end]);
    } else {
        // Relative reference: use pre-encoded path
        let path_str = normalize_pct_encoded_unreserved(parsed.path().as_str());
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

    if let Some(frag) = parsed.fragment() {
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
    test_name remove_query_string remove_path_digits input expected_output;
    [remove_query_string_1] [true] [false] ["http://foo.com/"] ["http://foo.com/"];
    [remove_query_string_2] [true] [false] ["http://foo.com/123"] ["http://foo.com/123"];
    [remove_query_string_3] [true] [false] ["http://foo.com/id/123/page/1?search=bar&page=2"] ["http://foo.com/id/123/page/1?"];
    [remove_query_string_4] [true] [false] ["http://foo.com/id/123/page/1?search=bar&page=2#fragment"] ["http://foo.com/id/123/page/1?#fragment"];
    [remove_query_string_5] [true] [false] ["http://foo.com/id/123/page/1?blabla"] ["http://foo.com/id/123/page/1?"];
    [remove_query_string_6] [true] [false] ["http://foo.com/id/123/pa%3Fge/1?blabla"] ["http://foo.com/id/123/pa%3Fge/1?"];
    [remove_query_string_7] [true] [false] ["http://user:password@foo.com/1/2/3?q=james"] ["http://foo.com/1/2/3?"];
    [remove_path_digits_1] [false] [true] ["http://foo.com/"] ["http://foo.com/"];
    [remove_path_digits_2] [false] [true] ["http://foo.com/name?query=search"] ["http://foo.com/name?query=search"];
    [remove_path_digits_3] [false] [true] ["http://foo.com/id/123/page/1?search=bar&page=2"] ["http://foo.com/id/?/page/??search=bar&page=2"];
    [remove_path_digits_4] [false] [true] ["http://foo.com/id/a1/page/1qwe233?search=bar&page=2#fragment-123"] ["http://foo.com/id/?/page/??search=bar&page=2#fragment-123"];
    [remove_path_digits_5] [false] [true] ["http://foo.com/123"] ["http://foo.com/?"];
    [remove_path_digits_6] [false] [true] ["http://foo.com/123/abcd9"] ["http://foo.com/?/?"];
    [remove_path_digits_7] [false] [true] ["http://foo.com/123/name/abcd9"] ["http://foo.com/?/name/?"];
    [remove_path_digits_8] [false] [true] ["http://foo.com/1%3F3/nam%3Fe/abcd9"] ["http://foo.com/?/nam%3Fe/?"];
    [empty_input] [false] [false] [""] [""];
    [non_printable_chars] [false] [false] ["\u{10}"] ["\u{10}"];
    [non_printable_chars_and_unicode] [true] [true] ["\u{10}ჸ"] ["?"];
    [hashtag] [true] [true] ["#"] [""];
    [fuzzing_1050521893] [true] [true] ["ჸ"] ["%E1%83%B8"];
    [fuzzing_594901251] [true] [true] ["%"] ["?"];
    [fuzzing_3638045804] [true] [true] ["."] ["."];
    [fuzzing_1928485962] [true] [true] ["0"] ["?"];
    [fuzzing_4273565798] [true] [true] ["!ჸ"] ["%21%E1%83%B8"];
    [fuzzing_1457007156] [true] [true] ["!"] ["!"];
    [fuzzing_3119724369] [true] [true] [":"] ["?"];
    [fuzzing_1092426409] [true] [true] ["#ჸ"] ["#%E1%83%B8"];
    [fuzzing_1323831861] [true] [true] ["#\u{01}"] ["#%01"];
    [fuzzing_35626170] [true] [true] ["#\u{01}ჸ"] ["#%01%E1%83%B8"];
    [fuzzing_618280270] [true] [true] ["\\"] ["%5C"];
    [fuzzing_1505427946] [true] [true] ["[ჸ"] ["%5B%E1%83%B8"];
    [fuzzing_backslash_unicode] [true] [true] ["\\ჸ"] ["%5C%E1%83%B8"];
    [fuzzing_2438023093] [true] [true] ["ჸ#"] ["%E1%83%B8"];
    [fuzzing_2729083127] [true] [true] ["!#ჸ"] ["!#%E1%83%B8"];
    [fuzzing_slash_unicode] [true] [true] ["/ჸ"] ["/%E1%83%B8"];
    [fuzzing_3710129001] [true] [true] ["##"] ["#%23"];
    [fuzzing_1009954227] [true] [true] ["ჸ#\u{10}"] ["%E1%83%B8#%10"];
    [fuzzing_hash_exclamation] [true] [true] ["ჸ#!"] ["%E1%83%B8#!"];
    [fuzzing_578834728] [true] [true] ["#%"] ["?"];
    [fuzzing_3991369296] [true] [true] ["#'ჸ"] ["#%27%E1%83%B8"];
    [fuzzing_path_frag_quote] [true] [true] ["ჸ#'ჸ"] ["%E1%83%B8#%27%E1%83%B8"];
    [fuzzing_hash_excl_unicode] [true] [true] ["#!ჸ"] ["#!%E1%83%B8"];
    [fuzzing_2455396347_cat1_triggers_cat2] [true] [true] ["<!"] ["%3C%21"];
    [fuzzing_3886417401] [true] [true] ["ჸ#%\u{1}"] ["?"];
    [parity_double_quote_cat1] [true] [true] ["\"!"] ["%22%21"];
    [parity_dot_hash_unicode] [true] [true] [".#ჸ"] [".#%E1%83%B8"];
    [parity_dot_hash] [true] [true] [".#"] ["."];
    [parity_unicode_hash_digit] [true] [true] ["ჸ#0"] ["%E1%83%B8#0"];
    [parity_scheme_empty_frag] [true] [true] ["C:#"] ["c:"];
    [parity_relative_dotdot_unicode] [true] [true] ["../ჸ"] ["../%E1%83%B8"];
    [parity_query_hash_unicode_both] [true] [true] ["?#ჸ"] ["?#%E1%83%B8"];
    [parity_query_hash_unicode_digits] [false] [true] ["?#ჸ"] ["?#%E1%83%B8"];
    [parity_excl_query_unicode] [true] [true] ["!?ჸ"] ["!?"];
    [parity_query_unicode_keep] [false] [true] ["?ჸ"] ["?ჸ"];
    [parity_space_unicode] [true] [true] [" ჸ"] ["%20%E1%83%B8"];
    [parity_unicode_query_unicode_keep] [false] [true] ["ჸ?ჸ"] ["%E1%83%B8?ჸ"];
    [parity_unicode_query_hash_both] [true] [true] ["?ჸ#ჸ"] ["?#%E1%83%B8"];
    [parity_unicode_query_empty_hash] [false] [true] ["ჸ?#"] ["%E1%83%B8?"];
    [parity_pct_unreserved_normalize] [true] [false] ["%30ჸ"] ["0%E1%83%B8"];
    [parity_unicode_query_invalid_pct] [true] [true] ["ჸ?%"] ["%E1%83%B8?"];
    [parity_not_a_url_both_false] [false] [false] ["this is not a valid url"] ["this%20is%20not%20a%20valid%20url"];
    [parity_not_a_url_both_true] [true] [true] ["this is not a valid url"] ["this%20is%20not%20a%20valid%20url"];
    [parity_disabled_userinfo] [false] [false] ["http://user:password@foo.com/1/2/3?q=james"] ["http://foo.com/1/2/3?q=james"];
    // Regression for APMSP-3086: malformed percent-encoding fails strict parsing and used to
    // fall back to returning the URL (with userinfo) unchanged.
    [regression_malformed_pct_encoding_strips_userinfo] [false] [false] ["http://user:password@foo.com/%"] ["http://foo.com/%"];
    // Regression for APMSP-3086: a control char in the path also fell back to returning the URL
    // (with userinfo) unchanged when both flags are false.
    [regression_control_char_strips_userinfo] [false] [false] ["http://user:password@foo.com/\u{1}"] ["http://foo.com/\u{1}"];
    // Regression: an '@' in the query (not real userinfo) must not be mistaken for userinfo
    // when a control char forces the best-effort stripping fallback. The authority scan must
    // stop at the query delimiter, not the fragment delimiter.
    [regression_control_char_query_at_not_mistaken_for_userinfo] [false] [false] ["http://example.com?email=a@b\u{0}"] ["http://example.com?email=a@b\u{0}"];
    [parity_colon_both_false] [false] [false] [":"] [":"];
    [parity_pct_both_false] [false] [false] ["%"] ["%"];
    [parity_ctrl_in_scheme_both_false] [false] [false] ["C:\u{1}"] ["C:\u{1}"];
    [parity_ctrl_both_false] [false] [false] ["\u{1}"] ["\u{1}"];
    [parity_frag_curly_brace] [true] [true] ["ჸ#{ჸ"] ["%E1%83%B8#%7B%E1%83%B8"];
    [parity_opaque_url_unicode] [true] [true] ["A:ჸ"] ["a:ჸ"];
    [no_decode_dash] [false] [false] ["http://foo.com/foo%20bar/"] ["http://foo.com/foo%20bar/"];
    [parity_fuzzing_supp_unicode_frag] [true] [true] ["\u{91cb8}\u{9232f}झ\u{44db0}#\u{3}\n\u{5bb50}\u{925d9}\u{925d5}\u{925d5}\u{925d5}\u{925d5}䕞\u{9a70d}\u{3d2ff}\u{10ef4f}\u{87307}\u{6}\u{10ef0a}\u{10ffff}\u{ad7e5}\u{33f}筚\u{361}➑\u{2}{\u{10de13}\u{10ffff}\u{10ffff}'"] ["%F2%91%B2%B8%F2%92%8C%AF%E0%A4%9D%F1%84%B6%B0#%03%0A%F1%9B%AD%90%F2%92%97%99%F2%92%97%95%F2%92%97%95%F2%92%97%95%F2%92%97%95%E4%95%9E%F2%9A%9C%8D%F0%BD%8B%BF%F4%8E%BD%8F%F2%87%8C%87%06%F4%8E%BC%8A%F4%8F%BF%BF%F2%AD%9F%A5%CC%BF%E7%AD%9A%CD%A1%E2%9E%91%02%7B%F4%8D%B8%93%F4%8F%BF%BF%F4%8F%BF%BF%27"];
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_url_string(input, remove_query_string, remove_path_digits);
        assert_eq!(result, expected_output);
    }

    #[test]
    fn should_obfuscate_url_precheck() {
        use super::should_obfuscate_url;

        // Control char in query with remove_path_digits triggers obfuscation.
        assert!(should_obfuscate_url("http://foo.com/p?q=\0", false, true));
        // Same input without remove_path_digits: query restored verbatim, no trigger.
        assert!(!should_obfuscate_url("http://foo.com/p?q=\0", false, false));
        // Clean query, both flags false: no trigger.
        assert!(!should_obfuscate_url("http://foo.com/p?q=1", false, false));
        // Clean path, both flags false: no trigger.
        assert!(!should_obfuscate_url("http://foo.com/path", false, false));
        // Path digit with remove_path_digits triggers obfuscation.
        assert!(should_obfuscate_url("http://foo.com/p1", false, true));
    }
}
