// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::{default_signals, shared::constants, signal_from_signum};
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
    additional_files: Vec<String>,
    create_alt_stack: bool,
    use_alt_stack: bool,
    endpoint: Option<Endpoint>,
    resolve_frames: StacktraceCollection,
    signals: Vec<i32>,
    timeout_ms: u32,
    unix_socket_path: Option<String>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        additional_files: Vec<String>,
        create_alt_stack: bool,
        use_alt_stack: bool,
        endpoint: Option<Endpoint>,
        resolve_frames: StacktraceCollection,
        mut signals: Vec<i32>,
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
        if signals.is_empty() {
            signals = default_signals();
        } else {
            // Ensure we don't have double elements in the signals list.
            let before_len = signals.len();
            signals.sort();
            signals.dedup();
            anyhow::ensure!(
                before_len == signals.len(),
                "Signals contained duplicate elements"
            );
            // Ensure that all signal values translate to a valid signum
            signals
                .iter()
                .try_for_each(|x| signal_from_signum(*x).map(|_| ()))?;
        }

        // Note:  don't check the receiver socket upfront, since a configuration can be interned
        // before the receiver is started when using an async-receiver.
        Ok(Self {
            additional_files,
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            signals,
            timeout_ms,
            unix_socket_path,
        })
    }

    pub fn additional_files(&self) -> &Vec<String> {
        &self.additional_files
    }

    pub fn create_alt_stack(&self) -> bool {
        self.create_alt_stack
    }

    pub fn use_alt_stack(&self) -> bool {
        self.use_alt_stack
    }

    pub fn endpoint(&self) -> &Option<Endpoint> {
        &self.endpoint
    }

    pub fn resolve_frames(&self) -> StacktraceCollection {
        self.resolve_frames
    }

    pub fn signals(&self) -> &Vec<i32> {
        &self.signals
    }

    pub fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }

    pub fn unix_socket_path(&self) -> &Option<String> {
        &self.unix_socket_path
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
