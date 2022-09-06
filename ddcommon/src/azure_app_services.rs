// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use lazy_static::lazy_static;
use std::env;

const WEBSITE_ONWER_NAME: &str = "WEBSITE_OWNER_NAME";
const WEBSITE_SITE_NAME: &str = "WEBSITE_SITE_NAME";
const WEBSITE_RESOURCE_GROUP: &str = "WEBSITE_RESOURCE_GROUP";

macro_rules! get_trimmed_env_var {
    ($name:expr) => {
        env::var($name).ok().map(|v| v.trim().to_string())
    };
}

fn extract_subscription_id(s: Option<String>) -> Option<String> {
    s?.split('+')
        .next()
        .filter(|s| !s.trim().is_empty())
        .map(|v| v.to_string())
}

fn get_subscription_id() -> Option<String> {
    extract_subscription_id(get_trimmed_env_var!(WEBSITE_ONWER_NAME))
}

fn extract_site_name() -> Option<String> {
    get_trimmed_env_var!(WEBSITE_SITE_NAME)
}

fn extract_resource_group() -> Option<String> {
    get_trimmed_env_var!(WEBSITE_RESOURCE_GROUP)
}

/*
 * Computation of the resource id follow the same way the .NET tracer is doing:
 * https://github.com/DataDog/dd-trace-dotnet/blob/834a4b05b4ed91a819eb78761bf1ddb805969f65/tracer/src/Datadog.Trace/PlatformHelpers/AzureAppServices.cs#L215
 */
fn build_resource_id(
    subscription_id: Option<String>,
    site_name: Option<String>,
    resource_group: Option<String>,
) -> Option<String> {
    match (subscription_id, site_name, resource_group) {
        (Some(id_sub), Some(sitename), Some(res_grp)) => Some(
            format!(
                "/subscriptions/{}/resourcegroups/{}/providers/microsoft.web/sites/{}",
                id_sub, res_grp, sitename
            )
            .to_lowercase(),
        ),
        _ => None,
    }
}

pub fn get_resource_id() -> Option<&'static str> {
    lazy_static! {
        static ref AAS_RESOURCE_ID: Option<String> = build_resource_id(
            get_subscription_id(),
            extract_site_name(),
            extract_resource_group(),
        );
    }
    AAS_RESOURCE_ID.as_deref()
}

#[cfg(test)]
mod tests {

    use super::{build_resource_id, extract_subscription_id};

    #[test]
    fn test_extract_subscription_without_plus_sign() {
        let expected_id = "foo";
        let id = extract_subscription_id(Some(expected_id.to_string()));

        assert!(id.is_some());
        assert_eq!(id.unwrap(), expected_id.to_string());
    }

    #[test]
    fn test_extract_subscription_with_plus_sign() {
        let expected_id = "foo".to_string();
        let subscription_id = "foo+bar".to_string();
        let id = extract_subscription_id(Some(subscription_id));

        assert!(id.is_some());
        assert_eq!(id.unwrap(), expected_id);
    }

    #[test]
    fn test_extract_subscription_with_empty_string() {
        let id = extract_subscription_id(Some("".to_string()));
        assert!(id.is_none());
    }

    #[test]
    fn test_extract_subscription_with_only_whitespaces() {
        let id = extract_subscription_id(Some("    ".to_string()));
        assert!(id.is_none());
    }

    #[test]
    fn test_extract_subscription_with_only_plus_sign() {
        let id = extract_subscription_id(Some("+".to_string()));
        assert!(id.is_none());
    }

    #[test]
    fn test_extract_subscription_with_whitespaces_separated_by_plus() {
        let id = extract_subscription_id(Some("   + ".to_string()));
        assert!(id.is_none());
    }

    #[test]
    fn test_extract_subscription_plus_sign_and_other_string() {
        let id = extract_subscription_id(Some("+other".to_string()));
        assert!(id.is_none());
    }

    #[test]
    fn test_build_resource_id() {
        let resource_id = build_resource_id(
            Some("foo".to_string()),
            Some("my_website".to_string()),
            Some("resource_group".to_string()),
        );

        assert!(resource_id.is_some());

        assert_eq!(
            resource_id.unwrap(),
            "/subscriptions/foo/resourcegroups/resource_group/providers/microsoft.web/sites/my_website"
        )
    }

    #[test]
    fn test_build_resource_id_with_missing_subscription_id() {
        let resource_id = build_resource_id(
            None,
            Some("my_website".to_string()),
            Some("resource_group".to_string()),
        );

        assert!(resource_id.is_none());
    }

    #[test]
    fn test_build_resource_id_with_missing_site_name() {
        let resource_id = build_resource_id(
            Some("foo+bar".to_string()),
            None,
            Some("my_website".to_string()),
        );

        assert!(resource_id.is_none());
    }

    #[test]
    fn test_build_resource_id_with_missing_resource_group() {
        let resource_id = build_resource_id(
            Some("foo+bar".to_string()),
            Some("my_website".to_string()),
            None,
        );

        assert!(resource_id.is_none());
    }

    #[test]
    fn test_build_resource_id_with_missing_info() {
        let resource_id = build_resource_id(None, None, None);

        assert!(resource_id.is_none());
    }
}
