// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::{fs, io};

use anyhow::Context;

/// This struct holds maps used to match and template configurations.
///
/// They are computed lazily so that if the templating feature is not necessary, we don't
/// have to create the maps.
///
/// Maps
///  * tags: This one is fairly simple, the format is tag_key: tag_value
///  * envs: Splits env variables based on the KEY=VALUE
///  * args: Either splits args base on key=value, or if the argument is a long arg then parses
///    --key value
struct MatchMaps<'a> {
    tags: &'a HashMap<String, String>,
    env_map: OnceCell<HashMap<&'a str, &'a str>>,
    args_map: OnceCell<HashMap<&'a str, &'a str>>,
}

impl<'a> MatchMaps<'a> {
    fn env(
        &self,
        process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
    ) -> &HashMap<&'a str, &'a str> {
        self.env_map.get_or_init(|| {
            let mut map = HashMap::new();
            for e in process_info.envp {
                let Ok(s) = std::str::from_utf8(e.deref()) else {
                    continue;
                };
                let (k, v) = match s.split_once('=') {
                    Some((k, v)) => (k, v),
                    None => (s, ""),
                };
                map.insert(k, v);
            }
            map
        })
    }

    fn args(
        &self,
        process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
    ) -> &HashMap<&str, &str> {
        self.args_map.get_or_init(|| {
            let mut map = HashMap::new();
            let mut args = process_info.args.iter().peekable();
            loop {
                let Some(arg) = args.next() else {
                    break;
                };
                let Ok(arg) = std::str::from_utf8(arg.deref()) else {
                    continue;
                };
                if let Some((k, v)) = arg.split_once('=') {
                    map.insert(k, v);
                } else if args
                    .peek()
                    .map(|next_arg| next_arg.starts_with(b"-"))
                    .unwrap_or(false)
                {
                    let Ok(next) = std::str::from_utf8(args.next().unwrap()) else {
                        continue;
                    };
                    map.insert(arg, next);
                }
            }
            map
        })
    }
}

struct Matcher<'a, T: Deref<Target = [u8]>> {
    process_info: &'a ProcessInfo<'a, T>,
    match_maps: MatchMaps<'a>,
}

impl<'a, T: Deref<Target = [u8]>> Matcher<'a, T> {
    fn new(process_info: &'a ProcessInfo<'a, T>, tags: &'a HashMap<String, String>) -> Self {
        Self {
            process_info,
            match_maps: MatchMaps {
                tags,
                env_map: OnceCell::new(),
                args_map: OnceCell::new(),
            },
        }
    }

    fn find_stable_config<'b>(
        &'a self,
        cfg: &'b StableConfig,
    ) -> Option<&'b HashMap<LibraryConfigName, String>> {
        for rule in &cfg.rules {
            if rule.selectors.iter().all(|s| self.selector_match(s)) {
                return Some(&rule.configuration);
            }
        }
        None
    }

    // Returns true if the selector matches the process info
    // Any element in the "matches" section of the selector must match, they are ORed,
    // as selectors are ANDed.
    fn selector_match(&'a self, selector: &Selector) -> bool {
        match selector.origin {
            Origin::Language => string_selector(selector, self.process_info.language.deref()),
            Origin::ProcessArguments => match &selector.key {
                Some(key) => {
                    let arg_map = self.match_maps.args(self.process_info);
                    map_operator_match(selector, arg_map, key)
                }
                None => string_list_selector(selector, self.process_info.args),
            },
            Origin::EnvironmentVariables => match &selector.key {
                Some(key) => {
                    let env_map = self.match_maps.env(self.process_info);
                    map_operator_match(selector, env_map, key)
                }
                None => string_list_selector(selector, self.process_info.envp),
            },
            Origin::Tags => match &selector.key {
                Some(key) => map_operator_match(selector, self.match_maps.tags, key),
                None => false,
            },
        }
    }

    fn template_configs(
        &'a self,
        config: &HashMap<LibraryConfigName, String>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        config
            .iter()
            .map(|(&name, v)| {
                Ok(LibraryConfig {
                    name,
                    value: self.template_config(v)?,
                })
            })
            .collect()
    }

    /// Templates a config string.
    ///
    /// variables are enclosed in double curly brackets "{{" and "}}"
    ///
    /// For instance:
    ///
    /// with the following varriable definition, var = "abc" var2 = "def", this transforms \
    /// "foo_{{ var }}_bar_{{ var2 }}" -> "foo_abc_bar_def"
    fn template_config(&'a self, config_val: &str) -> anyhow::Result<String> {
        let mut rest = config_val;
        let mut templated = String::with_capacity(config_val.len());
        loop {
            let Some((head, after_bracket)) = rest.split_once("{{") else {
                templated.push_str(rest);
                return Ok(templated);
            };
            templated.push_str(head);
            let Some((template_var, tail)) = after_bracket.split_once("}}") else {
                anyhow::bail!("unterminated template in config")
            };
            let (template_var, index) = parse_template_var(template_var.trim());
            let val = match template_var {
                "language" => String::from_utf8_lossy(self.process_info.language.deref()),
                "environment_variables" => {
                    template_map_key(index, self.match_maps.env(self.process_info))
                }
                "process_arguments" => {
                    template_map_key(index, self.match_maps.args(self.process_info))
                }
                "tags" => template_map_key(index, self.match_maps.tags),
                _ => std::borrow::Cow::Borrowed("UNDEFINED"),
            };
            templated.push_str(&val);
            rest = tail;
        }
    }
}

fn map_operator_match(selector: &Selector, map: &impl Get, key: &str) -> bool {
    let Some(val) = map.get(key) else {
        return false;
    };
    string_selector(selector, val.as_bytes())
}

fn parse_template_var(template_var: &str) -> (&str, Option<&str>) {
    match template_var.trim().split_once('[') {
        Some((template_var, idx)) => {
            let Some((index, _)) = idx.split_once(']') else {
                return (template_var, None);
            };
            (template_var, Some(index.trim()))
        }
        None => (template_var, None),
    }
}

fn template_map_key<'a>(key: Option<&str>, map: &'a impl Get) -> Cow<'a, str> {
    let Some(key) = key else {
        return Cow::Borrowed("UNDEFINED");
    };
    Cow::Borrowed(map.get(key).unwrap_or("UNDEFINED"))
}

#[repr(C)]
pub struct ProcessInfo<'a, T: Deref<Target = [u8]>> {
    pub args: &'a [T],
    pub envp: &'a [T],
    pub language: T,
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
    pub fn to_str(&self) -> &'static str {
        use LibraryConfigName::*;
        match self {
            DdTraceDebug => "DD_TRACE_DEBUG",
            DdService => "DD_SERVICE",
            DdEnv => "DD_ENV",
            DdVersion => "DD_VERSION",
            DdProfilingEnabled => "DD_PROFILING_ENABLED",
        }
    }
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Origin {
    ProcessArguments,
    EnvironmentVariables,
    Language,
    Tags,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "operator")]
enum Operator {
    Exists,
    Equals { matches: Vec<String> },
    PrefixMatches { matches: Vec<String> },
    SuffixMatches { matches: Vec<String> },
    // todo
    // WildcardMatches,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
struct Selector {
    origin: Origin,
    #[serde(default)]
    key: Option<String>,
    // matches: Vec<String>,
    #[serde(flatten)]
    operator: Operator,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
struct Rule {
    selectors: Vec<Selector>,
    configuration: HashMap<LibraryConfigName, String>,
}

#[derive(serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct StableConfig {
    #[serde(default)]
    tags: HashMap<String, String>,
    rules: Vec<Rule>,
}

trait Get {
    fn get(&self, k: &str) -> Option<&str>;
}

impl<'a> Get for HashMap<&'a str, &'a str> {
    fn get(&self, k: &str) -> Option<&'a str> {
        self.get(k).copied()
    }
}

impl Get for HashMap<String, String> {
    fn get(&self, k: &str) -> Option<&str> {
        self.get(k).map(|v| v.as_str())
    }
}

fn string_list_selector<B: Deref<Target = [u8]>>(selector: &Selector, l: &[B]) -> bool {
    l.iter().any(|v| string_selector(selector, v.deref()))
}

fn string_selector(selector: &Selector, value: &[u8]) -> bool {
    let matches = match &selector.operator {
        Operator::Exists => return true,
        Operator::Equals { matches } => matches,
        Operator::PrefixMatches { matches } => matches,
        Operator::SuffixMatches { matches } => matches,
    };
    matches
        .iter()
        .any(|m| string_operator_match(&selector.operator, m.as_bytes(), value))
}

fn string_operator_match(op: &Operator, matches: &[u8], value: &[u8]) -> bool {
    match op {
        Operator::Equals { .. } => matches == value,
        Operator::PrefixMatches { .. } => value.starts_with(matches),
        Operator::SuffixMatches { .. } => value.ends_with(matches),
        Operator::Exists => true,
        // Operator::WildcardMatches => todo!("Wildcard matches is not implemented"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct LibraryConfig {
    pub name: LibraryConfigName,
    pub value: String,
}

#[derive(Debug)]
pub struct Configurator {
    debug_logs: bool,
}

impl Configurator {
    pub fn new(debug_logs: bool) -> Self {
        Self { debug_logs }
    }

    fn log_process_info(&self, process_info: &ProcessInfo<'_, impl Deref<Target = [u8]>>) {
        if self.debug_logs {
            eprintln!("Called library_config_common_component:");
            eprintln!("\tconfigurator: {:?}", self);
            eprintln!("\tprocess args:");
            process_info
                .args
                .iter()
                .map(|arg| String::from_utf8_lossy(arg))
                .for_each(|e| eprintln!("\t\t{:?}", e.as_ref()));

            // TODO: this is for testing purpose, we don't want to log env variables
            // eprintln!("\tprocess envs:");
            // process_info
            //     .envp
            //     .iter()
            //     .map(|arg| String::from_utf8_lossy(&*arg))
            //     .for_each(|e: std::borrow::Cow<'_, str>| eprintln!(" {:?}", e.as_ref()));
            eprintln!(
                "\tprocess language: {:?}",
                String::from_utf8_lossy(&process_info.language).as_ref()
            );
        }
    }

    pub fn get_config_from_file(
        &self,
        path: &Path,
        process_info: ProcessInfo<'_, impl Deref<Target = [u8]>>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let stable_config = match fs::File::open(path) {
            Ok(file) => self.parse_stable_config(&mut io::BufReader::new(file))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StableConfig::default(),
            Err(e) => return Err(e).context("failed to open config file"),
        };
        self.get_config(&stable_config, process_info)
    }

    pub fn get_config_from_bytes(
        &self,
        s: &[u8],
        process_info: ProcessInfo<'_, impl Deref<Target = [u8]>>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let stable_config: StableConfig = self.parse_stable_config(&mut io::Cursor::new(s))?;
        self.get_config(&stable_config, process_info)
    }

    fn parse_stable_config<F: io::Read>(&self, f: &mut F) -> anyhow::Result<StableConfig> {
        let stable_config = serde_yaml::from_reader(f)?;
        if self.debug_logs {
            eprintln!("Read the following static config: {stable_config:?}");
        }
        Ok(stable_config)
    }

    fn get_config(
        &self,
        stable_config: &StableConfig,
        process_info: ProcessInfo<'_, impl Deref<Target = [u8]>>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        self.log_process_info(&process_info);
        let matcher = Matcher::new(&process_info, &stable_config.tags);
        let Some(configs) = matcher.find_stable_config(stable_config) else {
            if self.debug_logs {
                eprintln!("No selector matched");
            }
            return Ok(Vec::new());
        };
        let library_config = matcher.template_configs(configs)?;
        if self.debug_logs {
            eprintln!("Will apply the following configuration:\n\t{library_config:?}");
        }
        Ok(library_config)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, io::Write};

    use super::{Configurator, ProcessInfo};
    use crate::{
        LibraryConfig, LibraryConfigName, Matcher, Operator, Origin, Rule, Selector, StableConfig,
    };

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
    fn test_get_config() {
        let process_info: ProcessInfo<'_, &[u8]> = ProcessInfo::<&[u8]> {
            args: &[b"-Djava_config_key=my_config", b"-jar", b"HelloWorld.jar"],
            envp: &[b"ENV=VAR"],
            language: b"java",
        };
        let configurator = Configurator::new(true);
        let config = configurator.get_config_from_bytes(b"
tags:
  cluster_name: my_cluster 
rules:
- selectors:
  - origin: language
    matches: [\"java\"]
    operator: equals
  - origin: process_arguments
    key: \"-Djava_config_key\"
    operator: exists
  - origin: process_arguments
    matches: [\"HelloWorld.jar\"]
    operator: equals
  configuration:
    DD_SERVICE: my_service_{{ tags[cluster_name] }}_{{ process_arguments[-Djava_config_key] }}_{{ language }}
", process_info).unwrap();
        assert_eq!(
            config,
            vec![LibraryConfig {
                name: LibraryConfigName::DdService,
                value: "my_service_my_cluster_my_config_java".to_string()
            }]
        );
    }

    #[test]
    fn test_match_missing_config() {
        let configurator = Configurator::new(true);
        let cfg = configurator
            .get_config_from_file(
                "/file/is/missing".as_ref(),
                ProcessInfo::<&[u8]> {
                    args: &[b"-jar HelloWorld.jar"],
                    envp: &[b"ENV=VAR"],
                    language: b"java",
                },
            )
            .unwrap();
        assert_eq!(cfg, vec![]);
    }

    #[test]
    fn test_parse_static_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.reopen()
            .unwrap()
            .write_all(
                b"
rules:
- selectors:
  - origin: language
    matches: [\"java\"]
    operator: equals
  configuration:
    DD_PROFILING_ENABLED: true
    DD_SERVICE: my-service
",
            )
            .unwrap();
        let configurator = Configurator::new(true);
        let cfg = configurator.parse_stable_config(tmp.as_file_mut()).unwrap();
        assert_eq!(
            cfg,
            StableConfig {
                tags: HashMap::default(),
                rules: vec![Rule {
                    selectors: vec![Selector {
                        origin: Origin::Language,
                        operator: Operator::Equals {
                            matches: vec!["java".to_owned()]
                        },
                        key: None,
                    }],
                    configuration: map![
                        (LibraryConfigName::DdProfilingEnabled, "true".to_owned()),
                        (LibraryConfigName::DdService, "my-service".to_owned())
                    ],
                }]
            }
        )
    }

    #[test]
    fn test_selector_match() {
        let process_info = ProcessInfo::<&[u8]> {
            args: &[b"-jar HelloWorld.jar"],
            envp: &[b"ENV=VAR"],
            language: b"java",
        };
        let tags = HashMap::new();
        let matcher = Matcher::new(&process_info, &tags);

        let test_cases = &[
            (
                Selector {
                    key: None,
                    origin: Origin::Language,
                    operator: Operator::Equals {
                        matches: vec!["java".to_owned()],
                    },
                },
                true,
            ),
            (
                Selector {
                    key: None,
                    origin: Origin::ProcessArguments,
                    operator: Operator::Equals {
                        matches: vec!["-jar HelloWorld.jar".to_owned()],
                    },
                },
                true,
            ),
            (
                Selector {
                    key: None,
                    origin: Origin::EnvironmentVariables,
                    operator: Operator::Equals {
                        matches: vec!["ENV=VAR".to_owned()],
                    },
                },
                true,
            ),
            (
                Selector {
                    key: None,
                    origin: Origin::Language,
                    operator: Operator::Equals {
                        matches: vec!["python".to_owned()],
                    },
                },
                false,
            ),
        ];
        for (i, (selector, matches)) in test_cases.iter().enumerate() {
            assert_eq!(matcher.selector_match(selector), *matches, "case {i}");
        }
    }
}
