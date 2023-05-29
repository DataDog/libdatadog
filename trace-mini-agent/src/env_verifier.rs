// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use async_trait::async_trait;
use hyper::{Body, Client, Method, Request, Response};
use log::{debug, error};
use serde::{Deserialize, Serialize};
use std::{env, time::Duration};

#[cfg(not(test))]
use std::process;

use datadog_trace_utils::trace_utils;

const GCP_METADATA_URL: &str = "http://metadata.google.internal/computeMetadata/v1/?recursive=true";
const AZURE_FUNCTION_LOCAL_URL_ENV_VAR: &str = "ASPNETCORE_URLS";

const EXPECTED_AZURE_FUNCTION_LOCAL_URL_RESPONSE: &str =
    "Your Azure Function App is up and running.";

#[derive(Default, Debug, Deserialize, Serialize)]
pub struct GCPMetadata {
    pub instance: GCPInstance,
    pub project: GCPProject,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GCPInstance {
    pub region: String,
}
impl Default for GCPInstance {
    fn default() -> Self {
        Self {
            region: "unknown".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GCPProject {
    pub project_id: String,
}
impl Default for GCPProject {
    fn default() -> Self {
        Self {
            project_id: "unknown".to_string(),
        }
    }
}

#[async_trait]
pub trait EnvVerifier {
    /// Verifies the mini agent is running in the intended environment. if not, exit the process.
    /// Returns MiniAgentMetadata, a struct of metadata collected from the environment.
    async fn verify_environment(
        &self,
        verify_env_timeout: u64,
        env_type: &trace_utils::EnvironmentType,
    ) -> trace_utils::MiniAgentMetadata;
}

pub struct ServerlessEnvVerifier {}

#[async_trait]
impl EnvVerifier for ServerlessEnvVerifier {
    async fn verify_environment(
        &self,
        verify_env_timeout: u64,
        env_type: &trace_utils::EnvironmentType,
    ) -> trace_utils::MiniAgentMetadata {
        match env_type {
            // this environment variable is set by Azure Functions
            trace_utils::EnvironmentType::AzureFunction => {
                return verify_azure_environment(verify_env_timeout).await;
            }
            trace_utils::EnvironmentType::CloudFunction => {
                return verify_gcp_environment(verify_env_timeout).await;
            }
            trace_utils::EnvironmentType::Unknown => {
                return verify_gcp_environment(verify_env_timeout).await; // TODO
            }
        }
    }
}

async fn verify_azure_environment(verify_env_timeout: u64) -> trace_utils::MiniAgentMetadata {
    let azure_local_function_url_request =
        ensure_azure_function_environment(Box::new(AzureVerificationClientWrapper {}));
    match tokio::time::timeout(
        Duration::from_millis(verify_env_timeout),
        azure_local_function_url_request,
    )
    .await
    {
        Ok(result) => match result {
            Ok(res) => {
                debug!("Successfully verified Azure Function Environment.");
                res
            }
            Err(e) => {
                error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {e}");
                #[cfg(not(test))]
                process::exit(1);
                #[cfg(test)]
                trace_utils::MiniAgentMetadata {
                    gcp_project_id: None,
                    gcp_region: None,
                }
            }
        },
        Err(_) => {
            error!("Local Azure Function URL request timeout of {verify_env_timeout} ms exceeded. Using default values.");
            trace_utils::MiniAgentMetadata {
                gcp_project_id: None,
                gcp_region: None,
            }
        }
    };
    trace_utils::MiniAgentMetadata {
        gcp_project_id: None,
        gcp_region: None,
    }
}

/// AzureVerificationClient trait is used so we can mock the azure function local url response in unit tests
#[async_trait]
trait AzureVerificationClient {
    async fn get_metadata(&self, local_function_url: String) -> anyhow::Result<Response<Body>>;
}
struct AzureVerificationClientWrapper {}

#[async_trait]
impl AzureVerificationClient for AzureVerificationClientWrapper {
    async fn get_metadata(&self, local_function_url: String) -> anyhow::Result<Response<Body>> {
        let req = match Request::builder()
            .method(Method::GET)
            .uri(local_function_url)
            .body(Body::empty())
        {
            Ok(res) => res,
            Err(err) => {
                anyhow::bail!(err.to_string())
            }
        };
        let client = Client::new();
        match client.request(req).await {
            Ok(res) => Ok(res),
            Err(err) => anyhow::bail!(err.to_string()),
        }
    }
}

/// Checks if we are running in an Azure Function environment.
/// If true, returns MiniAgentMetadata default.
/// Otherwise, returns an error with the verification failure reason.
async fn ensure_azure_function_environment(
    metadata_client: Box<dyn AzureVerificationClient + Send + Sync>,
) -> anyhow::Result<trace_utils::MiniAgentMetadata> {
    let url = match env::var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR) {
        Ok(res) => res,
        Err(_) => {
            anyhow::bail!("Azure local function url env var not found.")
        }
    };
    let response = match metadata_client.get_metadata(url).await {
        Ok(res) => res,
        Err(e) => {
            anyhow::bail!("Can't communicate with local Azure Function URL. Error: {e}")
        }
    };
    let body = response.into_body();

    let bytes = hyper::body::to_bytes(body).await?;
    let body_str = String::from_utf8(bytes.to_vec())?;

    if !body_str.contains(EXPECTED_AZURE_FUNCTION_LOCAL_URL_RESPONSE) {
        anyhow::bail!("Incorrect response from Azure Function URL.")
    }

    Ok(trace_utils::MiniAgentMetadata {
        gcp_project_id: None,
        gcp_region: None,
    })
}

async fn verify_gcp_environment(verify_env_timeout: u64) -> trace_utils::MiniAgentMetadata {
    let gcp_metadata_request =
        ensure_gcp_function_environment(Box::new(GoogleMetadataClientWrapper {}));
    let gcp_metadata = match tokio::time::timeout(
        Duration::from_millis(verify_env_timeout),
        gcp_metadata_request,
    )
    .await
    {
        Ok(result) => match result {
            Ok(res) => {
                debug!("Successfully fetched Google Metadata.");
                res
            }
            Err(e) => {
                error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {e}");
                #[cfg(not(test))]
                process::exit(1);
                #[cfg(test)]
                GCPMetadata::default()
            }
        },
        Err(_) => {
            error!("Google Metadata request timeout of {verify_env_timeout} ms exceeded. Using default values.");
            GCPMetadata::default()
        }
    };
    trace_utils::MiniAgentMetadata {
        gcp_project_id: Some(gcp_metadata.project.project_id),
        gcp_region: Some(get_region_from_gcp_region_string(
            gcp_metadata.instance.region,
        )),
    }
}

/// The region found in GCP Metadata comes in the format: "projects/123123/regions/us-east1"
/// This function extracts just the region (us-east1) from this GCP region string.
/// If the string does not have 4 parts (separated by "/") or extraction fails, return "unknown"
fn get_region_from_gcp_region_string(str: String) -> String {
    let split_str = str.split('/').collect::<Vec<&str>>();
    if split_str.len() != 4 {
        return "unknown".to_string();
    }
    match split_str.last() {
        Some(res) => res.to_string(),
        None => "unknown".to_string(),
    }
}

/// GoogleMetadataClient trait is used so we can mock a google metadata server response in unit tests
#[async_trait]
trait GoogleMetadataClient {
    async fn get_metadata(&self) -> anyhow::Result<Response<Body>>;
}
struct GoogleMetadataClientWrapper {}

#[async_trait]
impl GoogleMetadataClient for GoogleMetadataClientWrapper {
    async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
        let req = match Request::builder()
            .method(Method::POST)
            .uri(GCP_METADATA_URL)
            .header("Metadata-Flavor", "Google")
            .body(Body::empty())
        {
            Ok(res) => res,
            Err(err) => {
                anyhow::bail!(err.to_string())
            }
        };
        let client = Client::new();
        match client.request(req).await {
            Ok(res) => Ok(res),
            Err(err) => anyhow::bail!(err.to_string()),
        }
    }
}

/// Checks if we are running in a Google Cloud Function environment.
/// If true, returns Metadata from the Google Cloud environment.
/// Otherwise, returns an error with the verification failure reason.
async fn ensure_gcp_function_environment(
    metadata_client: Box<dyn GoogleMetadataClient + Send + Sync>,
) -> anyhow::Result<GCPMetadata> {
    let response = match metadata_client.get_metadata().await {
        Ok(res) => res,
        Err(e) => {
            anyhow::bail!("Can't communicate with Google Metadata Server. Error: {e}")
        }
    };
    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    match headers.get("Server") {
        Some(val) => {
            if val != "Metadata Server for Serverless" {
                anyhow::bail!("In Google Cloud, but not in a function environment.")
            }
        }
        None => {
            anyhow::bail!("In Google Cloud, but server identifier not found.")
        }
    }

    let gcp_metadata = match get_gcp_metadata_from_body(body).await {
        Ok(res) => res,
        Err(err) => {
            error!("Failed to get GCP Function Metadata. Will not enrich spans. {err}");
            return Ok(GCPMetadata::default());
        }
    };

    Ok(gcp_metadata)
}

async fn get_gcp_metadata_from_body(body: hyper::Body) -> anyhow::Result<GCPMetadata> {
    let bytes = hyper::body::to_bytes(body).await?;
    let body_str = String::from_utf8(bytes.to_vec())?;
    let gcp_metadata: GCPMetadata = serde_json::from_str(&body_str)?;
    Ok(gcp_metadata)
}

#[cfg(test)]
mod tests {
    use std::env;

    use async_trait::async_trait;
    use datadog_trace_utils::trace_utils::{self, MiniAgentMetadata};
    use hyper::{Body, Response, StatusCode};
    use serial_test::serial;

    use crate::env_verifier::{
        ensure_azure_function_environment, ensure_gcp_function_environment,
        get_region_from_gcp_region_string, AzureVerificationClient, GoogleMetadataClient,
        AZURE_FUNCTION_LOCAL_URL_ENV_VAR, EXPECTED_AZURE_FUNCTION_LOCAL_URL_RESPONSE,
    };

    use super::{EnvVerifier, ServerlessEnvVerifier};

    #[tokio::test]
    async fn test_ensure_gcp_env_false_if_metadata_server_unreachable() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                anyhow::bail!("Random Error")
            }
        }
        let res = ensure_gcp_function_environment(Box::new(MockGoogleMetadataClient {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Can't communicate with Google Metadata Server. Error: Random Error"
        );
    }

    #[tokio::test]
    async fn test_ensure_gcp_env_false_if_no_server_in_response_headers() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let res = ensure_gcp_function_environment(Box::new(MockGoogleMetadataClient {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "In Google Cloud, but server identifier not found."
        );
    }

    #[tokio::test]
    async fn test_ensure_gcp_env_if_server_header_not_serverless() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Server", "Metadata Server NOT for Serverless")
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let res = ensure_gcp_function_environment(Box::new(MockGoogleMetadataClient {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "In Google Cloud, but not in a function environment."
        );
    }

    #[tokio::test]
    async fn test_ensure_gcp_env_true_if_cloud_function_env() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Server", "Metadata Server for Serverless")
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let res = ensure_gcp_function_environment(Box::new(MockGoogleMetadataClient {})).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_gcp_verify_environment_timeout_exceeded_gives_unknown_values() {
        let env_verifier = ServerlessEnvVerifier {};
        let res = env_verifier
            .verify_environment(0, &trace_utils::EnvironmentType::CloudFunction)
            .await; // set the verify_env_timeout to timeout immediately
        assert_eq!(
            res,
            MiniAgentMetadata {
                gcp_project_id: Some("unknown".to_string()),
                gcp_region: Some("unknown".to_string()),
            }
        )
    }

    #[test]
    fn test_gcp_region_string_extraction_valid_string() {
        let res = get_region_from_gcp_region_string("projects/123123/regions/us-east1".to_string());
        assert_eq!(res, "us-east1");
    }

    #[test]
    fn test_gcp_region_string_extraction_wrong_number_of_parts() {
        let res = get_region_from_gcp_region_string("invalid/parts/count".to_string());
        assert_eq!(res, "unknown");
    }

    #[test]
    fn test_gcp_region_string_extraction_empty_string() {
        let res = get_region_from_gcp_region_string("".to_string());
        assert_eq!(res, "unknown");
    }

    #[tokio::test]
    #[serial]
    async fn test_ensure_azure_env_false_if_local_url_env_var_missing() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            async fn get_metadata(&self, _: String) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("Wrong Response"))
                    .unwrap())
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Azure local function url env var not found."
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_ensure_azure_env_false_if_local_url_wrong_response() {
        env::set_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR, "http://localhost:9091");
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            async fn get_metadata(&self, _: String) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("Wrong Response"))
                    .unwrap())
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Incorrect response from Azure Function URL."
        );
        env::remove_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR);
    }

    #[tokio::test]
    #[serial]
    async fn test_ensure_azure_env_false_if_local_url_unreachable() {
        env::set_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR, "http://localhost:9091");
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            async fn get_metadata(&self, _: String) -> anyhow::Result<Response<Body>> {
                anyhow::bail!("Random Error")
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {})).await;
        assert!(res.is_err());

        assert_eq!(
            res.unwrap_err().to_string(),
            "Can't communicate with local Azure Function URL. Error: Random Error"
        );
        env::remove_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR);
    }

    #[tokio::test]
    #[serial]
    async fn test_ensure_azure_env_true_if_azure_function_env() {
        env::set_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR, "http://localhost:9091");
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            async fn get_metadata(&self, _: String) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(EXPECTED_AZURE_FUNCTION_LOCAL_URL_RESPONSE))
                    .unwrap())
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {})).await;
        assert!(res.is_ok());
        env::remove_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR);
    }

    #[tokio::test]
    #[serial]
    async fn test_azure_verify_environment_timeout_exceeded_gives_unknown_values() {
        env::set_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR, "http://localhost:9091");
        let env_verifier = ServerlessEnvVerifier {};
        let res = env_verifier
            .verify_environment(0, &trace_utils::EnvironmentType::AzureFunction)
            .await; // set the verify_env_timeout to timeout immediately
        assert_eq!(
            res,
            MiniAgentMetadata {
                gcp_project_id: None,
                gcp_region: None,
            }
        );
        env::remove_var(AZURE_FUNCTION_LOCAL_URL_ENV_VAR);
    }
}
