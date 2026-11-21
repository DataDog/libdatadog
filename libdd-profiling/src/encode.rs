// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lowering a [`Profile`](crate::profile::Profile) to pprof or OTLP.
//!
//! Both encoders are always compiled; which one runs is the caller's choice
//! at runtime, and a caller may run both on the same profile. Each emits
//! only the strings, functions and frames its samples use, renumbered
//! densely, so a long-lived dictionary does not bloat any one profile.

#[cfg(any(feature = "pprof", feature = "otel"))]
use crate::collections::alloc::Global;
#[cfg(any(feature = "pprof", feature = "otel"))]
use crate::collections::index_set::IndexSet;
use core::fmt;

#[cfg(feature = "otel")]
pub mod otlp;
#[cfg(feature = "pprof")]
pub mod pprof;
#[cfg(any(feature = "pprof", feature = "otel"))]
mod proto;

/// Why encoding failed. The profile is unchanged.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    OutOfMemory,
    /// The encoded profile would exceed [`EncodeOptions::max_bytes`].
    TooLarge,
    /// There is nothing to encode for this format, such as no OTLP profile
    /// requested.
    NothingToEncode,
    /// An encode option does not fit the profile, such as an OTLP profile
    /// whose source sample type the profile does not have.
    InvalidArgument,
    /// The compressor reported an error.
    Compression,
    /// The requested compression was not built in (`zstd` or, for gzip, `otel`).
    CompressionUnavailable,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfMemory => "out of memory",
            Self::TooLarge => "encoded profile exceeds the size limit",
            Self::NothingToEncode => "no sample type for this format",
            Self::InvalidArgument => "invalid argument",
            Self::Compression => "compression failed",
            Self::CompressionUnavailable => "compression not built in",
        })
    }
}

impl core::error::Error for EncodeError {}

/// A sampling period as a format records it: what is counted between
/// samples (such as `wall-time` in `nanoseconds`), and how much. The caller
/// chooses it; it does not affect any value.
#[cfg(any(feature = "pprof", feature = "otel"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Period<'a> {
    pub name: &'a str,
    pub unit: &'a str,
    pub value: i64,
}

/// Encoding limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodeOptions {
    /// The largest encoded (uncompressed) profile, in bytes.
    pub max_bytes: usize,
}

impl EncodeOptions {
    /// 20 MiB: the OTLP default.
    pub const DEFAULT_MAX_BYTES: usize = 20 << 20;
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
        }
    }
}

/// What produced the profile: known fields, mapped to each format's names
/// (Datadog tags for pprof, semantic-convention attributes for OTLP). Empty
/// strings and a zero process id are omitted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Resource<'a> {
    pub service: &'a str,
    pub env: &'a str,
    pub version: &'a str,
    pub host_name: &'a str,
    pub runtime_id: &'a str,
    pub language: &'a str,
    pub runtime_name: &'a str,
    pub runtime_version: &'a str,
    /// OTLP only (`process.runtime.description`): a longer description of
    /// the runtime, such as Python's `sys.version`.
    pub runtime_description: &'a str,
    pub profiler_version: &'a str,
    pub container_id: &'a str,
    /// OTLP only (`datadog.container.cgroup_inode`, an int): the inode of the
    /// process's cgroup, for when the container id cannot be read (cgroup v2
    /// with a private cgroup namespace). An OpenTelemetry collector such as
    /// DDOT uses it, with `process.pid`, to find the container. 0 is none.
    pub cgroup_inode: u64,
    pub process_id: u32,
    /// Further tags, passed through as-is.
    pub tags: &'a [(&'a str, &'a str)],
}

#[cfg(any(feature = "pprof", feature = "otel"))]
impl<'a> Resource<'a> {
    /// The known fields as (Datadog tag, OTLP attribute, value).
    pub(crate) const fn fields(&self) -> [(&'static str, &'static str, &'a str); 10] {
        [
            ("service", "service.name", self.service),
            ("env", "deployment.environment.name", self.env),
            ("version", "service.version", self.version),
            ("host", "host.name", self.host_name),
            ("runtime-id", "service.instance.id", self.runtime_id),
            ("language", "telemetry.sdk.language", self.language),
            ("runtime", "process.runtime.name", self.runtime_name),
            (
                "runtime_version",
                "process.runtime.version",
                self.runtime_version,
            ),
            (
                "profiler_version",
                "telemetry.sdk.version",
                self.profiler_version,
            ),
            ("container_id", "container.id", self.container_id),
        ]
    }
}

#[cfg(any(feature = "pprof", feature = "otel"))]
/// A string table built while encoding: index 0 is `""`, and each string
/// gets the next index the first time it is used.
pub(crate) struct StringTable<'p> {
    strings: IndexSet<&'p str, Global>,
}

#[cfg(any(feature = "pprof", feature = "otel"))]
impl<'p> StringTable<'p> {
    pub fn new() -> Result<Self, EncodeError> {
        let mut strings = IndexSet::new_in(Global);
        strings
            .try_insert("")
            .map_err(|_| EncodeError::OutOfMemory)?;
        Ok(Self { strings })
    }

    /// The index of `s`, adding it if new.
    pub fn index(&mut self, s: &'p str) -> Result<u64, EncodeError> {
        let index = match self.strings.get_index_of(&s) {
            Some(index) => index,
            None => {
                self.strings
                    .try_insert(s)
                    .map_err(|_| EncodeError::OutOfMemory)?
                    .0
            }
        };
        Ok(index as u64)
    }

    pub fn iter(&self) -> impl Iterator<Item = &'p str> + '_ {
        self.strings.iter().copied()
    }
}

#[cfg(any(feature = "pprof", feature = "otel"))]
/// A dense numbering of `T`s in first-use order, starting at `first`.
pub(crate) struct Numbering<T> {
    seen: IndexSet<T, Global>,
}

#[cfg(any(feature = "pprof", feature = "otel"))]
impl<T: Eq + core::hash::Hash> Numbering<T> {
    pub fn new() -> Self {
        Self {
            seen: IndexSet::new_in(Global),
        }
    }

    /// The index of `value` (from 0), adding it if new.
    pub fn index(&mut self, value: T) -> Result<u64, EncodeError> {
        self.seen
            .try_insert(value)
            .map(|(index, _)| index as u64)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    /// The value with `index`; only OTLP looks values up by index.
    #[cfg(feature = "otel")]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.seen.get_index(index)
    }

    /// The values in index order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> + '_ {
        self.seen.iter()
    }
}
