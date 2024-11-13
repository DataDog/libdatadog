// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::shared::constants;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};

/// Stacktrace collection occurs in the context of a crashing process.
/// If the stack is sufficiently corruputed, it is possible (but unlikely),
/// for stack trace collection itself to crash.
/// We recommend fully enabling stacktrace collection, but having an environment
/// variable to allow downgrading the collector.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StacktraceCollection {
    /// Stacktrace collection occurs in the
    Disabled,
    WithoutSymbols,
    EnabledWithInprocessSymbols,
    EnabledWithSymbolsInReceiver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerConfiguration {
    // Paths to any additional files to track, if any
    pub additional_files: Vec<String>,
    pub create_alt_stack: bool,
    pub use_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub resolve_frames: StacktraceCollection,
    pub timeout_ms: u32,
    pub unix_socket_path: Option<String>,
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
            stderr_filename.is_some() && stderr_filename != stdout_filename,
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
    pub fn new(
        additional_files: Vec<String>,
        create_alt_stack: bool,
        use_alt_stack: bool,
        endpoint: Option<Endpoint>,
        resolve_frames: StacktraceCollection,
        timeout_ms: u32,
        unix_socket_path: Option<String>,
    ) -> anyhow::Result<Self> {
        // Requesting to create, but not use, the altstack is considered paradoxical.
        anyhow::ensure!(
            !create_alt_stack || use_alt_stack,
            "Cannot create an altstack without using it"
        );
        let timeout_ms = if timeout_ms == 0 {
            constants::DD_CRASHTRACK_DEFAULT_TIMEOUT_MS
        } else if timeout_ms > i32::MAX as u32 {
            anyhow::bail!("Timeout must be less than i32::MAX")
        } else {
            timeout_ms
        };
        // Note:  don't check the receiver socket upfront, since a configuration can be interned
        // before the receiver is started when using an async-receiver.
        Ok(Self {
            additional_files,
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            timeout_ms,
            unix_socket_path,
        })
    }
}
