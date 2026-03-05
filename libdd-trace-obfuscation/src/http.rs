// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use percent_encoding::percent_decode_str;
use url::Url;

/// Encode path characters that Go's url.EscapedPath() encodes but the url crate doesn't.
/// Only applied to the path portion (before the first '?').
///
/// Two categories:
/// 1. Always encoded: chars not in Go's validEncoded allowlist (e.g. '\', '^', '{', '}', '|')
/// 2. Encoded only when escape() fallback occurs (non-ASCII present): '!', '\'', '(', ')', '*'
///    These are in validEncoded's allowlist so RawPath is used for pure-ASCII paths.
fn encode_go_path_chars(url_str: &str) -> String {
    // Only encode up to the first '?' or '#' — the fragment has different encoding rules
    // (e.g., '!' is allowed in fragments per Go's shouldEscape for encodeFragment).
    let path_end = url_str
        .find(|c| c == '?' || c == '#')
        .unwrap_or(url_str.len());
    let path_part = &url_str[..path_end];
    let rest = &url_str[path_end..];

    let mut encoded = String::with_capacity(path_part.len());
    for c in path_part.chars() {
        match c {
            // Category 1: always encoded (not in validEncoded's explicit allowlist)
            '\\' | '^' | '{' | '}' | '|' | '<' | '>' | '`' | ' ' => {
                encoded.push('%');
                encoded.push_str(&format!("{:02X}", c as u8));
            }
            // Category 2: encoded only when escape() fallback (handled by caller check)
            // These are in Go's validEncoded allowlist but get encoded when escape() is called
            '!' | '\'' | '(' | ')' | '*' | '[' | ']' => {
                encoded.push('%');
                encoded.push_str(&format!("{:02X}", c as u8));
            }
            _ => encoded.push(c),
        }
    }
    if rest.is_empty() {
        encoded
    } else {
        format!("{encoded}{rest}")
    }
}

/// Apply path-digit removal to a relative URL string returned by go_like_reference.
/// Operates only on the path portion (before the first '?'), matching Go's behavior of
/// splitting path by '/' and replacing segments containing digits with '?'.
fn remove_relative_path_digits(url_str: &str) -> String {
    // Only apply digit removal to the path (before '?' or '#'); fragments are not paths.
    let path_end = url_str
        .find(|c: char| c == '?' || c == '#')
        .unwrap_or(url_str.len());
    let path_part = &url_str[..path_end];
    let rest = &url_str[path_end..];

    let mut segments: Vec<&str> = path_part.split('/').collect();
    let mut changed = false;
    for segment in segments.iter_mut() {
        if let Ok(decoded) = percent_decode_str(segment).decode_utf8() {
            if decoded.chars().any(|c| char::is_ascii_digit(&c)) {
                *segment = "?";
                changed = true;
            }
        }
    }
    if changed {
        format!("{}{}", segments.join("/"), rest)
    } else {
        url_str.to_string()
    }
}

fn has_invalid_percent_encoding(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Go-ish behavior:
/// - Accepts almost anything as a URL reference
/// - If it's absolute, return it as-is (normalized/encoded)
/// - If it's relative, return the encoded relative reference (no dummy base in output)
pub fn go_like_reference(input: &str, remove_query_string: bool) -> String {
    // Dummy base just to let the parser resolve relatives
    let base = Url::parse("https://example.invalid/").unwrap();

    // Try absolute first (like "https://...", "mailto:...", etc.)
    if let Ok(abs) = Url::parse(input) {
        return abs.to_string();
    }

    // Otherwise parse as a relative reference against the dummy base
    let resolved = base.join(input).unwrap_or_else(|_| {
        // If join fails (rare, but can happen with weird inputs), fall back to putting it in the
        // path.
        let mut u = base.clone();
        u.set_path(input);
        u
    });

    // Strip the dummy origin back off so you get "hello%20world", "/x%20y", "?q=a%20b", "#frag",
    // etc.
    let full = resolved.as_str();

    // base.as_str() is "https://example.invalid/"
    let base_prefix = base.as_str();

    // For absolute-path inputs (starting with '/'), use the no-trailing-slash strip
    // to preserve the leading '/' in the result. Otherwise base.join("/ჸ") resolves to
    // "https://example.invalid/%E1%83%B8" and stripping the base WITH trailing slash
    // drops the leading '/'.
    if input.starts_with('/') {
        if let Some(rest) = full.strip_prefix("https://example.invalid") {
            if remove_query_string && resolved.query().is_some() {
                let path_end = rest.find('?').unwrap_or(rest.len());
                return format!("{}?", &rest[..path_end]);
            }
            return rest.to_string();
        }
    }

    if let Some(rest) = full.strip_prefix(base_prefix) {
        // relative path (e.g. "hello%20world" or "dir/hello%20world")
        if remove_query_string && resolved.query().is_some() {
            // Strip the query string, preserving the path with a trailing "?"
            let path_end = rest.find('?').unwrap_or(rest.len());
            return format!("{}?", &rest[..path_end]);
        }
        rest.to_string()
    } else if let Some(rest) = full.strip_prefix("https://example.invalid") {
        // covers cases like "/path" where the base origin remains
        rest.to_string()
    } else {
        // shouldn't happen, but safe fallback
        full.to_string()
    }
}

pub fn obfuscate_url_string(
    url: &str,
    remove_query_string: bool,
    remove_path_digits: bool,
) -> String {
    // Go rejects control chars in the path (returns '?'). Check before Url::parse since
    // the url crate may silently drop control chars and succeed where Go would fail.
    if remove_query_string || remove_path_digits {
        let path_end = url.find('#').unwrap_or(url.len());
        if url[..path_end].bytes().any(|b| b < 0x20 || b == 0x7F) {
            return String::from("?");
        }
    }
    let mut parsed_url = match Url::parse(url) {
        Ok(res) => {
            // For cannot-be-a-base (opaque) URIs like "A:ᏤᏤ", Go keeps the opaque
            // path verbatim. Return with lowercased scheme.
            // Exception: if the opaque part has control chars, Go's url.Parse fails
            // and obfuscateUserInfo returns the original URL unchanged.
            if res.cannot_be_a_base() {
                let scheme_len = url.find(':').unwrap_or(0);
                let opaque_part = &url[scheme_len..];
                if opaque_part.bytes().any(|b| b < 0x20 || b == 0x7F) {
                    return url.to_string(); // Go returns original on parse error
                }
                return url[..scheme_len].to_lowercase() + opaque_part;
            }
            res
        }
        Err(_) => {
            // Fragment-only references (e.g. "#", "#frag") are valid relative URL references.
            // Go's url.Parse handles them successfully: "#" → "" (empty fragment → empty string),
            // "#frag" → "#frag". Handle these before the go_like_reference fallback to prevent
            // the "empty result → ?" heuristic from incorrectly triggering.
            if let Some(fragment) = url.strip_prefix('#') {
                if fragment.is_empty() {
                    return String::new();
                }
                // Go also rejects invalid percent-encoding in fragments.
                if has_invalid_percent_encoding(fragment) {
                    return String::from("?");
                }
                // Go's url.Parse percent-encodes certain chars in fragments:
                // - Always: control chars, '#'
                // - When non-ASCII present (escape() fallback): '!', '\'', '(', ')', '*', '[', ']'
                //   (These are in validEncoded's allowlist so kept for pure-ASCII fragments,
                //    but escape() encodes them too.)
                let frag_has_non_ascii = fragment.bytes().any(|b| b > 127);
                let url_for_join =
                    if fragment.bytes().any(|b| b < 0x20 || b == 0x7F || b == b'#')
                        || (frag_has_non_ascii
                            && fragment
                                .chars()
                                .any(|c| matches!(c, '\'' | '[' | ']')))
                    {
                        let mut encoded = String::from('#');
                        for c in fragment.chars() {
                            let cp = c as u32;
                            if cp < 0x20 || cp == 0x7F || c == '#' {
                                encoded.push_str(&format!("%{cp:02X}"));
                            } else if frag_has_non_ascii
                                && matches!(c, '\'' | '[' | ']')
                            {
                                encoded.push_str(&format!("%{:02X}", c as u8));
                            } else {
                                encoded.push(c);
                            }
                        }
                        encoded
                    } else {
                        url.to_string()
                    };
                return go_like_reference(&url_for_join, remove_query_string);
            }
            // Go's url.Parse rejects control characters (bytes < 0x20 or 0x7F) in the PATH and
            // returns "?". BUT when both options are false, Go's obfuscateUserInfo returns
            // the original URL on parse failure (no "?").
            // Control chars in the FRAGMENT are percent-encoded, not rejected.
            {
                let path_end = url.find('#').unwrap_or(url.len());
                if url[..path_end].bytes().any(|b| b < 0x20 || b == 0x7F) {
                    if !remove_query_string && !remove_path_digits {
                        return url.to_string();
                    }
                    return String::from("?");
                }
                // Pre-encode control chars in the fragment (if any) before go_like_reference.
                if path_end < url.len()
                    && url[path_end + 1..].bytes().any(|b| b < 0x20 || b == 0x7F || b == b'#')
                {
                    let mut pre_encoded = url[..path_end].to_string();
                    pre_encoded.push('#');
                    for c in url[path_end + 1..].chars() {
                        let cp = c as u32;
                        if cp < 0x20 || cp == 0x7F || c == '#' {
                            pre_encoded.push_str(&format!("%{cp:02X}"));
                        } else {
                            pre_encoded.push(c);
                        }
                    }
                    // Use the pre-encoded URL for the rest of the processing
                    let url = pre_encoded.as_str();
                    // Continue to go_like_reference below using the pre-encoded url
                    // (fall through with modified url)
                    let url_pre_encoded_for_backslash;
                    let url_for_go_like = if url.contains('\\') {
                        url_pre_encoded_for_backslash = url.replace('\\', "%5C");
                        url_pre_encoded_for_backslash.as_str()
                    } else {
                        url
                    };
                    let raw = go_like_reference(url_for_go_like, remove_query_string);
                    let raw = if raw.ends_with('#') { raw[..raw.len()-1].to_string() } else { raw };
                    let result = if raw.is_empty() && !url.is_empty() { url.to_string() } else { raw };
                    let path_end_for_ascii = url.find('#').unwrap_or(url.len());
                    let has_non_ascii = url[..path_end_for_ascii].bytes().any(|b| b > 127);
                    let result = if has_non_ascii { encode_go_path_chars(&result) } else {
                        let qs = result.find('?').unwrap_or(result.len());
                        let pp = &result[..qs]; let rr = &result[qs..];
                        let mut enc = String::with_capacity(pp.len()); let mut changed = false;
                        for c in pp.chars() { match c { '\\' | '^' | '{' | '}' | '|' | '<' | '>' | '`' | ' ' => { enc.push('%'); enc.push_str(&format!("{:02X}", c as u8)); changed = true; } _ => enc.push(c), } }
                        if changed { if rr.is_empty() { enc } else { format!("{enc}{rr}") } } else { result }
                    };
                    if remove_path_digits { return remove_relative_path_digits(&result); }
                    return result;
                }
            }
            // Go's url.Parse rejects invalid percent-encoding sequences (bare '%' or '%' not
            // followed by exactly two hex digits). The `url` crate re-encodes them as '%25',
            // so we must detect and reject them explicitly.
            if has_invalid_percent_encoding(url) {
                return String::from("?");
            }
            // Go's url.Parse rejects URLs where the first path segment contains ':' (RFC 3986
            // §4.2): this is ambiguous with a scheme separator. E.g., ":" and "1:b" both fail
            // with "missing protocol scheme" or "first path segment cannot contain colon".
            // The url crate silently accepts these as path chars.
            {
                let segment_end = url
                    .find(|c| matches!(c, '/' | '?' | '#'))
                    .unwrap_or(url.len());
                if url[..segment_end].contains(':') {
                    return String::from("?");
                }
            }
            // For query-only references (starting with '?'), Go keeps the query raw.
            // With remove_query_string=true, return "?". Otherwise return original.
            if url.starts_with('?') {
                if has_invalid_percent_encoding(&url[1..]) {
                    return String::from("?");
                }
                if remove_query_string {
                    return String::from("?");
                }
                // Return original (Go keeps query chars raw, including non-ASCII)
                return url.to_string();
            }
            // The url crate treats '\' as a path separator, silently consuming it.
            // Go encodes '\' as '%5C'. Pre-encode backslashes before go_like_reference
            // so they are preserved through base.join() and appear as '%5C' in the output.
            let url_pre_encoded;
            let url_for_go_like = if url.contains('\\') {
                url_pre_encoded = url.replace('\\', "%5C");
                url_pre_encoded.as_str()
            } else {
                url
            };
            let fixme_url_go_parsing_raw =
                go_like_reference(url_for_go_like, remove_query_string);
            // Go's url.URL.String() omits a trailing empty fragment (bare '#').
            // The url crate keeps it. Strip it here for parity.
            let fixme_url_go_parsing = if fixme_url_go_parsing_raw.ends_with('#') {
                fixme_url_go_parsing_raw[..fixme_url_go_parsing_raw.len() - 1].to_string()
            } else {
                fixme_url_go_parsing_raw
            };
            let result = if fixme_url_go_parsing.is_empty() && !url.is_empty() {
                // The url crate resolved away dot path segments (e.g. "." or "..") via RFC 3986
                // normalization. Go's url.Parse preserves them literally. Return the original,
                // but strip a trailing empty fragment '#' (Go omits empty fragments).
                let fallback = if url.ends_with('#') { &url[..url.len()-1] } else { url };
                fallback.to_string()
            } else {
                // If the original URL had a dot-segment prefix (., .., ./, ../) that
                // base.join() resolved away, Go preserves it literally. Re-prepend it.
                let frag_or_end = url.find(|c| c == '#' || c == '?').unwrap_or(url.len());
                let orig_path = &url[..frag_or_end];
                let dot_prefix_len = {
                    let mut i = 0;
                    loop {
                        if orig_path[i..].starts_with("../") { i += 3; }
                        else if orig_path[i..].starts_with("./") { i += 2; }
                        else if &orig_path[i..] == ".." || &orig_path[i..] == "." {
                            i += orig_path[i..].len(); break;
                        } else { break; }
                    }
                    i
                };
                if dot_prefix_len > 0 {
                    let dot_prefix = &url[..dot_prefix_len];
                    // Prepend the lost dot prefix
                    if !fixme_url_go_parsing.starts_with(dot_prefix) {
                        format!("{}{}", dot_prefix, fixme_url_go_parsing)
                    } else {
                        fixme_url_go_parsing
                    }
                } else if fixme_url_go_parsing.starts_with('#') {
                    // Non-dot path resolved to fragment only - prepend original path
                    if !orig_path.is_empty() {
                        format!("{}{}", orig_path, fixme_url_go_parsing)
                    } else {
                        fixme_url_go_parsing
                    }
                } else {
                    fixme_url_go_parsing
                }
            };
            // Encode path chars that Go encodes but the url crate doesn't.
            // Always apply encode_go_path_chars since it handles:
            // - Category 1 (always encoded): \, ^, {, }, |, <, >, `, space
            // - Category 2 (only when non-ASCII triggers escape() fallback): !, ', (, ), *
            // For category 2, we still apply them here unconditionally since encode_go_path_chars
            // would encode them for non-ASCII inputs; for pure-ASCII those chars were already
            // handled by validEncoded allowing them in RawPath. But since we're post-processing
            // the url crate's output (which keeps them), we must encode them only when non-ASCII.
            // Simplification: apply all encodings, but for category 2 chars only when non-ASCII.
            // Only check path portion (before '#') for non-ASCII; a non-ASCII fragment
            // does not trigger Go's escape() fallback for the path encoding.
            let path_end_for_ascii_check = url.find('#').unwrap_or(url.len());
            let has_non_ascii = url[..path_end_for_ascii_check].bytes().any(|b| b > 127);
            let result = if has_non_ascii {
                // Full encoding: both category 1 and category 2 in path + fragment.
                // When non-ASCII is present, Go's escape() also encodes cat2 chars in fragments.
                let encoded = encode_go_path_chars(&result);
                // Check if original URL's fragment also has non-ASCII
                let url_frag_start = url.find('#').map(|i| i + 1).unwrap_or(url.len());
                let frag_has_non_ascii = url[url_frag_start..].bytes().any(|b| b > 127);
                if frag_has_non_ascii {
                    // Also encode cat2 chars in the result's fragment
                    if let Some(frag_start) = encoded.find('#') {
                        let path_and_hash = &encoded[..=frag_start];
                        let frag = &encoded[frag_start + 1..];
                        // In fragments, Go encodes ' [ ] when non-ASCII triggers escape(),
                        // but NOT ! ( ) * (shouldEscape returns false for those in encodeFragment)
                        if frag.chars().any(|c| matches!(c, '\'' | '[' | ']')) {
                            let mut out = path_and_hash.to_string();
                            for c in frag.chars() {
                                if matches!(c, '\'' | '[' | ']') {
                                    out.push_str(&format!("%{:02X}", c as u8));
                                } else {
                                    out.push(c);
                                }
                            }
                            out
                        } else {
                            encoded
                        }
                    } else {
                        encoded
                    }
                } else {
                    encoded
                }
            } else {
                // ASCII-only: only category 1 chars (\, ^, etc.)
                // Category 2 (!, ', (, ), *) are left as-is for pure ASCII inputs
                // Also stop at '#' since fragment has different encoding rules
                let path_end = result.find(|c| c == '?' || c == '#').unwrap_or(result.len());
                let path_part = &result[..path_end];
                let rest = &result[path_end..];
                let mut encoded = String::with_capacity(path_part.len());
                let mut changed = false;
                for c in path_part.chars() {
                    match c {
                        '\\' | '^' | '{' | '}' | '|' | '<' | '>' | '`' | ' ' => {
                            encoded.push('%');
                            encoded.push_str(&format!("{:02X}", c as u8));
                            changed = true;
                        }
                        _ => encoded.push(c),
                    }
                }
                if changed {
                    if rest.is_empty() {
                        encoded
                    } else {
                        format!("{encoded}{rest}")
                    }
                } else {
                    result
                }
            };
            // Go keeps the query string raw (url.RawQuery in Go's URL struct).
            // The url crate encodes query chars; restore the original query from the input.
            let result = if !remove_query_string {
                if let Some(orig_q_start) = url.find('?') {
                    let orig_query = &url[orig_q_start..]; // includes '?' and up to '#'
                    if let Some(result_q_start) = result.find('?') {
                        format!("{}{}", &result[..result_q_start], orig_query)
                    } else {
                        result
                    }
                } else {
                    result
                }
            } else {
                result
            };
            if remove_path_digits {
                return remove_relative_path_digits(&result);
            }
            return result;
        }
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
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_url_string(input, remove_query_string, remove_path_digits);
        assert_eq!(result, expected_output);
    }
}
