// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dd_sds::{
    ProximityKeywordsConfig, RegexRuleConfig, RootRuleConfig, RuleConfig, Scanner, ScannerBuilder,
    SecondaryValidator, SimpleEvent,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdsMode {
    #[default]
    #[serde(alias = "VAULT")]
    Vault,
    #[serde(alias = "REDACT")]
    Redact,
    #[serde(alias = "OFF")]
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SdsRuleConfig {
    pub id: String,
    pub prefix: String,
    pub pattern: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub validator: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SdsPolicyUpdate {
    pub enabled: Option<bool>,
    pub mode: Option<SdsMode>,
    pub presets: Option<Vec<String>>,
    pub rules: Option<Vec<SdsRuleConfig>>,
    pub block_rehydrate_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenizeResult {
    pub texts: Vec<String>,
    pub vault: HashMap<String, String>,
    pub matched_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetokenizeResult {
    pub text: String,
    pub blocked_tokens: Vec<String>,
}

#[derive(Clone)]
pub struct CompiledSdsVault {
    pub enabled: bool,
    pub mode: SdsMode,
    presets: Vec<String>,
    custom_rules: Vec<SdsRuleConfig>,
    rule_meta: Vec<(String, String)>,
    scanner: Option<Arc<Scanner>>,
    block_rehydrate_tools: HashSet<String>,
}

impl std::fmt::Debug for CompiledSdsVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSdsVault")
            .field("enabled", &self.enabled)
            .field("mode", &self.mode)
            .field("presets", &self.presets)
            .field("custom_rules_count", &self.custom_rules.len())
            .field("compiled_rules_count", &self.rule_meta.len())
            .field("block_rehydrate_tools", &self.block_rehydrate_tools)
            .finish()
    }
}

fn preset_rule(name: &str) -> Result<SdsRuleConfig, String> {
    match name.to_ascii_lowercase().as_str() {
        "credit_card" | "cc" => Ok(SdsRuleConfig {
            id: "sds:credit_card".to_string(),
            prefix: "CC".to_string(),
            pattern: r"\b(?:\d[ -]*?){13,19}\b".to_string(),
            keywords: vec![
                "card".to_string(),
                "credit".to_string(),
                "cc".to_string(),
                "visa".to_string(),
                "mastercard".to_string(),
                "amex".to_string(),
                "payment".to_string(),
            ],
            validator: Some("LuhnChecksum".to_string()),
        }),
        "email" => Ok(SdsRuleConfig {
            id: "sds:email".to_string(),
            prefix: "EMAIL".to_string(),
            pattern: r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b".to_string(),
            keywords: Vec::new(),
            validator: None,
        }),
        "ssn" => Ok(SdsRuleConfig {
            id: "sds:ssn".to_string(),
            prefix: "SSN".to_string(),
            pattern: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
            keywords: vec![
                "ssn".to_string(),
                "social".to_string(),
                "tax".to_string(),
                "tin".to_string(),
            ],
            validator: None,
        }),
        "github_token" => Ok(SdsRuleConfig {
            id: "sds:github_token".to_string(),
            prefix: "GH_TOKEN".to_string(),
            pattern: r"\bgh[pousr]_[A-Za-z0-9_]{36,}\b".to_string(),
            keywords: Vec::new(),
            validator: Some("GithubTokenChecksum".to_string()),
        }),
        "iban" => Ok(SdsRuleConfig {
            id: "sds:iban".to_string(),
            prefix: "IBAN".to_string(),
            pattern: r"\b[A-Z]{2}\d{2}[A-Z0-9]{11,30}\b".to_string(),
            keywords: Vec::new(),
            validator: Some("IbanChecker".to_string()),
        }),
        unknown => Err(format!("unknown SDS preset '{unknown}'")),
    }
}

fn parse_validator(name: &str) -> Result<SecondaryValidator, String> {
    serde_json::from_value(serde_json::json!({ "type": name }))
        .map_err(|e| format!("invalid SDS SecondaryValidator '{name}': {e}"))
}

fn compile_scanner(
    presets: &[String],
    custom_rules: &[SdsRuleConfig],
) -> Result<(Option<Arc<Scanner>>, Vec<(String, String)>), String> {
    let mut all_rules = Vec::with_capacity(presets.len() + custom_rules.len());
    for p in presets {
        all_rules.push(preset_rule(p)?);
    }
    all_rules.extend(custom_rules.iter().cloned());

    if all_rules.is_empty() {
        return Ok((None, Vec::new()));
    }

    let mut sds_rules: Vec<RootRuleConfig<Arc<dyn RuleConfig>>> = Vec::with_capacity(all_rules.len());
    let mut rule_meta = Vec::with_capacity(all_rules.len());

    for r in all_rules {
        let mut cfg = RegexRuleConfig::new(&r.pattern);
        if !r.keywords.is_empty() {
            cfg = cfg.with_proximity_keywords(ProximityKeywordsConfig {
                look_ahead_character_count: 30,
                included_keywords: r.keywords,
                excluded_keywords: Vec::new(),
            });
        }
        if let Some(ref v_name) = r.validator {
            let trimmed = v_name.trim();
            if !trimmed.is_empty() {
                cfg = cfg.with_validator(Some(parse_validator(trimmed)?));
            }
        }
        sds_rules.push(RootRuleConfig::new_dyn(cfg));
        rule_meta.push((r.id, r.prefix));
    }

    let scanner = ScannerBuilder::new(&sds_rules)
        .with_return_matches(true)
        .build()
        .map_err(|e| format!("failed to compile SDS scanner: {e:?}"))?;

    Ok((Some(Arc::new(scanner)), rule_meta))
}

impl CompiledSdsVault {
    pub fn from_update(
        update: SdsPolicyUpdate,
        prev: Option<&CompiledSdsVault>,
    ) -> Result<Self, String> {
        let enabled = update
            .enabled
            .unwrap_or_else(|| prev.map(|p| p.enabled).unwrap_or(true));
        let mode = update
            .mode
            .unwrap_or_else(|| prev.map(|p| p.mode).unwrap_or(SdsMode::Vault));
        let rules_changed = update.presets.is_some() || update.rules.is_some();

        let presets = update
            .presets
            .unwrap_or_else(|| prev.map(|p| p.presets.clone()).unwrap_or_default());
        let custom_rules = update
            .rules
            .unwrap_or_else(|| prev.map(|p| p.custom_rules.clone()).unwrap_or_default());
        let block_rehydrate_tools = match update.block_rehydrate_tools {
            Some(tools) => tools.into_iter().collect(),
            None => prev
                .map(|p| p.block_rehydrate_tools.clone())
                .unwrap_or_default(),
        };

        let (scanner, rule_meta) = if !rules_changed {
            if let Some(p) = prev {
                (p.scanner.clone(), p.rule_meta.clone())
            } else {
                compile_scanner(&presets, &custom_rules)?
            }
        } else {
            compile_scanner(&presets, &custom_rules)?
        };

        Ok(Self {
            enabled,
            mode,
            presets,
            custom_rules,
            rule_meta,
            scanner,
            block_rehydrate_tools,
        })
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.mode != SdsMode::Off && self.scanner.is_some()
    }

    pub fn rules_count(&self) -> usize {
        self.rule_meta.len()
    }

    pub fn tokenize(&self, texts: &[&str]) -> TokenizeResult {
        if !self.is_active() {
            return TokenizeResult {
                texts: texts.iter().map(|s| (*s).to_string()).collect(),
                vault: HashMap::new(),
                matched_rule_ids: Vec::new(),
            };
        }
        let Some(ref scanner) = self.scanner else {
            return TokenizeResult {
                texts: texts.iter().map(|s| (*s).to_string()).collect(),
                vault: HashMap::new(),
                matched_rule_ids: Vec::new(),
            };
        };

        let mut vault: HashMap<String, String> = HashMap::new();
        let mut value_to_token: HashMap<String, String> = HashMap::new();
        let mut prefix_counts: HashMap<String, usize> = HashMap::new();
        let mut matched_set: HashSet<String> = HashSet::new();
        let mut matched_rule_ids: Vec<String> = Vec::new();
        let mut out_texts = Vec::with_capacity(texts.len());

        for &input in texts {
            if input.is_empty() {
                out_texts.push(String::new());
                continue;
            }
            let mut event = SimpleEvent::String(input.to_owned());
            let matches = match scanner.scan(&mut event) {
                Ok(m) => m,
                Err(err) => {
                    warn!(error = ?err, "SDS scan failed; leaving text unmodified");
                    out_texts.push(input.to_owned());
                    continue;
                }
            };
            if matches.is_empty() {
                out_texts.push(input.to_owned());
                continue;
            }

            let mut spans: Vec<(usize, usize, usize)> = matches
                .into_iter()
                .filter(|m| {
                    m.start_index < m.end_index_exclusive
                        && m.end_index_exclusive <= input.len()
                        && input.is_char_boundary(m.start_index)
                        && input.is_char_boundary(m.end_index_exclusive)
                })
                .map(|m| (m.start_index, m.end_index_exclusive, m.rule_index))
                .collect();

            spans.sort_by_key(|&(start, end, rule_idx)| (start, Reverse(end), rule_idx));

            let mut selected: Vec<(usize, usize, usize)> = Vec::with_capacity(spans.len());
            let mut last_end = 0usize;
            for span in spans {
                if span.0 >= last_end {
                    last_end = span.1;
                    selected.push(span);
                }
            }

            let mut rewritten = String::with_capacity(input.len());
            let mut cursor = 0usize;
            for (start, end, rule_idx) in selected {
                rewritten.push_str(&input[cursor..start]);
                let raw_val = &input[start..end];
                let (rule_id, prefix) = self
                    .rule_meta
                    .get(rule_idx)
                    .map(|(id, p)| (id.as_str(), p.as_str()))
                    .unwrap_or(("sds:unknown", "SENSITIVE"));

                if matched_set.insert(rule_id.to_string()) {
                    matched_rule_ids.push(rule_id.to_string());
                }

                match self.mode {
                    SdsMode::Vault => {
                        let token = if let Some(existing) = value_to_token.get(raw_val) {
                            existing.clone()
                        } else {
                            let counter = prefix_counts.entry(prefix.to_string()).or_insert(0);
                            *counter += 1;
                            let tok = format!("<{prefix}_{counter}>");
                            value_to_token.insert(raw_val.to_string(), tok.clone());
                            vault.insert(tok.clone(), raw_val.to_string());
                            tok
                        };
                        rewritten.push_str(&token);
                    }
                    SdsMode::Redact => {
                        rewritten.push_str(&format!("[REDACTED:{prefix}]"));
                    }
                    SdsMode::Off => {
                        rewritten.push_str(raw_val);
                    }
                }
                cursor = end;
            }
            rewritten.push_str(&input[cursor..]);
            out_texts.push(rewritten);
        }

        TokenizeResult {
            texts: out_texts,
            vault,
            matched_rule_ids,
        }
    }

    pub fn detokenize(
        &self,
        text: &str,
        vault: &HashMap<String, String>,
        tool_name: Option<&str>,
    ) -> DetokenizeResult {
        if text.is_empty() || vault.is_empty() {
            return DetokenizeResult {
                text: text.to_string(),
                blocked_tokens: Vec::new(),
            };
        }

        let blocked = tool_name
            .map(|t| self.block_rehydrate_tools.contains(t))
            .unwrap_or(false);

        if blocked {
            let mut blocked_tokens: Vec<String> = vault
                .keys()
                .filter(|tok| text.contains(tok.as_str()))
                .cloned()
                .collect();
            blocked_tokens.sort();
            return DetokenizeResult {
                text: text.to_string(),
                blocked_tokens,
            };
        }

        // Single-pass scan over `<...>` spans prevents chained substitution if an
        // original value happened to contain another placeholder string.
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                if let Some(rel_end) = bytes[i + 1..].iter().position(|&b| b == b'>') {
                    let end = i + 1 + rel_end + 1;
                    let candidate = &text[i..end];
                    if let Some(orig) = vault.get(candidate) {
                        out.push_str(orig);
                        i = end;
                        continue;
                    }
                }
            }
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }

        DetokenizeResult {
            text: out,
            blocked_tokens: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::eval::PolicyStore;

    #[test]
    fn sds_luhn_preset_tokenizes_valid_cc_and_ignores_invalid_luhn() {
        let store = PolicyStore::new();
        let cfg = br#"{
            "sds": {
                "enabled": true,
                "mode": "vault",
                "presets": ["credit_card", "email"],
                "block_rehydrate_tools": ["send_external_webhook"]
            }
        }"#;
        store.apply_json(cfg).expect("apply sds preset config");
        assert!(store.sds_active());

        // 4532-0151-1283-0366 passes Luhn; 4532-0151-1283-0367 fails Luhn.
        let res = store.tokenize(&[
            "Charge credit card 4532-0151-1283-0366 for alice@acme.com",
            "Order ref card 4532-0151-1283-0367 also notify alice@acme.com",
        ]);
        assert_eq!(
            res.texts[0],
            "Charge credit card <CC_1> for <EMAIL_1>"
        );
        // Same email reuses <EMAIL_1>; invalid Luhn number is untouched.
        assert_eq!(
            res.texts[1],
            "Order ref card 4532-0151-1283-0367 also notify <EMAIL_1>"
        );
        assert_eq!(res.vault.get("<CC_1>").map(String::as_str), Some("4532-0151-1283-0366"));
        assert_eq!(res.vault.get("<EMAIL_1>").map(String::as_str), Some("alice@acme.com"));
        assert_eq!(res.vault.len(), 2);

        // Normal user response re-hydrates cleanly.
        let detok = store.detokenize(
            "Receipt sent to <EMAIL_1> for card <CC_1>.",
            &res.vault,
            None,
        );
        assert_eq!(
            detok.text,
            "Receipt sent to alice@acme.com for card 4532-0151-1283-0366."
        );
        assert!(detok.blocked_tokens.is_empty());

        // Blocked tool refuses re-hydration and reports blocked tokens.
        let blocked = store.detokenize(
            r#"{"to":"<EMAIL_1>","card":"<CC_1>"}"#,
            &res.vault,
            Some("send_external_webhook"),
        );
        assert_eq!(blocked.text, r#"{"to":"<EMAIL_1>","card":"<CC_1>"}"#);
        assert_eq!(blocked.blocked_tokens, vec!["<CC_1>", "<EMAIL_1>"]);
    }

    #[test]
    fn custom_proximity_rule_and_mode_flips_preserve_scanner() {
        let store = PolicyStore::new();
        let cfg = br#"{
            "sds": {
                "enabled": true,
                "mode": "vault",
                "rules": [{
                    "id": "proj-code",
                    "prefix": "SECRET",
                    "pattern": "ACME-[0-9]{6}",
                    "keywords": ["codename", "project"]
                }]
            }
        }"#;
        store.apply_json(cfg).expect("apply custom sds rule");

        let hit = store.tokenize(&["Deploying codename ACME-123456 tonight"]);
        assert_eq!(hit.texts[0], "Deploying codename <SECRET_1> tonight");
        assert_eq!(hit.matched_rule_ids, vec!["proj-code"]);

        // Missing proximity keyword -> no match.
        let miss = store.tokenize(&["Tracking invoice ACME-123456 tonight"]);
        assert_eq!(miss.texts[0], "Tracking invoice ACME-123456 tonight");
        assert!(miss.vault.is_empty());

        // Partial flip to redact mode preserves compiled rules and leaves vault empty.
        store
            .apply_json(br#"{"sds":{"mode":"redact"}}"#)
            .expect("flip mode=redact");
        let red = store.tokenize(&["Deploying codename ACME-123456 tonight"]);
        assert_eq!(red.texts[0], "Deploying codename [REDACTED:SECRET] tonight");
        assert!(red.vault.is_empty());

        // Partial non-SDS update (blocking=false) preserves SDS scanner.
        store
            .apply_json(br#"{"blocking":false}"#)
            .expect("flip blocking=false");
        assert!(store.sds_active());

        // Top-level kill switch disables SDS too.
        store
            .apply_json(br#"{"enabled":false}"#)
            .expect("kill switch");
        assert!(!store.sds_active());
        let off = store.tokenize(&["Deploying codename ACME-123456 tonight"]);
        assert_eq!(off.texts[0], "Deploying codename ACME-123456 tonight");
    }
}
