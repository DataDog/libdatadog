// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(tokio_taskdump)]
use chrono::{DateTime, Utc};
#[cfg(tokio_taskdump)]
use std::time::{Duration, SystemTime};
#[cfg(tokio_taskdump)]
use tokio::time::timeout;

#[cfg(not(tokio_taskdump))]
pub async fn dump() -> String {
    "".to_string()
}

#[cfg(tokio_taskdump)]
pub async fn dump() -> String {
    let mut dumps = "".to_string();
    if let Some(traces) = dump_tasks().await {
        dumps.push_str(&traces);
    }
    dumps
}

#[cfg(tokio_taskdump)]
async fn dump_tasks() -> Option<String> {
    let handle = tokio::runtime::Handle::current();
    if let Ok(dump) = timeout(Duration::from_secs(2), handle.dump()).await {
        let mut log = format!(
            "All tasks running at {}\n",
            DateTime::<Utc>::from(SystemTime::now())
        );
        for (i, task) in dump.tasks().iter().enumerate() {
            let trace = task.trace();
            log.push_str(&format!("task {i} trace:\n{trace}\n\n"));
        }
        Some(log)
    } else {
        None
    }
}
