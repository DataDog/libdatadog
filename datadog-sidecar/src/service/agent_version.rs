// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Checks on the agent version reported by `/info`.

use semver::Version;

/// The first agent whose stats for V1 payloads keep `lang`: the fix (datadog-agent #50485) was
/// backported to 7.79.x only, never to 7.77.x or 7.78.x.
const V1_STATS_LANG_FIXED_IN: &str = "7.79.0-rc.6";

/// Whether to send V1 traces to an agent reporting `version` in `/info`, or `None` if `version`
/// isn't `MAJOR.MINOR.PATCH[-PRE][+BUILD]` semver (e.g. a serverless agent's scheme).
///
/// Agents before 7.79.0-rc.6 drop `lang` from the stats they compute for V1 payloads, which breaks
/// Service Remapping, and V1 is opt-in before 7.77 anyway. The pre-release cut mirrors
/// dd-trace-go's `agentOmitsLangInV1Stats` (`ddtrace/tracer/agent_version.go`).
pub(super) fn agent_supports_v1_traces(version: &str) -> Option<bool> {
    let fixed_in = Version::parse(V1_STATS_LANG_FIXED_IN).ok()?;
    Version::parse(version).ok().map(|v| v >= fixed_in)
}

#[cfg(all(test, not(miri)))]
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
            "7.77.99999999999999999999",
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
