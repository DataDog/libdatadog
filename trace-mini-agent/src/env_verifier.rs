// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use async_trait::async_trait;
use hyper::{Body, Client, Error, Response, Uri};
use log::{debug, error};
use std::process;

const GCP_METADATA_URL: &str = "http://metadata.google.internal";

#[async_trait]
pub trait EnvVerifier {
    /// verifies the mini agent is running in the intended environment. if not, exit the process.
    async fn verify_environment(&self);
}

pub struct ServerlessEnvVerifier {}

#[async_trait]
impl EnvVerifier for ServerlessEnvVerifier {
    async fn verify_environment(&self) {
        if let Err(e) =
            ensure_gcp_function_environment(Box::new(GoogleMetadataClientWrapper {})).await
        {
            error!("Google Cloud Function environment verification failed. The Mini Agent cannot be run in a non cloud function environment. Error: {}. Shutting down now.", e);
            process::exit(1);
        }
        debug!("Google Cloud Function environment verification suceeded.")
    }
}

/// GoogleMetadataClient trait is used so we can mock google metadata server response in unit tests
#[async_trait]
trait GoogleMetadataClient {
    async fn get_metadata(&self) -> Result<Response<Body>, Error>;
}
struct GoogleMetadataClientWrapper {}

#[async_trait]
impl GoogleMetadataClient for GoogleMetadataClientWrapper {
    async fn get_metadata(&self) -> Result<Response<Body>, Error> {
        let client = Client::new();
        client.get(Uri::from_static(GCP_METADATA_URL)).await
    }
}

/// Checks if we are running in a Google Cloud Function environment.
/// if not, returns an error with the verification failure reason.
async fn ensure_gcp_function_environment(
    metadata_client: Box<dyn GoogleMetadataClient + Send + Sync>,
) -> anyhow::Result<()> {
    let response = match metadata_client.get_metadata().await {
        Ok(res) => res,
        Err(e) => {
            anyhow::bail!("Can't communicate with Google Metadata Server. {}", e)
        }
    };
    let headers = response.headers();
    match headers.get("Server") {
        Some(val) => {
            if val != "Metadata Server for Serverless" {
                anyhow::bail!("Using Google Compute Engine, but not in cloud function environment.")
            }
        }
        None => {
            anyhow::bail!("Using Google Compute Engine, but server identifier not found.")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use hyper::{Body, Error, Response, StatusCode};

    use crate::env_verifier::{ensure_gcp_function_environment, GoogleMetadataClient};

    use super::GoogleMetadataClientWrapper;

    #[tokio::test]
    async fn test_verify_env_false_if_metadata_server_unreachable() {
        // unit tests will always run in an environment where http://metadata.google.internal is unreachable
        let res = ensure_gcp_function_environment(Box::new(GoogleMetadataClientWrapper {})).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Can't communicate with Google Metadata Server. error trying to connect: dns error: failed to lookup address information: nodename nor servname provided, or not known"
        );
    }

    #[tokio::test]
    async fn test_verify_env_false_if_no_server_in_response_headers() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> Result<Response<Body>, Error> {
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
            "Using Google Compute Engine, but server identifier not found."
        );
    }

    #[tokio::test]
    async fn test_verify_env_false_if_server_header_not_serverless() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> Result<Response<Body>, Error> {
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
            "Using Google Compute Engine, but not in cloud function environment."
        );
    }

    #[tokio::test]
    async fn test_verify_env_true_if_cloud_function_env() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> Result<Response<Body>, Error> {
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
}
