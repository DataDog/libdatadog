// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::crash_handler::{
    register_crash_handler, replace_receiver, restore_old_handler, setup_receiver,
    shutdown_receiver,
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

pub fn shutdown_crash_handler() -> anyhow::Result<()> {
    restore_old_handler()?;
    shutdown_receiver()?;
    Ok(())
}

// Would you prefer this to cache the configuration and metadata?
pub fn on_fork(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    // Leave the old signal handler in place
    replace_receiver(&config, &metadata)?;
    Ok(())
}

//TODO pass key/value pairs to the reciever.
pub fn init(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    setup_receiver(&config, &metadata)?;
    register_crash_handler()?;
    Ok(())
}
