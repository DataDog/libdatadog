// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Packing DogStatsD lines into datagrams for the Agent.

use crate::MetricBatch;
use crate::point::MetricError;

/// The largest DogStatsD datagram over UDP: the usual Ethernet MTU less the
/// IP and UDP headers, as Datadog's DogStatsD clients use, so that a datagram
/// crosses a network without IP fragmentation.
pub const DOGSTATSD_UDP_MAX_PAYLOAD: usize = 1432;

/// The largest DogStatsD datagram over a Unix domain socket, as Datadog's
/// DogStatsD clients use.
pub const DOGSTATSD_UDS_MAX_PAYLOAD: usize = 8192;

/// `line` with `constant_tags` (`key:value` strings, such as a client's
/// `env` or `version`) appended to its tags.
pub fn with_constant_tags<S: AsRef<str>>(line: &str, constant_tags: &[S]) -> String {
    let mut tags = constant_tags
        .iter()
        .map(AsRef::as_ref)
        .filter(|tag| !tag.is_empty());
    let Some(first) = tags.next() else {
        return line.to_string();
    };
    let mut out = String::with_capacity(line.len() + 64);
    out.push_str(line);
    out.push_str(if line.contains("|#") { "," } else { "|#" });
    out.push_str(first);
    for tag in tags {
        out.push(',');
        out.push_str(tag);
    }
    out
}

/// Join DogStatsD lines, with `constant_tags` appended to each, into
/// newline-separated datagrams of at most `max_payload` bytes, in order. A
/// line longer than `max_payload` is a datagram of its own, which the
/// transport may refuse. Empty lines are skipped.
pub fn pack_dogstatsd<L: AsRef<str>, S: AsRef<str>>(
    lines: &[L],
    constant_tags: &[S],
    max_payload: usize,
) -> Vec<String> {
    let mut datagrams = Vec::new();
    let mut packet = String::new();
    for line in lines.iter().map(AsRef::as_ref).filter(|l| !l.is_empty()) {
        let line = with_constant_tags(line, constant_tags);
        if !packet.is_empty() && packet.len() + 1 + line.len() > max_payload {
            datagrams.push(std::mem::take(&mut packet));
        }
        if !packet.is_empty() {
            packet.push('\n');
        }
        packet.push_str(&line);
    }
    if !packet.is_empty() {
        datagrams.push(packet);
    }
    datagrams
}

impl MetricBatch {
    /// The batch as DogStatsD datagrams: [`MetricBatch::dogstatsd_lines`]
    /// packed by [`pack_dogstatsd`].
    pub fn dogstatsd_datagrams<S: AsRef<str>>(
        &self,
        constant_tags: &[S],
        max_payload: usize,
    ) -> Result<Vec<String>, MetricError> {
        Ok(pack_dogstatsd(
            &self.dogstatsd_lines()?,
            constant_tags,
            max_payload,
        ))
    }
}
