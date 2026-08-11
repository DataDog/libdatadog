// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native regex capability, backed by `libdd_common::regex_engine`.
//!
//! Which underlying engine is used (full `regex` vs `regex_lite`) is decided
//! by the `regex-lite` / `require-regex-full` features on `libdd-common` — this
//! impl is agnostic.

use libdd_capabilities::regex::{Captures, Match, RegexCapability, RegexError};
use libdd_common::regex_engine::{Captures as EngineCaptures, Regex};

#[derive(Clone, Debug)]
pub struct NativeRegexCapability;

impl RegexCapability for NativeRegexCapability {
    type Handle = Regex;

    fn compile(pattern: &str) -> Result<Self::Handle, RegexError> {
        Regex::new(pattern).map_err(|e| RegexError::InvalidPattern {
            pattern: pattern.to_owned(),
            message: e.to_string(),
        })
    }

    fn is_match(handle: &Self::Handle, haystack: &str) -> bool {
        handle.is_match(haystack)
    }

    fn find(handle: &Self::Handle, haystack: &str) -> Option<Match> {
        handle.find(haystack).map(|m| Match {
            start: m.start(),
            end: m.end(),
        })
    }

    fn find_all(handle: &Self::Handle, haystack: &str) -> Vec<Match> {
        handle
            .find_iter(haystack)
            .map(|m| Match {
                start: m.start(),
                end: m.end(),
            })
            .collect()
    }

    fn captures(handle: &Self::Handle, haystack: &str) -> Option<Captures> {
        handle.captures(haystack).map(engine_captures_to_owned)
    }

    fn captures_all(handle: &Self::Handle, haystack: &str) -> Vec<Captures> {
        handle
            .captures_iter(haystack)
            .map(engine_captures_to_owned)
            .collect()
    }

    fn pattern(handle: &Self::Handle) -> &str {
        handle.as_str()
    }
}

fn engine_captures_to_owned(caps: EngineCaptures<'_>) -> Captures {
    Captures {
        groups: caps
            .iter()
            .map(|g| {
                g.map(|m| Match {
                    start: m.start(),
                    end: m.end(),
                })
            })
            .collect(),
    }
}
