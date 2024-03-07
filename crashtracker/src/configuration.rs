// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashtrackerResolveFrames {
    Never,
    /// Resolving frames in process is experimental, and can fail/crash
    ExperimentalInProcess,
    InReceiver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerConfiguration {
    pub collect_stacktrace: bool,
    pub create_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub path_to_receiver_binary: String,
    pub resolve_frames: CrashtrackerResolveFrames,
    pub stderr_filename: Option<String>,
    pub stdout_filename: Option<String>,
    pub timeout: Duration,
}

impl CrashtrackerConfiguration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        collect_stacktrace: bool,
        create_alt_stack: bool,
        endpoint: Option<Endpoint>,
        path_to_receiver_binary: String,
        resolve_frames: CrashtrackerResolveFrames,
        stderr_filename: Option<String>,
        stdout_filename: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !path_to_receiver_binary.is_empty(),
            "Expected a receiver binary"
        );
        anyhow::ensure!(stderr_filename.is_none() && stdout_filename.is_none() || stderr_filename != stdout_filename,
        "Can't give the same filename for stderr and stdout, they will conflict with each other"
    );
        Ok(Self {
            collect_stacktrace,
            create_alt_stack,
            endpoint,
            path_to_receiver_binary,
            resolve_frames,
            stderr_filename,
            stdout_filename,
            timeout,
        })
    }
}
