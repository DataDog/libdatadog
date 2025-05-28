// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod flare {

    use serde::Deserialize;

    use crate::RemoteConfigProduct;

    #[derive(Debug, Deserialize)]
    pub struct AgentConfigFile {
        pub name: String,
        pub config: AgentConfig,
    }

    #[derive(Debug, Deserialize)]
    pub struct AgentConfig {
        pub log_level: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct AgentTaskFile {
        pub args: AgentTask,
        pub task_type: String,
        pub uuid: String, // uuid ?
    }

    #[derive(Debug, Deserialize)]
    pub struct AgentTask {
        pub case_id: String, // int ? an other type of id ?
        pub hostname: Option<String>,
        pub user_handle: String, // like a email
    }

    #[derive(Debug, Deserialize)]
    pub enum FlareConfigFile {
        Config(AgentConfigFile),
        Task(AgentTaskFile),
    }

    pub fn parse_json(
        data: &[u8],
        product: RemoteConfigProduct,
    ) -> serde_json::error::Result<FlareConfigFile> {
        match product {
            RemoteConfigProduct::AgentConfig => match serde_json::from_slice(data) {
                Ok(config) => Ok(FlareConfigFile::Config(config)),
                Err(err) => Err(err),
            },
            RemoteConfigProduct::AgentTask => match serde_json::from_slice(data) {
                Ok(config) => Ok(FlareConfigFile::Task(config)),
                Err(err) => Err(err),
            },
            _ => unreachable!(),
        }
    }
}
