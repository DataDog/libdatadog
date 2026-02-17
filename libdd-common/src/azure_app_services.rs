// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use regex::Regex;
use std::env;
use std::sync::LazyLock;

const WEBSITE_OWNER_NAME: &str = "WEBSITE_OWNER_NAME";
const WEBSITE_SITE_NAME: &str = "WEBSITE_SITE_NAME";
const WEBSITE_RESOURCE_GROUP: &str = "WEBSITE_RESOURCE_GROUP";
const SITE_EXTENSION_VERSION: &str = "DD_AAS_DOTNET_EXTENSION_VERSION";
const WEBSITE_OS: &str = "WEBSITE_OS";
const INSTANCE_NAME: &str = "COMPUTERNAME";
const INSTANCE_ID: &str = "WEBSITE_INSTANCE_ID";
const SERVICE_CONTEXT: &str = "DD_AZURE_APP_SERVICES";
const FUNCTIONS_WORKER_RUNTIME: &str = "FUNCTIONS_WORKER_RUNTIME";
const FUNCTIONS_WORKER_RUNTIME_VERSION: &str = "FUNCTIONS_WORKER_RUNTIME_VERSION";
const FUNCTIONS_EXTENSION_VERSION: &str = "FUNCTIONS_EXTENSION_VERSION";
const DD_AZURE_RESOURCE_GROUP: &str = "DD_AZURE_RESOURCE_GROUP";
const WEBSITE_SKU: &str = "WEBSITE_SKU";

const UNKNOWN_VALUE: &str = "unknown";

enum AzureContext {
    AzureFunctions,
    AzureAppService,
}

macro_rules! get_trimmed_env_var {
    ($name:expr) => {
        env::var($name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
    };
}

macro_rules! get_value_or_unknown {
    ($name:expr) => {
        $name.as_ref().map(|s| s.as_str()).unwrap_or(UNKNOWN_VALUE)
    };
}

trait ToBoolean {
    fn to_bool(&self) -> bool;
}

impl ToBoolean for String {
    fn to_bool(&self) -> bool {
        matches!(
            self.to_lowercase().as_str(),
            "true" | "t" | "y" | "1" | "yes"
        )
    }
}

pub trait QueryEnv {
    fn get_var(&self, var: &str) -> Option<String>;
}

struct RealEnv;

impl QueryEnv for RealEnv {
    fn get_var(&self, var: &str) -> Option<String> {
        get_trimmed_env_var!(var)
    }
}

#[derive(Default)]
pub struct AzureMetadata {
    resource_id: Option<String>,
    subscription_id: Option<String>,
    site_name: Option<String>,
    resource_group: Option<String>,
    extension_version: Option<String>,
    operating_system: String,
    instance_name: Option<String>,
    instance_id: Option<String>,
    site_kind: String,
    site_type: String,
    runtime: Option<String>,
    runtime_version: Option<String>,
    function_runtime_version: Option<String>,
}

impl AzureMetadata {
    fn get_azure_context<T: QueryEnv>(query: &T) -> AzureContext {
        match (
            query.get_var(FUNCTIONS_WORKER_RUNTIME),
            query.get_var(FUNCTIONS_EXTENSION_VERSION),
        ) {
            (Some(_), Some(_)) => AzureContext::AzureFunctions,
            (Some(_), None) => AzureContext::AzureFunctions,
            (None, Some(_)) => AzureContext::AzureFunctions,
            (None, None) => AzureContext::AzureAppService,
        }
    }

    fn extract_subscription_id(s: Option<String>) -> Option<String> {
        s?.split('+')
            .next()
            .filter(|s| !s.trim().is_empty())
            .map(|v| v.to_string())
    }

    fn extract_resource_group(s: Option<String>) -> Option<String> {
        #[allow(clippy::unwrap_used)]
        let re: Regex = Regex::new(r".+\+(.+)-.+webspace(-Linux)?").unwrap();

        s.as_ref().and_then(|text| {
            re.captures(text)
                .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        })
    }

    /*
     * Computation of the resource id follow the same way the .NET tracer is doing:
     * https://github.com/DataDog/dd-trace-dotnet/blob/834a4b05b4ed91a819eb78761bf1ddb805969f65/tracer/src/Datadog.Trace/PlatformHelpers/AzureAppServices.cs#L215
     */
    fn build_resource_id(
        subscription_id: Option<&String>,
        site_name: Option<&String>,
        resource_group: Option<&String>,
    ) -> Option<String> {
        match (subscription_id, site_name, resource_group) {
            (Some(id_sub), Some(sitename), Some(res_grp)) => Some(
                format!("/subscriptions/{id_sub}/resourcegroups/{res_grp}/providers/microsoft.web/sites/{sitename}")
                .to_lowercase(),
            ),
            _ => None,
        }
    }

    fn build_metadata<T: QueryEnv>(query: T) -> Option<Self> {
        let subscription_id =
            AzureMetadata::extract_subscription_id(query.get_var(WEBSITE_OWNER_NAME));
        let site_name = query.get_var(WEBSITE_SITE_NAME);

        let (site_kind, site_type) = match AzureMetadata::get_azure_context(&query) {
            AzureContext::AzureFunctions => ("functionapp".to_owned(), "function".to_owned()),
            _ => ("app".to_owned(), "app".to_owned()),
        };

        let resource_group = query
            .get_var(DD_AZURE_RESOURCE_GROUP)
            .or_else(|| query.get_var(WEBSITE_RESOURCE_GROUP))
            .or_else(|| {
                // Check if we're in flex consumption plan first
                match query.get_var(WEBSITE_SKU).as_deref() {
                    Some("FlexConsumption") => None,
                    /* Flex Consumption plans need the `DD_AZURE_RESOURCE_GROUP` env var. If this
                     * logic ever changes, update the logic in
                     * `serverless-components/src/datadog-trace-agent` and the serverless compat
                     * layers accordingly. */
                    _ => AzureMetadata::extract_resource_group(query.get_var(WEBSITE_OWNER_NAME)),
                }
            });

        let resource_id = AzureMetadata::build_resource_id(
            subscription_id.as_ref(),
            site_name.as_ref(),
            resource_group.as_ref(),
        );
        let extension_version = query.get_var(SITE_EXTENSION_VERSION);
        let operating_system = query
            .get_var(WEBSITE_OS)
            .unwrap_or(std::env::consts::OS.to_string());
        let instance_name = query.get_var(INSTANCE_NAME);
        let instance_id = query.get_var(INSTANCE_ID);

        let runtime = query.get_var(FUNCTIONS_WORKER_RUNTIME);
        let runtime_version = query.get_var(FUNCTIONS_WORKER_RUNTIME_VERSION);
        let function_runtime_version = query.get_var(FUNCTIONS_EXTENSION_VERSION);

        Some(AzureMetadata {
            resource_id,
            subscription_id,
            site_name,
            resource_group,
            extension_version,
            operating_system,
            instance_name,
            instance_id,
            site_kind,
            site_type,
            runtime,
            runtime_version,
            function_runtime_version,
        })
    }

    pub fn new<T: QueryEnv>(query: T) -> Option<Self> {
        let is_relevant = query
            .get_var(SERVICE_CONTEXT)
            .map(|s| s.to_bool())
            .unwrap_or(false);

        if !is_relevant {
            return None;
        }

        AzureMetadata::build_metadata(query)
    }

    pub fn new_function<T: QueryEnv>(query: T) -> Option<Self> {
        match matches!(
            AzureMetadata::get_azure_context(&query),
            AzureContext::AzureFunctions
        ) {
            true => AzureMetadata::build_metadata(query),
            false => None,
        }
    }

    pub fn get_resource_id(&self) -> &str {
        get_value_or_unknown!(self.resource_id)
    }

    pub fn get_subscription_id(&self) -> &str {
        get_value_or_unknown!(self.subscription_id)
    }

    pub fn get_site_name(&self) -> &str {
        get_value_or_unknown!(self.site_name)
    }

    pub fn get_resource_group(&self) -> &str {
        get_value_or_unknown!(self.resource_group)
    }

    pub fn get_extension_version(&self) -> &str {
        get_value_or_unknown!(self.extension_version)
    }

    pub fn get_operating_system(&self) -> &str {
        self.operating_system.as_str()
    }

    pub fn get_instance_name(&self) -> &str {
        get_value_or_unknown!(self.instance_name)
    }

    pub fn get_instance_id(&self) -> &str {
        get_value_or_unknown!(self.instance_id)
    }

    pub fn get_site_type(&self) -> &str {
        self.site_type.as_str()
    }

    pub fn get_site_kind(&self) -> &str {
        self.site_kind.as_str()
    }

    pub fn get_runtime(&self) -> &str {
        get_value_or_unknown!(self.runtime)
    }

    pub fn get_runtime_version(&self) -> &str {
        get_value_or_unknown!(self.runtime_version)
    }

    pub fn get_function_runtime_version(&self) -> &str {
        get_value_or_unknown!(self.function_runtime_version)
    }

    /// Returns Azure App Services tags as an iterator of (tag_name, tag_value) tuples.
    /// These tags are specific to Azure App Services (web apps) environments.
    pub fn get_app_service_tags(&self) -> impl ExactSizeIterator<Item = (&'static str, &str)> {
        [
            (
                "aas.environment.extension_version",
                self.get_extension_version(),
            ),
            ("aas.environment.instance_id", self.get_instance_id()),
            ("aas.environment.instance_name", self.get_instance_name()),
            ("aas.environment.os", self.get_operating_system()),
            ("aas.resource.group", self.get_resource_group()),
            ("aas.resource.id", self.get_resource_id()),
            ("aas.site.kind", self.get_site_kind()),
            ("aas.site.name", self.get_site_name()),
            ("aas.site.type", self.get_site_type()),
            ("aas.subscription.id", self.get_subscription_id()),
        ]
        .into_iter()
    }

    /// Returns Azure Functions tags as an iterator of (tag_name, tag_value) tuples.
    /// These tags are specific to Azure Functions (serverless) environments.
    pub fn get_function_tags(&self) -> impl ExactSizeIterator<Item = (&'static str, &str)> {
        [
            ("aas.environment.instance_id", self.get_instance_id()),
            ("aas.environment.instance_name", self.get_instance_name()),
            ("aas.environment.os", self.get_operating_system()),
            ("aas.environment.runtime", self.get_runtime()),
            (
                "aas.environment.runtime_version",
                self.get_runtime_version(),
            ),
            (
                "aas.environment.function_runtime",
                self.get_function_runtime_version(),
            ),
            ("aas.resource.group", self.get_resource_group()),
            ("aas.resource.id", self.get_resource_id()),
            ("aas.site.kind", self.get_site_kind()),
            ("aas.site.name", self.get_site_name()),
            ("aas.site.type", self.get_site_type()),
            ("aas.subscription.id", self.get_subscription_id()),
        ]
        .into_iter()
    }
}

pub static AAS_METADATA: LazyLock<Option<AzureMetadata>> =
    LazyLock::new(|| AzureMetadata::new(RealEnv {}));

pub static AAS_METADATA_FUNCTION: LazyLock<Option<AzureMetadata>> =
    LazyLock::new(|| AzureMetadata::new_function(RealEnv {}));

#[cfg(test)]
mod tests {

    use indexmap::IndexMap;

    use crate::azure_app_services::{QueryEnv, WEBSITE_OWNER_NAME};

    use super::*;

    struct MockEnv {
        pub env_vars: IndexMap<String, String>,
    }

    impl MockEnv {
        pub fn new(vars: &[(&str, &str)]) -> Self {
            let mut env_vars: IndexMap<String, String> = IndexMap::new();
            vars.iter().for_each(|(name, value)| {
                env_vars.insert(name.to_string(), value.to_string());
            });

            MockEnv { env_vars }
        }
    }

    impl QueryEnv for MockEnv {
        fn get_var(&self, var: &str) -> Option<String> {
            self.env_vars.get(var).cloned()
        }
    }

    #[test]
    fn test_metadata_is_not_relevant_by_default() {
        let mocked_env = MockEnv::new(&[]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_none());
    }

    #[test]
    fn test_metadata_is_relevant_first() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "true")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_relevant_second() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "t")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_relevant_third() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "TrUe")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_relevant_fourth() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_relevant_fifth() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "yEs")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_relevant_sixth() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "Y")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_metadata_is_not_relevant_if_explicit() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "0")]);

        let metadata = AzureMetadata::new(mocked_env);
        assert!(metadata.is_none());
    }

    #[test]
    fn test_extract_subscription_without_plus_sign() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "foo"), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        let expected_id = "foo";

        assert_eq!(metadata.get_subscription_id(), expected_id);
    }

    #[test]
    fn test_extract_subscription_with_plus_sign() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "foo+bar"), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        let expected_id = "foo";
        assert_eq!(metadata.get_subscription_id(), expected_id);
    }

    #[test]
    fn test_extract_subscription_with_empty_string() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, ""), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_subscription_id(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_extract_subscription_with_only_whitespaces() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "    "), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_subscription_id(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_extract_subscription_with_only_plus_sign() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "+"), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_subscription_id(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_extract_subscription_with_whitespaces_separated_by_plus() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "   + "), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_subscription_id(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_extract_subscription_plus_sign_and_other_string() {
        let mocked_env = MockEnv::new(&[(WEBSITE_OWNER_NAME, "+other"), (SERVICE_CONTEXT, "1")]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_subscription_id(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_extract_resource_group_pattern_match_linux() {
        let mocked_env = MockEnv::new(&[
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+test-rg-EastUSwebspace-Linux",
            ),
            ("FUNCTIONS_WORKER_RUNTIME", "node"),
            ("FUNCTIONS_EXTENSION_VERSION", "~4"),
        ]);

        let metadata = AzureMetadata::new_function(mocked_env).unwrap();

        let expected_resource_group = "test-rg";

        assert_eq!(metadata.get_resource_group(), expected_resource_group);
    }

    #[test]
    fn test_extract_resource_group_pattern_match_windows() {
        let mocked_env = MockEnv::new(&[
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+test-rg-EastUSwebspace",
            ),
            ("FUNCTIONS_WORKER_RUNTIME", "node"),
            ("FUNCTIONS_EXTENSION_VERSION", "~4"),
        ]);

        let metadata = AzureMetadata::new_function(mocked_env).unwrap();

        let expected_resource_group = "test-rg";

        assert_eq!(metadata.get_resource_group(), expected_resource_group);
    }

    #[test]
    fn test_extract_resource_group_no_pattern_match() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_OWNER_NAME, "foo"),
            (FUNCTIONS_WORKER_RUNTIME, "node"),
            (FUNCTIONS_EXTENSION_VERSION, "~4"),
        ]);

        let metadata = AzureMetadata::new_function(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_group(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_use_resource_group_from_env_var_if_available() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_RESOURCE_GROUP, "test-rg-env-var"),
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+test-rg-EastUSwebspace-Linux",
            ),
            (SERVICE_CONTEXT, "1"),
            (WEBSITE_SKU, "ElasticPremium"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        let expected_resource_group = "test-rg-env-var";

        assert_eq!(metadata.get_resource_group(), expected_resource_group);
    }

    #[test]
    fn test_flex_consumption_resource_group_is_none_without_dd_azure_resource_group() {
        let mocked_env = MockEnv::new(&[
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+flex-EastUSwebspace-Linux",
            ),
            (WEBSITE_SKU, "FlexConsumption"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_group(), UNKNOWN_VALUE);
    }

    #[test]
    fn test_flex_consumption_uses_dd_azure_resource_group() {
        let mocked_env = MockEnv::new(&[
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+flex-EastUSwebspace-Linux",
            ),
            (DD_AZURE_RESOURCE_GROUP, "test-flex-rg"),
            (WEBSITE_SKU, "FlexConsumption"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        // Should use the DD_AZURE_RESOURCE_GROUP value instead of extracting from
        // WEBSITE_OWNER_NAME
        assert_eq!(metadata.get_resource_group(), "test-flex-rg");
    }

    #[test]
    fn test_dd_azure_resource_group_has_highest_priority() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_RESOURCE_GROUP, "test-rg-env-var"),
            (
                WEBSITE_OWNER_NAME,
                "00000000-0000-0000-0000-000000000000+test-rg-EastUSwebspace-Linux",
            ),
            (DD_AZURE_RESOURCE_GROUP, "dd-azure-rg-override"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        // DD_AZURE_RESOURCE_GROUP should have highest priority over WEBSITE_RESOURCE_GROUP and
        // WEBSITE_OWNER_NAME
        let expected_resource_group = "dd-azure-rg-override";

        assert_eq!(metadata.get_resource_group(), expected_resource_group);
    }

    #[test]
    fn test_build_resource_id() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_OWNER_NAME, "foo"),
            (WEBSITE_SITE_NAME, "my_website"),
            (WEBSITE_RESOURCE_GROUP, "resource_group"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(
            metadata.get_resource_id(),
            "/subscriptions/foo/resourcegroups/resource_group/providers/microsoft.web/sites/my_website"
        )
    }

    #[test]
    fn test_build_resource_id_with_missing_subscription_id() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_SITE_NAME, "my_website"),
            (WEBSITE_RESOURCE_GROUP, "resource_group"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_id(), UNKNOWN_VALUE)
    }

    #[test]
    fn test_build_resource_id_with_missing_site_name() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_OWNER_NAME, "foo"),
            (WEBSITE_RESOURCE_GROUP, "resource_group"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_id(), UNKNOWN_VALUE)
    }

    #[test]
    fn test_build_resource_id_with_missing_resource_group() {
        let mocked_env = MockEnv::new(&[
            (WEBSITE_OWNER_NAME, "foo"),
            (WEBSITE_SITE_NAME, "my_website"),
            (SERVICE_CONTEXT, "1"),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_id(), UNKNOWN_VALUE)
    }

    #[test]
    fn test_build_resource_id_with_missing_info() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "1")]);
        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_resource_id(), UNKNOWN_VALUE)
    }

    #[test]
    fn test_site_type_and_kind_default() {
        let mocked_env = MockEnv::new(&[(SERVICE_CONTEXT, "1")]);
        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_site_type(), "app");
        assert_eq!(metadata.get_site_kind(), "app")
    }

    #[test]
    fn test_site_type_and_kind_if_worker_runtime_not_specified() {
        let mocked_env = MockEnv::new(&[
            (FUNCTIONS_WORKER_RUNTIME, "my_runtime"),
            (SERVICE_CONTEXT, "1"),
        ]);
        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_site_kind(), "functionapp");
        assert_eq!(metadata.get_site_type(), "function")
    }

    #[test]
    fn test_site_type_and_kind_if_extension_version_not_specified() {
        let mocked_env = MockEnv::new(&[
            (FUNCTIONS_EXTENSION_VERSION, "next_version"),
            (SERVICE_CONTEXT, "1"),
        ]);
        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_site_kind(), "functionapp");
        assert_eq!(metadata.get_site_type(), "function")
    }

    #[test]
    fn test_site_type_and_kind_if_both_specified() {
        let mocked_env = MockEnv::new(&[
            (FUNCTIONS_WORKER_RUNTIME, "my_runtime"),
            (FUNCTIONS_EXTENSION_VERSION, "next_version"),
            (SERVICE_CONTEXT, "1"),
        ]);
        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(metadata.get_site_kind(), "functionapp");
        assert_eq!(metadata.get_site_type(), "function")
    }

    #[test]
    fn test_check_other_simple_env_retrieval() {
        let expected_site_name = "my_site_name".to_owned();
        let expected_resource_group = "my_resource_group".to_owned();
        let expected_site_version = "v42".to_owned();
        let expected_operating_system = "FreeBSD".to_owned();
        let expected_instance_name = "my_instance_name".to_owned();
        let expected_instance_id = "my_instance_id".to_owned();
        let expected_function_extension_version = "~4".to_owned();
        let expected_runtime = "node".to_owned();
        let expected_runtime_version = "18".to_owned();

        let mocked_env = MockEnv::new(&[
            (WEBSITE_SITE_NAME, expected_site_name.as_str()),
            (WEBSITE_RESOURCE_GROUP, expected_resource_group.as_str()),
            (SITE_EXTENSION_VERSION, expected_site_version.as_str()),
            (WEBSITE_OS, expected_operating_system.as_str()),
            (INSTANCE_NAME, expected_instance_name.as_str()),
            (INSTANCE_ID, expected_instance_id.as_str()),
            (SERVICE_CONTEXT, "1"),
            (
                FUNCTIONS_EXTENSION_VERSION,
                expected_function_extension_version.as_str(),
            ),
            (FUNCTIONS_WORKER_RUNTIME, expected_runtime.as_str()),
            (
                FUNCTIONS_WORKER_RUNTIME_VERSION,
                expected_runtime_version.as_str(),
            ),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        assert_eq!(expected_site_name, metadata.get_site_name());
        assert_eq!(expected_resource_group, metadata.get_resource_group());
        assert_eq!(expected_site_version, metadata.get_extension_version());
        assert_eq!(expected_operating_system, metadata.get_operating_system());
        assert_eq!(expected_instance_name, metadata.get_instance_name());
        assert_eq!(expected_instance_id, metadata.get_instance_id());
        assert_eq!(
            expected_function_extension_version,
            metadata.get_function_runtime_version()
        );
        assert_eq!(expected_runtime, metadata.get_runtime());
        assert_eq!(expected_runtime_version, metadata.get_runtime_version());
    }

    #[test]
    fn test_get_app_service_tags() {
        let expected_site_name = "my_site_name";
        let expected_resource_group = "my_resource_group";
        let expected_site_version = "v42";
        let expected_operating_system = "FreeBSD";
        let expected_instance_name = "my_instance_name";
        let expected_instance_id = "my_instance_id";
        let expected_subscription_id = "sub-123";
        let expected_resource_id = "/subscriptions/sub-123/resourcegroups/my_resource_group/providers/microsoft.web/sites/my_site_name";

        let mocked_env = MockEnv::new(&[
            (WEBSITE_SITE_NAME, expected_site_name),
            (WEBSITE_RESOURCE_GROUP, expected_resource_group),
            (SITE_EXTENSION_VERSION, expected_site_version),
            (WEBSITE_OS, expected_operating_system),
            (INSTANCE_NAME, expected_instance_name),
            (INSTANCE_ID, expected_instance_id),
            (SERVICE_CONTEXT, "1"),
            (
                WEBSITE_OWNER_NAME,
                &format!("{}+rg-webspace", expected_subscription_id),
            ),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        // Collect tags into a HashMap for easy lookup
        let tags: std::collections::HashMap<&str, &str> = metadata.get_app_service_tags().collect();

        // Verify all 10 App Service tags are present
        assert_eq!(tags.len(), 10);
        assert_eq!(tags.get("aas.resource.id"), Some(&expected_resource_id));
        assert_eq!(
            tags.get("aas.environment.extension_version"),
            Some(&expected_site_version)
        );
        assert_eq!(
            tags.get("aas.environment.instance_id"),
            Some(&expected_instance_id)
        );
        assert_eq!(
            tags.get("aas.environment.instance_name"),
            Some(&expected_instance_name)
        );
        assert_eq!(
            tags.get("aas.environment.os"),
            Some(&expected_operating_system)
        );
        assert_eq!(
            tags.get("aas.resource.group"),
            Some(&expected_resource_group)
        );
        assert_eq!(tags.get("aas.site.name"), Some(&expected_site_name));
        assert_eq!(tags.get("aas.site.kind"), Some(&"app"));
        assert_eq!(tags.get("aas.site.type"), Some(&"app"));
        assert_eq!(
            tags.get("aas.subscription.id"),
            Some(&expected_subscription_id)
        );

        // Verify runtime tags are NOT present
        assert_eq!(tags.get("aas.environment.runtime"), None);
        assert_eq!(tags.get("aas.environment.runtime_version"), None);
        assert_eq!(tags.get("aas.environment.function_runtime"), None);

        // Verify it's an ExactSizeIterator
        let iter = metadata.get_app_service_tags();
        assert_eq!(iter.len(), 10);
    }

    #[test]
    fn test_get_function_tags() {
        let expected_site_name = "my_site_name";
        let expected_resource_group = "my_resource_group";
        let expected_operating_system = "FreeBSD";
        let expected_instance_name = "my_instance_name";
        let expected_instance_id = "my_instance_id";
        let expected_function_extension_version = "~4";
        let expected_runtime = "node";
        let expected_runtime_version = "18";
        let expected_subscription_id = "sub-123";
        let expected_resource_id = "/subscriptions/sub-123/resourcegroups/my_resource_group/providers/microsoft.web/sites/my_site_name";

        let mocked_env = MockEnv::new(&[
            (WEBSITE_SITE_NAME, expected_site_name),
            (WEBSITE_RESOURCE_GROUP, expected_resource_group),
            (WEBSITE_OS, expected_operating_system),
            (INSTANCE_NAME, expected_instance_name),
            (INSTANCE_ID, expected_instance_id),
            (SERVICE_CONTEXT, "1"),
            (
                FUNCTIONS_EXTENSION_VERSION,
                expected_function_extension_version,
            ),
            (FUNCTIONS_WORKER_RUNTIME, expected_runtime),
            (FUNCTIONS_WORKER_RUNTIME_VERSION, expected_runtime_version),
            (
                WEBSITE_OWNER_NAME,
                &format!("{}+rg-webspace", expected_subscription_id),
            ),
        ]);

        let metadata = AzureMetadata::new(mocked_env).unwrap();

        // Collect tags into a HashMap for easy lookup
        let tags: std::collections::HashMap<&str, &str> = metadata.get_function_tags().collect();

        // Verify all 12 Function tags are present
        assert_eq!(tags.len(), 12);
        assert_eq!(tags.get("aas.resource.id"), Some(&expected_resource_id));
        assert_eq!(
            tags.get("aas.environment.instance_id"),
            Some(&expected_instance_id)
        );
        assert_eq!(
            tags.get("aas.environment.instance_name"),
            Some(&expected_instance_name)
        );
        assert_eq!(
            tags.get("aas.environment.os"),
            Some(&expected_operating_system)
        );
        assert_eq!(tags.get("aas.environment.runtime"), Some(&expected_runtime));
        assert_eq!(
            tags.get("aas.environment.runtime_version"),
            Some(&expected_runtime_version)
        );
        assert_eq!(
            tags.get("aas.environment.function_runtime"),
            Some(&expected_function_extension_version)
        );
        assert_eq!(
            tags.get("aas.resource.group"),
            Some(&expected_resource_group)
        );
        assert_eq!(tags.get("aas.site.name"), Some(&expected_site_name));
        assert_eq!(tags.get("aas.site.kind"), Some(&"functionapp"));
        assert_eq!(tags.get("aas.site.type"), Some(&"function"));
        assert_eq!(
            tags.get("aas.subscription.id"),
            Some(&expected_subscription_id)
        );

        // Verify extension_version tag is NOT present
        assert_eq!(tags.get("aas.environment.extension_version"), None);

        // Verify it's an ExactSizeIterator
        let iter = metadata.get_function_tags();
        assert_eq!(iter.len(), 12);
    }

    #[test]
    fn test_get_trimmed_env_var_empty_string() {
        env::remove_var("TEST_VAR_NONE");
        assert_eq!(get_trimmed_env_var!("TEST_VAR_NONE"), None);

        env::set_var("TEST_VAR_EMPTY_STRING", "");
        assert_eq!(get_trimmed_env_var!("TEST_VAR_EMPTY_STRING"), None);
    }
}
