use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::{fs, io};

use anyhow::Context;

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
    PrefixMatches,
    SuffixMatches,
    // todox
    // WildcardMatches,
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
struct StableConfig {
    rules: Vec<Rule>,
}

fn find_stable_config<'a, 'b>(
    cfg: &'b StableConfig,
    process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
) -> Option<&'b HashMap<LibraryConfigName, String>> {
    for rule in &cfg.rules {
        if rule
            .selectors
            .iter()
            .all(|s| selector_match(s, process_info))
        {
            return Some(&rule.configuration);
        }
    }
    None
}

// Returns true if the selector matches the process info
// Any element in the "matches" section of the selector must match, they are ORed,
// as selectors are ANDed.
fn selector_match<'a>(
    selector: &Selector,
    process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
) -> bool {
    match selector.origin {
        Origin::Language => string_selector(selector, process_info.language.deref()),
        Origin::ProcessArguments => string_list_selector(selector, process_info.args),
        Origin::EnvironmentVariable => string_list_selector(selector, process_info.envp),
    }
}

fn string_list_selector<'a, B: Deref<Target = [u8]>>(selector: &Selector, l: &'a [B]) -> bool {
    l.into_iter().any(|v| string_selector(selector, v.deref()))
}

fn string_selector(selector: &Selector, matches: &[u8]) -> bool {
    selector
        .matches
        .iter()
        .any(|m| string_operator_match(&selector.operator, m.as_bytes(), matches))
}

fn string_operator_match(op: &Operator, matches: &[u8], value: &[u8]) -> bool {
    match op {
        Operator::Equals => matches == value,
        Operator::PrefixMatches => value.starts_with(matches),
        Operator::SuffixMatches => value.ends_with(matches),
        // Operator::WildcardMatches => todo!("Wildcard matches is not implemented"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct LibraryConfig {
    pub name: LibraryConfigName,
    pub value: String,
}

fn template_configs<'a>(
    config: &HashMap<LibraryConfigName, String>,
    process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
) -> anyhow::Result<Vec<LibraryConfig>> {
    config
        .iter()
        .map(|(&name, v)| {
            Ok(LibraryConfig {
                name,
                value: template_config(v, process_info)?,
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
fn template_config<'a>(
    config_val: &str,
    process_info: &'a ProcessInfo<'a, impl Deref<Target = [u8]>>,
) -> anyhow::Result<String> {
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
        let template_var = template_var.trim();
        let val = match template_var {
            "language" => String::from_utf8_lossy(process_info.language.deref()),
            _ => std::borrow::Cow::Borrowed("UNDEFINED"),
        };
        templated.push_str(&val);
        rest = tail;
    }
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
                .map(|arg| String::from_utf8_lossy(&*arg))
                .for_each(|e| eprintln!(" {:?}", e.as_ref()));

            // TODO: this is for testing purpose, we don't want to log env variables
            // eprintln!("\tprocess envs:");
            // process_info
            //     .envp
            //     .iter()
            //     .map(|arg| String::from_utf8_lossy(&*arg))
            //     .for_each(|e: std::borrow::Cow<'_, str>| eprintln!(" {:?}", e.as_ref()));
            eprintln!(
                "\tprocess language: {:?}",
                String::from_utf8_lossy(&*process_info.language).as_ref()
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
        let Some(configs) = find_stable_config(stable_config, &process_info) else {
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
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, Write};

    use super::{template_config, Configurator, ProcessInfo};
    use crate::{LibraryConfigName, Operator, Origin, Rule, Selector, StableConfig};

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
    fn test_template_config() {
        let config_template = "my_{{ language }}_service";
        let out = template_config(
            config_template,
            &ProcessInfo::<&[u8]> {
                args: &[],
                envp: &[],
                language: b"java",
            },
        )
        .expect("templating failed");
        assert_eq!(&out, "my_java_service");
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
                rules: vec![Rule {
                    selectors: vec![Selector {
                        origin: Origin::Language,
                        matches: vec!["java".to_owned()],
                        operator: Operator::Equals,
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
        let selector = Selector {
            origin: Origin::Language,
            matches: vec!["java".to_owned()],
            operator: Operator::Equals,
        };
        assert!(super::selector_match(&selector, &process_info));

        let selector = Selector {
            origin: Origin::ProcessArguments,
            matches: vec!["-jar HelloWorld.jar".to_owned()],
            operator: Operator::Equals,
        };
        assert!(super::selector_match(&selector, &process_info));

        let selector = Selector {
            origin: Origin::EnvironmentVariable,
            matches: vec!["ENV=VAR".to_owned()],
            operator: Operator::Equals,
        };
        assert!(super::selector_match(&selector, &process_info));

        let selector = Selector {
            origin: Origin::Language,
            matches: vec!["python".to_owned()],
            operator: Operator::Equals,
        };
        assert!(!super::selector_match(&selector, &process_info));
    }
}
