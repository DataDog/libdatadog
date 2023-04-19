// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use async_trait::async_trait;
use log::{debug, error};
use std::process;

use datadog_trace_utils::trace_utils;

#[async_trait]
pub trait EnvVerifier {
    /// verifies the mini agent is running in the intended environment. if not, exit the process.
    async fn verify_environment(&self);
}

#[derive(Clone)]
pub struct ServerlessEnvVerifier {}

#[async_trait]
impl EnvVerifier for ServerlessEnvVerifier {
    async fn verify_environment(&self) {
        if let Err(e) = trace_utils::check_is_gcp_function().await {
            error!("Google Cloud Function environment verification failed. The Mini Agent cannot be run in a non cloud function environment. Error: {}. Shutting down now.", e);
            process::exit(1);
        }
        debug!("Google Cloud Function environment verification suceeded.")
    }
}
