// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    if std::env::var_os("DD_TEST_REPLACE_STACKTRACE").is_some() {
        return receive_with_sentinel_stack();
    }
    libdd_crashtracker::receiver_entry_point_stdin()
}

/// Replace the collector's stack with a sentinel while preserving its real
/// ucontext and the live connection to the crashed process. This lets a bin
/// test verify that receiver-side unwinding actually promotes the crash stack.
#[cfg(unix)]
fn receive_with_sentinel_stack() -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    const BEGIN_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_STACKTRACE";
    const END_STACKTRACE: &str = "DD_CRASHTRACK_END_STACKTRACE";
    const DONE: &str = "DD_CRASHTRACK_DONE";
    const SENTINEL_FRAME: &str = "{\"function\":\"collector_fallback_sentinel\"}";

    // The collector waits for this connection to close. Reading until EOF
    // would deadlock; retain the original stdin handle through symbolization.
    let stdin = std::io::stdin();
    let mut source = stdin.lock();
    let mut rewritten = Vec::new();
    let mut in_stacktrace = false;
    let mut replacements = 0;

    loop {
        let mut line = String::new();
        anyhow::ensure!(
            source.read_line(&mut line)? != 0,
            "report ended before DONE"
        );
        let marker = line.trim_end_matches(['\r', '\n']);

        match marker {
            BEGIN_STACKTRACE => {
                anyhow::ensure!(!in_stacktrace, "nested stacktrace block");
                in_stacktrace = true;
                replacements += 1;
                rewritten.write_all(line.as_bytes())?;
                writeln!(rewritten, "{SENTINEL_FRAME}")?;
            }
            END_STACKTRACE => {
                anyhow::ensure!(in_stacktrace, "unexpected end of stacktrace block");
                in_stacktrace = false;
                rewritten.write_all(line.as_bytes())?;
            }
            _ if !in_stacktrace => rewritten.write_all(line.as_bytes())?,
            _ => {}
        }

        if marker == DONE {
            break;
        }
    }

    anyhow::ensure!(!in_stacktrace, "unterminated stacktrace block");
    anyhow::ensure!(
        replacements == 1,
        "expected one stacktrace block, found {replacements}"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stream = tokio::io::BufReader::new(std::io::Cursor::new(rewritten));
    runtime.block_on(libdd_crashtracker::async_receiver_entry_point_stream(
        stream,
    ))?;
    Ok(())
}
