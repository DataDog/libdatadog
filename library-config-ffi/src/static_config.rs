// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use ddcommon::cstr;
use ddcommon_ffi::{self as ffi};
use std::{collections::HashMap, io::ErrorKind, path::PathBuf};

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Origin {
    ProcessArguments,
    EnvironmentVariable,
    Language,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Operator {
    Equals,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
struct Selector {
    origin: Origin,
    matches: Vec<String>,
    operator: Operator,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
struct Rule {
    selectors: Vec<Selector>,
    configuration: HashMap<LibraryConfigName, String>,
}

#[derive(serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct StaticConfig {
    rules: Vec<Rule>,
}

fn find_static_config<'a, 'b>(
    cfg: &'a StaticConfig,
    process_info: &ProcessInfo<'b>,
) -> Option<&'a HashMap<LibraryConfigName, String>> {
    for rule in &cfg.rules {
        if rule
            .selectors
            .iter()
            .all(|s| selector_match(&s, process_info))
        {
            return Some(&rule.configuration);
        }
    }
    None
}

fn selector_match(selector: &Selector, process_info: &ProcessInfo) -> bool {
    // todo: match selectors
    true
}

fn template_configs(
    config: &HashMap<LibraryConfigName, String>,
    process_info: &ProcessInfo,
) -> anyhow::Result<Vec<LibraryConfig>> {
    config
        .iter()
        .map(|(&name, v)| {
            Ok(LibraryConfig {
                name: name,
                value: LibraryConfigValue::StrVal(ffi::CString::new(template_config(
                    v,
                    process_info,
                ))?),
            })
        })
        .collect()
}

fn template_config(config_val: &str, process_info: &ProcessInfo) -> String {
    // todo: template configuration
    config_val.to_owned()
}

#[derive(Debug)]
pub struct Configurator {
    pub debug_logs: bool,
    #[allow(dead_code)]
    pub static_config_file_path: PathBuf,
}

#[repr(C)]
#[derive(Clone, Copy, serde::Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibraryConfigName {
    DdTraceDebug = 0,
    DdProfilingEnabled =  1,
}

impl LibraryConfigName {
    pub fn to_env_name(self) -> &'static std::ffi::CStr {
        use LibraryConfigName::*;
        match self {
            DdTraceDebug => cstr!("DD_TRACE_DEBUG"),
            DdProfilingEnabled => cstr!("DD_PROFILING_ENABLED"),
        }
    }
}

#[repr(C)]
#[derive(Debug)]
pub enum LibraryConfigValue {
    NumVal(i64),
    BoolVal(bool),
    StrVal(ffi::CString),
}

#[repr(C)]
#[derive(Debug)]
pub struct LibraryConfig {
    pub name: LibraryConfigName,
    pub value: LibraryConfigValue,
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

    pub fn get_configuration<'a>(
        &self,
        process_info: ProcessInfo<'a>,
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
        Ok(vec![LibraryConfig {
            name: LibraryConfigName::DdTraceDebug,
            value: LibraryConfigValue::BoolVal(true),
        }])
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::static_config::{LibraryConfigName, Operator, Origin, Rule, Selector, StaticConfig};

    use super::Configurator;

    macro_rules! map {
        ($(($key:expr , $value:expr)),* $(,)?) => {
            {
                #[allow(unused_mut)]
                let mut map = std::collections::HashMap::new();
                $(
                    map.insert($key, $value);
                )*
                map
            }
        };
    }

    #[test]
    fn test_parse_static_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.as_file_mut()
            .write_all(
                b"
rules:
- selectors:
  - origin: language
    matches: [\"java\"]
    operator: equals
  configuration:
    DD_PROFILING_ENABLED: true
",
            )
            .unwrap();
        let confgurator = Configurator::new(true, tmp.path().to_path_buf());
        let cfg = confgurator.parse_static_config().unwrap();
        assert_eq!(
            cfg,
            StaticConfig {
                rules: vec![Rule {
                    selectors: vec![Selector {
                        origin: Origin::Language,
                        matches: vec!["java".to_owned()],
                        operator: Operator::Equals,
                    }],
                    configuration: map![(LibraryConfigName::DdProfilingEnabled, "true".to_owned()),],
                }]
            }
        )
    }
}
