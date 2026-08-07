// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regex capability trait.
//!
//! Callers compile a pattern once into an opaque `Handle` and reuse the handle
//! for every match. Match results are byte offsets into the caller-owned
//! haystack, so replacement logic (e.g. `regex::Replacer` expansion) can live
//! entirely in caller code and stay independent of the backing engine.

use crate::MaybeSend;

/// A single non-overlapping match, expressed as **UTF-8 byte offsets** into the
/// haystack the caller passed in.
///
/// Both impls are required to return byte offsets so callers can safely
/// `&haystack[m.start..m.end]` regardless of the backing engine. The native
/// impl gets this for free from `regex`; the wasm impl translates JS's UTF-16
/// code-unit indices to UTF-8 bytes before returning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

/// Captures for a single match. `groups[0]` is always the full match;
/// `groups[i]` for `i > 0` is the i-th capture group, or `None` if the group
/// did not participate in this match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captures {
    pub groups: Vec<Option<Match>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegexError {
    #[error("invalid regex pattern `{pattern}`: {message}")]
    InvalidPattern { pattern: String, message: String },
}

pub trait RegexCapability {
    /// Opaque compiled-pattern handle.
    type Handle: Clone + std::fmt::Debug + MaybeSend + Sync;

    fn compile(pattern: &str) -> Result<Self::Handle, RegexError>;

    fn is_match(handle: &Self::Handle, haystack: &str) -> bool;

    fn find(handle: &Self::Handle, haystack: &str) -> Option<Match>;

    /// All non-overlapping matches, batched.
    fn find_all(handle: &Self::Handle, haystack: &str) -> Vec<Match>;

    fn captures(handle: &Self::Handle, haystack: &str) -> Option<Captures>;

    /// All non-overlapping matches with their capture groups, batched.
    fn captures_all(handle: &Self::Handle, haystack: &str) -> Vec<Captures>;

    /// The source pattern the handle was compiled from.
    fn pattern(handle: &Self::Handle) -> &str;
}
