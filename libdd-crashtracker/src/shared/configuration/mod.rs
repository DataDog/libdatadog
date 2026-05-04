// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
mod builder;
pub use builder::CrashtrackerConfigurationBuilder;
use libdd_common::Endpoint;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Stacktrace collection occurs in the context of a crashing process.
/// If the stack is sufficiently corruputed, it is possible (but unlikely),
/// for stack trace collection itself to crash.
/// We recommend fully enabling stacktrace collection, but having an environment
/// variable to allow downgrading the collector.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StacktraceCollection {
    #[default]
    Disabled,
    WithoutSymbols,
    /// This option uses `backtrace::resolve_frame_unsynchronized()` to gather symbol information
    /// and also unwind inlined functions. Enabling this feature will not only provide symbolic
    /// details, but may also yield additional or less stack frames compared to other
    /// configurations.
    EnabledWithInprocessSymbols,
    EnabledWithSymbolsInReceiver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerConfiguration {
    // Paths to any additional files to track, if any
    additional_files: Vec<String>,
    #[serde(default)]
    collect_all_threads: bool,
    create_alt_stack: bool,
    // Whether to demangle symbol names in stack traces
    demangle_names: bool,
    endpoint: Option<Endpoint>,
    #[serde(default = "default_max_threads")]
    max_threads: usize,
    resolve_frames: StacktraceCollection,
    signals: Vec<i32>,
    timeout: Duration,
    unix_socket_path: Option<String>,
    use_alt_stack: bool,
}

pub const fn default_max_threads() -> usize {
    2048
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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
            stderr_filename.is_none() && stdout_filename.is_none()
                || stderr_filename != stdout_filename,
            "Can't give the same filename for stderr ({stderr_filename:?})
        and stdout ({stdout_filename:?}), they will conflict with each other"
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
    pub fn builder() -> CrashtrackerConfigurationBuilder {
        CrashtrackerConfigurationBuilder::default()
    }

    pub fn additional_files(&self) -> &Vec<String> {
        &self.additional_files
    }

    pub fn collect_all_threads(&self) -> bool {
        self.collect_all_threads
    }

    pub fn create_alt_stack(&self) -> bool {
        self.create_alt_stack
    }

    pub fn max_threads(&self) -> usize {
        self.max_threads
    }

    pub fn use_alt_stack(&self) -> bool {
        self.use_alt_stack
    }

    pub(crate) fn endpoint(&self) -> &Option<Endpoint> {
        &self.endpoint
    }

    pub fn resolve_frames(&self) -> StacktraceCollection {
        self.resolve_frames
    }

    pub fn signals(&self) -> &Vec<i32> {
        &self.signals
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn unix_socket_path(&self) -> &Option<String> {
        &self.unix_socket_path
    }

    pub fn demangle_names(&self) -> bool {
        self.demangle_names
    }

    pub fn set_collect_all_threads(&mut self, collect: bool) {
        self.collect_all_threads = collect;
    }

    pub fn set_max_threads(&mut self, max: usize) {
        self.max_threads = max;
    }

    pub fn set_create_alt_stack(&mut self, create_alt_stack: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            !create_alt_stack || self.use_alt_stack,
            "Cannot create an altstack without using it"
        );
        self.create_alt_stack = create_alt_stack;
        Ok(())
    }

    pub fn set_use_alt_stack(&mut self, use_alt_stack: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.create_alt_stack || use_alt_stack,
            "Cannot create an altstack without using it"
        );
        self.use_alt_stack = use_alt_stack;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CrashtrackerReceiverConfig;

    #[test]
    fn test_receiver_config_new() -> anyhow::Result<()> {
        let args = vec!["foo".to_string()];
        let env = vec![
            ("bar".to_string(), "baz".to_string()),
            ("apple".to_string(), "banana".to_string()),
        ];
        let path_to_receiver_binary = "/tmp/crashtracker-receiver-binary".to_string();
        let stderr_filename = None;
        let stdout_filename = None;

        let config = CrashtrackerReceiverConfig::new(
            args.clone(),
            env.clone(),
            path_to_receiver_binary.clone(),
            stderr_filename.clone(),
            stdout_filename.clone(),
        )?;
        assert_eq!(config.args, args);
        assert_eq!(config.env, env);
        assert_eq!(config.path_to_receiver_binary, path_to_receiver_binary);
        assert_eq!(config.stderr_filename, stderr_filename);
        assert_eq!(config.stdout_filename, stdout_filename);

        let stderr_filename = None;
        let stdout_filename = Some("/tmp/stdout.txt".to_string());
        let config = CrashtrackerReceiverConfig::new(
            args.clone(),
            env.clone(),
            path_to_receiver_binary.clone(),
            stderr_filename.clone(),
            stdout_filename.clone(),
        )?;
        assert_eq!(config.args, args);
        assert_eq!(config.env, env);
        assert_eq!(config.path_to_receiver_binary, path_to_receiver_binary);
        assert_eq!(config.stderr_filename, stderr_filename);
        assert_eq!(config.stdout_filename, stdout_filename);

        let stderr_filename = Some("/tmp/stderr.txt".to_string());
        let stdout_filename = None;
        let config = CrashtrackerReceiverConfig::new(
            args.clone(),
            env.clone(),
            path_to_receiver_binary.clone(),
            stderr_filename.clone(),
            stdout_filename.clone(),
        )?;
        assert_eq!(config.args, args);
        assert_eq!(config.env, env);
        assert_eq!(config.path_to_receiver_binary, path_to_receiver_binary);
        assert_eq!(config.stderr_filename, stderr_filename);
        assert_eq!(config.stdout_filename, stdout_filename);

        let stderr_filename = Some("/tmp/stderr.txt".to_string());
        let stdout_filename = Some("/tmp/stdout.txt".to_string());
        let config = CrashtrackerReceiverConfig::new(
            args.clone(),
            env.clone(),
            path_to_receiver_binary.clone(),
            stderr_filename.clone(),
            stdout_filename.clone(),
        )?;
        assert_eq!(config.args, args);
        assert_eq!(config.env, env);
        assert_eq!(config.path_to_receiver_binary, path_to_receiver_binary);
        assert_eq!(config.stderr_filename, stderr_filename);
        assert_eq!(config.stdout_filename, stdout_filename);

        let stderr_filename = Some("/tmp/shared.txt".to_string());
        let stdout_filename = Some("/tmp/shared.txt".to_string());
        CrashtrackerReceiverConfig::new(
            args.clone(),
            env.clone(),
            path_to_receiver_binary.clone(),
            stderr_filename.clone(),
            stdout_filename.clone(),
        )
        .unwrap_err();
        Ok(())
    }
}
