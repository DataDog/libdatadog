// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use async_trait::async_trait;
use hyper::{Body, Client, Method, Request, Response};
use log::{debug, error};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;
use std::time::Duration;
use std::{thread, time};
use sysinfo::{ProcessExt, System, SystemExt};

#[cfg(not(test))]
use std::process;

use datadog_trace_utils::trace_utils;

const GCP_METADATA_URL: &str = "http://metadata.google.internal/computeMetadata/v1/?recursive=true";
const AZURE_LINUX_PROCESS_EXE_NAME: &str =
    "/azure-functions-host/Microsoft.Azure.WebJobs.Script.WebHost";

// C:\Program Files (x86)\SiteExtensions\Functions\4.21.3\32bit\Microsoft.Azure.WebJobs.Script.dll
const AZURE_WINDOWS_DLL_PATH_REGEX_PATTERN: &str = r#"C:\\Program Files \(x86\)\\SiteExtensions\\Functions\\.+\\.+\\Microsoft\.Azure\.WebJobs\.Script\.dll"#;

#[derive(Default, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct GCPMetadata {
    pub instance: GCPInstance,
    pub project: GCPProject,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
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

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
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
            trace_utils::EnvironmentType::AzureFunction => {
                return verify_azure_environment_or_exit();
            }
            trace_utils::EnvironmentType::CloudFunction => {
                return verify_gcp_environment_or_exit(verify_env_timeout).await;
            }
        }
    }
}

async fn verify_gcp_environment_or_exit(verify_env_timeout: u64) -> trace_utils::MiniAgentMetadata {
    let gcp_metadata_request =
        ensure_gcp_function_environment(Box::new(GoogleMetadataClientWrapper {}));
    let gcp_metadata = match tokio::time::timeout(
        Duration::from_millis(verify_env_timeout),
        gcp_metadata_request,
    )
    .await
    {
        Ok(result) => match result {
            Ok(metadata) => {
                debug!("Successfully fetched Google Metadata.");
                metadata
            }
            Err(err) => {
                error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {err}");
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
        let req = Request::builder()
            .method(Method::POST)
            .uri(GCP_METADATA_URL)
            .header("Metadata-Flavor", "Google")
            .body(Body::empty())
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

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
    let response = metadata_client.get_metadata().await.map_err(|err| {
        anyhow::anyhow!("Can't communicate with Google Metadata Server. Error: {err}")
    })?;

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

fn verify_azure_environment_or_exit() -> trace_utils::MiniAgentMetadata {
    match ensure_azure_function_environment(Box::new(AzureVerificationClientWrapper {})) {
        Ok(metadata) => {
            debug!("Successfully verified Azure Function Environment.");
            metadata
        }
        Err(e) => {
            error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {e}");
            #[cfg(not(test))]
            process::exit(1);
            #[cfg(test)]
            trace_utils::MiniAgentMetadata::default()
        }
    }
}

/// AzureVerificationClient trait is used so we can mock the azure function local url response in unit tests
#[async_trait]
trait AzureVerificationClient {
    fn get_process_files_linux(&self) -> Vec<String>;
    fn get_w3wp_dlls_windows(&self) -> Vec<String>;
}
struct AzureVerificationClientWrapper {}

#[async_trait]
impl AzureVerificationClient for AzureVerificationClientWrapper {
    fn get_w3wp_dlls_windows(&self) -> Vec<String> {
        let output_bytes =
            Command::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
                .args([
                    "Get-Process",
                    "w3wp*",
                    "|",
                    "select",
                    "-expand",
                    "modules", // "select",
                               // "ModuleName", // "|",
                               // "select",
                               // "-ExpandProperty",
                               // "modules",
                               // "|",
                               // "group",
                               // "-Property",
                               // "FileName",
                               // "|",
                               // "select",
                               // "name",
                ])
                .output()
                .expect("failed to execute process");
        let output_string = String::from_utf8(output_bytes.stdout).unwrap_or_else(|_| {
            error!("Failed to process windows environment verification output.");
            String::new()
        });
        debug!("output string: {output_string:?}");
        output_string
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn get_process_files_linux(&self) -> Vec<String> {
        let mut sys = System::new_all();
        sys.refresh_all();

        let processes = sys.processes();

        let mut paths: Vec<String> = Vec::new();
        for process in processes.values() {
            debug!(
                "exe | {:?} | cmd | {:?} | root | {:?} | ",
                process.exe(),
                process.cmd(),
                process.root()
            );
            paths.push(process.exe().to_string_lossy().to_string());
        }

        debug!(
            "Environment process exe paths used for Azure env verification: {:?}",
            paths
        );

        paths
    }
}

/// Checks if we are running in an Azure Function environment.
/// If true, returns MiniAgentMetadata default.
/// Otherwise, returns an error with the verification failure reason.
fn ensure_azure_function_environment(
    verification_client: Box<dyn AzureVerificationClient + Send + Sync>,
) -> anyhow::Result<trace_utils::MiniAgentMetadata> {
    match env::consts::OS {
        "linux" => {
            let paths = verification_client.get_process_files_linux();
            println!("paths: {paths:?}");

            for path in paths {
                if path == AZURE_LINUX_PROCESS_EXE_NAME {
                    return Ok(trace_utils::MiniAgentMetadata::default());
                }
            }
            anyhow::bail!("Unable to find Azure Function process.");
        }
        "windows" => {
            let open_dlls = verification_client.get_w3wp_dlls_windows();

            debug!("open dlls: {open_dlls:?}");

            let azure_windows_process_exe_regex = Regex::new(AZURE_WINDOWS_DLL_PATH_REGEX_PATTERN)
                .map_err(|_| anyhow::anyhow!("Error Parsing Azure Windows EXE Regex"))?;

            for dll in open_dlls {
                if azure_windows_process_exe_regex.is_match(&dll) {
                    return Ok(trace_utils::MiniAgentMetadata::default());
                }
            }
            anyhow::bail!("Unable to find open Azure Function dll.");
        }
        _ => {
            anyhow::bail!("The Serverless Mini Agent does not support this platform.");
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use datadog_trace_utils::trace_utils;
    use duplicate::duplicate_item;
    use hyper::{Body, Response, StatusCode};
    use serde_json::json;

    use crate::env_verifier::{
        ensure_azure_function_environment, ensure_gcp_function_environment,
        get_region_from_gcp_region_string, AzureVerificationClient, GCPInstance, GCPMetadata,
        GCPProject, GoogleMetadataClient, AZURE_LINUX_PROCESS_EXE_NAME,
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
                    .body(Body::from(
                        json!({
                            "instance": {
                                "region": "projects/123123/regions/us-east1",
                            },
                            "project": {
                                "projectId": "my-project"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap())
            }
        }
        let res = ensure_gcp_function_environment(Box::new(MockGoogleMetadataClient {})).await;
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            GCPMetadata {
                instance: GCPInstance {
                    region: "projects/123123/regions/us-east1".to_string()
                },
                project: GCPProject {
                    project_id: "my-project".to_string()
                }
            }
        )
    }

    #[tokio::test]
    async fn test_gcp_verify_environment_timeout_exceeded_gives_unknown_values() {
        let env_verifier = ServerlessEnvVerifier {};
        let res = env_verifier
            .verify_environment(0, &trace_utils::EnvironmentType::CloudFunction)
            .await; // set the verify_env_timeout to timeout immediately
        assert_eq!(
            res,
            trace_utils::MiniAgentMetadata {
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

    #[test]
    fn test_ensure_azure_env_true_if_linux_function_env() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_process_files(&self, _sys: sysinfo::System) -> Vec<String> {
                vec![AZURE_LINUX_PROCESS_EXE_NAME.to_string()]
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}));
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), trace_utils::MiniAgentMetadata::default());
    }

    #[duplicate_item(
        test_name                                                               path_version    bitness;
        [test_ensure_azure_env_true_if_windows_function_env_32bit]              ["4.21.3"]      ["32bit"];
        [test_ensure_azure_env_true_if_windows_function_env_64bit]              ["4.21.3"]      ["64bit"];
        [test_ensure_azure_env_true_if_windows_function_env_random_path_ver]    ["5.5555"]      ["32bit"];
    )]
    #[test]
    fn test_name() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_process_files(&self, sys: sysinfo::System) -> Vec<String> {
                vec![format!("C:\\Program Files (x86)\\SiteExtensions\\Functions\\{}\\{}\\Microsoft.Azure.WebJobs.Script.dll", path_version, bitness)]
            }
        }
        let res = ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}));
        // assert!(res.is_ok());
        assert_eq!(res.unwrap(), trace_utils::MiniAgentMetadata::default());
    }
}
