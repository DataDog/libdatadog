// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::Endpoint;
use std::fmt;
use std::hash::{Hash, Hasher};
use url::Url;

const DEFAULT_SITE: &str = "datadoghq.com";
const AGENTLESS_HOST_PREFIX: &str = "ufc-server.ff-cdn.";
const AGENTLESS_CONFIGURATION_PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";

/// A validated agentless endpoint. Only managed endpoints can own an API key,
/// ensuring a custom endpoint cannot accidentally receive the Datadog credential.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "SerializedEndpoint", into = "SerializedEndpoint")]
pub struct CdnEndpoint {
    // Keep construction and Debug private to the CDN boundary: the common
    // endpoint permits arbitrary credentials and prints its URL in Debug.
    endpoint: Endpoint,
}

impl PartialEq for CdnEndpoint {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for CdnEndpoint {}

impl Hash for CdnEndpoint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

// Preserve the existing subscription wire format. Request timing remains in
// CdnConfig; generic endpoint defaults are not independent CDN settings.
#[derive(serde::Serialize, serde::Deserialize)]
struct SerializedEndpoint {
    url: String,
    api_key: Option<String>,
}

impl TryFrom<SerializedEndpoint> for CdnEndpoint {
    type Error = EndpointError;

    fn try_from(value: SerializedEndpoint) -> Result<Self, Self::Error> {
        Self::from_parts(&value.url, value.api_key)
    }
}

impl From<CdnEndpoint> for SerializedEndpoint {
    fn from(value: CdnEndpoint) -> Self {
        Self {
            url: value.endpoint.url.to_string(),
            api_key: value.endpoint.api_key.map(|key| key.into_owned()),
        }
    }
}

impl CdnEndpoint {
    // http::Uri compares the entire authority case-insensitively, including
    // userinfo. Preserve case-sensitive credentials in subscription identity.
    fn identity(&self) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
        (
            self.uri().scheme_str(),
            self.uri().authority().map(|authority| authority.as_str()),
            self.uri().path_and_query().map(|path| path.as_str()),
            self.api_key(),
        )
    }

    fn from_parts(url: &str, api_key: Option<String>) -> Result<Self, EndpointError> {
        if let Some(key) = &api_key {
            if key.is_empty() {
                return Err(EndpointError::MissingApiKey);
            }
            http::HeaderValue::from_str(key).map_err(|_| EndpointError::InvalidApiKey)?;
        }
        Ok(Self {
            endpoint: Endpoint {
                url: Self::parse_uri(url)?,
                api_key: api_key.map(Into::into),
                ..Endpoint::default()
            },
        })
    }

    fn parse_uri(raw: &str) -> Result<http::Uri, EndpointError> {
        if let Ok(uri) = raw.parse() {
            return Ok(uri);
        }

        // URL userinfo may contain characters (such as Unicode or '[') that
        // http::Uri rejects. The previous transport encoded them with Url
        // before removing userinfo and constructing the HTTP request.
        let parsed = Url::parse(raw).map_err(|_| EndpointError::InvalidUrl)?;
        if parsed.username().is_empty() && parsed.password().is_none() {
            return Err(EndpointError::InvalidUrl);
        }
        let (_, authority_and_path) = raw.split_once("://").ok_or(EndpointError::InvalidUrl)?;
        let path_start = authority_and_path
            .find(['/', '?', '#'])
            .unwrap_or(authority_and_path.len());
        let authority = &parsed[url::Position::BeforeUsername..url::Position::AfterPort];
        let encoded = format!(
            "{}://{}{}",
            parsed.scheme(),
            authority,
            &authority_and_path[path_start..]
        );
        // Keep an already valid path/query byte-for-byte. If it also needs
        // encoding (for example Unicode), retain the previous Url behavior.
        encoded
            .parse()
            .or_else(|_| parsed.as_str().parse())
            .map_err(|_| EndpointError::InvalidUrl)
    }

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

    pub fn uri(&self) -> &http::Uri {
        &self.endpoint.url
    }

    pub fn is_managed(&self) -> bool {
        self.endpoint.api_key.is_some()
    }

    pub fn api_key(&self) -> Option<&str> {
        self.endpoint.api_key.as_deref()
    }

    pub(super) fn is_valid(&self) -> bool {
        let endpoint_url = self.endpoint.url.to_string();
        let Ok(url) = Url::parse(&endpoint_url) else {
            return false;
        };
        match self.api_key() {
            None => Self::custom(&endpoint_url).is_ok_and(|endpoint| endpoint == *self),
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

        Self::from_parts(url.as_str(), Some(api_key.to_owned()))
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
            // Preserve custom paths and query strings, including their percent
            // encoding. http::Uri normalizes only the HTTP(S) scheme's case.
            base_url.to_owned()
        };

        Self::from_parts(&url, None)
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
    InvalidApiKey,
    InvalidUrl,
    UnsupportedScheme,
    UrlContainsWhitespace,
    InvalidSite,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingApiKey => "no API key is configured for the managed endpoint",
            Self::InvalidApiKey => "the configured API key is not a valid HTTP header value",
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
    let site = site.trim();
    let site = if site.is_empty() { DEFAULT_SITE } else { site };
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
    fn url_credentials_are_case_sensitive_subscription_identity() {
        let endpoints: std::collections::HashSet<_> = [
            "https://user:Secret@localhost/config",
            "https://user:secret@localhost/config",
            "https://User:secret@localhost/config",
        ]
        .into_iter()
        .map(|url| CdnEndpoint::custom(url).unwrap())
        .collect();
        assert_eq!(
            endpoints.len(),
            3,
            "different credentials must not share a fetcher"
        );
        assert!(
            endpoints
                .contains(&CdnEndpoint::custom("https://user:Secret@localhost/config").unwrap())
        );
    }

    #[test]
    fn shared_endpoint_preserves_the_subscription_wire_contract() {
        // The previous endpoint was serialized as these two fields. Keep
        // compatibility with callers and sidecars built before this refactor.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct PreviousEndpoint {
            url: String,
            api_key: Option<String>,
        }

        for endpoint in [
            CdnEndpoint::managed("datadoghq.com", "production", "secret-key").unwrap(),
            CdnEndpoint::custom("https://user:password@[::1]:8443/Config%2fraw?x=a%2Fb&x=c+d")
                .unwrap(),
        ] {
            let previous = PreviousEndpoint {
                url: endpoint.uri().to_string(),
                api_key: endpoint.api_key().map(str::to_owned),
            };
            let previous_bytes = bincode::serialize(&previous).unwrap();
            let current_bytes = bincode::serialize(&endpoint).unwrap();
            assert_eq!(current_bytes, previous_bytes);
            let decoded: CdnEndpoint = bincode::deserialize(&previous_bytes).unwrap();
            assert_eq!(decoded, endpoint);
            assert!(decoded.is_valid());
            let old_reader: PreviousEndpoint = bincode::deserialize(&current_bytes).unwrap();
            assert_eq!(old_reader.url, previous.url);
            assert_eq!(old_reader.api_key, previous.api_key);
        }
    }

    #[test]
    fn invalid_serialized_endpoint_does_not_disclose_credentials() {
        let wire = serde_json::json!({
            "url": "https://private-credential@invalid host/config",
            "api_key": null,
        });
        let error = serde_json::from_value::<CdnEndpoint>(wire).unwrap_err();
        assert!(!error.to_string().contains("private-credential"));
    }

    #[test]
    fn managed_endpoint_uses_normalized_site_and_encoded_environment() {
        let endpoint = CdnEndpoint::build("", " DATADOGHQ.EU \t", "prod / blue", "key")
            .expect("managed endpoint");

        assert!(endpoint.is_managed());
        assert_eq!(endpoint.api_key(), Some("key"));
        assert_eq!(
            endpoint.uri().to_string(),
            "https://ufc-server.ff-cdn.datadoghq.eu/api/v2/feature-flagging/config/rules-based/server?dd_env=prod+%2F+blue"
        );
    }

    #[test]
    fn managed_endpoint_omits_an_absent_environment_and_defaults_blank_site() {
        let endpoint = CdnEndpoint::build("", "  ", "", "key").expect("managed endpoint");
        assert_eq!(
            endpoint.uri().to_string(),
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
    fn managed_endpoint_rejects_invalid_api_key_headers_without_disclosing_them() {
        for key in ["secret\r", "secret\n", "secret\0", "secret\u{7f}"] {
            let error = CdnEndpoint::managed("datadoghq.com", "", key).unwrap_err();
            assert_eq!(error, EndpointError::InvalidApiKey);
            assert!(!error.to_string().contains("secret"));
            assert!(!format!("{error:?}").contains("secret"));
            let wire = serde_json::json!({
                "url": "https://ufc-server.ff-cdn.datadoghq.com/api/v2/feature-flagging/config/rules-based/server",
                "api_key": key,
            });
            let error = serde_json::from_value::<CdnEndpoint>(wire).unwrap_err();
            assert!(!error.to_string().contains("secret"));
            // Custom endpoints ignore the Datadog credential entirely.
            assert!(CdnEndpoint::build("http://localhost/config", "", "", key).is_ok());
        }
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
                endpoint.uri().to_string(),
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
            endpoint.uri().to_string(),
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
            endpoint.uri().to_string(),
            "https://LOCALHOST/custom/path?tenant=a%2Fb"
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
