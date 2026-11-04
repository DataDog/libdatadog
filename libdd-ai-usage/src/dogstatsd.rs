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
/// line longer than `max_payload` with its tags is dropped: the Agent
/// refuses a datagram that long ([`DOGSTATSD_UDP_MAX_PAYLOAD`],
/// [`DOGSTATSD_UDS_MAX_PAYLOAD`]). [`pack_dogstatsd_counted`] also counts
/// them. Empty lines are skipped.
pub fn pack_dogstatsd<L: AsRef<str>, S: AsRef<str>>(
    lines: &[L],
    constant_tags: &[S],
    max_payload: usize,
) -> Vec<String> {
    pack_dogstatsd_counted(lines, constant_tags, max_payload).0
}

/// [`pack_dogstatsd`], also returning the number of lines dropped for being
/// longer than `max_payload`.
pub fn pack_dogstatsd_counted<L: AsRef<str>, S: AsRef<str>>(
    lines: &[L],
    constant_tags: &[S],
    max_payload: usize,
) -> (Vec<String>, usize) {
    let mut datagrams = Vec::new();
    let mut packet = String::new();
    let mut dropped = 0;
    for line in lines.iter().map(AsRef::as_ref).filter(|l| !l.is_empty()) {
        let line = with_constant_tags(line, constant_tags);
        if line.len() > max_payload {
            dropped += 1;
            continue;
        }
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
    (datagrams, dropped)
}

/// [`pack_dogstatsd_counted`] written into one text: the datagrams in
/// order, each separated from the next by `separator` (a NUL byte for a
/// caller that splits them itself), with no line or datagram copied on its
/// own. Returns the text and the number of lines dropped for being longer
/// than `max_payload` with their tags.
pub fn pack_dogstatsd_joined<'a, S: AsRef<str>>(
    lines: impl IntoIterator<Item = &'a str>,
    constant_tags: &[S],
    max_payload: usize,
    separator: char,
) -> (String, usize) {
    let tags: Vec<&str> = constant_tags
        .iter()
        .map(AsRef::as_ref)
        .filter(|tag| !tag.is_empty())
        .collect();
    // The tags joined by commas, and its length with its leading `|#` or `,`.
    let joined = tags.join(",");
    let suffix_len = if tags.is_empty() { 0 } else { joined.len() + 1 };
    let mut out = String::new();
    let mut datagram = 0;
    let mut dropped = 0;
    for line in lines.into_iter().filter(|line| !line.is_empty()) {
        let tagged = line.contains("|#");
        let len = line.len() + suffix_len + usize::from(suffix_len > 0 && !tagged);
        if len > max_payload {
            dropped += 1;
            continue;
        }
        if datagram > 0 && datagram + 1 + len > max_payload {
            out.push(separator);
            datagram = 0;
        } else if datagram > 0 {
            out.push('\n');
            datagram += 1;
        }
        out.push_str(line);
        if suffix_len > 0 {
            out.push_str(if tagged { "," } else { "|#" });
            out.push_str(&joined);
        }
        datagram += len;
    }
    (out, dropped)
}

impl MetricBatch {
    /// The batch as DogStatsD datagrams: [`MetricBatch::dogstatsd_lines`]
    /// packed by [`pack_dogstatsd`].
    pub fn dogstatsd_datagrams<S: AsRef<str>>(
        &self,
        constant_tags: &[S],
        max_payload: usize,
    ) -> Result<Vec<String>, MetricError> {
        Ok(self
            .dogstatsd_datagrams_counted(constant_tags, max_payload)?
            .0)
    }

    /// [`MetricBatch::dogstatsd_datagrams`], also returning the number of
    /// lines dropped for being longer than `max_payload`.
    pub fn dogstatsd_datagrams_counted<S: AsRef<str>>(
        &self,
        constant_tags: &[S],
        max_payload: usize,
    ) -> Result<(Vec<String>, usize), MetricError> {
        Ok(pack_dogstatsd_counted(
            &self.dogstatsd_lines()?,
            constant_tags,
            max_payload,
        ))
    }
}
