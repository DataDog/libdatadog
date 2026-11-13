// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared debug-log capture for tests. `log::set_logger` can only succeed once
//! per process, so every test that inspects logs must use this logger.

use std::sync::{Mutex, Once};
use std::thread::ThreadId;

#[derive(Clone)]
pub(crate) struct CapturedLog {
    pub(crate) level: log::Level,
    pub(crate) message: String,
}

struct CapturingLogger {
    records: Mutex<Vec<(ThreadId, CapturedLog)>>,
}

impl log::Log for CapturingLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.records.lock().unwrap().push((
                std::thread::current().id(),
                CapturedLog {
                    level: record.level(),
                    message: record.args().to_string(),
                },
            ));
        }
    }

    fn flush(&self) {}
}

static TEST_LOGGER: CapturingLogger = CapturingLogger {
    records: Mutex::new(Vec::new()),
};
static INIT_LOGGER: Once = Once::new();

/// Starts capturing for the current thread. Records are kept per thread so
/// tests running in parallel do not see or clear each other's logs.
pub(crate) fn start_log_capture() {
    INIT_LOGGER.call_once(|| {
        let _ = log::set_logger(&TEST_LOGGER);
        log::set_max_level(log::LevelFilter::Debug);
    });
    let current = std::thread::current().id();
    TEST_LOGGER
        .records
        .lock()
        .unwrap()
        .retain(|(thread, _)| *thread != current);
}

pub(crate) fn captured_logs() -> Vec<CapturedLog> {
    let current = std::thread::current().id();
    TEST_LOGGER
        .records
        .lock()
        .unwrap()
        .iter()
        .filter(|(thread, _)| *thread == current)
        .map(|(_, record)| record.clone())
        .collect()
}
