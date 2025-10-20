use crate::rules_based::{
    AttributeValue, Configuration,
    error::EvaluationFailure,
    ufc::{Allocation, Assignment, Condition, Flag, RuleWire, Shard, Split},
};

use super::eval_assignment::AllocationNonMatchReason;

pub(super) trait EvalAssignmentVisitor {
    // Type-foo here basically means that AllocationVisitor may hold references to EvalFlagVisitor
    // but should not outlive it.
    type AllocationVisitor<'a>: EvalAllocationVisitor + 'a
    where
        Self: 'a;

    /// Called when (if) evaluation gets configuration.
    fn on_configuration(&mut self, configuration: &Configuration);

    /// Called when evaluation finds the flag configuration.
    fn on_flag_configuration(&mut self, flag: &Flag);

    /// Called before evaluation an allocation.
    fn visit_allocation<'a>(&'a mut self, allocation: &Allocation) -> Self::AllocationVisitor<'a>;

    /// Called with evaluation result.
    fn on_result(&mut self, result: &Result<Assignment, EvaluationFailure>);
}

pub(super) trait EvalAllocationVisitor {
    type RuleVisitor<'a>: EvalRuleVisitor + 'a
    where
        Self: 'a;

    type SplitVisitor<'a>: EvalSplitVisitor + 'a
    where
        Self: 'a;

    /// Called before evaluating a rule.
    fn visit_rule<'a>(&'a mut self, rule: &RuleWire) -> Self::RuleVisitor<'a>;

    /// Called before evaluating a split.
    fn visit_split<'a>(&'a mut self, split: &Split) -> Self::SplitVisitor<'a>;

    /// Called when allocation evaluation result is known. This functions gets passed either the
    /// split matched, or the reason why this allocation was not matched.
    fn on_result(&mut self, result: Result<&Split, AllocationNonMatchReason>);
}

pub(super) trait EvalRuleVisitor {
    fn on_condition_eval(
        &mut self,
        condition: &Condition,
        attribute_value: Option<&AttributeValue>,
        result: bool,
    );

    fn on_result(&mut self, result: bool);
}

pub(super) trait EvalSplitVisitor {
    fn on_shard_eval(&mut self, shard: &Shard, shard_value: u32, matches: bool);

    fn on_result(&mut self, matches: bool);
}

/// Dummy visitor that does nothing.
///
/// It is designed so that all calls to it are optimized away (zero-cost).
pub(super) struct NoopEvalVisitor;

impl EvalAssignmentVisitor for NoopEvalVisitor {
    type AllocationVisitor<'a> = NoopEvalVisitor;

    #[inline]
    fn visit_allocation<'a>(&'a mut self, _allocation: &Allocation) -> Self::AllocationVisitor<'a> {
        NoopEvalVisitor
    }

    #[inline]
    fn on_configuration(&mut self, _configuration: &Configuration) {}

    #[inline]
    fn on_flag_configuration(&mut self, _flag: &Flag) {}

    #[inline]
    fn on_result(&mut self, _result: &Result<Assignment, EvaluationFailure>) {}
}

impl EvalAllocationVisitor for NoopEvalVisitor {
    type RuleVisitor<'a> = NoopEvalVisitor;

    type SplitVisitor<'a> = NoopEvalVisitor;

    #[inline]
    fn visit_rule<'a>(&'a mut self, _rule: &RuleWire) -> Self::RuleVisitor<'a> {
        NoopEvalVisitor
    }

    #[inline]
    fn visit_split<'a>(&'a mut self, _split: &Split) -> Self::SplitVisitor<'a> {
        NoopEvalVisitor
    }

    #[inline]
    fn on_result(&mut self, _result: Result<&Split, AllocationNonMatchReason>) {}
}

impl EvalRuleVisitor for NoopEvalVisitor {
    #[inline]
    fn on_condition_eval(
        &mut self,
        _condition: &Condition,
        _attribute_value: Option<&AttributeValue>,
        _result: bool,
    ) {
    }

    #[inline]
    fn on_result(&mut self, _result: bool) {}
}

impl EvalSplitVisitor for NoopEvalVisitor {
    #[inline]
    fn on_shard_eval(&mut self, _shard: &Shard, _shard_value: u32, _matches: bool) {}

    #[inline]
    fn on_result(&mut self, _matches: bool) {}
}
