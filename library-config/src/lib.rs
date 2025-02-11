// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::{env, fs, io, mem};

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
    fn find_stable_config<'b>(&'a self, cfg: &'b StableConfig) -> Option<&'b ConfigMap> {
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

/// A (key, value) struct
///
/// This type has a custom serde Deserialize implementation from maps:
/// * It skips invalid/unknown keys in the map
/// * Since the storage is a Boxed slice and not a Hashmap, it doesn't over-allocate
#[derive(Debug, Default, PartialEq, Eq)]
struct ConfigMap(Box<[(LibraryConfigName, String)]>);

impl<'de> serde::Deserialize<'de> for ConfigMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ConfigMapVisitor;
        impl<'de> serde::de::Visitor<'de> for ConfigMapVisitor {
            type Value = ConfigMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("struct ConfigMap(HashMap<LibraryConfig, String>)")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut configs = Vec::new();
                configs.reserve_exact(map.size_hint().unwrap_or(0));
                loop {
                    let k = match map.next_key::<LibraryConfigName>() {
                        Ok(Some(k)) => k,
                        Ok(None) => break,
                        Err(_) => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                            continue;
                        }
                    };
                    let v = map.next_value::<String>()?;
                    configs.push((k, v));
                }
                Ok(ConfigMap(configs.into_boxed_slice()))
            }
        }
        deserializer.deserialize_map(ConfigMapVisitor)
    }
}

#[repr(C)]
#[derive(Clone, Copy, serde::Deserialize, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::enum_variant_names)]
pub enum LibraryConfigName {
    // Phase 1: product enablement
    DdApmTracingEnabled,
    DdRuntimeMetricsEnabled,
    DdLogsInjection,
    DdProfilingEnabled,
    DdDataStreamsEnabled,
    DdAppsecEnabled,
    DdIastEnabled,
    DdDynamicInstrumentationEnabled,
    DdDataJobsEnabled,
    DdAppsecScaEnabled,

    // Phase 2: Service tagging + misceanellous stuff
    DdTraceDebug,
    DdService,
    DdEnv,
    DdVersion,
}

impl LibraryConfigName {
    pub fn to_str(&self) -> &'static str {
        use LibraryConfigName::*;
        match self {
            DdApmTracingEnabled => "DD_APM_TRACING_ENABLED",
            DdRuntimeMetricsEnabled => "DD_RUNTIME_METRICS_ENABLED",
            DdLogsInjection => "DD_LOGS_INJECTION",
            DdProfilingEnabled => "DD_PROFILING_ENABLED",
            DdDataStreamsEnabled => "DD_DATA_STREAMS_ENABLED",
            DdAppsecEnabled => "DD_APPSEC_ENABLED",
            DdIastEnabled => "DD_IAST_ENABLED",
            DdDynamicInstrumentationEnabled => "DD_DYNAMIC_INSTRUMENTATION_ENABLED",
            DdDataJobsEnabled => "DD_DATA_JOBS_ENABLED",
            DdAppsecScaEnabled => "DD_APPSEC_SCA_ENABLED",

            DdTraceDebug => "DD_TRACE_DEBUG",
            DdService => "DD_SERVICE",
            DdEnv => "DD_ENV",
            DdVersion => "DD_VERSION",
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
    configuration: ConfigMap,
}

#[derive(serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct StableConfig {
    // Phase 1
    #[serde(default)]
    config_id: Option<String>,
    #[serde(default)]
    apm_configuration_default: ConfigMap,

    // Phase 2
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    rules: Vec<Rule>,
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
/// LibraryConfig represent a configuration item and is part of the public API
/// of this module
pub struct LibraryConfig {
    pub name: LibraryConfigName,
    pub value: String,
    pub source: LibraryConfigSource,
    pub config_id: Option<String>,
}

#[derive(Debug)]
/// This struct is used to hold configuration item data in a Hashmap, while the name of
/// the configuration is the key used for deduplication
struct LibraryConfigVal {
    value: String,
    source: LibraryConfigSource,
    config_id: Option<String>,
}

#[derive(Debug)]
pub struct Configurator {
    debug_logs: bool,
}

pub enum Target {
    Linux,
    Macos,
    Windows,
}

impl Target {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    const fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::Macos
        }
        #[cfg(windows)]
        {
            Self::Windows
        }
    }
}

impl Configurator {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub const FLEET_STABLE_CONFIGURATION_PATH: &'static str =
        Self::fleet_stable_configuration_path(Target::current());

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub const LOCAL_STABLE_CONFIGURATION_PATH: &'static str =
        Self::local_stable_configuration_path(Target::current());

    pub const fn local_stable_configuration_path(target: Target) -> &'static str {
        match target {
            Target::Linux => "/etc/datadog-agent/application_monitoring.yaml",
            Target::Macos => "/opt/datadog-agent/etc/application_monitoring.yaml",
            Target::Windows => "C:\\ProgramData\\Datadog\\application_monitoring.yaml",
        }
    }

    pub const fn fleet_stable_configuration_path(target: Target) -> &'static str {
        match target {
            Target::Linux => "/etc/datadog-agent/managed/datadog-agent/stable/application_monitoring.yaml",
            Target::Macos => "/opt/datadog-agent/etc/stable/application_monitoring.yaml",
            Target::Windows => "C:\\ProgramData\\Datadog\\managed\\datadog-agent\\stable\\application_monitoring.yaml",
        }
    }

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

    fn parse_stable_config_slice(&self, buf: &[u8]) -> anyhow::Result<StableConfig> {
        if buf.is_empty() {
            let stable_config = StableConfig::default();
            eprintln!("Read the following static config: {stable_config:?}");
            return Ok(stable_config);
        }
        let stable_config = serde_yaml::from_slice(buf)?;
        if self.debug_logs {
            eprintln!("Read the following static config: {stable_config:?}");
        }
        Ok(stable_config)
    }

    fn parse_stable_config_file<F: io::Read>(&self, mut f: F) -> anyhow::Result<StableConfig> {
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        self.parse_stable_config_slice(utils::trim_bytes(&buffer))
    }

    pub fn get_config_from_file(
        &self,
        path_local: &Path,
        path_managed: &Path,
        process_info: ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let local_config = match fs::File::open(path_local) {
            Ok(file) => self.parse_stable_config_file(file)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StableConfig::default(),
            Err(e) => return Err(e).context("failed to open config file"),
        };
        let fleet_config = match fs::File::open(path_managed) {
            Ok(file) => self.parse_stable_config_file(file)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StableConfig::default(),
            Err(e) => return Err(e).context("failed to open config file"),
        };

        self.get_config(local_config, fleet_config, &process_info)
    }

    pub fn get_config_from_bytes(
        &self,
        s_local: &[u8],
        s_managed: &[u8],
        process_info: ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let local_config: StableConfig = self.parse_stable_config_slice(s_local)?;
        let fleet_config: StableConfig = self.parse_stable_config_slice(s_managed)?;

        self.get_config(local_config, fleet_config, &process_info)
    }

    fn get_config(
        &self,
        local_config: StableConfig,
        fleet_config: StableConfig,
        process_info: &ProcessInfo,
    ) -> anyhow::Result<Vec<LibraryConfig>> {
        let mut cfg = HashMap::new();
        // First get local configuration
        self.get_single_source_config(
            local_config,
            LibraryConfigSource::LocalStableConfig,
            process_info,
            &mut cfg,
        )?;
        // Merge with fleet config override
        self.get_single_source_config(
            fleet_config,
            LibraryConfigSource::FleetStableConfig,
            process_info,
            &mut cfg,
        )?;
        Ok(cfg
            .into_iter()
            .map(|(k, v)| LibraryConfig {
                name: k,
                value: v.value,
                source: v.source,
                config_id: v.config_id,
            })
            .collect())
    }

    /// Get config from a stable config file and associate them with the file origin
    ///
    /// This is done in two steps:
    ///     * First take the global host config
    ///     * Merge the global config with the process specific config
    fn get_single_source_config(
        &self,
        mut stable_config: StableConfig,
        source: LibraryConfigSource,
        process_info: &ProcessInfo,
        cfg: &mut HashMap<LibraryConfigName, LibraryConfigVal>,
    ) -> anyhow::Result<()> {
        self.log_process_info(process_info, source);

        // Phase 1: take host default config
        cfg.extend(
            mem::take(&mut stable_config.apm_configuration_default)
                .0
                // TODO(paullgdc): use Box<[I]>::into_iter when we can use rust 1.80
                .to_vec()
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        LibraryConfigVal {
                            value: v,
                            source,
                            config_id: stable_config.config_id.clone(),
                        },
                    )
                }),
        );

        // Phase 2: process specific config
        self.get_single_source_process_config(stable_config, source, process_info, cfg)?;
        Ok(())
    }

    /// Get config from a stable config using process matching rules
    fn get_single_source_process_config(
        &self,
        stable_config: StableConfig,
        source: LibraryConfigSource,
        process_info: &ProcessInfo,
        library_config: &mut HashMap<LibraryConfigName, LibraryConfigVal>,
    ) -> anyhow::Result<()> {
        let matcher = Matcher::new(process_info, &stable_config.tags);
        let Some(configs) = matcher.find_stable_config(&stable_config) else {
            if self.debug_logs {
                eprintln!("No selector matched for source {source:?}");
            }
            return Ok(());
        };

        for (name, config_val) in configs.0.iter() {
            let value = matcher.template_config(config_val)?;
            library_config.insert(
                *name,
                LibraryConfigVal {
                    value,
                    source,
                    config_id: stable_config.config_id.clone(),
                },
            );
        }

        if self.debug_logs {
            eprintln!("Will apply the following configuration:\n\tsource {source:?}\n\t{library_config:?}");
        }
        Ok(())
    }
}

use utils::Get;
mod utils {
    use std::collections::HashMap;

    /// Removes leading and trailing ascci whitespaces from a byte slice
    pub(crate) fn trim_bytes(mut b: &[u8]) -> &[u8] {
        while b.first().map(u8::is_ascii_whitespace).unwrap_or(false) {
            b = &b[1..];
        }
        while b.last().map(u8::is_ascii_whitespace).unwrap_or(false) {
            b = &b[..b.len() - 1];
        }
        b
    }

    /// Helper trait so we don't have to duplicate code for
    /// HashMap<&str, &str> and HashMap<String, String>
    pub(crate) trait Get {
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
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, io::Write};

    use super::{Configurator, ProcessInfo};
    use crate::{
        ConfigMap, LibraryConfig, LibraryConfigName, LibraryConfigSource, Matcher, Operator,
        Origin, Rule, Selector, StableConfig,
    };

    fn test_config(local_cfg: &[u8], fleet_cfg: &[u8], expected: Vec<LibraryConfig>) {
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
        let mut actual = configurator
            .get_config_from_bytes(local_cfg, fleet_cfg, process_info)
            .unwrap();

        // Sort by name for determinism
        actual.sort_by_key(|c| c.name);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_empty_configs() {
        test_config(b"", b"", vec![]);
    }

    #[test]
    fn test_missing_files() {
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
    fn test_local_host_global_config() {
        use LibraryConfigName::*;
        use LibraryConfigSource::*;
        test_config(
            b"
apm_configuration_default:
  DD_APM_TRACING_ENABLED: true
  DD_RUNTIME_METRICS_ENABLED: true
  DD_LOGS_INJECTION: true
  DD_PROFILING_ENABLED: true
  DD_DATA_STREAMS_ENABLED: true
  DD_APPSEC_ENABLED: true
  DD_IAST_ENABLED: true
  DD_DYNAMIC_INSTRUMENTATION_ENABLED: true
  DD_DATA_JOBS_ENABLED: true
  DD_APPSEC_SCA_ENABLED: true
    ",
            b"",
            vec![
                LibraryConfig {
                    name: DdApmTracingEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdRuntimeMetricsEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdLogsInjection,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdProfilingEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdDataStreamsEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdAppsecEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdIastEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdDynamicInstrumentationEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdDataJobsEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdAppsecScaEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
            ],
        );
    }

    #[test]
    fn test_fleet_host_global_config() {
        use LibraryConfigName::*;
        use LibraryConfigSource::*;
        test_config(
            b"",
            b"
config_id: abc
apm_configuration_default:
  DD_APM_TRACING_ENABLED: true
  DD_RUNTIME_METRICS_ENABLED: true
  DD_LOGS_INJECTION: true
  DD_PROFILING_ENABLED: true
  DD_DATA_STREAMS_ENABLED: true
  DD_APPSEC_ENABLED: true
  DD_IAST_ENABLED: true
  DD_DYNAMIC_INSTRUMENTATION_ENABLED: true
  DD_DATA_JOBS_ENABLED: true
  DD_APPSEC_SCA_ENABLED: true
  # extra keys should be skipped without errors
  FOO_BAR: hqecuh
wtf:
- 1
    ",
            vec![
                LibraryConfig {
                    name: DdApmTracingEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdRuntimeMetricsEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdLogsInjection,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdProfilingEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdDataStreamsEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdAppsecEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdIastEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdDynamicInstrumentationEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdDataJobsEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdAppsecScaEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
            ],
        );
    }

    #[test]
    fn test_merge_local_fleet() {
        use LibraryConfigName::*;
        use LibraryConfigSource::*;

        test_config(
            b"
apm_configuration_default:
  DD_APM_TRACING_ENABLED: true
  DD_RUNTIME_METRICS_ENABLED: true
  DD_PROFILING_ENABLED: true
        ",
            b"
config_id: abc
apm_configuration_default:
  DD_APM_TRACING_ENABLED: true
  DD_LOGS_INJECTION: true
  DD_PROFILING_ENABLED: false
",
            vec![
                LibraryConfig {
                    name: DdApmTracingEnabled,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdRuntimeMetricsEnabled,
                    value: "true".to_owned(),
                    source: LocalStableConfig,
                    config_id: None,
                },
                LibraryConfig {
                    name: DdLogsInjection,
                    value: "true".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
                LibraryConfig {
                    name: DdProfilingEnabled,
                    value: "false".to_owned(),
                    source: FleetStableConfig,
                    config_id: Some("abc".to_owned()),
                },
            ],
        );
    }

    #[test]
    fn test_process_config() {
        test_config(
    b"
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
    ",
    b"", 
    vec![LibraryConfig {
            name: LibraryConfigName::DdService,
            value: "my_service_my_cluster_my_config_java".to_string(),
            source: LibraryConfigSource::LocalStableConfig,
            config_id: Some("abc".to_string()),
        }],
        );
    }

    #[test]
    fn test_parse_static_config() {
        use LibraryConfigName::*;
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
    # extra keys should be skipped without errors
    FOOBAR: maybe??
",
            )
            .unwrap();
        let configurator = Configurator::new(true);
        let cfg = configurator
            .parse_stable_config_file(tmp.as_file_mut())
            .unwrap();
        assert_eq!(
            cfg,
            StableConfig {
                config_id: None,
                apm_configuration_default: ConfigMap::default(),
                tags: HashMap::default(),
                rules: vec![Rule {
                    selectors: vec![Selector {
                        origin: Origin::Language,
                        operator: Operator::Equals {
                            matches: vec!["java".to_owned()]
                        },
                        key: None,
                    }],
                    configuration: ConfigMap(
                        vec![
                            (DdProfilingEnabled, "true".to_owned()),
                            (DdService, "my-service".to_owned())
                        ]
                        .into_boxed_slice()
                    ),
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
