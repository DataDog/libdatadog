// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionAction {
    Allow,
    Deny,
    FallbackRemote,
}

impl DecisionAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "ALLOW",
            Self::Deny => "DENY",
            Self::FallbackRemote => "FALLBACK_REMOTE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: DecisionAction,
    pub rule_id: Option<String>,
    pub blocking: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RuleType {
    #[serde(alias = "REGEX")]
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum PolicyAction {
    Allow,
    Deny,
    Evaluate,
    FallbackRemote,
}

impl PolicyAction {
    fn to_decision_action(self) -> Option<DecisionAction> {
        match self {
            Self::Allow => Some(DecisionAction::Allow),
            Self::Deny => Some(DecisionAction::Deny),
            Self::Evaluate | Self::FallbackRemote => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct StaticRuleConfig {
    id: String,
    #[serde(rename = "type")]
    rule_type: RuleType,
    pattern: String,
    action: PolicyAction,
}

#[derive(Debug, Clone, Deserialize)]
struct PolicyUpdate {
    enabled: Option<bool>,
    blocking: Option<bool>,
    rules: Option<Vec<StaticRuleConfig>>,
    tool_policies: Option<HashMap<String, PolicyAction>>,
}

#[derive(Debug)]
struct CompiledRule {
    id: String,
    regex: Regex,
    action: PolicyAction,
}

#[derive(Debug)]
struct CompiledPolicy {
    enabled: bool,
    blocking: bool,
    rules: Vec<CompiledRule>,
    tool_policies: HashMap<String, PolicyAction>,
}

#[derive(Debug, Default, Clone)]
pub struct PolicyStore {
    active: Arc<ArcSwap<Option<CompiledPolicy>>>,
}

impl PolicyStore {
    pub fn new() -> Self {
        Self {
            active: Arc::new(ArcSwap::from_pointee(None)),
        }
    }

    pub fn has_policy(&self) -> bool {
        self.active.load().is_some()
    }

    pub fn apply_json(&self, raw: &[u8]) -> Result<(), String> {
        let update: PolicyUpdate =
            serde_json::from_slice(raw).map_err(|e| format!("invalid policy JSON: {e}"))?;

        if update.enabled.is_none()
            && update.blocking.is_none()
            && update.rules.is_none()
            && update.tool_policies.is_none()
        {
            return Err("empty policy update".to_string());
        }

        let prev = self.active.load();
        let prev_ref = prev.as_ref().as_ref();

        let enabled = update
            .enabled
            .unwrap_or_else(|| prev_ref.map(|p| p.enabled).unwrap_or(true));
        let blocking = update
            .blocking
            .unwrap_or_else(|| prev_ref.map(|p| p.blocking).unwrap_or(true));

        let rules = match update.rules {
            Some(raw_rules) => {
                let mut compiled = Vec::with_capacity(raw_rules.len());
                for r in raw_rules {
                    match r.rule_type {
                        RuleType::Regex => {
                            let regex = Regex::new(&r.pattern).map_err(|e| {
                                format!("invalid regex in rule '{}': {e}", r.id)
                            })?;
                            compiled.push(CompiledRule {
                                id: r.id,
                                regex,
                                action: r.action,
                            });
                        }
                    }
                }
                compiled
            }
            None => prev_ref
                .map(|p| {
                    p.rules
                        .iter()
                        .map(|r| CompiledRule {
                            id: r.id.clone(),
                            regex: r.regex.clone(),
                            action: r.action,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        let tool_policies = match update.tool_policies {
            Some(tp) => tp,
            None => prev_ref
                .map(|p| p.tool_policies.clone())
                .unwrap_or_default(),
        };

        info!(
            enabled,
            blocking,
            rules_count = rules.len(),
            tool_policies_count = tool_policies.len(),
            "swapped active local AI Guard policy"
        );

        self.active.store(Arc::new(Some(CompiledPolicy {
            enabled,
            blocking,
            rules,
            tool_policies,
        })));
        Ok(())
    }

    pub fn try_apply_payload(&self, raw: &[u8]) -> bool {
        let trimmed = raw
            .iter()
            .copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .next();
        if trimmed != Some(b'{') {
            return false;
        }
        match self.apply_json(raw) {
            Ok(()) => true,
            Err(err) => {
                warn!(error = %err, "ignoring JSON payload that is not a valid AI Guard policy");
                false
            }
        }
    }

    pub fn evaluate(&self, content: &str, tool_name: Option<&str>) -> Decision {
        let guard = self.active.load();
        let Some(policy) = guard.as_ref().as_ref() else {
            return Decision {
                action: DecisionAction::FallbackRemote,
                rule_id: None,
                blocking: false,
            };
        };

        if !policy.enabled {
            debug!("AI Guard local kill switch active (enabled=false)");
            return Decision {
                action: DecisionAction::Allow,
                rule_id: Some("kill_switch".to_string()),
                blocking: false,
            };
        }

        if let Some(tool) = tool_name {
            if let Some(policy_action) = policy.tool_policies.get(tool) {
                if let Some(action) = policy_action.to_decision_action() {
                    return Decision {
                        action,
                        rule_id: Some(format!("tool:{tool}")),
                        blocking: policy.blocking,
                    };
                }
            }
        }

        for rule in &policy.rules {
            if rule.regex.is_match(content) {
                if let Some(action) = rule.action.to_decision_action() {
                    return Decision {
                        action,
                        rule_id: Some(rule.id.clone()),
                        blocking: policy.blocking,
                    };
                }
            }
        }

        Decision {
            action: DecisionAction::FallbackRemote,
            rule_id: None,
            blocking: policy.blocking,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_policy_falls_back_to_remote() {
        let store = PolicyStore::new();
        let d = store.evaluate("hello world", None);
        assert_eq!(
            d,
            Decision {
                action: DecisionAction::FallbackRemote,
                rule_id: None,
                blocking: false,
            }
        );
    }

    #[test]
    fn regex_tool_policies_mode_flip_and_kill_switch() {
        let store = PolicyStore::new();

        let cfg = br#"{
            "enabled": true,
            "blocking": true,
            "rules": [
                {"id": "r1", "type": "REGEX", "pattern": "(?i)secret_project", "action": "DENY"}
            ],
            "tool_policies": {
                "rm_rf": "DENY",
                "read_docs": "ALLOW"
            }
        }"#;
        store.apply_json(cfg).expect("apply full config");

        assert_eq!(
            store.evaluate("tell me about SECRET_PROJECT", None),
            Decision {
                action: DecisionAction::Deny,
                rule_id: Some("r1".to_string()),
                blocking: true,
            }
        );

        assert_eq!(
            store.evaluate("clean prompt", Some("rm_rf")),
            Decision {
                action: DecisionAction::Deny,
                rule_id: Some("tool:rm_rf".to_string()),
                blocking: true,
            }
        );

        assert_eq!(
            store.evaluate("clean prompt", Some("read_docs")),
            Decision {
                action: DecisionAction::Allow,
                rule_id: Some("tool:read_docs".to_string()),
                blocking: true,
            }
        );

        assert_eq!(
            store.evaluate("normal question", Some("search")),
            Decision {
                action: DecisionAction::FallbackRemote,
                rule_id: None,
                blocking: true,
            }
        );

        // Live partial flip to monitor-only keeps existing rules & tool_policies.
        store
            .apply_json(br#"{"blocking": false}"#)
            .expect("flip blocking=false");
        assert_eq!(
            store.evaluate("tell me about secret_project", None),
            Decision {
                action: DecisionAction::Deny,
                rule_id: Some("r1".to_string()),
                blocking: false,
            }
        );

        // Instant kill switch short-circuits to ALLOW.
        store
            .apply_json(br#"{"enabled": false}"#)
            .expect("flip enabled=false");
        assert_eq!(
            store.evaluate("tell me about secret_project", Some("rm_rf")),
            Decision {
                action: DecisionAction::Allow,
                rule_id: Some("kill_switch".to_string()),
                blocking: false,
            }
        );
    }

    #[test]
    fn non_json_payload_is_ignored_without_overwriting_policy() {
        let store = PolicyStore::new();
        assert!(!store.try_apply_payload(b"hello"));
        assert!(!store.has_policy());
    }
}
