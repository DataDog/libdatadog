// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;
use ipnet::IpNet;
use libdd_regex::{Regex, RegexBuilder};

/// Classification metadata attached to a matching string.
pub type ScannerTags = BTreeMap<String, String>;

/// A compiled scanner condition, reusable across requests.
#[derive(Clone, Debug)]
pub struct Matcher(Condition);

#[derive(Clone, Debug)]
enum Condition {
    Regex { regex: Regex, min_length: usize },
    Exact(Vec<String>),
    Ip(Vec<IpNet>),
}

/// Invalid scanner configuration.
#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    /// A regular expression could not be compiled.
    #[error("invalid scanner regular expression: {0}")]
    Regex(#[source] libdd_regex::Error),
    /// A scanner pattern could not be parsed.
    #[error("invalid scanner regular expression: {0}")]
    RegexSyntax(#[source] Box<regex_syntax::ast::Error>),
    /// A network is neither an IP address nor a CIDR.
    #[error("invalid scanner network: {0}")]
    Network(String),
    /// A scanner must have a key or value condition.
    #[error("scanner has no key or value matcher")]
    MissingMatcher,
}

impl Matcher {
    /// Compile a regex once, using libdatadog's shared regex engine.
    ///
    /// Minimum length counts bytes. Empty input never matches, even if the
    /// expression itself matches an empty string, matching libddwaf.
    ///
    /// # Errors
    /// Returns a compilation error for unsupported or invalid expressions.
    pub fn regex(
        pattern: &str,
        min_length: usize,
        case_sensitive: bool,
    ) -> Result<Self, ScannerError> {
        RegexBuilder::new(&crate::scanner_regex::normalize(pattern)?)
            .case_insensitive(!case_sensitive)
            .size_limit(512 * 1024)
            .build()
            .map(|regex| Self(Condition::Regex { regex, min_length }))
            .map_err(ScannerError::Regex)
    }

    /// Match one of the supplied strings exactly.
    #[must_use]
    pub fn exact(mut values: Vec<String>) -> Self {
        values.sort_unstable();
        values.dedup();
        Self(Condition::Exact(values))
    }

    /// Compile IP addresses or CIDR networks.
    ///
    /// # Errors
    /// Returns the first invalid network.
    pub fn ip(values: &[&str]) -> Result<Self, ScannerError> {
        values
            .iter()
            .map(|value| {
                value
                    .parse::<IpNet>()
                    .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
                    .map_err(|_| ScannerError::Network(String::from(*value)))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|networks| Self(Condition::Ip(networks)))
    }

    fn matches(&self, value: &str) -> bool {
        match &self.0 {
            Condition::Regex { regex, min_length } => {
                !value.is_empty() && value.len() >= *min_length && regex.is_match(value)
            }
            Condition::Exact(values) => values
                .binary_search_by(|candidate| candidate.as_str().cmp(value))
                .is_ok(),
            Condition::Ip(networks) => value
                .parse::<IpAddr>()
                .is_ok_and(|address| networks.iter().any(|network| network.contains(&address))),
        }
    }
}

/// A key/value classifier. Both configured conditions must match.
///
/// Scanners are compiled once and borrowed by extraction. With multiple
/// matching scanners, extraction uses the first in caller-supplied order.
#[derive(Clone, Debug)]
pub struct Scanner {
    id: String,
    tags: ScannerTags,
    key: Option<Matcher>,
    value: Option<Matcher>,
}

impl Scanner {
    /// Construct a scanner from compiled conditions.
    ///
    /// # Errors
    /// Returns an error if neither condition is configured.
    pub fn new(
        id: String,
        tags: ScannerTags,
        key: Option<Matcher>,
        value: Option<Matcher>,
    ) -> Result<Self, ScannerError> {
        if key.is_none() && value.is_none() {
            return Err(ScannerError::MissingMatcher);
        }
        Ok(Self {
            id,
            tags,
            key,
            value,
        })
    }

    /// The configuration identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Metadata emitted for a matching string.
    #[must_use]
    pub const fn tags(&self) -> &ScannerTags {
        &self.tags
    }

    /// Evaluate a key and an optional string value.
    ///
    /// A missing value or empty key fails only its configured condition.
    /// A key-only scanner can therefore match an invalid or non-string value
    /// when called directly; schema extraction classifies only strings.
    #[must_use]
    pub fn matches(&self, key: &str, value: Option<&str>) -> bool {
        self.matches_key(key) && self.matches_value(value)
    }

    pub(crate) fn matches_key(&self, key: &str) -> bool {
        self.key
            .as_ref()
            .is_none_or(|matcher| !key.is_empty() && matcher.matches(key))
    }

    pub(crate) const fn needs_value(&self) -> bool {
        self.value.is_some()
    }

    pub(crate) fn matches_value(&self, value: Option<&str>) -> bool {
        self.value
            .as_ref()
            .is_none_or(|matcher| value.is_some_and(|value| matcher.matches(value)))
    }
}
