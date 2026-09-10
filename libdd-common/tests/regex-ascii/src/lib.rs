// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use libdd_common::regex_engine::Regex;

    #[cfg(unix)]
    use libdd_common::entity_id;

    #[test]
    fn uses_full_engine_without_unicode_tables() {
        assert!(Regex::new(r"\p{Greek}").is_err());
        assert!(Regex::new(r"[a-z&&[^aeiou]]+").unwrap().is_match("rhythm"));
    }

    #[cfg(unix)]
    #[test]
    fn initializes_internal_regexes_without_unicode_tables() {
        let cgroup_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../cgroup.docker");
        entity_id::set_cgroup_file(cgroup_path.to_string());

        assert_eq!(
            entity_id::get_container_id(),
            Some("9d5b23edb1ba181e8910389a99906598d69ac9a0ead109ee55730cc416d95f7f")
        );
    }
}
