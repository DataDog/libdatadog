// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use datadog_profiling::crashtracker::*;
use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};
use uuid::Uuid;

// The Bools track if we need a comma preceding the next item
#[derive(Debug)]
enum StdinState {
    Counters {
        comma_needed: bool,
    },
    SigInfo {
        comma_needed: bool,
    },
    StackTrace {
        comma_needed: bool,
    },
    Waiting,
    File {
        comma_needed: bool,
        filename: String,
    },
}

// // TODO, add commas as needed
fn process_line(w: &mut impl Write, line: String, state: StdinState) -> anyhow::Result<StdinState> {
    let next = match state {
        StdinState::SigInfo { comma_needed } => {
            if line.starts_with(DD_CRASHTRACK_END_SIGINFO) {
                write!(w, "\n}}")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t{line}")?;
                StdinState::SigInfo { comma_needed: true }
            }
        }
        StdinState::Counters { comma_needed } => {
            if line.starts_with(DD_CRASHTRACK_END_COUNTERS) {
                write!(w, "\n}}")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t{line}")?;
                StdinState::Counters { comma_needed: true }
            }
        }
        StdinState::Waiting => {
            if line.starts_with(DD_CRASHTRACK_BEGIN_FILE) {
                let (_, filename) = line.split_once(' ').unwrap_or(("", "MISSING_FILENAME"));
                writeln!(w, ",")?;
                writeln!(w, "{filename}: [")?;
                StdinState::File {
                    comma_needed: false,
                    filename: filename.to_string(),
                }
            } else if line.starts_with(DD_CRASHTRACK_BEGIN_STACKTRACE) {
                writeln!(w, ",")?;
                writeln!(w, "\"stacktrace\": [")?;
                StdinState::StackTrace {
                    comma_needed: false,
                }
            } else if line.starts_with(DD_CRASHTRACK_BEGIN_SIGINFO) {
                writeln!(w, ",")?;
                writeln!(w, "\"siginfo\": {{")?;
                StdinState::SigInfo {
                    comma_needed: false,
                }
            } else if line.starts_with(DD_CRASHTRACK_BEGIN_COUNTERS) {
                writeln!(w, ",")?;
                writeln!(w, "\"counters\": {{")?;
                StdinState::Counters {
                    comma_needed: false,
                }
            } else {
                eprint!("Unexpected line: {line}");
                StdinState::Waiting
            }
        }
        StdinState::File {
            comma_needed,
            filename,
        } => {
            if line.starts_with(DD_CRASHTRACK_END_FILE) {
                write!(w, "\n]")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t\"{line}\"")?;
                StdinState::File {
                    comma_needed: true,
                    filename,
                }
            }
        }
        StdinState::StackTrace { comma_needed } => {
            if line.starts_with(DD_CRASHTRACK_END_STACKTRACE) {
                write!(w, "]")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t{line}")?;
                StdinState::StackTrace { comma_needed: true }
            }
        }
    };
    Ok(next)
}

fn emit_text_file_as_json(w: &mut impl Write, filename: &str) -> anyhow::Result<()> {
    let file = File::open(filename)?;
    writeln!(w, ",")?;
    writeln!(w, "{filename}: [")?;

    let mut comma_needed = false;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if comma_needed {
            writeln!(w, ",")?;
        }
        write!(w, "\t\"{line}\"")?;
        comma_needed = true;
    }
    writeln!(w, "]")?;
    Ok(())
}

fn emit_json_prefix(w: &mut impl Write, uuid: &Uuid, metadata: &Metadata) -> anyhow::Result<()> {
    writeln!(w, "{{")?;

    // uuid
    writeln!(w, "\"uuid\": \"{uuid}\",")?;

    // OS info
    let info = os_info::get();
    writeln!(w, "\"os_info\": {},", serde_json::to_string(&info)?)?;
    // No trailing comma or newline on final entry
    write!(w, "\"metadata\": {}", serde_json::to_string(&metadata)?)?;

    #[cfg(target_os = "linux")]
    emit_text_file_as_json(w, "/proc/meminfo")?;

    #[cfg(target_os = "linux")]
    emit_text_file_as_json(w, "/proc/cpuinfo")?;

    Ok(())
}

fn emit_json_suffix(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "\n}}")?;

    Ok(())
}

pub enum CrashReportStatus {
    NoCrash,
    CrashReport(Vec<u8>),
    PartialCrashReport(Vec<u8>),
}

pub fn receive_report(metadata: &Metadata) -> anyhow::Result<CrashReportStatus> {
    let uuid = Uuid::new_v4();
    let mut buf = vec![];
    emit_json_prefix(&mut buf, &uuid, metadata)?;
    let mut stdin_state = StdinState::Waiting;
    let mut crash_seen = false;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        stdin_state = process_line(&mut buf, line, stdin_state)?;
        if matches!(stdin_state, StdinState::SigInfo { .. }) {
            crash_seen = true;
        }
    }
    if !crash_seen {
        Ok(CrashReportStatus::NoCrash)
    } else if matches!(stdin_state, StdinState::Waiting) {
        emit_json_suffix(&mut buf)?;
        Ok(CrashReportStatus::CrashReport(buf))
    } else {
        Ok(CrashReportStatus::PartialCrashReport(buf))
    }
}
