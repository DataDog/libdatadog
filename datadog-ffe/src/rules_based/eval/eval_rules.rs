use crate::rules_based::{
    AttributeValue,
    ufc::{ComparisonOperator, Condition, ConditionCheck, RuleWire, TryParse},
};

use super::{eval_visitor::EvalRuleVisitor, subject::Subject};

impl RuleWire {
    pub(super) fn eval<V: EvalRuleVisitor>(&self, visitor: &mut V, subject: &Subject) -> bool {
        self.conditions.iter().all(|condition| match condition {
            TryParse::Parsed(condition) => condition.eval(visitor, subject),
            TryParse::ParseFailed(_) => false,
        })
    }
}

impl Condition {
    fn eval<V: EvalRuleVisitor>(&self, visitor: &mut V, subject: &Subject) -> bool {
        let attribute = subject.get_attribute(self.attribute.as_ref());
        let result = self.check.eval(attribute);
        visitor.on_condition_eval(self, attribute, result);
        result
    }
}

impl ConditionCheck {
    /// Check if `attribute` matches.
    fn eval(&self, attribute: Option<&AttributeValue>) -> bool {
        self.try_eval(attribute).unwrap_or(false)
    }

    /// Try applying `Operator` to the values, returning `None` if the operator cannot be applied.
    fn try_eval(&self, attribute: Option<&AttributeValue>) -> Option<bool> {
        let result = match self {
            ConditionCheck::Comparison {
                operator,
                comparand,
            } => {
                let attribute = attribute?.clone();
                let attribute = attribute.coerce_to_number()?;
                let ordering = attribute.partial_cmp(comparand)?;
                match operator {
                    ComparisonOperator::Gte => ordering.is_gt() || ordering.is_eq(),
                    ComparisonOperator::Gt => ordering.is_gt(),
                    ComparisonOperator::Lte => ordering.is_lt() || ordering.is_eq(),
                    ComparisonOperator::Lt => ordering.is_lt(),
                }
            }
            ConditionCheck::Regex {
                expected_match,
                regex,
            } => regex.is_match(attribute?.coerce_to_string()?.as_ref()) == *expected_match,
            ConditionCheck::Membership {
                expected_membership,
                values,
            } => {
                let s = attribute?.coerce_to_string()?;
                let s = s.as_ref();
                values.into_iter().any(|it| it.as_ref() == s) == *expected_membership
            }
            ConditionCheck::Null { expected_null } => {
                let is_present = attribute.is_some_and(|it| !it.is_null());
                let is_null = !is_present;
                is_null == *expected_null
            }
        };

        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use crate::rules_based::{
        eval::{eval_visitor::NoopEvalVisitor, subject::Subject},
        ufc::{ComparisonOperator, Condition, ConditionCheck, RuleWire},
    };

    #[test]
    fn matches_regex() {
        let check = ConditionCheck::Regex {
            expected_match: true,
            regex: "^test.*".try_into().unwrap(),
        };
        assert!(check.eval(Some(&"test@example.com".into())));
        assert!(!check.eval(Some(&"example@test.com".into())));
        assert!(!check.eval(None));
    }

    #[test]
    fn not_matches_regex() {
        let check = ConditionCheck::Regex {
            expected_match: false,
            regex: "^test.*".try_into().unwrap(),
        };
        assert!(!check.eval(Some(&"test@example.com".into())));
        assert!(check.eval(Some(&"example@test.com".into())));
        assert!(!check.eval(None));
    }

    #[test]
    fn one_of() {
        let check = ConditionCheck::Membership {
            expected_membership: true,
            values: ["alice".into(), "bob".into()].into(),
        };
        assert!(check.eval(Some(&"alice".into())));
        assert!(check.eval(Some(&"bob".into())));
        assert!(!check.eval(Some(&"charlie".into())));
    }

    #[test]
    fn not_one_of() {
        let check = ConditionCheck::Membership {
            expected_membership: false,
            values: ["alice".into(), "bob".into()].into(),
        };
        assert!(!check.eval(Some(&"alice".into())));
        assert!(!check.eval(Some(&"bob".into())));
        assert!(check.eval(Some(&"charlie".into())));

        // NOT_ONE_OF fails when attribute is not specified
        assert!(!check.eval(None));
    }

    #[test]
    fn one_of_int() {
        assert!(
            ConditionCheck::Membership {
                expected_membership: true,
                values: ["42".into()].into()
            }
            .eval(Some(&42.0.into()))
        );
    }

    #[test]
    fn one_of_bool() {
        let true_check = ConditionCheck::Membership {
            expected_membership: true,
            values: ["true".into()].into(),
        };
        let false_check = ConditionCheck::Membership {
            expected_membership: true,
            values: ["false".into()].into(),
        };
        assert!(true_check.eval(Some(&true.into())));
        assert!(false_check.eval(Some(&false.into())));
        assert!(!true_check.eval(Some(&1.0.into())));
        assert!(!false_check.eval(Some(&0.0.into())));
        assert!(!true_check.eval(None));
        assert!(!false_check.eval(None));
    }

    #[test]
    fn is_null() {
        assert!(
            ConditionCheck::Null {
                expected_null: true
            }
            .eval(None)
        );
        assert!(
            !ConditionCheck::Null {
                expected_null: true
            }
            .eval(Some(&10.0.into()))
        );
    }

    #[test]
    fn is_not_null() {
        assert!(
            !ConditionCheck::Null {
                expected_null: false
            }
            .eval(None)
        );
        assert!(
            ConditionCheck::Null {
                expected_null: false
            }
            .eval(Some(&10.0.into()))
        );
    }

    #[test]
    fn gte() {
        let check = ConditionCheck::Comparison {
            operator: ComparisonOperator::Gte,
            comparand: 18.0,
        };
        assert!(check.eval(Some(&18.0.into())));
        assert!(!check.eval(Some(&17.0.into())));
    }
    #[test]
    fn gt() {
        let check = ConditionCheck::Comparison {
            operator: ComparisonOperator::Gt,
            comparand: 18.0,
        };
        assert!(check.eval(Some(&19.0.into())));
        assert!(!check.eval(Some(&18.0.into())));
    }
    #[test]
    fn lte() {
        let check = ConditionCheck::Comparison {
            operator: ComparisonOperator::Lte,
            comparand: 18.0,
        };
        assert!(check.eval(Some(&18.0.into())));
        assert!(!check.eval(Some(&19.0.into())));
    }
    #[test]
    fn lt() {
        let check = ConditionCheck::Comparison {
            operator: ComparisonOperator::Lt,
            comparand: 18.0,
        };
        assert!(check.eval(Some(&17.0.into())));
        assert!(!check.eval(Some(&18.0.into())));
    }

    #[test]
    fn empty_rule() {
        let rule = RuleWire { conditions: vec![] };
        assert!(rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new("key".into(), Default::default())
        ));
    }

    #[test]
    fn single_condition_rule() {
        let rule = RuleWire {
            conditions: vec![
                Condition {
                    attribute: "age".into(),
                    check: ConditionCheck::Comparison {
                        operator: ComparisonOperator::Gt,
                        comparand: 10.0,
                    },
                }
                .into(),
            ],
        };
        assert!(rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new(
                "key".into(),
                Arc::new(HashMap::from([("age".into(), 11.0.into())]))
            )
        ));
    }

    #[test]
    fn two_condition_rule() {
        let rule = RuleWire {
            conditions: vec![
                Condition {
                    attribute: "age".into(),
                    check: ConditionCheck::Comparison {
                        operator: ComparisonOperator::Gt,
                        comparand: 18.0,
                    },
                }
                .into(),
                Condition {
                    attribute: "age".into(),
                    check: ConditionCheck::Comparison {
                        operator: ComparisonOperator::Lt,
                        comparand: 100.0,
                    },
                }
                .into(),
            ],
        };
        assert!(rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new(
                "key".into(),
                Arc::new(HashMap::from([("age".into(), 20.0.into())]))
            )
        ));
        assert!(!rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new(
                "key".into(),
                Arc::new(HashMap::from([("age".into(), 17.0.into())]))
            )
        ));
        assert!(!rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new(
                "key".into(),
                Arc::new(HashMap::from([("age".into(), 110.0.into())]))
            )
        ));
    }

    #[test]
    fn missing_attribute() {
        let rule = RuleWire {
            conditions: vec![
                Condition {
                    attribute: "age".into(),
                    check: ConditionCheck::Comparison {
                        operator: ComparisonOperator::Gt,
                        comparand: 10.0,
                    },
                }
                .into(),
            ],
        };
        assert!(!rule.eval(
            &mut NoopEvalVisitor,
            &Subject::new(
                "key".into(),
                Arc::new(HashMap::from([("name".into(), "alice".into())]))
            )
        ));
    }
}
