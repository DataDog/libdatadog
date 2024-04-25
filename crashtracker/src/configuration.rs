// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashtrackerStacktraceCollectionOptions {
    DontCollectStacktrace,
    CollectStacktraceButDoNotResolveSymbols,
    CollectStacktraceAndResolveSymbolsInReceiver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerConfiguration {
    pub create_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub resolve_frames: CrashtrackerStacktraceCollectionOptions,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerReceiverConfig {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub path_to_receiver_binary: String,
    pub stderr_filename: Option<String>,
    pub stdout_filename: Option<String>,
}

impl CrashtrackerReceiverConfig {
    pub fn new(
        args: Vec<String>,
        env: Vec<(String, String)>,
        path_to_receiver_binary: String,
        stderr_filename: Option<String>,
        stdout_filename: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !path_to_receiver_binary.is_empty(),
            "Expected a receiver binary"
        );
        anyhow::ensure!(
            stderr_filename.is_none() && stdout_filename.is_none()
                || stderr_filename != stdout_filename,
            "Can't give the same filename for stderr
        and stdout, they will conflict with each other"
        );

        Ok(Self {
            args,
            env,
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        })
    }
}

impl CrashtrackerConfiguration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        create_alt_stack: bool,
        endpoint: Option<Endpoint>,
        resolve_frames: CrashtrackerStacktraceCollectionOptions,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            create_alt_stack,
            endpoint,
            resolve_frames,
            timeout,
        })
    }
}
