// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{
    counters::reset_counters,
    crash_handler::{
        register_crash_handlers, replace_receiver, restore_old_handlers, setup_receiver,
        shutdown_receiver,
    },
};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    pub tags: Option<Vec<Tag>>,
}

impl Metadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Option<Vec<Tag>>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    pub create_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub output_filename: Option<String>,
    pub path_to_reciever_binary: String,
    pub resolve_frames: bool,
}

impl Configuration {
    pub fn new(
        create_alt_stack: bool,
        endpoint: Option<Endpoint>,
        output_filename: Option<String>,
        path_to_reciever_binary: String,
        resolve_frames: bool,
    ) -> Self {
        Self {
            create_alt_stack,
            endpoint,
            output_filename,
            path_to_reciever_binary,
            resolve_frames,
        }
    }
}

pub fn shutdown_crash_handler() -> anyhow::Result<()> {
    restore_old_handlers()?;
    shutdown_receiver()?;
    Ok(())
}

// Would you prefer this to cache the configuration and metadata?
/// Safety: This is not atomic.  There should be no other profiler operations
/// occuring while this is running.
pub fn on_fork(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    reset_counters()?;
    // Leave the old signal handler in place
    replace_receiver(&config, &metadata)?;
    Ok(())
}

//TODO pass key/value pairs to the reciever.
pub fn init(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    setup_receiver(&config, &metadata)?;
    register_crash_handlers(&config)?;
    Ok(())
}

#[ignore]
#[test]
fn test_crash() {
    use crate::crashtracker::begin_profiling_op;
    use chrono::Utc;

    let endpoint = None;
    let output_filename = Some(format!("/tmp/crashreports/{}.txt", Utc::now().to_rfc3339()));

    #[cfg(target_os = "macos")]
    let path_to_reciever_binary = "/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/target/debug/profiling-crashtracking-receiver".to_string();
    #[cfg(target_os = "linux")]
    let path_to_reciever_binary =
        "/tmp/libdatadog/debug/profiling-crashtracking-receiver".to_string();
    let create_alt_stack = true;
    let resolve_frames = true;
    let config = Configuration::new(
        create_alt_stack,
        endpoint,
        output_filename,
        path_to_reciever_binary,
        resolve_frames,
    );
    let metadata = Metadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        None,
    );
    init(config, metadata).expect("not to fail");
    begin_profiling_op(crate::crashtracker::ProfilingOpTypes::CollectingSample)
        .expect("Not to fail");
    let p: *const u32 = std::ptr::null();
    let q = unsafe { *p };
    assert_eq!(q, 3);
}
