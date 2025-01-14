// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

mod entry_points;
pub use entry_points::{
    async_receiver_entry_point_unix_socket, receiver_entry_point_stdin,
    receiver_entry_point_unix_socket,
};
mod receive_report;

#[cfg(test)]
mod tests {
    use super::receive_report::*;
    use crate::shared::constants::*;
    use crate::{CrashtrackerConfiguration, SigInfo, StacktraceCollection};
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
                signame: Some("SIGSEGV".to_string()),
                signum: 11,
                faulting_address: None,
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
                3000,
                None,
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
        assert!(matches!(
            crash_report,
            CrashReportStatus::PartialCrashReport(_, _, _)
        ));
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
        assert!(matches!(crash_report, CrashReportStatus::CrashReport(_, _)));
        join_handle2.await??;
        Ok(())
    }
}
