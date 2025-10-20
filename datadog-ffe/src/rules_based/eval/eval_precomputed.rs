use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::rules_based::eval::get_assignment;
use crate::rules_based::precomputed::{PrecomputedAssignment, PrecomputedConfiguration};
use crate::rules_based::ufc::ConfigurationFormat;
use crate::rules_based::{Attributes, Configuration, Str};

pub fn get_precomputed_configuration(
    configuration: Option<&Configuration>,
    subject_key: &Str,
    subject_attributes: &Arc<Attributes>,
    now: DateTime<Utc>,
) -> PrecomputedConfiguration {
    let Some(configuration) = configuration else {
        log::warn!(target: "eppo",
                   subject_key;
                   "evaluating a flag before Eppo configuration has been fetched");
        return PrecomputedConfiguration {
            obfuscated: serde_bool::False,
            format: ConfigurationFormat::Precomputed,
            created_at: now,
            environment: None,
            flags: HashMap::new(),
        };
    };

    let generic_attributes = subject_attributes;

    let flags = configuration
        .flags
        .compiled
        .flags
        .keys()
        .filter_map(|flag_key| {
            get_assignment(
                Some(configuration),
                flag_key,
                subject_key,
                generic_attributes,
                None,
                now,
            )
            .unwrap_or_else(|err| {
                log::warn!(
                    target: "eppo",
                    subject_key,
                    flag_key,
                    err:?;
                    "Failed to evaluate assignment"
                );
                None
            })
            .map(|assignment| (flag_key.clone(), PrecomputedAssignment::from(assignment)))
        })
        .collect::<HashMap<_, _>>();

    let result = PrecomputedConfiguration {
        obfuscated: serde_bool::False,
        created_at: now,
        format: ConfigurationFormat::Precomputed,
        environment: Some(configuration.flags.compiled.environment.clone()),
        flags,
    };

    log::trace!(
        target: "eppo",
        subject_key,
        configuration:serde = result;
        "evaluated precomputed assignments");

    result
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::rules_based::{
        Configuration, SdkMetadata, eval::get_precomputed_configuration, ufc::UniversalFlagConfig,
    };

    #[test]
    fn test_precomputed_assignment_basic() {
        let _ = env_logger::builder().is_test(true).try_init();

        let configuration = {
            // Load test configuration
            let ufc_config = UniversalFlagConfig::from_json(
                SdkMetadata {
                    name: "test",
                    version: "0.1.0",
                },
                {
                    let path = if std::path::Path::new("tests/data/flags-v1.json").exists() {
                        "tests/data/flags-v1.json"
                    } else {
                        "domains/ffe/libs/flagging/rust/evaluation/tests/data/flags-v1.json"
                    };
                    std::fs::read(path).unwrap()
                },
            )
            .unwrap();
            Configuration::from_server_response(ufc_config)
        };

        let subject_key = "test-subject-1".into();
        let subject_attributes = Default::default();
        let now = Utc::now();

        // Get precomputed assignments
        let precomputed = get_precomputed_configuration(
            Some(&configuration),
            &subject_key,
            &subject_attributes,
            now,
        );

        assert!(
            !precomputed.flags.is_empty(),
            "Should have precomputed flags"
        );

        // Each flag in the configuration should have an entry
        for flag_key in precomputed.flags.keys() {
            assert!(
                precomputed.flags.contains_key(flag_key),
                "Should have precomputed assignment for flag {}",
                flag_key
            );
        }

        // Uncomment next section to dump configuration to console.
        // eprintln!(
        //     "{}",
        //     serde_json::to_string_pretty(&precomputed.obfuscate()).unwrap()
        // );
        // assert!(false);
    }
}
