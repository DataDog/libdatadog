// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use chrono::Utc;
use datadog_profiling::crashtracker::{Configuration, Metadata};
use datadog_profiling::exporter::{self, Tag};
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub fn _print_to_file(data: &[u8]) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let path = format!("{now}.txt");
    let path = Path::new(&path);
    let mut file = File::create(path)?;
    file.write_all(data)?;
    Ok(())
}

pub fn upload_to_dd(
    data: &[u8],
    config: &Configuration,
    metadata: &Metadata,
) -> anyhow::Result<hyper::Response<hyper::Body>> {
    //let site = "intake.profile.datad0g.com/api/v2/profile";
    //let site = "datad0g.com";
    //let api_key = std::env::var("DD_API_KEY")?;
    let tag = match Tag::new("service", "local-crash-test-upload") {
        Ok(tag) => tag,
        Err(e) => anyhow::bail!("{}", e),
    };
    let tags: Option<Vec<Tag>> = Some(vec![tag]);
    let time = Utc::now();
    // TODO make this configurable
    // Comment that this is to prevent us waiting forever and keeping the container alive forever
    let timeout = std::time::Duration::from_secs(30);
    let crash_file = exporter::File {
        name: "crash-info.json",
        bytes: data,
    };
    let exporter = exporter::ProfileExporter::new(
        metadata.profiling_library_name.clone(),
        metadata.profiling_library_version.clone(),
        metadata.family.clone(),
        tags,
        config.endpoint.clone(),
    )?;
    let request = exporter.build(time, time, &[crash_file], &[], None, None, None, timeout)?;
    let response = exporter.send(request, None)?;
    //TODO, do we need to wait a bit for the agent to finish upload?
    Ok(response)
}
