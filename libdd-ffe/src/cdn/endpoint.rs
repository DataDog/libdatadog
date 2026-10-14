// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use url::Url;

const DEFAULT_SITE: &str = "datadoghq.com";
const AGENTLESS_HOST_PREFIX: &str = "ufc-server.ff-cdn.";
const AGENTLESS_CONFIGURATION_PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";

/// A validated agentless endpoint. Only managed endpoints can own an API key,
/// ensuring a custom endpoint cannot accidentally receive the Datadog credential.
#[derive(Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdnEndpoint {
    url: String,
    api_key: Option<String>,
}

impl CdnEndpoint {
    pub fn build(
        base_url: &str,
        site: &str,
        environment: &str,
        api_key: &str,
    ) -> Result<Self, EndpointError> {
        if base_url.trim().is_empty() {
            Self::managed(site, environment, api_key)
        } else {
            Self::custom(base_url)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.url
    }

    pub fn is_managed(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub(super) fn is_valid(&self) -> bool {
        let Ok(url) = Url::parse(&self.url) else {
            return false;
        };
        match self.api_key() {
            None => Self::custom(&self.url).is_ok(),
            Some(key) => {
                let Some(site) = url
                    .host_str()
                    .and_then(|host| host.strip_prefix(AGENTLESS_HOST_PREFIX))
                else {
                    return false;
                };
                let environment = url
                    .query_pairs()
                    .find(|(name, _)| name == "dd_env")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_default();
                Self::managed(site, &environment, key).is_ok_and(|endpoint| endpoint == *self)
            }
        }
    }

    pub fn managed(site: &str, environment: &str, api_key: &str) -> Result<Self, EndpointError> {
        if api_key.is_empty() {
            return Err(EndpointError::MissingApiKey);
        }

        let normalized_site = normalize_site(site)?;
        let expected_host = format!("{AGENTLESS_HOST_PREFIX}{normalized_site}");
        let mut url = Url::parse(&format!(
            "https://{expected_host}{AGENTLESS_CONFIGURATION_PATH}"
        ))
        .map_err(|_| EndpointError::InvalidSite)?;

        // Compare the parsed host before this endpoint is allowed to carry
        // managed authentication. This is defense in depth after rejecting
        // URL-significant site characters in `normalize_site`.
        if url.host_str() != Some(expected_host.as_str()) {
            return Err(EndpointError::InvalidSite);
        }

        if !environment.is_empty() {
            url.query_pairs_mut().append_pair("dd_env", environment);
        }

        Ok(Self {
            url: url.to_string(),
            api_key: Some(api_key.to_owned()),
        })
    }

    pub fn custom(base_url: &str) -> Result<Self, EndpointError> {
        if base_url.chars().any(char::is_whitespace) {
            return Err(EndpointError::UrlContainsWhitespace);
        }

        let mut url = Url::parse(base_url).map_err(|_| EndpointError::InvalidUrl)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(EndpointError::UnsupportedScheme);
        }
        if url.host_str().is_none() || url.fragment().is_some() {
            return Err(EndpointError::InvalidUrl);
        }

        let url = if matches!(url.path(), "" | "/") {
            url.set_path(AGENTLESS_CONFIGURATION_PATH);
            url.to_string()
        } else {
            // A custom non-root URL is opaque and must be requested exactly as
            // the operator supplied it, including its query string.
            base_url.to_owned()
        };

        Ok(Self { url, api_key: None })
    }
}

impl fmt::Debug for CdnEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CdnEndpoint")
            .field("url", &"[REDACTED]")
            .field("managed", &self.is_managed())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointError {
    MissingApiKey,
    InvalidUrl,
    UnsupportedScheme,
    UrlContainsWhitespace,
    InvalidSite,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingApiKey => "no API key is configured for the managed endpoint",
            Self::InvalidUrl => "the configured URL is not a valid absolute URL",
            Self::UnsupportedScheme => "the configured URL must use HTTP or HTTPS",
            Self::UrlContainsWhitespace => "the configured URL contains whitespace",
            Self::InvalidSite => "the configured site is not a valid host",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for EndpointError {}

fn normalize_site(site: &str) -> Result<String, EndpointError> {
    let site = if site.trim().is_empty() {
        DEFAULT_SITE
    } else {
        site
    };
    if !site.is_ascii()
        || site.chars().any(|character| {
            character.is_whitespace() || matches!(character, '/' | '\\' | '?' | '#' | '@' | ':')
        })
    {
        return Err(EndpointError::InvalidSite);
    }
    Ok(site.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_endpoint_uses_normalized_site_and_encoded_environment() {
        let endpoint =
            CdnEndpoint::build("", "DATADOGHQ.EU", "prod / blue", "key").expect("managed endpoint");

        assert!(endpoint.is_managed());
        assert_eq!(endpoint.api_key(), Some("key"));
        assert_eq!(
            endpoint.as_str(),
            "https://ufc-server.ff-cdn.datadoghq.eu/api/v2/feature-flagging/config/rules-based/server?dd_env=prod+%2F+blue"
        );
    }

    #[test]
    fn managed_endpoint_omits_an_absent_environment_and_defaults_blank_site() {
        let endpoint = CdnEndpoint::build("", "  ", "", "key").expect("managed endpoint");
        assert_eq!(
            endpoint.as_str(),
            "https://ufc-server.ff-cdn.datadoghq.com/api/v2/feature-flagging/config/rules-based/server"
        );
    }

    #[test]
    fn managed_endpoint_requires_an_api_key() {
        assert_eq!(
            CdnEndpoint::build("", "datadoghq.com", "", "").unwrap_err(),
            EndpointError::MissingApiKey
        );
    }

    #[test]
    fn managed_endpoint_rejects_sites_that_can_change_the_url_meaning() {
        for site in [
            "https://datadoghq.com",
            "datadoghq.com@attacker.example",
            "datadoghq.com/path",
            "datadoghq.com?query",
            "datadoghq.com#fragment",
            "datadoghq.com:443",
            " datadoghq.com",
            "data doghq.com",
            "dâtadoghq.com",
        ] {
            assert_eq!(
                CdnEndpoint::build("", site, "", "key").unwrap_err(),
                EndpointError::InvalidSite,
                "site should be rejected"
            );
        }
    }

    #[test]
    fn custom_root_endpoint_gets_the_canonical_path_and_keeps_its_query() {
        for base_url in ["http://localhost:8080", "http://localhost:8080/"] {
            let endpoint = CdnEndpoint::build(base_url, "ignored", "ignored", "secret")
                .expect("custom endpoint");
            assert!(!endpoint.is_managed());
            assert_eq!(
                endpoint.as_str(),
                "http://localhost:8080/api/v2/feature-flagging/config/rules-based/server"
            );
        }

        let endpoint = CdnEndpoint::build(
            "https://user:pass@localhost/?tenant=a%2Fb",
            "ignored",
            "ignored",
            "secret",
        )
        .expect("custom endpoint");
        assert_eq!(
            endpoint.as_str(),
            "https://user:pass@localhost/api/v2/feature-flagging/config/rules-based/server?tenant=a%2Fb"
        );
    }

    #[test]
    fn custom_non_root_endpoint_is_preserved_without_environment() {
        let endpoint = CdnEndpoint::build(
            "HTTPS://LOCALHOST/custom/path?tenant=a%2Fb",
            "ignored",
            "must-not-be-added",
            "must-not-be-sent",
        )
        .expect("custom endpoint");
        assert_eq!(
            endpoint.as_str(),
            "HTTPS://LOCALHOST/custom/path?tenant=a%2Fb"
        );
        assert!(!endpoint.is_managed());
        assert_eq!(endpoint.api_key(), None);
    }

    #[test]
    fn custom_endpoint_validation_errors_never_contain_the_url() {
        let credential = "credential-that-must-not-leak";
        for (url, expected) in [
            (
                format!("ftp://{credential}@localhost/path"),
                EndpointError::UnsupportedScheme,
            ),
            (
                format!("https://{credential}@local host/path"),
                EndpointError::UrlContainsWhitespace,
            ),
            (format!("not-a-url-{credential}"), EndpointError::InvalidUrl),
        ] {
            let error = CdnEndpoint::build(&url, "", "", "key").unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains(credential));
        }
    }

    #[test]
    fn endpoint_debug_output_is_redacted() {
        let endpoint = CdnEndpoint::build("https://user:password@localhost/custom", "", "", "key")
            .expect("custom endpoint");
        let debug = format!("{endpoint:?}");
        assert!(!debug.contains("user"));
        assert!(!debug.contains("password"));
        assert!(debug.contains("[REDACTED]"));
    }
}
