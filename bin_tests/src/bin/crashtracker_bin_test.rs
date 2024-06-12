// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix::main()
}

#[cfg(unix)]
mod unix {
    use anyhow::Context;
    use bin_tests::ReceiverType;
    use std::{env, fs::File, time::Duration};

    use datadog_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerMetadata,
        CrashtrackerReceiverConfig,
    };
    use ddcommon::tag;

    #[inline(never)]
    unsafe fn deref_ptr(p: *mut u8) {
        *std::hint::black_box(p) = std::hint::black_box(1);
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let mode = args.next().context("Unexpected number of arguments")?;
        let output_url = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let unix_socket_reciever_binary = args.next().context("Unexpected number of arguments")?;
        let stderr_filename = args.next().context("Unexpected number of arguments")?;
        let stdout_filename = args.next().context("Unexpected number of arguments")?;
        let socket_path = args.next().context("Unexpected number of arguments")?;
        anyhow::ensure!(args.next().is_none(), "unexpected extra arguments");

        let timeout = Duration::from_secs(30);
        let wait_for_receiver = true;

        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(ddcommon::Endpoint {
                url: ddcommon::parse_uri(&output_url)?,
                api_key: None,
            })
        };

        let config = CrashtrackerConfiguration {
            additional_files: vec![],
            create_alt_stack: true,
            resolve_frames: crashtracker::StacktraceCollection::WithoutSymbols,
            endpoint,
            timeout,
            wait_for_receiver,
        };

        let metadata = CrashtrackerMetadata {
            profiling_library_name: "libdatadog".to_owned(),
            profiling_library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                tag!("service", "foo"),
                tag!("service_version", "bar"),
                tag!("runtime-id", "xyz"),
                tag!("language", "native"),
            ],
        };

        if mode == format!("{:?}", ReceiverType::ChildProcessStdin) {
            crashtracker::init_with_receiver(
                config,
                CrashtrackerReceiverConfig::new(
                    vec![],
                    env::vars().collect(),
                    receiver_binary,
                    Some(stderr_filename),
                    Some(stdout_filename),
                )?,
                metadata,
            )?;
        } else if mode == format!("{:?}", ReceiverType::UnixSocket) {
            let stdout = File::create(stdout_filename)?;
            let stderr = File::create(stderr_filename)?;

            // Fork a unix socket receiver
            // For now, this exits when a single message is received.
            // When the listener is updated, we'll need to keep the handle and kill the receiver
            // to avoid leaking a process.
            std::process::Command::new(unix_socket_reciever_binary)
                .stderr(stderr)
                .stdout(stdout)
                .arg(&socket_path)
                .spawn()
                .context("failed to spawn unix receiver")?;

            // Wait long enough for the receiver to establish the socket
            std::thread::sleep(std::time::Duration::from_secs(1));

            crashtracker::init_with_unix_socket(config, &socket_path, metadata)?;
        } else {
            anyhow::bail!("unexpected mode: {mode}")
        }

        crashtracker::begin_profiling_op(crashtracker::ProfilingOpTypes::CollectingSample)?;
        unsafe {
            deref_ptr(std::ptr::null_mut::<u8>());
        }
        crashtracker::end_profiling_op(crashtracker::ProfilingOpTypes::CollectingSample)?;
        Ok(())
    }
}
