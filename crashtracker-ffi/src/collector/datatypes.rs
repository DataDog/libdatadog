// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::option_from_char_slice;
pub use datadog_crashtracker::{OpTypes, StacktraceCollection};
use ddcommon::Endpoint;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::{Error, Slice};

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

impl<'a> TryFrom<ReceiverConfig<'a>> for datadog_crashtracker::CrashtrackerReceiverConfig {
    type Error = anyhow::Error;
    fn try_from(value: ReceiverConfig<'a>) -> anyhow::Result<Self> {
        let args = {
            let mut vec = Vec::with_capacity(value.args.len());
            for x in value.args.iter() {
                vec.push(x.try_to_utf8()?.to_string());
            }
            vec
        };
        let env = {
            let mut vec = Vec::with_capacity(value.env.len());
            for x in value.env.iter() {
                vec.push((
                    x.key.try_to_utf8()?.to_string(),
                    x.val.try_to_utf8()?.to_string(),
                ));
            }
            vec
        };
        let path_to_receiver_binary = value.path_to_receiver_binary.try_to_utf8()?.to_string();
        let stderr_filename = option_from_char_slice(value.optional_stderr_filename)?;
        let stdout_filename = option_from_char_slice(value.optional_stdout_filename)?;
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
pub struct Config<'a> {
    pub additional_files: Slice<'a, CharSlice<'a>>,
    pub create_alt_stack: bool,
    pub use_alt_stack: bool,
    /// The endpoint to send the crash report to (can be a file://).
    /// If None, the crashtracker will infer the agent host from env variables.
    pub endpoint: Option<&'a Endpoint>,
    pub resolve_frames: StacktraceCollection,
    /// Timeout in milliseconds before the signal handler starts tearing things down to return.
    /// This is given as a uint32_t, but the actual timeout needs to fit inside of an i32 (max
    /// 2^31-1). This is a limitation of the various interfaces used to guarantee the timeout.
    pub timeout_ms: u32,
    /// Optional filename for a unix domain socket if the receiver is used asynchonously
    pub optional_unix_socket_filename: CharSlice<'a>,
}

impl<'a> TryFrom<Config<'a>> for datadog_crashtracker::CrashtrackerConfiguration {
    type Error = anyhow::Error;
    fn try_from(value: Config<'a>) -> anyhow::Result<Self> {
        let additional_files = {
            let mut vec = Vec::with_capacity(value.additional_files.len());
            for x in value.additional_files.iter() {
                vec.push(x.try_to_utf8()?.to_string());
            }
            vec
        };
        let create_alt_stack = value.create_alt_stack;
        let use_alt_stack = value.use_alt_stack;
        let endpoint = value.endpoint.cloned();
        let resolve_frames = value.resolve_frames;
        let timeout_ms = value.timeout_ms;
        let unix_socket_path = option_from_char_slice(value.optional_unix_socket_filename)?;
        Self::new(
            additional_files,
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            timeout_ms,
            unix_socket_path,
        )
    }
}

#[repr(C)]
pub enum UsizeResult {
    Ok(usize),
    #[allow(dead_code)]
    Err(Error),
}

impl From<anyhow::Result<usize>> for UsizeResult {
    fn from(value: anyhow::Result<usize>) -> Self {
        match value {
            Ok(x) => Self::Ok(x),
            Err(err) => Self::Err(err.into()),
        }
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
