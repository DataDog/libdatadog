use std::time::{Duration, SystemTime};
use chrono::{DateTime, Utc};
use tokio::time::timeout;

pub async fn dump() -> String {
    let mut dumps = "".to_string();
    if let Some(traces) = dump_tasks().await {
        dumps.push_str(&traces);
    }
    dumps
}

async fn dump_tasks() -> Option<String> {
    let handle = tokio::runtime::Handle::current();
    if let Ok(dump) = timeout(Duration::from_secs(2), handle.dump()).await {
        let mut log = format!("All tasks running at {}\n", DateTime::<Utc>::from(SystemTime::now()));
        for (i, task) in dump.tasks().iter().enumerate() {
            let trace = task.trace();
            log.push_str(&format!("task {i} trace:\n{trace}\n\n"));
        }
        Some(log)
    } else {
        None
    }
}