use chrono::{DateTime, Utc};
use datadog_profiling::exporter::{self, Tag};
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use uuid::Uuid;

fn _print_to_file(data: &[u8]) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let path = format!("{now}.txt");
    let path = Path::new(&path);
    let mut file = File::create(path)?;
    file.write_all(data)?;
    Ok(())
}

fn upload_to_dd(data: &[u8]) -> anyhow::Result<hyper::Response<hyper::Body>> {
    let site = "intake.profile.datad0g.com/api/v2/profile";
    let api_key = std::env::var("DD_API_KEY")?;
    let endpoint = exporter::config::agentless(site, api_key)?;
    let profiling_library_name = "dd_trace_py";
    let profiling_library_version = "1.2.3";
    let family = "";
    let tag = match Tag::new("service", "local-crash-test-upload") {
        Ok(tag) => tag,
        Err(e) => anyhow::bail!("{}", e),
    };
    let tags = Some(vec![tag]);
    let time = Utc::now();
    let timeout = std::time::Duration::from_secs(30);
    let crash_file = exporter::File {
        name: "crash-info.json",
        bytes: data,
    };
    let exporter = exporter::ProfileExporter::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
    )?;
    let request = exporter.build(
        time.clone(),
        time,
        &[crash_file],
        &[],
        None,
        None,
        None,
        timeout,
    )?;
    let response = exporter.send(request, None)?;
    Ok(response)
}

/// Recieves data on stdin, and forwards it to somewhere its useful
/// For now, just sent to a file.
/// Future enhancement: set of key/value pairs sent over pipe to setup
/// Future enhancement: publish to DD endpoint
pub fn main() -> anyhow::Result<()> {
    let uuid = Uuid::new_v4();
    let mut buf = vec![];
    let stdin = std::io::stdin();
    writeln!(buf, "{uuid}")?;
    for line in stdin.lock().lines() {
        let line = line?;
        writeln!(buf, "{}", line)?;
    }
    _print_to_file(&buf)?;
    upload_to_dd(&buf)?;
    Ok(())
}
