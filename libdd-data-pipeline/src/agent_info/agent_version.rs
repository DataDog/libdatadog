// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Checks on the agent version reported by `/info`.

use std::cmp::Ordering;

/// The first agent whose stats for V1 payloads keep `lang`: the fix (datadog-agent #50485) was
/// backported to 7.79.x only, never to 7.77.x or 7.78.x.
const V1_STATS_LANG_FIXED_IN: Version<'static> = Version {
    core: ["7", "79", "0"],
    pre: Some("rc.6"),
};

/// Whether to send V1 traces to an agent reporting `version` in `/info`, or `None` if `version`
/// isn't `MAJOR.MINOR.PATCH[-PRE][+BUILD]` semver (e.g. a serverless agent's scheme).
///
/// Agents before 7.79.0-rc.6 drop `lang` from the stats they compute for V1 payloads, which breaks
/// Service Remapping, and V1 is opt-in before 7.77 anyway. The pre-release cut mirrors
/// dd-trace-go's `agentOmitsLangInV1Stats` (`ddtrace/tracer/agent_version.go`).
pub fn agent_supports_v1_traces(version: &str) -> Option<bool> {
    Version::parse(version).map(|v| v.cmp_precedence(&V1_STATS_LANG_FIXED_IN).is_ge())
}

/// The parts of a semver version which determine its precedence (build metadata doesn't).
struct Version<'a> {
    /// Major, minor and patch: digits without leading zeros.
    core: [&'a str; 3],
    /// Pre-release identifiers, after the `-`.
    pre: Option<&'a str>,
}

impl<'a> Version<'a> {
    /// Parses `MAJOR.MINOR.PATCH[-PRE][+BUILD]` per semver 2.0.0.
    fn parse(v: &'a str) -> Option<Self> {
        let (v, build) = match v.split_once('+') {
            Some((v, build)) => (v, Some(build)),
            None => (v, None),
        };
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        let mut parts = core.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let core = [major, minor, patch];
        let ident =
            |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let valid = core.iter().all(|n| is_number(n))
            && pre.is_none_or(|pre| {
                pre.split('.')
                    .all(|id| ident(id) && (!is_numeric(id) || is_number(id)))
            })
            && build.is_none_or(|build| build.split('.').all(ident));
        valid.then_some(Version { core, pre })
    }

    /// Semver precedence.
    fn cmp_precedence(&self, other: &Version<'_>) -> Ordering {
        // Numbers have no leading zeros, so they compare by length first.
        self.core
            .map(|n| (n.len(), n))
            .cmp(&other.core.map(|n| (n.len(), n)))
            .then_with(|| match (self.pre, other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a
                    .split('.')
                    .map(PreId::new)
                    .cmp(b.split('.').map(PreId::new)),
            })
    }
}

/// A pre-release identifier, in precedence order: numeric ones compare as numbers (they have no
/// leading zeros, so by length first) and precede alphanumeric ones, which compare as ASCII.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum PreId<'a> {
    Numeric(usize, &'a str),
    Alphanumeric(&'a str),
}

impl<'a> PreId<'a> {
    fn new(id: &'a str) -> Self {
        if is_numeric(id) {
            PreId::Numeric(id.len(), id)
        } else {
            PreId::Alphanumeric(id)
        }
    }
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Digits without leading zeros.
fn is_number(s: &str) -> bool {
    is_numeric(s) && (s == "0" || !s.starts_with('0'))
}

#[cfg(test)]
mod tests {
    use super::agent_supports_v1_traces;

    #[test]
    fn agents_before_the_v1_stats_fix() {
        for version in [
            "7.73.0",
            "7.76.2",
            "7.77.0",
            "7.78.9",
            "7.78.0-devel+git.42.abcdef1",
            "7.79.0-rc.5",
            "7.79.0-rc.5+build.1",
            "7.79.0-rc",
            "7.79.0-beta.10",
            "7.77.99999999999999999999",
            "6.0.0",
            "1.2.3",
        ] {
            assert_eq!(
                agent_supports_v1_traces(version),
                Some(false),
                "{version:?}"
            );
        }
    }

    #[test]
    fn agents_with_the_v1_stats_fix() {
        for version in [
            "7.79.0-rc.6",
            "7.79.0-rc.10",
            "7.79.0-rc.6+build.1",
            "7.79.0",
            "7.79.1-rc.1",
            "7.80.1",
            "7.100.0",
            "8.0.0",
        ] {
            assert_eq!(agent_supports_v1_traces(version), Some(true), "{version:?}");
        }
    }

    #[test]
    fn unparseable_versions() {
        for version in [
            "",
            "abc",
            "dev",
            "datadogexporter-otelcol-0.155.0",
            "1.2",
            "7",
            "v7.77.0",
            "V7.77.0",
            "7.77.0.1",
            "7.77.",
            "08.77.0",
            "7.077.0",
            "7.77.0-rc.01",
            "7.77-rc.1",
            "7.77.0-",
            "7.77.0+",
            "7.77.0-rc..1",
            "7.77.0-rc_1",
            "7.77.0-rc.1+",
            "7.77.0 ",
            "7.77.0-rc\u{e9}",
        ] {
            assert_eq!(agent_supports_v1_traces(version), None, "{version:?}");
        }
    }
}
