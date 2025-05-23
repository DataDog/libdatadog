// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use regex::Regex;
use std::{borrow::Cow, collections::HashSet, net::Ipv6Addr, sync::OnceLock};

const ALLOWED_IP_ADDRESSES: [&str; 4] = [
    // localhost
    "127.0.0.1",
    "::1",
    // link-local cloud provider metadata server addresses
    "169.254.169.254",
    "fd00:ec2::254",
];

const PREFIX_REGEX_LITTERAL: &str = r"^((?:dnspoll|ftp|file|http|https):/{2,3})";
static PREFIX_REGEX: OnceLock<Regex> = OnceLock::new();

/// Quantizes a comma separated list of hosts.
///
/// Each entry which is an IP address is replaced using quantizeIP. Duplicate entries
/// post-quantization or collapsed into a single unique value. Entries which are not IP addresses
/// are left unchanged. Comma-separated host lists are common for peer tags like
/// peer.cassandra.contact.points, peer.couchbase.seed.nodes, peer.kafka.bootstrap.servers
///
/// The quantized value is return as a `Cow` containing the input slice `s` if no modification was
/// done or a new String if the value has been modified.
///
/// This quantization is used to reduce cardinality on peer tags in trace metrics. As such it is
/// not exhaustive and some ip format may not be obfuscated.
/// The reference implementation lives in [dd-go](https://github.com/DataDog/dd-go/blob/393e6de733807b20597d80b1e5103d6e823d8a0c/trace/pkg/peertags/peer_tags.go#L56)
pub fn quantize_peer_ip_addresses<'a>(s: &'a str) -> Cow<'a, str> {
    let values = s.split(',');
    let mut should_return_new_string = false; // Set to true if the function should return a modified
                                              // version of the string

    let quantized_values = values
        .map(|v| {
            if let Some(quantize_string) = quantize_ip(v) {
                should_return_new_string = true;
                Cow::from(quantize_string)
            } else {
                Cow::from(v)
            }
        })
        .collect::<Vec<Cow<'a, str>>>();

    // Quantized value list without duplicates
    let mut quantized_values_dedup: Vec<&str> = Vec::new();
    let mut quantized_values_set: HashSet<&str> = HashSet::new();

    for quantized_value in quantized_values.iter() {
        if quantized_values_set.insert(quantized_value) {
            quantized_values_dedup.push(quantized_value);
        } else {
            should_return_new_string = true;
        }
    }
    if should_return_new_string {
        Cow::from(quantized_values_dedup.join(","))
    } else {
        Cow::from(s)
    }
}

/// Replace valid ip address in `s` to allow quantization.
///
/// The ip is replaced if it is a valid IPv4 or v6
///
/// # Caveats
/// - IPv6 with zone specifier '%' are not detected
/// - IPv6 with suffix are not detected e.g. `::1-foo`
fn quantize_ip(s: &str) -> Option<String> {
    let (prefix, stripped_s) = split_prefix(s);
    if let Some((ip, suffix)) = parse_ip(stripped_s) {
        if !ALLOWED_IP_ADDRESSES.contains(&ip) {
            return Some(format!("{prefix}blocked-ip-address{suffix}"));
        }
    }
    None
}

/// Split the ip prefix, can be either a provider specific prefix or a protocol
fn split_prefix(s: &str) -> (&str, &str) {
    #[allow(clippy::unwrap_used)]
    if let Some(tail) = s.strip_prefix("ip-") {
        ("ip-", tail)
    } else if let Some(protocol) = PREFIX_REGEX
        .get_or_init(|| Regex::new(PREFIX_REGEX_LITTERAL).unwrap())
        .find(s)
    {
        s.split_at(protocol.end())
    } else {
        ("", s)
    }
}

/// Check if `s` starts with a valid ip. If it does return Some((ip, suffix)), else return None.
fn parse_ip(s: &str) -> Option<(&str, &str)> {
    for ch in s.chars() {
        // Determine the version of the ip
        match ch {
            '0'..='9' => continue,
            '.' | '-' | '_' => return parse_ip_v4(s, ch),
            ':' | 'A'..='F' | 'a'..='f' => {
                if s.parse::<Ipv6Addr>().is_ok() {
                    return Some((s, ""));
                } else {
                    return None;
                }
            }
            '[' => {
                // Parse IPv6 in [host]:port format
                if let Some((host, port)) = s[1..].split_once(']') {
                    if host.parse::<Ipv6Addr>().is_ok() {
                        return Some((host, port));
                    }
                }
                return None;
            }
            _ => return None,
        }
    }
    None
}

/// Check if `s` starts with a valid ipv4. If it does return Some(ip, suffix), else return None.
/// We implement a custom ipv4 parsing to allow `-` and `_` as separator.
fn parse_ip_v4(s: &str, sep: char) -> Option<(&str, &str)> {
    let mut field_value = 0;
    let mut field_digits = 0;
    let mut current_field = 0;
    let mut last_index = s.len();
    for (i, ch) in s.char_indices() {
        #[allow(clippy::unwrap_used)]
        if ch.is_ascii_digit() {
            // A field can't have a leading 0
            if field_digits == 1 && field_value == 0 {
                return None;
            }
            // Add digit to value, safe since ch is a digit
            field_value = field_value * 10 + ch.to_digit(10).unwrap();
            field_digits += 1;
            if field_value > 255 {
                return None;
            }
        } else if ch == sep {
            // A valid field has at least one digit
            if field_digits == 0 {
                return None;
            }
            // If we already have 4 fields, parsing is over
            if current_field == 3 {
                last_index = i;
                break;
            }
            field_value = 0;
            field_digits = 0;
            current_field += 1;
        } else {
            // An invalid character ends parsing
            last_index = i;
            break;
        }
    }
    // Check that we found at 4 fields and that the last one as at least one digit
    if field_digits > 0 && current_field == 3 {
        Some(s.split_at(last_index))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_prefix() {
        assert_eq!(split_prefix("ip-1.1.1.1"), ("ip-", "1.1.1.1"));
        assert_eq!(split_prefix("https://1.1.1.1"), ("https://", "1.1.1.1"));
        assert_eq!(split_prefix("ftp:///1.1.1.1"), ("ftp:///", "1.1.1.1"));
        assert_eq!(split_prefix("1.1.1.1"), ("", "1.1.1.1"));
        assert_eq!(split_prefix("foo,bar-1.1.1.1"), ("", "foo,bar-1.1.1.1"));
    }

    #[test]
    fn test_quantize_peer_ip_addresses() {
        // Special cases
        // - localhost
        assert_eq!(quantize_peer_ip_addresses("127.0.0.1"), "127.0.0.1");
        assert_eq!(quantize_peer_ip_addresses("::1"), "::1");
        // - link-local IP address, aka "metadata server" for various cloud providers
        assert_eq!(
            quantize_peer_ip_addresses("169.254.169.254"),
            "169.254.169.254"
        );
        // blocking cases
        assert_eq!(quantize_peer_ip_addresses(""), "");
        assert_eq!(quantize_peer_ip_addresses("foo.dog"), "foo.dog");
        assert_eq!(
            quantize_peer_ip_addresses("192.168.1.1"),
            "blocked-ip-address"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192.168.1.1.foo"),
            "blocked-ip-address.foo"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192.168.1.1.2.3.4.5"),
            "blocked-ip-address.2.3.4.5"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192_168_1_1"),
            "blocked-ip-address"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192-168-1-1"),
            "blocked-ip-address"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192-168-1-1.foo"),
            "blocked-ip-address.foo"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192-168-1-1-foo"),
            "blocked-ip-address-foo"
        );
        assert_eq!(
            quantize_peer_ip_addresses("2001:db8:3333:4444:CCCC:DDDD:EEEE:FFFF"),
            "blocked-ip-address"
        );
        assert_eq!(
            quantize_peer_ip_addresses("2001:db8:3c4d:15::1a2f:1a2b"),
            "blocked-ip-address"
        );
        assert_eq!(
            quantize_peer_ip_addresses("[fe80::1ff:fe23:4567:890a]:8080"),
            "blocked-ip-address:8080"
        );
        assert_eq!(
            quantize_peer_ip_addresses("192.168.1.1:1234"),
            "blocked-ip-address:1234"
        );
        assert_eq!(
            quantize_peer_ip_addresses("dnspoll:///10.21.120.145:6400"),
            "dnspoll:///blocked-ip-address:6400"
        );
        assert_eq!(
            quantize_peer_ip_addresses("http://10.21.120.145:6400"),
            "http://blocked-ip-address:6400"
        );
        assert_eq!(
            quantize_peer_ip_addresses("https://10.21.120.145:6400"),
            "https://blocked-ip-address:6400"
        );
        assert_eq!(
            quantize_peer_ip_addresses(
                "192.168.1.1:1234,10.23.1.1:53,10.23.1.1,fe80::1ff:fe23:4567:890a,foo.dog"
            ),
            "blocked-ip-address:1234,blocked-ip-address:53,blocked-ip-address,foo.dog"
        );
        assert_eq!(quantize_peer_ip_addresses("http://172.24.160.151:8091,172.24.163.33:8091,172.24.164.111:8091,172.24.165.203:8091,172.24.168.235:8091,172.24.170.130:8091"), "http://blocked-ip-address:8091,blocked-ip-address:8091");
        assert_eq!(
            quantize_peer_ip_addresses("10-60-160-172.my-service.namespace.svc.abc.cluster.local"),
            "blocked-ip-address.my-service.namespace.svc.abc.cluster.local"
        );
        assert_eq!(
            quantize_peer_ip_addresses("ip-10-152-4-129.ec2.internal"),
            "ip-blocked-ip-address.ec2.internal"
        );
        assert_eq!(quantize_peer_ip_addresses("1-foo"), "1-foo");
        assert_eq!(quantize_peer_ip_addresses("1-2-foo"), "1-2-foo");
        assert_eq!(quantize_peer_ip_addresses("1-2-3-foo"), "1-2-3-foo");
        assert_eq!(quantize_peer_ip_addresses("1-2-3-999"), "1-2-3-999");
        assert_eq!(quantize_peer_ip_addresses("1-2-999-foo"), "1-2-999-foo");
        assert_eq!(quantize_peer_ip_addresses("1-2-3-999-foo"), "1-2-3-999-foo");
        assert_eq!(
            quantize_peer_ip_addresses("1-2-3-4-foo"),
            "blocked-ip-address-foo"
        );
        assert_eq!(
            quantize_peer_ip_addresses("7-55-2-app.agent.datadoghq.com"),
            "7-55-2-app.agent.datadoghq.com"
        );
    }
}
