// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::crash_handler::register_crash_handler;
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};

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
    pub endpoint: Endpoint,
    pub path_to_reciever_binary: String,
}

impl Configuration {
    pub fn new(endpoint: Endpoint, path_to_reciever_binary: String) -> Self {
        Self {
            endpoint,
            path_to_reciever_binary,
        }
    }
}

//TODO pass key/value pairs to the reciever.
pub fn init(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    //TODO, do something to stderr/stdout eventually
    //TODO, figure out what happens on fork.
    let receiver = Command::new(&config.path_to_reciever_binary)
        .arg("reciever")
        .stdin(Stdio::piped())
        .spawn()?;

    // Write the args into the reciever.
    // Use the pipe to avoid secrets ending up on the commandline
    writeln!(
        receiver.stdin.as_ref().unwrap(),
        "{}",
        serde_json::to_string(&config)?
    )?;
    writeln!(
        receiver.stdin.as_ref().unwrap(),
        "{}",
        serde_json::to_string(&metadata)?
    )?;

    register_crash_handler(receiver)?;
    Ok(())
}
