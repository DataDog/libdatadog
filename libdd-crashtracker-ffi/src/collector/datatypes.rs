// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_common_ffi::{Error, Slice};
pub use libdd_crashtracker::{OpTypes, StacktraceCollection};
use std::time::Duration;

#[repr(C)]
pub struct EnvVar<'a> {
    key: CharSlice<'a>,
    val: CharSlice<'a>,
}

#[repr(C)]
pub struct ReceiverConfig<'a> {
    pub args: Slice<'a, CharSlice<'a>>,
    pub env: Slice<'a, EnvVar<'a>>,
    pub path_to_receiver_binary: CharSlice<'a>,
    /// Optional filename to forward stderr to (useful for logging/debugging)
    pub optional_stderr_filename: CharSlice<'a>,
    /// Optional filename to forward stdout to (useful for logging/debugging)
    pub optional_stdout_filename: CharSlice<'a>,
}

impl<'a> TryFrom<ReceiverConfig<'a>> for libdd_crashtracker::CrashtrackerReceiverConfig {
    type Error = anyhow::Error;
    fn try_from(value: ReceiverConfig<'a>) -> anyhow::Result<Self> {
        let args = {
            let mut vec = Vec::with_capacity(value.args.len());
            for x in value.args.iter() {
                vec.push(x.try_to_string()?);
            }
            vec
        };
        let env = {
            let mut vec = Vec::with_capacity(value.env.len());
            for x in value.env.iter() {
                vec.push((x.key.try_to_string()?, x.val.try_to_string()?));
            }
            vec
        };
        let path_to_receiver_binary = value.path_to_receiver_binary.try_to_string()?;
        let stderr_filename = value.optional_stderr_filename.try_to_string_option()?;
        let stdout_filename = value.optional_stdout_filename.try_to_string_option()?;
        Self::new(
            args,
            env,
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
    }
}

#[repr(C)]
pub struct EndpointConfig<'a> {
    pub url: CharSlice<'a>,
    pub api_key: CharSlice<'a>,
    pub test_token: CharSlice<'a>,
    pub use_system_resolver: bool,
}

#[repr(C)]
pub struct Config<'a> {
    pub additional_files: Slice<'a, CharSlice<'a>>,
    pub create_alt_stack: bool,
    pub demangle_names: bool,
    /// The endpoint to send the crash report to (can be a file://).
    /// If None, the crashtracker will infer the agent host from env variables.
    pub endpoint: EndpointConfig<'a>,
    /// Optional filename for a unix domain socket if the receiver is used asynchonously
    pub optional_unix_socket_filename: CharSlice<'a>,
    pub resolve_frames: StacktraceCollection,
    /// The set of signals we should be registered for.
    /// If empty, use the default set.
    pub signals: Slice<'a, i32>,
    /// Timeout in milliseconds before the signal handler starts tearing things down to return.
    /// If 0, uses the default timeout as specified in
    /// `libdd_crashtracker::shared::constants::DD_CRASHTRACK_DEFAULT_TIMEOUT`. Otherwise, uses
    /// the specified timeout value.
    /// This is given as a uint32_t, but the actual timeout needs to fit inside of an i32 (max
    /// 2^31-1). This is a limitation of the various interfaces used to guarantee the timeout.
    pub timeout_ms: u32,
    pub use_alt_stack: bool,
}

impl<'a> TryFrom<Config<'a>> for libdd_crashtracker::CrashtrackerConfiguration {
    type Error = anyhow::Error;
    fn try_from(value: Config<'a>) -> anyhow::Result<Self> {
        let additional_files = {
            let mut vec = Vec::with_capacity(value.additional_files.len());
            for x in value.additional_files.iter() {
                vec.push(x.try_to_string()?);
            }
            vec
        };
        let mut builder = Self::builder()
            .additional_files(additional_files)
            .create_alt_stack(value.create_alt_stack)
            .demangle_names(value.demangle_names)
            .resolve_frames(value.resolve_frames)
            .signals(value.signals.iter().copied().collect())
            .use_alt_stack(value.use_alt_stack)
            .endpoint_use_system_resolver(value.endpoint.use_system_resolver);
        if let Some(api_key) = value.endpoint.api_key.try_to_string_option()? {
            builder = builder.endpoint_api_key(&api_key);
        }
        if let Some(test_token) = value.endpoint.test_token.try_to_string_option()? {
            builder = builder.endpoint_test_token(&test_token);
        }
        if let Some(url) = value.endpoint.url.try_to_string_option()? {
            builder = builder.endpoint_url(&url);
        }
        if value.timeout_ms != 0 {
            builder = builder.timeout(Duration::from_millis(value.timeout_ms as u64));
        }
        if let Some(path) = value.optional_unix_socket_filename.try_to_string_option()? {
            builder = builder.unix_socket_path(path);
        }
        builder.build()
    }
}

#[repr(C)]
pub enum CrashtrackerGetCountersResult {
    Ok([i64; OpTypes::SIZE as usize]),
    #[allow(dead_code)]
    Err(Error),
}

impl From<anyhow::Result<[i64; OpTypes::SIZE as usize]>> for CrashtrackerGetCountersResult {
    fn from(value: anyhow::Result<[i64; OpTypes::SIZE as usize]>) -> Self {
        match value {
            Ok(x) => Self::Ok(x),
            Err(err) => Self::Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_crashtracker::CrashtrackerConfiguration;

    fn create_config_default<'a>() -> Config<'a> {
        Config {
            additional_files: Slice::empty(),
            create_alt_stack: false,
            demangle_names: false,
            endpoint: EndpointConfig {
                url: CharSlice::empty(),
                api_key: CharSlice::empty(),
                test_token: CharSlice::empty(),
                use_system_resolver: false,
            },
            optional_unix_socket_filename: CharSlice::empty(),
            resolve_frames: StacktraceCollection::Disabled,
            signals: Slice::empty(),
            timeout_ms: 0,
            use_alt_stack: false,
        }
    }

    #[test]
    fn test_config_try_from_defaults() -> anyhow::Result<()> {
        let ffi_config = create_config_default();
        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder().build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_endpoint_url() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.endpoint.url = CharSlice::from("http://localhost:8126");
        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_endpoint_api_key() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.endpoint.url = CharSlice::from("http://localhost:8126");
        ffi_config.endpoint.api_key = CharSlice::from("my-api-key");

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_api_key("my-api-key")
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_endpoint_test_token() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.endpoint.url = CharSlice::from("http://localhost:8126");
        ffi_config.endpoint.test_token = CharSlice::from("test-session-token");

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_test_token("test-session-token")
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_endpoint_use_system_resolver() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.endpoint.url = CharSlice::from("http://localhost:8126");
        ffi_config.endpoint.use_system_resolver = true;

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_use_system_resolver(true)
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_timeout_zero_uses_default() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.timeout_ms = 0;

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder().build()?;
        assert_eq!(config.timeout(), expected.timeout());
        Ok(())
    }

    #[test]
    fn test_config_try_from_custom_timeout() -> anyhow::Result<()> {
        use std::time::Duration;

        let mut ffi_config = create_config_default();
        ffi_config.timeout_ms = 5000;

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .timeout(Duration::from_millis(5000))
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_unix_socket() -> anyhow::Result<()> {
        let mut ffi_config = create_config_default();
        ffi_config.optional_unix_socket_filename = CharSlice::from("/run/crashtracker.sock");

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .unix_socket_path("/run/crashtracker.sock".to_string())
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_additional_files() -> anyhow::Result<()> {
        let files = [
            CharSlice::from("/tmp/extra1.txt"),
            CharSlice::from("/tmp/extra2.txt"),
        ];

        let mut ffi_config = create_config_default();
        ffi_config.additional_files = Slice::from(files.as_slice());

        let config = CrashtrackerConfiguration::try_from(ffi_config)?;
        let expected = CrashtrackerConfiguration::builder()
            .additional_files(vec![
                "/tmp/extra1.txt".to_string(),
                "/tmp/extra2.txt".to_string(),
            ])
            .build()?;
        assert_eq!(config, expected);
        Ok(())
    }

    #[test]
    fn test_config_try_from_create_alt_stack_without_use_fails() {
        let mut ffi_config = create_config_default();
        ffi_config.create_alt_stack = true;

        assert!(CrashtrackerConfiguration::try_from(ffi_config).is_err());
    }
}
