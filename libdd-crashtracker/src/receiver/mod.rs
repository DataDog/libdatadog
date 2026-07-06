// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

mod entry_points;
pub use entry_points::{
    async_receiver_entry_point_unix_listener, async_receiver_entry_point_unix_socket,
    get_receiver_unix_socket, receiver_entry_point_stdin, receiver_entry_point_unix_socket,
};
#[cfg(target_os = "linux")]
mod ptrace_collector;
mod receive_report;

#[cfg(feature = "benchmarking")]
pub mod benchmark;

#[cfg(test)]
mod tests {
    use super::receive_report::*;
    use crate::collector::default_signals;
    use crate::crash_info::{SiCodes, SigInfo, SignalNames};
    use crate::shared::constants::*;
    use crate::{CrashtrackerConfiguration, ErrorKind};
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

        to_socket(sender, DD_CRASHTRACK_BEGIN_KIND).await?;
        to_socket(sender, serde_json::to_string(&ErrorKind::UnixSignal)?).await?;
        to_socket(sender, DD_CRASHTRACK_END_KIND).await?;

        to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
        let builder = CrashtrackerConfiguration::builder();
        let config = builder
            .signals(default_signals())
            .timeout(Duration::from_secs(3))
            .use_alt_stack(true)
            .build()?;
        to_socket(sender, serde_json::to_string(&config)?).await?;
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

    #[cfg(feature = "collector_signal-safe")]
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn signal_safe_emitted_report_round_trips_through_receiver_parser() -> anyhow::Result<()>
    {
        use crate::collector_signal_safe as signal_safe;
        use signal_safe::capabilities::{Capabilities, Degradations};

        let config = CrashtrackerConfiguration::builder()
            .signals(default_signals())
            .timeout(Duration::from_secs(3))
            .build()?;
        let config_json = serde_json::to_string(&config)?;
        let signal =
            signal_safe::SignalInfo::new(libc::SIGSEGV, signal_safe::SEGV_MAPERR, 0x1234, true);
        let frames = [0x10usize, 0x20usize];
        let context = signal_safe::CrashContext {
            signal,
            pid: std::process::id() as i32,
            tid: std::process::id() as i32,
            frames: &frames,
        };
        let report = signal_safe::Report {
            config_json: &config_json,
            library_name: "dd-test",
            library_version: "1.2.3",
            family: "native",
            default_service: "default-service",
            service: "svc",
            env: "prod",
            app_version: "v1",
            runtime_id: "rid",
            platform: "linux",
            stage_name: "application",
            stackwalk_method: "fp_pvr",
            capabilities: Capabilities::from_bits(0x21),
            degradations: Degradations::from_bits(1 << 8), // DEGRADED_REPORT_TO_FD
        };

        let mut buf = [0u8; 8192];
        let mut sink = signal_safe::SliceSink::new(&mut buf);
        assert!(signal_safe::emit_report(&mut sink, &report, &context));
        let emitted = sink.as_slice().to_vec();

        let (mut sender, receiver) = tokio::net::UnixStream::pair()?;
        let writer = tokio::spawn(async move {
            sender.write_all(&emitted).await?;
            sender.shutdown().await
        });

        let parsed = receive_report_from_stream(Duration::from_secs(2), BufReader::new(receiver))
            .await?
            .expect("signal-safe report should parse");
        writer.await??;

        let (_config, crashinfo) = parsed;
        assert_eq!(crashinfo.error.kind, ErrorKind::UnixSignal);
        assert_eq!(crashinfo.metadata.library_name, "dd-test");
        assert_eq!(crashinfo.metadata.tags[4], "service:svc");

        let sig_info = crashinfo.sig_info.expect("siginfo parsed");
        assert_eq!(sig_info.si_signo_human_readable, SignalNames::SIGSEGV);
        assert_eq!(sig_info.si_code_human_readable, SiCodes::SEGV_MAPERR);

        let tags = crashinfo
            .experimental
            .expect("additional tags parsed")
            .additional_tags;
        assert!(tags.iter().any(|tag| tag == "stage:application"));
        assert!(tags.iter().any(|tag| tag == "stackwalk_method:fp_pvr"));
        assert!(tags.iter().any(|tag| tag == "report_degraded:report_to_fd"));

        Ok(())
    }

    #[cfg(feature = "collector_signal-safe")]
    #[test]
    fn signal_safe_signal_names_stay_receiver_compatible() -> anyhow::Result<()> {
        use crate::collector_signal_safe as signal_safe;

        let signals = [
            (libc::SIGSEGV, SignalNames::SIGSEGV),
            (libc::SIGABRT, SignalNames::SIGABRT),
            (libc::SIGBUS, SignalNames::SIGBUS),
            (libc::SIGILL, SignalNames::SIGILL),
            (libc::SIGFPE, SignalNames::SIGFPE),
        ];
        for (signum, expected) in signals {
            let siginfo = siginfo_from_signal_safe_names(signum, signal_safe::SI_USER)?;
            assert_eq!(siginfo.si_signo_human_readable, expected);
            assert_eq!(siginfo.si_code_human_readable, SiCodes::SI_USER);
        }

        let sicodes = [
            (
                libc::SIGSEGV,
                signal_safe::SEGV_MAPERR,
                SiCodes::SEGV_MAPERR,
            ),
            (
                libc::SIGSEGV,
                signal_safe::SEGV_ACCERR,
                SiCodes::SEGV_ACCERR,
            ),
            (libc::SIGBUS, signal_safe::BUS_ADRALN, SiCodes::BUS_ADRALN),
            (libc::SIGBUS, signal_safe::BUS_ADRERR, SiCodes::BUS_ADRERR),
            (libc::SIGBUS, signal_safe::BUS_OBJERR, SiCodes::BUS_OBJERR),
            (libc::SIGILL, signal_safe::ILL_ILLOPC, SiCodes::ILL_ILLOPC),
            (libc::SIGILL, signal_safe::ILL_ILLOPN, SiCodes::ILL_ILLOPN),
            (libc::SIGILL, signal_safe::ILL_ILLADR, SiCodes::ILL_ILLADR),
            (libc::SIGILL, signal_safe::ILL_ILLTRP, SiCodes::ILL_ILLTRP),
            (libc::SIGILL, signal_safe::ILL_PRVOPC, SiCodes::ILL_PRVOPC),
            (libc::SIGILL, signal_safe::ILL_PRVREG, SiCodes::ILL_PRVREG),
            (libc::SIGILL, signal_safe::ILL_COPROC, SiCodes::ILL_COPROC),
            (libc::SIGILL, signal_safe::ILL_BADSTK, SiCodes::ILL_BADSTK),
            (libc::SIGFPE, signal_safe::FPE_INTDIV, SiCodes::FPE_INTDIV),
        ];
        for (signum, si_code, expected) in sicodes {
            let siginfo = siginfo_from_signal_safe_names(signum, si_code)?;
            assert_eq!(siginfo.si_code_human_readable, expected);
        }

        let siginfo = siginfo_from_signal_safe_names(libc::SIGSEGV, 999)?;
        assert_eq!(siginfo.si_code_human_readable, SiCodes::UNKNOWN);
        Ok(())
    }

    #[cfg(feature = "collector_signal-safe")]
    fn siginfo_from_signal_safe_names(signum: i32, si_code: i32) -> anyhow::Result<SigInfo> {
        use crate::collector_signal_safe as signal_safe;

        Ok(serde_json::from_value(serde_json::json!({
            "si_addr": null,
            "si_code": si_code,
            "si_code_human_readable": signal_safe::rust_si_code_name(signum, si_code),
            "si_signo": signum,
            "si_signo_human_readable": signal_safe::rust_signal_name(signum),
        }))?)
    }
}
