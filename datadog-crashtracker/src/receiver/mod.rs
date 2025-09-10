// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

mod entry_points;
pub use entry_points::{
    async_receiver_entry_point_unix_listener, async_receiver_entry_point_unix_socket,
    get_receiver_unix_socket, receiver_entry_point_stdin, receiver_entry_point_unix_socket,
};
mod receive_report;

#[cfg(test)]
mod tests {
    use super::receive_report::*;
    use crate::collector::default_signals;
    use crate::crash_info::{SiCodes, SigInfo, SignalNames};
    use crate::shared::constants::*;
    use crate::{CrashtrackerConfiguration, StacktraceCollection};
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    async fn to_socket(
        target: &mut tokio::net::UnixStream,
        msg: impl AsRef<str>,
    ) -> anyhow::Result<usize> {
        let msg = msg.as_ref();
        let n = target.write(format!("{msg}\n").as_bytes()).await?;
        target.flush().await?;
        Ok(n)
    }

    async fn send_report(delay: Duration, mut stream: UnixStream) -> anyhow::Result<()> {
        let sender = &mut stream;
        to_socket(sender, DD_CRASHTRACK_BEGIN_SIGINFO).await?;
        to_socket(
            sender,
            serde_json::to_string(&SigInfo {
                si_addr: None,
                si_code: 2,
                si_code_human_readable: SiCodes::BUS_ADRALN,
                si_signo: 11,
                si_signo_human_readable: SignalNames::SIGSEGV,
            })?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_SIGINFO).await?;

        to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
        to_socket(
            sender,
            serde_json::to_string(&CrashtrackerConfiguration::new(
                vec![],
                false,
                false,
                None,
                StacktraceCollection::Disabled,
                default_signals(),
                Some(Duration::from_secs(3)),
                None,
                true,
            )?)?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;
        tokio::time::sleep(delay).await;
        to_socket(sender, DD_CRASHTRACK_DONE).await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_receive_report_short_timeout() -> anyhow::Result<()> {
        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(1),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn(send_report(Duration::from_secs(2), sender));

        let crash_report = join_handle1.await??;
        let (_config, crashinfo) = crash_report.expect("Expect a report");
        assert!(crashinfo.incomplete);
        let sender_error = join_handle2.await?.unwrap_err().to_string();
        assert_eq!(sender_error, "Broken pipe (os error 32)");
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_receive_report_long_timeout() -> anyhow::Result<()> {
        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(2),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn(send_report(Duration::from_secs(1), sender));

        let crash_report = join_handle1.await??;
        let (_config, crashinfo) = crash_report.expect("Expect a report");
        assert!(crashinfo.incomplete);
        join_handle2.await??;
        Ok(())
    }

    async fn send_complete_report_with_file_endpoint(
        mut stream: UnixStream,
        crash_file: &std::path::Path,
    ) -> anyhow::Result<()> {
        let sender = &mut stream;

        // Send metadata first
        to_socket(sender, DD_CRASHTRACK_BEGIN_METADATA).await?;
        to_socket(
            sender,
            serde_json::to_string(&crate::crash_info::Metadata::new(
                "test-lib".to_string(),
                "1.0.0".to_string(),
                "native".to_string(),
                vec!["service:test".to_string()],
            ))?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_METADATA).await?;

        // Send config with file endpoint
        to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
        to_socket(
            sender,
            serde_json::to_string(&CrashtrackerConfiguration::new(
                vec![],
                false,
                false,
                Some(ddcommon::Endpoint::from_slice(&format!(
                    "file://{}",
                    crash_file.display()
                ))),
                StacktraceCollection::Disabled,
                default_signals(),
                Some(Duration::from_secs(3)),
                None,
                true,
            )?)?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;

        // Send siginfo
        to_socket(sender, DD_CRASHTRACK_BEGIN_SIGINFO).await?;
        to_socket(
            sender,
            serde_json::to_string(&SigInfo {
                si_addr: None,
                si_code: 2,
                si_code_human_readable: SiCodes::BUS_ADRALN,
                si_signo: 11,
                si_signo_human_readable: SignalNames::SIGSEGV,
            })?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_SIGINFO).await?;

        to_socket(sender, DD_CRASHTRACK_DONE).await?;
        Ok(())
    }

    async fn send_complete_report_with_http_endpoint(mut stream: UnixStream) -> anyhow::Result<()> {
        let sender = &mut stream;

        // Send metadata first
        to_socket(sender, DD_CRASHTRACK_BEGIN_METADATA).await?;
        to_socket(
            sender,
            serde_json::to_string(&crate::crash_info::Metadata::new(
                "test-lib".to_string(),
                "1.0.0".to_string(),
                "native".to_string(),
                vec!["service:test".to_string()],
            ))?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_METADATA).await?;

        // Send config with HTTP endpoint
        to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
        to_socket(
            sender,
            serde_json::to_string(&CrashtrackerConfiguration::new(
                vec![],
                false,
                false,
                Some(ddcommon::Endpoint::from_slice(
                    "http://localhost:8080/telemetry",
                )),
                StacktraceCollection::Disabled,
                default_signals(),
                Some(Duration::from_secs(3)),
                None,
                true,
            )?)?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;

        // Send siginfo
        to_socket(sender, DD_CRASHTRACK_BEGIN_SIGINFO).await?;
        to_socket(
            sender,
            serde_json::to_string(&SigInfo {
                si_addr: None,
                si_code: 2,
                si_code_human_readable: SiCodes::BUS_ADRALN,
                si_signo: 11,
                si_signo_human_readable: SignalNames::SIGSEGV,
            })?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_SIGINFO).await?;

        to_socket(sender, DD_CRASHTRACK_DONE).await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_with_file_endpoint() -> anyhow::Result<()> {
        use std::path::Path;
        use tempfile::TempDir;

        let temp_dir = TempDir::new()?;
        let crash_file = temp_dir.path().join("crash_test.json");
        let heartbeat_file = format!("{}.heartbeat", crash_file.display());

        // Create config with file endpoint
        let _config = CrashtrackerConfiguration::new(
            vec![],
            false,
            false,
            Some(ddcommon::Endpoint::from_slice(&format!(
                "file://{}",
                crash_file.display()
            ))),
            StacktraceCollection::Disabled,
            default_signals(),
            Some(Duration::from_secs(3)),
            None,
            true,
        )?;

        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn({
            let crash_file = crash_file.clone();
            async move { send_complete_report_with_file_endpoint(sender, &crash_file).await }
        });

        let crash_report = join_handle1.await??;
        let (_config, crashinfo) = crash_report.expect("Expect a report");
        join_handle2.await??;

        // Verify heartbeat file was created
        assert!(
            Path::new(&heartbeat_file).exists(),
            "Heartbeat file should be created"
        );

        // Verify heartbeat file contains valid JSON
        let heartbeat_content = std::fs::read_to_string(&heartbeat_file)?;
        let heartbeat_json: serde_json::Value = serde_json::from_str(&heartbeat_content)?;

        // Verify heartbeat properties
        assert_eq!(heartbeat_json["error"]["is_crash"], false);
        assert_eq!(
            heartbeat_json["error"]["message"],
            "Crashtracker heartbeat: crash processing started"
        );
        assert_eq!(heartbeat_json["uuid"], crashinfo.uuid);
        assert!(heartbeat_json["log_messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|msg| msg
                .as_str()
                .unwrap()
                .contains("Crashtracker heartbeat: crash processing started")));

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_with_non_file_endpoint() -> anyhow::Result<()> {
        // Create config with HTTP endpoint (will fail but that's ok for test)
        let _config = CrashtrackerConfiguration::new(
            vec![],
            false,
            false,
            Some(ddcommon::Endpoint::from_slice(
                "http://localhost:8080/telemetry",
            )),
            StacktraceCollection::Disabled,
            default_signals(),
            Some(Duration::from_secs(3)),
            None,
            true,
        )?;

        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));
        let join_handle2 =
            tokio::spawn(async move { send_complete_report_with_http_endpoint(sender).await });

        let crash_report = join_handle1.await??;
        let (_config, crashinfo) = crash_report.expect("Expect a report");
        join_handle2.await??;

        // Should have heartbeat failure logged but crash report should still succeed
        assert!(crashinfo
            .log_messages
            .iter()
            .any(|msg| msg.contains("Failed to send crash heartbeat")));

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_uuid_consistency() -> anyhow::Result<()> {
        use tempfile::TempDir;

        let temp_dir = TempDir::new()?;
        let crash_file = temp_dir.path().join("crash_uuid_test.json");
        let heartbeat_file = format!("{}.heartbeat", crash_file.display());

        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn({
            let crash_file = crash_file.clone();
            async move { send_complete_report_with_file_endpoint(sender, &crash_file).await }
        });

        let crash_report = join_handle1.await??;
        let (_config, crashinfo) = crash_report.expect("Expect a report");
        join_handle2.await??;

        // Read heartbeat file
        let heartbeat_content = std::fs::read_to_string(&heartbeat_file)?;
        let heartbeat_json: serde_json::Value = serde_json::from_str(&heartbeat_content)?;

        // Verify both heartbeat and crash report have the same UUID
        assert_eq!(heartbeat_json["uuid"].as_str().unwrap(), crashinfo.uuid);

        // Verify UUID format (should be valid UUID v4)
        let uuid_str = crashinfo.uuid;
        assert!(uuid_str.len() == 36, "UUID should be 36 characters");
        assert!(uuid_str.contains('-'), "UUID should contain hyphens");

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_only_sent_once() -> anyhow::Result<()> {
        use tempfile::TempDir;

        let temp_dir = TempDir::new()?;
        let crash_file = temp_dir.path().join("crash_once_test.json");
        let heartbeat_file = format!("{}.heartbeat", crash_file.display());

        let (mut sender, receiver) = tokio::net::UnixStream::pair()?;

        // Send a report that has multiple metadata/config blocks to ensure
        // heartbeat is only sent once
        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));

        let join_handle2 = tokio::spawn(async move {
            let sender = &mut sender;

            // Send first metadata
            to_socket(sender, DD_CRASHTRACK_BEGIN_METADATA).await?;
            to_socket(
                sender,
                serde_json::to_string(&crate::crash_info::Metadata::new(
                    "test-lib".to_string(),
                    "1.0.0".to_string(),
                    "native".to_string(),
                    vec![],
                ))?,
            )
            .await?;
            to_socket(sender, DD_CRASHTRACK_END_METADATA).await?;

            // Send config
            to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
            to_socket(
                sender,
                serde_json::to_string(&CrashtrackerConfiguration::new(
                    vec![],
                    false,
                    false,
                    Some(ddcommon::Endpoint::from_slice(&format!(
                        "file://{}",
                        crash_file.display()
                    ))),
                    StacktraceCollection::Disabled,
                    default_signals(),
                    Some(Duration::from_secs(3)),
                    None,
                    true,
                )?)?,
            )
            .await?;
            to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;

            // Send some additional data that would trigger heartbeat check again
            to_socket(sender, DD_CRASHTRACK_BEGIN_SIGINFO).await?;
            to_socket(
                sender,
                serde_json::to_string(&SigInfo {
                    si_addr: None,
                    si_code: 2,
                    si_code_human_readable: SiCodes::BUS_ADRALN,
                    si_signo: 11,
                    si_signo_human_readable: SignalNames::SIGSEGV,
                })?,
            )
            .await?;
            to_socket(sender, DD_CRASHTRACK_END_SIGINFO).await?;

            to_socket(sender, DD_CRASHTRACK_DONE).await?;
            Ok::<(), anyhow::Error>(())
        });

        let crash_report = join_handle1.await??;
        let (_config, _crashinfo) = crash_report.expect("Expect a report");
        join_handle2.await??;

        // Read heartbeat file content
        let heartbeat_content = std::fs::read_to_string(&heartbeat_file)?;

        // Count the number of JSON objects in the heartbeat file
        // Should only be 1 (heartbeat sent once)
        let json_count = heartbeat_content
            .lines()
            .filter(|line| line.trim().starts_with('{'))
            .count();

        assert_eq!(json_count, 1, "Heartbeat should only be sent once");

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_no_heartbeat_without_metadata() -> anyhow::Result<()> {
        use tempfile::TempDir;

        let temp_dir = TempDir::new()?;
        let crash_file = temp_dir.path().join("crash_no_metadata.json");
        let heartbeat_file = format!("{}.heartbeat", crash_file.display());

        let (mut sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));

        let join_handle2 = tokio::spawn(async move {
            let sender = &mut sender;

            // Send config but NO metadata
            to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
            to_socket(
                sender,
                serde_json::to_string(&CrashtrackerConfiguration::new(
                    vec![],
                    false,
                    false,
                    Some(ddcommon::Endpoint::from_slice(&format!(
                        "file://{}",
                        crash_file.display()
                    ))),
                    StacktraceCollection::Disabled,
                    default_signals(),
                    Some(Duration::from_secs(3)),
                    None,
                    true,
                )?)?,
            )
            .await?;
            to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;

            to_socket(sender, DD_CRASHTRACK_DONE).await?;
            Ok::<(), anyhow::Error>(())
        });

        let _crash_report = join_handle1.await??;
        join_handle2.await??;

        // Heartbeat file should NOT exist because metadata was missing
        assert!(
            !std::path::Path::new(&heartbeat_file).exists(),
            "Heartbeat file should not exist without metadata"
        );

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_heartbeat_and_crash_report_uuid_match() -> anyhow::Result<()> {
        use tempfile::TempDir;

        let temp_dir = TempDir::new()?;
        let crash_file = temp_dir.path().join("crash_uuid_match_test.json");
        let heartbeat_file = format!("{}.heartbeat", crash_file.display());

        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report_from_stream(
            Duration::from_secs(5),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn({
            let crash_file = crash_file.clone();
            async move { send_complete_report_with_file_endpoint(sender, &crash_file).await }
        });

        let crash_report = join_handle1.await??;
        let (config, crashinfo) = crash_report.expect("Expect a report");
        join_handle2.await??;

        // Upload the crash report to create the actual crash report file
        crashinfo
            .async_upload_to_endpoint(config.endpoint())
            .await?;

        // Read both files and parse their JSON
        let heartbeat_content = std::fs::read_to_string(&heartbeat_file)?;
        let heartbeat_json: serde_json::Value = serde_json::from_str(&heartbeat_content)?;

        let crash_report_content = std::fs::read_to_string(&crash_file)?;
        let crash_report_json: serde_json::Value = serde_json::from_str(&crash_report_content)?;

        // Extract UUIDs from both files
        let heartbeat_uuid = heartbeat_json["uuid"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Heartbeat UUID not found or not a string"))?;
        let crash_report_uuid = crash_report_json["uuid"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Crash report UUID not found or not a string"))?;

        // Verify they match
        assert_eq!(
            heartbeat_uuid, crash_report_uuid,
            "Heartbeat UUID ({}) should match crash report UUID ({})",
            heartbeat_uuid, crash_report_uuid
        );

        // Also verify they match the in-memory crash info
        assert_eq!(
            heartbeat_uuid, crashinfo.uuid,
            "Heartbeat UUID should match in-memory crash info UUID"
        );

        Ok(())
    }
}
