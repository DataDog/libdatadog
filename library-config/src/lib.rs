// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::{env, fs, io};

use anyhow::Context;

/// This struct holds maps used to match and template configurations.
///
/// They are computed lazily so that if the templating feature is not necessary, we don't
/// have to create the maps.
///
/// These maps come from one of three origins:
///  * tags: This one is fairly simple, the format is tag_key: tag_value
///  * envs: Splits env variables with format KEY=VALUE
///  * args: Splits args with format key=value. If the arg doesn't contain an '=', skip it
struct MatchMaps<'a> {
    tags: &'a HashMap<String, String>,
    env_map: OnceCell<HashMap<&'a str, &'a str>>,
    args_map: OnceCell<HashMap<&'a str, &'a str>>,
}

impl<'a> MatchMaps<'a> {
    fn env(&self, process_info: &'a ProcessInfo) -> &HashMap<&'a str, &'a str> {
        self.env_map.get_or_init(|| {
            let mut map = HashMap::new();
            for e in &process_info.envp {
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

    fn args(&self, process_info: &'a ProcessInfo) -> &HashMap<&str, &str> {
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
                // Split args between key and value on '='
                if let Some((k, v)) = arg.split_once('=') {
                    map.insert(k, v);
                    continue;
                }
            }
            map
        })
    }
}

struct Matcher<'a> {
    process_info: &'a ProcessInfo,
    match_maps: MatchMaps<'a>,
}

impl<'a> Matcher<'a> {
    fn new(process_info: &'a ProcessInfo, tags: &'a HashMap<String, String>) -> Self {
        Self {
            process_info,
            match_maps: MatchMaps {
                tags,
                env_map: OnceCell::new(),
                args_map: OnceCell::new(),
            },
        }
    }

    /// Returns the first set of configurations that match the current process
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

    /// Returns true if the selector matches the process
    ///
    /// Any element in the "matches" section of the selector must match, they are ORed,
    /// as selectors are ANDed.
    fn selector_match(&'a self, selector: &Selector) -> bool {
        match selector.origin {
            Origin::Language => string_selector(selector, self.process_info.language.deref()),
            Origin::ProcessArguments => match &selector.key {
                Some(key) => {
                    let arg_map = self.match_maps.args(self.process_info);
                    map_operator_match(selector, arg_map, key)
                }
                None => string_list_selector(selector, &self.process_info.args),
            },
            Origin::EnvironmentVariables => match &selector.key {
                Some(key) => {
                    let env_map = self.match_maps.env(self.process_info);
                    map_operator_match(selector, env_map, key)
                }
                None => string_list_selector(selector, &self.process_info.envp),
            },
            Origin::Tags => match &selector.key {
                Some(key) => map_operator_match(selector, self.match_maps.tags, key),
                None => false,
            },
        }
    }

    fn template_configs(
        &'a self,
        source: LibraryConfigSource,
        config: &HashMap<LibraryConfigName, String>,
        config_id: &Option<String>,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        config
            .iter()
            .map(|(&name, v)| {
                Ok(LibraryConfig {
                    name,
                    value: self.template_config(v)?,
                    source,
                    config_id: config_id.clone(),
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
pub struct ProcessInfo {
    pub args: Vec<Vec<u8>>,
    pub envp: Vec<Vec<u8>>,
    pub language: Vec<u8>,
}

fn process_envp() -> Vec<Vec<u8>> {
    #[allow(clippy::unnecessary_filter_map)]
    env::vars_os()
        .filter_map(|(k, v)| {
            #[cfg(not(unix))]
            {
                let mut env = Vec::new();
                env.extend(k.to_str()?.as_bytes());
                env.push(b'=');
                env.extend(v.to_str()?.as_bytes());
                Some(env)
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let mut env = Vec::new();
                env.extend(k.as_bytes());
                env.push(b'=');
                env.extend(v.as_bytes());
                Some(env)
            }
        })
        .collect()
}

fn process_args() -> Vec<Vec<u8>> {
    #[allow(clippy::unnecessary_filter_map)]
    env::args_os()
        .filter_map(|a| {
            #[cfg(not(unix))]
            {
                Some(a.into_string().ok()?.into_bytes())
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                Some(a.into_vec())
            }
        })
        .collect()
}

impl ProcessInfo {
    pub fn detect_global(language: String) -> Self {
        let envp = process_envp();
        let args = process_args();
        Self {
            args,
            envp,
            language: language.into_bytes(),
        }
    }
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

#[repr(C)]
#[derive(Clone, Copy, serde::Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::enum_variant_names)]
pub enum LibraryConfigSource {
    // Order matters, as it is used to determine the priority of the source.
    //  The higher the value, the higher the priority.
    LocalStableConfig = 0,
    FleetStableConfig = 1,
}

impl LibraryConfigSource {
    pub fn to_str(&self) -> &'static str {
        use LibraryConfigSource::*;
        match self {
            LocalStableConfig => "local_stable_config",
            FleetStableConfig => "fleet_stable_config",
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
    config_id: Option<String>,
}

/// Helper trait so we don't have to duplicate code for
/// HashMap<&str, &str> and HashMap<String, String>
trait Get {
    fn get(&self, k: &str) -> Option<&str>;
}

impl Get for HashMap<&str, &str> {
    fn get(&self, k: &str) -> Option<&str> {
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
    pub source: LibraryConfigSource,
    pub config_id: Option<String>,
}

#[derive(Debug)]
pub struct Configurator {
    debug_logs: bool,
}

impl Configurator {
    pub const FLEET_STABLE_CONFIGURATION_PATH: &'static str = {
        #[cfg(target_os = "linux")]
        {
            "/etc/datadog-agent/managed/datadog-agent/stable/application_monitoring.yaml"
        }
        #[cfg(target_os = "macos")]
        {
            "/opt/datadog-agent/etc/stable/application_monitoring.yaml"
        }
        #[cfg(windows)]
        {
            "C:\\ProgramData\\Datadog\\managed\\datadog-agent\\stable\\application_monitoring.yaml"
        }
    };

    pub const LOCAL_STABLE_CONFIGURATION_PATH: &'static str = {
        #[cfg(target_os = "linux")]
        {
            "/etc/datadog-agent/application_monitoring.yaml"
        }
        #[cfg(target_os = "macos")]
        {
            "/opt/datadog-agent/etc/application_monitoring.yaml"
        }
        #[cfg(windows)]
        {
            "C:\\ProgramData\\Datadog\\application_monitoring.yaml"
        }
    };

    pub fn new(debug_logs: bool) -> Self {
        Self { debug_logs }
    }

    fn log_process_info(&self, process_info: &ProcessInfo, source: LibraryConfigSource) {
        if self.debug_logs {
            eprintln!("Called library_config_common_component:");
            eprintln!("\tsource: {source:?}");
            eprintln!("\tconfigurator: {:?}", self);
            eprintln!("\tprocess args:");
            process_info
                .args
                .iter()
                .map(|arg| String::from_utf8_lossy(arg))
                .for_each(|e| eprintln!("\t\t{:?}", e.as_ref()));

            eprintln!(
                "\tprocess language: {:?}",
                String::from_utf8_lossy(&process_info.language).as_ref()
            );
        }
    }

    pub fn get_config_from_file(
        &self,
        path_local: &Path,
        path_managed: &Path,
        process_info: ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let stable_config_local = match fs::File::open(path_local) {
            Ok(file) => self.parse_stable_config(&mut io::BufReader::new(file))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StableConfig::default(),
            Err(e) => return Err(e).context("failed to open config file"),
        };
        let stable_config_managed = match fs::File::open(path_managed) {
            Ok(file) => self.parse_stable_config(&mut io::BufReader::new(file))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StableConfig::default(),
            Err(e) => return Err(e).context("failed to open config file"),
        };

        let managed_config = self.get_config(
            &stable_config_managed,
            LibraryConfigSource::FleetStableConfig,
            &process_info,
        )?;
        if !managed_config.is_empty() {
            return Ok(managed_config);
        }

        // If no managed config rule matches, try the local config
        let local_config = self.get_config(
            &stable_config_local,
            LibraryConfigSource::LocalStableConfig,
            &process_info,
        )?;
        Ok(local_config)
    }

    pub fn get_config_from_bytes(
        &self,
        s_local: &[u8],
        s_managed: &[u8],
        process_info: ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let stable_config_local: StableConfig =
            self.parse_stable_config(&mut io::Cursor::new(s_local))?;
        let stable_config_managed: StableConfig =
            self.parse_stable_config(&mut io::Cursor::new(s_managed))?;

        let managed_config = self.get_config(
            &stable_config_managed,
            LibraryConfigSource::FleetStableConfig,
            &process_info,
        )?;
        if !managed_config.is_empty() {
            return Ok(managed_config);
        }

        // If no managed config rule matches, try the local config
        let local_config = self.get_config(
            &stable_config_local,
            LibraryConfigSource::LocalStableConfig,
            &process_info,
        )?;
        Ok(local_config)
    }

    fn parse_stable_config<F: io::Read>(&self, f: &mut F) -> anyhow::Result<StableConfig> {
        let mut buffer = String::new();
        f.read_to_string(&mut buffer)?;
        if buffer.trim().is_empty() {
            let stable_config = StableConfig::default();
            eprintln!("Read the following static config: {stable_config:?}");
            return Ok(stable_config);
        }

        let stable_config = serde_yaml::from_str(&buffer)?;
        if self.debug_logs {
            eprintln!("Read the following static config: {stable_config:?}");
        }
        Ok(stable_config)
    }

    fn get_config(
        &self,
        stable_config: &StableConfig,
        source: LibraryConfigSource,
        process_info: &ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        self.log_process_info(process_info, source);
        let matcher = Matcher::new(process_info, &stable_config.tags);
        let Some(configs) = matcher.find_stable_config(stable_config) else {
            if self.debug_logs {
                eprintln!("No selector matched");
            }
            return Ok(Vec::new());
        };
        let library_config = matcher.template_configs(source, configs, &stable_config.config_id)?;
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
        LibraryConfig, LibraryConfigName, LibraryConfigSource, Matcher, Operator, Origin, Rule,
        Selector, StableConfig,
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
        let process_info: ProcessInfo = ProcessInfo {
            args: vec![
                b"-Djava_config_key=my_config".to_vec(),
                b"-jar".to_vec(),
                b"HelloWorld.jar".to_vec(),
            ],
            envp: vec![b"ENV=VAR".to_vec()],
            language: b"java".to_vec(),
        };
        let configurator = Configurator::new(true);
        let config = configurator.get_config_from_bytes(b"
config_id: abc
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
", b"", process_info).unwrap();
        assert_eq!(
            config,
            vec![LibraryConfig {
                name: LibraryConfigName::DdService,
                value: "my_service_my_cluster_my_config_java".to_string(),
                source: LibraryConfigSource::LocalStableConfig,
                config_id: Some("abc".to_string()),
            }]
        );
    }

    #[test]
    fn test_match_missing_config() {
        let configurator = Configurator::new(true);
        let cfg = configurator
            .get_config_from_file(
                "/file/is/missing".as_ref(),
                "/file/is/missing_too".as_ref(),
                ProcessInfo {
                    args: vec![b"-jar HelloWorld.jar".to_vec()],
                    envp: vec![b"ENV=VAR".to_vec()],
                    language: b"java".to_vec(),
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
                config_id: None,
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
        let process_info = ProcessInfo {
            args: vec![b"-jar HelloWorld.jar".to_vec()],
            envp: vec![b"ENV=VAR".to_vec()],
            language: b"java".to_vec(),
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

    #[test]
    fn test_fleet_over_local() {
        let process_info: ProcessInfo = ProcessInfo {
            args: vec![
                b"-Djava_config_key=my_config".to_vec(),
                b"-jar".to_vec(),
                b"HelloWorld.jar".to_vec(),
            ],
            envp: vec![b"ENV=VAR".to_vec()],
            language: b"java".to_vec(),
        };
        let configurator = Configurator::new(true);
        let config = configurator
            .get_config_from_bytes(
                b"
config_id: abc
tags:
  cluster_name: my_cluster 
rules:
- selectors:
  - origin: language
    matches: [\"java\"]
    operator: equals
  configuration:
    DD_SERVICE: local
",
                b"
config_id: def
rules:
- selectors:
  - origin: language
    matches: [\"java\"]
    operator: equals
  configuration:
    DD_SERVICE: managed",
                process_info,
            )
            .unwrap();
        assert_eq!(
            config,
            vec![LibraryConfig {
                name: LibraryConfigName::DdService,
                value: "managed".to_string(),
                source: LibraryConfigSource::FleetStableConfig,
                config_id: Some("def".to_string()),
            }]
        );
    }
}
