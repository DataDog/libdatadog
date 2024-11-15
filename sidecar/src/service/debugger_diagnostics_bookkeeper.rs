// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use datadog_live_debugger::debugger_defs::{
    DebuggerData, DebuggerPayload, Diagnostics, ProbeStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio_util::sync::CancellationToken;

pub struct DebuggerDiagnosticsBookkeeper {
    active_by_runtime_id: Arc<Mutex<HashMap<String, DebuggerActiveData>>>,
    cancel: CancellationToken,
}

struct LastProbeStatus {
    status: ProbeStatus,
    last_update: Instant,
}

#[derive(Default)]
struct DebuggerActiveData {
    pub active_probes: HashMap<String, LastProbeStatus>,
}

const MAX_TIME_BEFORE_FALLBACK: Duration = Duration::from_secs(300);
const MAX_TIME_BEFORE_REMOVAL: Duration = Duration::from_secs(600);

impl DebuggerDiagnosticsBookkeeper {
    pub fn start() -> DebuggerDiagnosticsBookkeeper {
        let buffer = DebuggerDiagnosticsBookkeeper {
            active_by_runtime_id: Default::default(),
            cancel: CancellationToken::new(),
        };
        let active = buffer.active_by_runtime_id.clone();
        let cancel = buffer.cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(MAX_TIME_BEFORE_REMOVAL / 2);
            loop {
                select! {
                    _ = interval.tick() => {
                        active.lock().unwrap().retain(|_, active| {
                            active.active_probes.retain(|_, status| {
                                status.last_update.elapsed() < MAX_TIME_BEFORE_REMOVAL
                            });
                            !active.active_probes.is_empty()
                        });
                    },
                    _ = cancel.cancelled() => {
                        break;
                    },
                }
            }
        });
        buffer
    }

    pub fn add_payload(&self, payload: &DebuggerPayload) -> bool {
        if let DebuggerData::Diagnostics(diagnostics) = &payload.debugger {
            let mut send = true;

            fn insert_probe(active_data: &mut DebuggerActiveData, diagnostics: &Diagnostics) {
                active_data.active_probes.insert(
                    diagnostics.probe_id.to_string(),
                    LastProbeStatus {
                        status: diagnostics.status,
                        last_update: Instant::now(),
                    },
                );
            }

            let mut buffers = self.active_by_runtime_id.lock().unwrap();
            let runtime_id = diagnostics
                .parent_id
                .as_ref()
                .unwrap_or(&diagnostics.runtime_id);
            if let Some(buffer) = buffers.get_mut(runtime_id.as_ref()) {
                if let Some(status) = buffer.active_probes.get_mut(diagnostics.probe_id.as_ref()) {
                    // This is a bit confusing now, but clippy requested me to collapse this:
                    // Essentially, we shall send if the last emitted/error/installed/etc. is older
                    // than MAX_TIME_BEFORE_FALLBACK. If it's installed, we also
                    // send it the current status is Received.
                    send = matches!(status.status, ProbeStatus::Received)
                        || (!matches!(diagnostics.status, ProbeStatus::Received)
                            && (matches!(status.status, ProbeStatus::Installed)
                                || !matches!(diagnostics.status, ProbeStatus::Installed)))
                        || status.last_update.elapsed() > MAX_TIME_BEFORE_FALLBACK;
                    if send {
                        status.last_update = Instant::now();
                        if status.status != diagnostics.status {
                            status.status = diagnostics.status;
                        } else {
                            send = false;
                        }
                    }
                } else {
                    insert_probe(buffer, diagnostics);
                }
            } else {
                buffers.insert(runtime_id.to_string(), {
                    let mut data = DebuggerActiveData::default();
                    insert_probe(&mut data, diagnostics);
                    data
                });
            }

            send
        } else {
            unreachable!("This is only for diagnostics");
        }
    }

    pub fn stats(&self) -> DebuggerDiagnosticsBookkeeperStats {
        let buffers = self.active_by_runtime_id.lock().unwrap();
        DebuggerDiagnosticsBookkeeperStats {
            runtime_ids: buffers.len() as u32,
            total_probes: buffers
                .iter()
                .map(|(_, active)| active.active_probes.len() as u32)
                .sum(),
        }
    }
}

impl Default for DebuggerDiagnosticsBookkeeper {
    fn default() -> Self {
        Self::start()
    }
}

impl Drop for DebuggerDiagnosticsBookkeeper {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Serialize, Deserialize)]
pub struct DebuggerDiagnosticsBookkeeperStats {
    runtime_ids: u32,
    total_probes: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_live_debugger::debugger_defs::{
        DebuggerData, DebuggerPayload, Diagnostics, ProbeStatus,
    };
    use std::borrow::Cow;

    fn create_payload<'a>(
        probe_id: &'a str,
        runtime_id: &'a str,
        status: ProbeStatus,
    ) -> DebuggerPayload<'a> {
        DebuggerPayload {
            service: Default::default(),
            ddsource: Default::default(),
            timestamp: 0,
            debugger: DebuggerData::Diagnostics(Diagnostics {
                probe_id: Cow::Borrowed(probe_id),
                runtime_id: Cow::Borrowed(runtime_id),
                parent_id: None,
                probe_version: 0,
                status,
                exception: None,
                details: None,
            }),
            message: None,
        }
    }

    #[tokio::test]
    async fn test_bookkeeper() {
        let bookkeeper = DebuggerDiagnosticsBookkeeper::start();
        assert!(bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Received)));
        // Second insert of same thing is rejected
        assert!(!bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Received)));
        // Different thing is allowed
        assert!(bookkeeper.add_payload(&create_payload("1", "3", ProbeStatus::Received)));
        assert!(bookkeeper.add_payload(&create_payload("2", "2", ProbeStatus::Received)));

        // We can move to installed
        assert!(bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Installed)));
        // But not back
        assert!(!bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Received)));
        assert!(!bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Installed)));

        // We can move to e.g. error or emitting
        assert!(bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Emitting)));
        assert!(bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Error)));
        assert!(bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Emitting)));
        // But not back
        assert!(!bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Received)));
        assert!(!bookkeeper.add_payload(&create_payload("1", "2", ProbeStatus::Installed)));
    }
}
