// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use ddcommon::cstr;
use ddcommon_ffi::{self as ffi};
use std::{io::ErrorKind, path::PathBuf};


#[derive(Debug)]
pub struct Configurator {
    pub debug_logs: bool,
    #[allow(dead_code)]
    pub static_config_file_path: PathBuf,
}

#[repr(C)]
#[derive(Clone, Copy, serde::Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::enum_variant_names)]
pub enum LibraryConfigName {
    DdTraceDebug = 0,
    DdService = 1,
    DdEnv = 2,
    DdVersion = 3,
    DdProfilingEnabled = 4,
}

impl LibraryConfigName {
    pub fn to_env_name(self) -> &'static std::ffi::CStr {
        use LibraryConfigName::*;
        match self {
            DdTraceDebug => cstr!("DD_TRACE_DEBUG"),
            DdService => cstr!("DD_SERVICE"),
            DdEnv => cstr!("DD_ENV"),
            DdVersion => cstr!("DD_VERSION"),
            DdProfilingEnabled => cstr!("DD_PROFILING_ENABLED"),
        }
    }
}

#[repr(C)]
pub struct ProcessInfo<'a> {
    pub args: ffi::Slice<'a, ffi::CharSlice<'a>>,
    pub envp: ffi::Slice<'a, ffi::CharSlice<'a>>,
    pub language: ffi::CharSlice<'a>,
}

impl Configurator {
    pub fn new(debug_logs: bool, static_config_file_path: PathBuf) -> Self {
        Self {
            debug_logs,
            static_config_file_path,
        }
    }

    pub fn get_configuration(
        &self,
        process_info: ProcessInfo<'_>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let static_config = self.parse_static_config()?;
        if self.debug_logs {
            eprintln!("Read the following static config: {static_config:?}");
        }

        let Some(configs) = find_static_config(&static_config, &process_info) else {
            if self.debug_logs {
                eprintln!("No selector matched");
            }
            return Ok(Vec::new());
        };
        let library_config = template_configs(configs, &process_info)?;
        if self.debug_logs {
            eprintln!("Will apply the following configuration: {library_config:?}");
        }
        Ok(library_config)
    }

    pub fn get_configuration_from_bytes(
        &self,
        process_info: ProcessInfo<'_>,
        config_bytes: ffi::CharSlice<'_>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let static_config = serde_yaml::from_str(config_bytes.to_string().as_str())?;
        if self.debug_logs {
            eprintln!("Read the following static config: {static_config:?}");
        }

        let Some(configs) = find_static_config(&static_config, &process_info) else {
            if self.debug_logs {
                eprintln!("No selector matched");
            }
            return Ok(Vec::new());
        };
        let library_config = template_configs(configs, &process_info)?;
        if self.debug_logs {
            eprintln!("Will apply the following configuration: {library_config:?}");
        }
        Ok(library_config)
    }

    pub fn log_process_info(&self, process_info: &ProcessInfo) {
        if self.debug_logs {
            eprintln!("Called library_config_common_component:");
            eprintln!("\tconfigurator: {:?}", self);
            let args: Vec<String> = process_info
                .args
                .iter()
                .map(|arg| arg.to_string())
                .collect();
            eprintln!("\tprocess args: {:?}", args);
            // TODO: this is for testing purpose, we don't want to log env variables
            let envs: Vec<String> = process_info
                .envp
                .iter()
                .map(|env| env.to_string())
                .collect();
            eprintln!("\tprocess envs: {:?}", envs);
            eprintln!(
                "\tprocess language: {:?}",
                process_info.language.to_string()
            );
        }
    }

    fn parse_static_config(&self) -> anyhow::Result<StaticConfig> {
        let mut f = match std::fs::File::open(&self.static_config_file_path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if self.debug_logs {
                    eprintln!(
                        "Static config file not found at {:?} returning empty rules",
                        self.static_config_file_path
                    );
                }
                return Ok(StaticConfig::default());
            }
            Err(e) => return Err(anyhow!(e)),
        };
        Ok(serde_yaml::from_reader(&mut f)?)
    }
}
