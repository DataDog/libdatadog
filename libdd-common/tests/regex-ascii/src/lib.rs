// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use libdd_common::regex_engine::Regex;

    #[test]
    fn uses_full_engine_without_unicode_tables() {
        assert!(Regex::new(r"\p{Greek}").is_err());
        assert!(Regex::new(r"[a-z&&[^aeiou]]+").unwrap().is_match("rhythm"));
    }
}
