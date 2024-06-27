// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::debugger_defs::SnapshotEvaluationError;
use crate::expr_defs::{
    BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
    Reference, StringComparison, StringSource, Value,
};
use regex::Regex;
use std::borrow::Cow;
use std::cmp::min;
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::usize;

#[derive(Debug)]
pub struct DslString(pub(crate) Vec<DslPart>);
#[derive(Debug)]
pub struct ProbeValue(pub(crate) Value);
#[derive(Debug)]
pub struct ProbeCondition(pub(crate) Condition);

impl Display for DslString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for p in self.0.iter() {
            p.fmt(f)?;
        }
        Ok(())
    }
}

impl Display for ProbeValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Display for ProbeCondition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub enum IntermediateValue<'a, I> {
    String(Cow<'a, str>),
    Number(f64),
    Bool(bool),
    Null,
    Referenced(&'a I),
}

impl<'a, I> Clone for IntermediateValue<'a, I> {
    fn clone(&self) -> Self {
        match self {
            IntermediateValue::String(s) => IntermediateValue::String(s.clone()),
            IntermediateValue::Number(n) => IntermediateValue::Number(*n),
            IntermediateValue::Bool(b) => IntermediateValue::Bool(*b),
            IntermediateValue::Null => IntermediateValue::Null,
            IntermediateValue::Referenced(r) => IntermediateValue::Referenced(*r),
        }
    }
}

pub trait Evaluator<'e, I> {
    fn equals(&mut self, a: IntermediateValue<'e, I>, b: IntermediateValue<'e, I>) -> bool;
    fn greater_than(&mut self, a: IntermediateValue<'e, I>, b: IntermediateValue<'e, I>) -> bool;
    fn greater_or_equals(
        &mut self,
        a: IntermediateValue<'e, I>,
        b: IntermediateValue<'e, I>,
    ) -> bool;
    fn fetch_identifier(&mut self, identifier: &str) -> Option<&'e I>; // special values: @duration, @return, @exception
    fn fetch_index(&mut self, value: &'e I, index: IntermediateValue<'e, I>) -> Option<&'e I>;
    fn fetch_nested(&mut self, value: &'e I, member: IntermediateValue<'e, I>) -> Option<&'e I>;
    fn length(&mut self, value: &'e I) -> usize;
    fn try_enumerate(&mut self, value: &'e I) -> Option<Vec<&'e I>>;
    fn stringify(&mut self, value: &'e I) -> Cow<'e, str>; // generic string representation
    fn get_string(&mut self, value: &'e I) -> Cow<'e, str>; // log output-formatted string
    fn convert_index(&mut self, value: &'e I) -> Option<usize>;
    fn instanceof(&mut self, value: &'e I, class: &'e str) -> bool;
}

enum UndefFetch<'a, I> {
    NoIterator,
    Identifier(&'a String),
    Index(
        &'a CollectionSource,
        &'a Value,
        &'a I,
        IntermediateValue<'a, I>,
    ),
    IndexEvaluated(&'a CollectionSource, &'a Value, Vec<&'a I>, usize),
    Nested(&'a Reference, &'a Value, &'a I, IntermediateValue<'a, I>),
}

#[derive(Debug)]
struct EvErr(String);

impl EvErr {
    pub fn str<T: ToString>(str: T) -> Self {
        EvErr(str.to_string())
    }

    pub fn refed<'a, 'e, I, E: Evaluator<'e, I>>(
        eval: &mut Eval<'a, 'e, I, E>,
        reference: UndefFetch<'e, I>,
    ) -> Self {
        EvErr(match reference {
            UndefFetch::NoIterator => "Attempted to use @it in non-iterator context".to_string(),
            UndefFetch::Identifier(reference) => format!("Could not fetch {reference}"),
            UndefFetch::Index(source, dim, base, index) => {
                if matches!(
                    dim,
                    Value::String(StringSource::String(_)) | Value::Number(NumberSource::Number(_))
                ) {
                    format!(
                        "Could not fetch index {dim} on {source} (evaluated to {})",
                        eval.iref_string(base)
                    )
                } else {
                    format!("Could not fetch index {dim} (evaluated to {}) on {source} (evaluated to {})", eval.get_string(index), eval.iref_string(base))
                }
            }
            UndefFetch::IndexEvaluated(source, dim, vec, index) => {
                let first_entries: Vec<_> = vec
                    .into_iter()
                    .take(5)
                    .map(|i| eval.iref_string(i))
                    .collect();
                if matches!(
                    dim,
                    Value::String(StringSource::String(_)) | Value::Number(NumberSource::Number(_))
                ) {
                    format!(
                        "Could not fetch index {dim} on {source} (evaluated to [{}])",
                        first_entries.join(", ")
                    )
                } else {
                    format!("Could not fetch index {dim} (evaluated to {index}) on {source} (evaluated to [{}])", first_entries.join(", "))
                }
            }
            UndefFetch::Nested(source, prop, base, nested) => {
                if matches!(
                    prop,
                    Value::String(StringSource::String(_)) | Value::Number(NumberSource::Number(_))
                ) {
                    format!(
                        "Could not fetch property {prop} on {source} (evaluated to {})",
                        eval.iref_string(base)
                    )
                } else {
                    format!("Could not fetch property {prop} (evaluated to {}) on {source} (evaluated to {})", eval.get_string(nested), eval.iref_string(base))
                }
            }
        })
    }
}

type EvalResult<T> = Result<T, EvErr>;

struct DefOrUndefRef<'a, I>(Result<&'a I, UndefFetch<'a, I>>);

impl<'e, I> DefOrUndefRef<'e, I> {
    pub fn into_imm(self) -> InternalImm<'e, I> {
        match self.0 {
            Ok(referenced) => InternalImm::Def(IntermediateValue::Referenced(referenced)),
            Err(reference) => InternalImm::Undef(reference),
        }
    }

    pub fn try_use<'a, E: Evaluator<'e, I>>(
        self,
        eval: &mut Eval<'a, 'e, I, E>,
    ) -> EvalResult<&'e I> {
        match self.0 {
            Ok(referenced) => Ok(referenced),
            Err(reference) => Err(EvErr::refed(eval, reference)),
        }
    }
}

enum InternalImm<'a, I> {
    Def(IntermediateValue<'a, I>),
    Undef(UndefFetch<'a, I>),
}

impl<'e, I> InternalImm<'e, I> {
    pub fn try_use<'a, E: Evaluator<'e, I>>(
        self,
        eval: &mut Eval<'a, 'e, I, E>,
    ) -> EvalResult<IntermediateValue<'e, I>> {
        match self {
            InternalImm::Def(referenced) => Ok(referenced),
            InternalImm::Undef(reference) => Err(EvErr::refed(eval, reference)),
        }
    }
}

struct Eval<'a, 'e, I, E: Evaluator<'e, I>> {
    eval: &'a mut E,
    it: Option<&'e I>,
}

impl<'a, 'e, I, E: Evaluator<'e, I>> Eval<'a, 'e, I, E> {
    fn iref_string(&mut self, value: &'e I) -> Cow<'e, str> {
        self.eval.get_string(value)
    }

    fn value(&mut self, value: &'e Value) -> EvalResult<InternalImm<'e, I>> {
        Ok(match value {
            Value::Bool(condition) => {
                InternalImm::Def(IntermediateValue::Bool(self.condition(condition)?))
            }
            Value::String(s) => self.string_source(s)?,
            Value::Number(n) => self.number_source(n)?,
        })
    }

    fn number_source(&mut self, value: &'e NumberSource) -> EvalResult<InternalImm<'e, I>> {
        Ok(match value {
            NumberSource::Number(n) => InternalImm::Def(IntermediateValue::Number(*n)),
            NumberSource::CollectionSize(collection) => {
                InternalImm::Def(IntermediateValue::Number(match collection {
                    CollectionSource::Reference(reference) => {
                        let immediate = self.reference(reference)?.try_use(self)?;
                        self.eval.length(immediate) as f64
                    }
                    CollectionSource::FilterOperator(_) => {
                        self.collection_source(collection)?.len() as f64
                    }
                }))
            }
            NumberSource::StringLength(reference) => {
                let immediate = self.reference(reference)?.try_use(self)?;
                InternalImm::Def(IntermediateValue::Number(self.eval.length(immediate) as f64))
            }
            NumberSource::Reference(reference) => self.reference(reference)?.into_imm(),
        })
    }

    fn convert_index(&mut self, value: IntermediateValue<'e, I>) -> EvalResult<usize> {
        match value {
            IntermediateValue::String(s) => usize::from_str(&s).map_err(EvErr::str),
            IntermediateValue::Number(n) => Ok(n as usize),
            IntermediateValue::Bool(_) => Err(EvErr::str("Cannot take index of boolean")),
            IntermediateValue::Null => Ok(0),
            IntermediateValue::Referenced(referenced) => {
                self.eval
                    .convert_index(referenced)
                    .ok_or(EvErr::str(format!(
                        "Cannot convert {} to an index",
                        self.iref_string(referenced)
                    )))
            }
        }
    }

    fn number_to_index(&mut self, value: &'e NumberSource) -> EvalResult<usize> {
        let val = self.number_source(value)?.try_use(self)?;
        self.convert_index(val)
            .map_err(|e| EvErr::str(format!("{} (from {value})", e.0)))
    }

    fn string_source(&mut self, value: &'e StringSource) -> EvalResult<InternalImm<'e, I>> {
        Ok(match value {
            StringSource::String(s) => {
                InternalImm::Def(IntermediateValue::String(Cow::Borrowed(s.as_str())))
            }
            StringSource::Substring(boxed) => {
                let (string, start, end) = &**boxed;
                let str = self.stringify(string)?;
                let start = self.number_to_index(start)?;
                let mut end = self.number_to_index(end)?;
                if start > end || start >= str.len() {
                    return Err(EvErr::str(format!(
                        "[{start}..{end}] is out of bounds of {value} (string size: {})",
                        str.len()
                    )));
                }
                end = min(end, str.len());
                InternalImm::Def(IntermediateValue::String(match str {
                    Cow::Owned(s) => Cow::Owned(s[start..end].to_string()),
                    Cow::Borrowed(s) => Cow::Borrowed(&s[start..end]),
                }))
            }
            StringSource::Null => InternalImm::Def(IntermediateValue::Null),
            StringSource::Reference(reference) => self.reference(reference)?.into_imm(),
        })
    }

    fn reference_collection(&mut self, reference: &'e Reference) -> EvalResult<Vec<&'e I>> {
        let immediate = self.reference(reference)?.try_use(self)?;
        self.eval.try_enumerate(immediate).ok_or_else(|| {
            EvErr::str(format!(
                "Cannot enumerate non iterable type: {reference}; evaluating to: {}",
                self.iref_string(immediate)
            ))
        })
    }

    fn reference(&mut self, reference: &'e Reference) -> EvalResult<DefOrUndefRef<'e, I>> {
        Ok(DefOrUndefRef(match reference {
            Reference::IteratorVariable => self.it.ok_or(UndefFetch::NoIterator),
            Reference::Base(ref identifier) => self
                .eval
                .fetch_identifier(identifier.as_str())
                .ok_or(UndefFetch::Identifier(identifier)),
            Reference::Index(ref boxed) => {
                let (source, dimension) = &**boxed;
                let dimension_val = self.value(dimension)?.try_use(self)?;
                match source {
                    CollectionSource::FilterOperator(_) => {
                        let index = self
                            .convert_index(dimension_val)
                            .map_err(|e| EvErr::str(format!("{} (from {dimension}", e.0)))?;
                        let vec = self.collection_source(source)?;
                        if index < vec.len() {
                            Ok(vec[index])
                        } else {
                            Err(UndefFetch::IndexEvaluated(source, dimension, vec, index))
                        }
                    }
                    CollectionSource::Reference(ref reference) => {
                        self.reference(reference)?.0.and_then(|reference_val| {
                            self.eval
                                .fetch_index(reference_val, dimension_val.clone())
                                .ok_or(UndefFetch::Index(
                                    source,
                                    dimension,
                                    reference_val,
                                    dimension_val,
                                ))
                        })
                    }
                }
            }
            Reference::Nested(ref boxed) => {
                let (source, member) = &**boxed;
                let member_val = self.value(member)?.try_use(self)?;
                self.reference(source)?.0.and_then(|source_val| {
                    self.eval
                        .fetch_nested(source_val, member_val.clone())
                        .ok_or(UndefFetch::Nested(source, member, source_val, member_val))
                })
            }
        }))
    }

    fn collection_source(&mut self, collection: &'e CollectionSource) -> EvalResult<Vec<&'e I>> {
        Ok(match collection {
            CollectionSource::Reference(ref reference) => self.reference_collection(reference)?,
            CollectionSource::FilterOperator(ref boxed) => {
                let (source, condition) = &**boxed;
                let mut values = vec![];
                let it = self.it;
                for item in self.collection_source(source)? {
                    self.it = Some(item);
                    if self.condition(condition)? {
                        values.push(item);
                    }
                }
                self.it = it;
                values
            }
        })
    }

    fn stringify_intermediate(&mut self, value: IntermediateValue<'e, I>) -> Cow<'e, str> {
        match value {
            IntermediateValue::String(s) => s,
            IntermediateValue::Number(n) => Cow::Owned(n.to_string()),
            IntermediateValue::Bool(b) => Cow::Owned(b.to_string()),
            IntermediateValue::Null => Cow::Borrowed(""),
            IntermediateValue::Referenced(referenced) => self.eval.stringify(referenced),
        }
    }

    fn get_string(&mut self, value: IntermediateValue<'e, I>) -> Cow<'e, str> {
        if let IntermediateValue::Referenced(referenced) = value {
            self.iref_string(referenced)
        } else {
            self.stringify_intermediate(value)
        }
    }

    fn stringify(&mut self, value: &'e StringSource) -> EvalResult<Cow<'e, str>> {
        let value = self.string_source(value)?.try_use(self)?;
        Ok(self.stringify_intermediate(value))
    }

    fn condition(&mut self, condition: &'e Condition) -> EvalResult<bool> {
        Ok(match condition {
            Condition::Always => true,
            Condition::Never => false,
            Condition::StringComparison(comparer, haystack, needle) => {
                let haystack = self.stringify(haystack)?;
                match comparer {
                    StringComparison::StartsWith => haystack.starts_with(needle),
                    StringComparison::EndsWith => haystack.ends_with(needle),
                    StringComparison::Contains => haystack.contains(needle),
                    StringComparison::Matches => {
                        return Regex::new(needle.as_str())
                            .map_err(|e| EvErr::str(format!("{needle} is an invalid regex: {e}")))
                            .map(|r| r.is_match(&haystack))
                    }
                }
            }
            Condition::BinaryComparison(a, comparer, b) => {
                let (a, b) = (self.value(a)?.try_use(self)?, self.value(b)?.try_use(self)?);
                match comparer {
                    BinaryComparison::Equals => self.eval.equals(a, b),
                    BinaryComparison::NotEquals => !self.eval.equals(a, b),
                    BinaryComparison::GreaterThan => self.eval.greater_than(a, b),
                    BinaryComparison::GreaterOrEquals => self.eval.greater_or_equals(a, b),
                    BinaryComparison::LowerThan => !self.eval.greater_or_equals(a, b),
                    BinaryComparison::LowerOrEquals => !self.eval.greater_than(a, b),
                }
            }
            Condition::CollectionMatch(match_type, reference, condition) => {
                let vec = self.reference_collection(reference)?;
                let it = self.it;
                let mut result;
                match match_type {
                    CollectionMatch::All => {
                        result = true;
                        for v in vec {
                            self.it = Some(v);
                            if !self.condition(condition)? {
                                result = false;
                                break;
                            }
                        }
                    }
                    CollectionMatch::Any => {
                        result = false;
                        for v in vec {
                            self.it = Some(v);
                            if self.condition(condition)? {
                                result = true;
                                break;
                            }
                        }
                    }
                }
                self.it = it;
                result
            }
            Condition::IsDefinedReference(reference) => self.reference(reference)?.0.is_ok(),
            Condition::IsEmptyReference(reference) => {
                let immediate = self.reference(reference)?.try_use(self)?;
                self.eval.length(immediate) == 0
            }
            Condition::Disjunction(boxed) => {
                let (a, b) = &**boxed;
                self.condition(a)? || self.condition(b)?
            }
            Condition::Conjunction(boxed) => {
                let (a, b) = &**boxed;
                self.condition(a)? && self.condition(b)?
            }
            Condition::Negation(boxed) => !self.condition(boxed)?,
            Condition::Instanceof(reference, name) => {
                let immediate = self.reference(reference)?.try_use(self)?;
                self.eval.instanceof(immediate, name.as_str())
            }
        })
    }
}

pub fn eval_condition<'e, I: 'e, E: Evaluator<'e, I>>(
    eval: &mut E,
    condition: &'e ProbeCondition,
) -> Result<bool, SnapshotEvaluationError> {
    Eval { eval, it: None }
        .condition(&condition.0)
        .map_err(|e| SnapshotEvaluationError {
            expr: condition.to_string(),
            message: e.0,
        })
}

pub fn eval_string<'a, 'e, 'v, I: 'e, E: Evaluator<'e, I>>(
    eval: &mut E,
    dsl: &'v DslString,
) -> (Cow<'a, str>, Vec<SnapshotEvaluationError>)
where
    'v: 'e,
    'e: 'a,
{
    let mut errors = vec![];
    let mut eval = Eval { eval, it: None };
    let mut map_error = |err: EvErr, expr: &dyn ToString| {
        errors.push(SnapshotEvaluationError {
            expr: expr.to_string(),
            message: err.0,
        });
        Cow::Borrowed("UNDEFINED")
    };
    let mut vec = dsl
        .0
        .iter()
        .map(|p| match p {
            DslPart::String(str) => Cow::Borrowed(str.as_str()),
            DslPart::Value(val) => eval
                .value(val)
                .and_then(|value| {
                    let immediate = value.try_use(&mut eval)?;
                    Ok(eval.get_string(immediate))
                })
                .unwrap_or_else(|err| map_error(err, val)),
            DslPart::Ref(reference) => match reference {
                CollectionSource::Reference(reference) => {
                    eval.reference(reference).and_then(|referenced| {
                        let immediate = referenced.try_use(&mut eval)?;
                        Ok(eval.get_string(IntermediateValue::Referenced(immediate)))
                    })
                }
                CollectionSource::FilterOperator(_) => {
                    eval.collection_source(reference).map(|vec| {
                        let mut strings = vec![];
                        for referenced in vec {
                            strings
                                .push(eval.get_string(IntermediateValue::Referenced(referenced)));
                        }
                        Cow::Owned(format!("[{}]", strings.join(", ")))
                    })
                }
            }
            .unwrap_or_else(|err| map_error(err, reference)),
        })
        .collect::<Vec<Cow<str>>>();
    (
        if vec.len() == 1 {
            vec.remove(0)
        } else {
            Cow::Owned(vec.join(""))
        },
        errors,
    )
}

pub fn eval_value<'e, 'v, I: 'e, E: Evaluator<'e, I>>(
    eval: &mut E,
    value: &'v ProbeValue,
) -> Result<IntermediateValue<'e, I>, SnapshotEvaluationError>
where
    'v: 'e,
{
    let mut eval = Eval { eval, it: None };
    eval.value(&value.0)
        .and_then(|v| v.try_use(&mut eval))
        .map_err(|e| SnapshotEvaluationError {
            expr: value.to_string(),
            message: e.0,
        })
}

pub fn eval_intermediate_to_string<'e, I, E: Evaluator<'e, I>>(
    eval: &mut E,
    value: IntermediateValue<'e, I>,
) -> Cow<'e, str> {
    let mut eval = Eval { eval, it: None };
    eval.get_string(value)
}

#[cfg(test)]
mod tests {
    use crate::expr_defs::Value;
    use crate::expr_defs::{
        BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
        Reference, StringComparison, StringSource,
    };
    use crate::{
        eval_condition, eval_intermediate_to_string, eval_string, eval_value, DslString, Evaluator,
        IntermediateValue, ProbeCondition, ProbeValue,
    };
    use std::borrow::Cow;
    use std::cmp::Ordering;
    use std::collections::HashMap;

    struct EvalCtx<'e> {
        variables: &'e HashMap<String, Val>,
    }

    #[derive(Clone, PartialEq)]
    struct OrdMap(HashMap<String, Val>);

    impl PartialOrd for OrdMap {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            self.0.len().partial_cmp(&other.0.len())
        }
    }

    #[derive(Clone, PartialOrd, PartialEq)]
    enum Val {
        Null,
        Num(f64),
        Str(String),
        Bool(bool),
        Vec(Vec<Val>),
        Obj(OrdMap),
    }

    impl<'a> From<IntermediateValue<'a, Val>> for Val {
        fn from(value: IntermediateValue<'a, Val>) -> Self {
            match value {
                IntermediateValue::String(s) => Val::Str(s.to_string()),
                IntermediateValue::Number(n) => Val::Num(n),
                IntermediateValue::Bool(b) => Val::Bool(b),
                IntermediateValue::Null => Val::Null,
                IntermediateValue::Referenced(v) => v.clone(),
            }
        }
    }

    impl<'e> Evaluator<'e, Val> for EvalCtx<'e> {
        fn equals(&mut self, a: IntermediateValue<'e, Val>, b: IntermediateValue<'e, Val>) -> bool {
            Val::from(a) == b.into()
        }

        fn greater_than(
            &mut self,
            a: IntermediateValue<'e, Val>,
            b: IntermediateValue<'e, Val>,
        ) -> bool {
            Val::from(a) > b.into()
        }

        fn greater_or_equals(
            &mut self,
            a: IntermediateValue<'e, Val>,
            b: IntermediateValue<'e, Val>,
        ) -> bool {
            Val::from(a) >= b.into()
        }

        fn fetch_identifier(&mut self, identifier: &str) -> Option<&'e Val> {
            self.variables.get(identifier)
        }

        fn fetch_index(
            &mut self,
            value: &'e Val,
            index: IntermediateValue<'e, Val>,
        ) -> Option<&'e Val> {
            if let Val::Vec(vec) = value {
                if let Val::Num(idx) = index.into() {
                    let idx = idx as usize;
                    if idx < vec.len() {
                        return Some(&vec[idx]);
                    }
                }
            }

            None
        }

        fn fetch_nested(
            &mut self,
            value: &'e Val,
            member: IntermediateValue<'e, Val>,
        ) -> Option<&'e Val> {
            if let Val::Obj(obj) = value {
                if let Val::Str(str) = member.into() {
                    return obj.0.get(&str);
                }
            }

            None
        }

        fn length(&mut self, value: &'e Val) -> usize {
            match value {
                Val::Null => 0,
                Val::Num(n) => n.to_string().len(),
                Val::Str(s) => s.len(),
                Val::Bool(_) => 0,
                Val::Vec(v) => v.len(),
                Val::Obj(o) => o.0.len(),
            }
        }

        fn try_enumerate(&mut self, value: &'e Val) -> Option<Vec<&'e Val>> {
            match value {
                Val::Vec(v) => Some(v.iter().collect()),
                Val::Obj(o) => Some(o.0.values().collect()),
                _ => None,
            }
        }

        fn stringify(&mut self, value: &'e Val) -> Cow<'e, str> {
            match value {
                Val::Null => Cow::Borrowed(""),
                Val::Num(n) => Cow::Owned(n.to_string()),
                Val::Str(s) => Cow::Borrowed(s.as_str()),
                Val::Bool(b) => Cow::Borrowed(if *b { "true" } else { "false" }),
                Val::Vec(v) => Cow::Owned(format!("vec[{}]", v.len())),
                Val::Obj(o) => Cow::Owned(format!("obj[{}]", o.0.len())),
            }
        }

        fn get_string(&mut self, value: &'e Val) -> Cow<'e, str> {
            match value {
                Val::Vec(v) => Cow::Owned(format!(
                    "vec{{{}}}",
                    v.iter()
                        .map(|e| self.get_string(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Val::Obj(o) => Cow::Owned(format!(
                    "obj{{{}}}",
                    o.0.iter()
                        .map(|(k, v)| format!("{k}: {}", self.get_string(v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                _ => self.stringify(value),
            }
        }

        fn convert_index(&mut self, value: &'e Val) -> Option<usize> {
            if let Val::Num(n) = value {
                Some(*n as usize)
            } else {
                None
            }
        }

        fn instanceof(&mut self, value: &'e Val, class: &str) -> bool {
            if let Val::Obj(o) = value {
                if let Some(Val::Str(s)) = o.0.get("class") {
                    return s == class;
                }
            }
            false
        }
    }

    fn num(n: f64) -> Value {
        Value::Number(NumberSource::Number(n))
    }

    fn string(s: &'static str) -> Value {
        Value::String(StringSource::String(s.to_string()))
    }

    fn numval(v: &'static str) -> Value {
        Value::Number(NumberSource::Reference(Reference::Base(v.to_string())))
    }

    fn strvar(v: &'static str) -> StringSource {
        StringSource::Reference(Reference::Base(v.to_string()))
    }

    fn strval(v: &'static str) -> Value {
        Value::String(strvar(v))
    }

    fn vecvar(v: &'static str) -> CollectionSource {
        CollectionSource::Reference(Reference::Base(v.to_string()))
    }

    fn it_ref() -> StringSource {
        StringSource::Reference(Reference::IteratorVariable)
    }

    macro_rules! assert_cond_err {
        ($vars:expr, $expr:expr, $err:expr) => {
            let cond = ProbeCondition($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            match eval_condition(&mut ctx, &cond) {
                Ok(_) => unreachable!(),
                Err(e) => assert_eq!(e.message, $err),
            }
        };
    }
    macro_rules! assert_cond_true {
        ($vars:expr, $expr:expr) => {
            let cond = ProbeCondition($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            assert!(eval_condition(&mut ctx, &cond).unwrap());
        };
    }
    macro_rules! assert_cond_false {
        ($vars:expr, $expr:expr) => {
            let cond = ProbeCondition($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            assert!(!eval_condition(&mut ctx, &cond).unwrap());
        };
    }

    macro_rules! assert_val_err {
        ($vars:expr, $expr:expr, $err:expr) => {
            let val = ProbeValue($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            match eval_value(&mut ctx, &val) {
                Ok(_) => unreachable!(),
                Err(e) => assert_eq!(e.message, $err),
            }
        };
    }
    macro_rules! assert_val_eq {
        ($vars:expr, $expr:expr, $eq:expr) => {
            let val = ProbeValue($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            let value = eval_value(&mut ctx, &val).unwrap();
            assert_eq!(eval_intermediate_to_string(&mut ctx, value), $eq);
        };
    }

    macro_rules! assert_dsl_eq {
        ($vars:expr, $expr:expr, $eq:expr) => {
            let dsl = &DslString($expr);
            let mut ctx = EvalCtx { variables: &$vars };
            let (result, errors) = eval_string(&mut ctx, dsl);
            assert_eq!(result, $eq);
            assert_eq!(errors.len(), 0);
        };
    }

    #[test]
    fn test_eval() {
        let vars = HashMap::from([
            ("var".to_string(), Val::Str("bar".to_string())),
            (
                "vec".to_string(),
                Val::Vec(vec![Val::Num(10.), Val::Num(11.), Val::Num(12.)]),
            ),
            (
                "vecvec".to_string(),
                Val::Vec(vec![
                    Val::Vec(vec![Val::Num(10.), Val::Num(11.)]),
                    Val::Vec(vec![Val::Num(12.)]),
                ]),
            ),
            ("empty".to_string(), Val::Str("".to_string())),
            ("emptyvec".to_string(), Val::Vec(vec![])),
            ("null".to_string(), Val::Null),
            ("zero".to_string(), Val::Num(0.)),
            ("two".to_string(), Val::Num(2.)),
            (
                "objA".to_string(),
                Val::Obj(OrdMap(HashMap::from([(
                    "class".to_string(),
                    Val::Str("A".to_string()),
                )]))),
            ),
            (
                "objB".to_string(),
                Val::Obj(OrdMap(HashMap::from([(
                    "class".to_string(),
                    Val::Str("B".to_string()),
                )]))),
            ),
        ]);

        assert_cond_true!(vars, Condition::Always);
        assert_cond_false!(vars, Condition::Never);

        assert_cond_err!(
            vars,
            Condition::IsEmptyReference(Reference::Base("foo".to_string())),
            "Could not fetch foo"
        );
        assert_cond_err!(
            vars,
            Condition::IsEmptyReference(Reference::Nested(Box::new((
                Reference::Base("foo".to_string()),
                string("bar")
            )))),
            "Could not fetch foo"
        );
        assert_cond_err!(
            vars,
            Condition::IsEmptyReference(Reference::Nested(Box::new((
                Reference::Base("objA".to_string()),
                string("foo")
            )))),
            "Could not fetch property \"foo\" on objA (evaluated to obj{class: A})"
        );
        assert_cond_err!(
            vars,
            Condition::IsEmptyReference(Reference::Index(Box::new((vecvar("foo"), num(0.))))),
            "Could not fetch foo"
        );
        assert_cond_err!(
            vars,
            Condition::IsEmptyReference(Reference::Index(Box::new((vecvar("vec"), num(3.))))),
            "Could not fetch index 3 on vec (evaluated to vec{10, 11, 12})"
        );
        assert_cond_false!(
            vars,
            Condition::IsDefinedReference(Reference::Base("foo".to_string()))
        );
        assert_cond_true!(
            vars,
            Condition::IsDefinedReference(Reference::Base("var".to_string()))
        );
        assert_cond_true!(
            vars,
            Condition::IsDefinedReference(Reference::Index(Box::new((vecvar("vec"), num(0.)))))
        );
        assert_cond_false!(
            vars,
            Condition::IsDefinedReference(Reference::Index(Box::new((vecvar("vec"), num(3.)))))
        );
        assert_cond_false!(
            vars,
            Condition::IsDefinedReference(Reference::Index(Box::new((vecvar("foo"), num(0.)))))
        );
        assert_cond_true!(
            vars,
            Condition::IsDefinedReference(Reference::Nested(Box::new((
                Reference::Base("objA".to_string()),
                string("class")
            ))))
        );
        assert_cond_false!(
            vars,
            Condition::IsDefinedReference(Reference::Nested(Box::new((
                Reference::Base("objA".to_string()),
                string("foo")
            ))))
        );

        assert_cond_true!(
            vars,
            Condition::IsEmptyReference(Reference::Base("empty".to_string()))
        );
        assert_cond_false!(
            vars,
            Condition::IsEmptyReference(Reference::Base("var".to_string()))
        );

        assert_cond_true!(
            vars,
            Condition::BinaryComparison(
                Value::String(StringSource::Null),
                BinaryComparison::Equals,
                strval("null")
            )
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(string("bar"), BinaryComparison::Equals, strval("var"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::Equals, numval("zero"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::Equals, numval("two"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(numval("zero"), BinaryComparison::Equals, num(0.))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::NotEquals, numval("zero"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::GreaterThan, numval("zero"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::GreaterOrEquals, numval("zero"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::LowerThan, numval("zero"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::LowerOrEquals, numval("zero"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::GreaterThan, numval("two"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::GreaterOrEquals, numval("two"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::LowerThan, numval("two"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(0.), BinaryComparison::LowerOrEquals, numval("two"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(4.), BinaryComparison::GreaterThan, numval("two"))
        );
        assert_cond_true!(
            vars,
            Condition::BinaryComparison(num(4.), BinaryComparison::GreaterOrEquals, numval("two"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(4.), BinaryComparison::LowerThan, numval("two"))
        );
        assert_cond_false!(
            vars,
            Condition::BinaryComparison(num(4.), BinaryComparison::LowerOrEquals, numval("two"))
        );

        assert_cond_false!(vars, Condition::Negation(Box::new(Condition::Always)));
        assert_cond_true!(vars, Condition::Negation(Box::new(Condition::Never)));

        assert_cond_true!(
            vars,
            Condition::Conjunction(Box::new((Condition::Always, Condition::Always)))
        );
        assert_cond_false!(
            vars,
            Condition::Conjunction(Box::new((Condition::Never, Condition::Always)))
        );
        assert_cond_false!(
            vars,
            Condition::Conjunction(Box::new((Condition::Always, Condition::Never)))
        );
        assert_cond_false!(
            vars,
            Condition::Conjunction(Box::new((Condition::Never, Condition::Never)))
        );

        assert_cond_true!(
            vars,
            Condition::Disjunction(Box::new((Condition::Always, Condition::Always)))
        );
        assert_cond_true!(
            vars,
            Condition::Disjunction(Box::new((Condition::Never, Condition::Always)))
        );
        assert_cond_true!(
            vars,
            Condition::Disjunction(Box::new((Condition::Always, Condition::Never)))
        );
        assert_cond_false!(
            vars,
            Condition::Disjunction(Box::new((Condition::Never, Condition::Never)))
        );

        assert_cond_true!(
            vars,
            Condition::StringComparison(
                StringComparison::StartsWith,
                StringSource::String("bar".to_string()),
                "ba".to_string()
            )
        );
        assert_cond_false!(
            vars,
            Condition::StringComparison(
                StringComparison::StartsWith,
                strvar("var"),
                "ar".to_string()
            )
        );
        assert_cond_false!(
            vars,
            Condition::StringComparison(
                StringComparison::EndsWith,
                strvar("var"),
                "ba".to_string()
            )
        );
        assert_cond_true!(
            vars,
            Condition::StringComparison(
                StringComparison::EndsWith,
                strvar("var"),
                "ar".to_string()
            )
        );
        assert_cond_true!(
            vars,
            Condition::StringComparison(StringComparison::Contains, strvar("var"), "a".to_string())
        );
        assert_cond_false!(
            vars,
            Condition::StringComparison(StringComparison::Contains, strvar("var"), "x".to_string())
        );
        assert_cond_true!(
            vars,
            Condition::StringComparison(StringComparison::Matches, strvar("var"), ".*".to_string())
        );
        assert_cond_false!(
            vars,
            Condition::StringComparison(StringComparison::Matches, strvar("var"), "o+".to_string())
        );

        assert_cond_true!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::Any,
                Reference::Base("vec".to_string()),
                Box::new(Condition::BinaryComparison(
                    num(10.),
                    BinaryComparison::Equals,
                    Value::String(it_ref())
                ))
            )
        );
        assert_cond_false!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::Any,
                Reference::Base("vec".to_string()),
                Box::new(Condition::BinaryComparison(
                    num(9.),
                    BinaryComparison::Equals,
                    Value::String(it_ref())
                ))
            )
        );
        assert_cond_true!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::All,
                Reference::Base("vec".to_string()),
                Box::new(Condition::BinaryComparison(
                    num(10.),
                    BinaryComparison::LowerOrEquals,
                    Value::String(it_ref())
                ))
            )
        );
        assert_cond_false!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::All,
                Reference::Base("vec".to_string()),
                Box::new(Condition::BinaryComparison(
                    num(10.),
                    BinaryComparison::GreaterOrEquals,
                    Value::String(it_ref())
                ))
            )
        );

        assert_cond_true!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::Any,
                Reference::Base("vecvec".to_string()),
                Box::new(Condition::CollectionMatch(
                    CollectionMatch::Any,
                    Reference::IteratorVariable,
                    Box::new(Condition::BinaryComparison(
                        num(10.),
                        BinaryComparison::Equals,
                        Value::String(it_ref())
                    ))
                ))
            )
        );
        assert_cond_false!(
            vars,
            Condition::CollectionMatch(
                CollectionMatch::All,
                Reference::Base("vecvec".to_string()),
                Box::new(Condition::CollectionMatch(
                    CollectionMatch::Any,
                    Reference::IteratorVariable,
                    Box::new(Condition::BinaryComparison(
                        num(10.),
                        BinaryComparison::Equals,
                        Value::String(it_ref())
                    ))
                ))
            )
        );

        assert_cond_true!(
            vars,
            Condition::Instanceof(Reference::Base("objA".to_string()), "A".to_string())
        );
        assert_cond_false!(
            vars,
            Condition::Instanceof(Reference::Base("objA".to_string()), "B".to_string())
        );

        assert_val_eq!(vars, string("foo"), "foo");
        assert_val_eq!(vars, strval("var"), "bar");
        assert_val_eq!(vars, strval("vec"), "vec{10, 11, 12}");
        assert_val_eq!(vars, strval("objA"), "obj{class: A}");

        assert_val_eq!(
            vars,
            Value::String(StringSource::Substring(Box::new((
                StringSource::String("bar".to_string()),
                NumberSource::Number(1.),
                NumberSource::Number(2.)
            )))),
            "a"
        );
        assert_val_eq!(
            vars,
            Value::String(StringSource::Substring(Box::new((
                strvar("vec"),
                NumberSource::Number(3.),
                NumberSource::Number(6.)
            )))),
            "[3]"
        );
        assert_val_err!(
            vars,
            Value::String(StringSource::Substring(Box::new((
                strvar("var"),
                NumberSource::Number(1.),
                NumberSource::Number(0.)
            )))),
            "[1..0] is out of bounds of substring(var, 1, 0) (string size: 3)"
        );
        assert_val_err!(
            vars,
            Value::String(StringSource::Substring(Box::new((
                strvar("var"),
                NumberSource::Number(10.),
                NumberSource::Number(13.)
            )))),
            "[10..13] is out of bounds of substring(var, 10, 13) (string size: 3)"
        );

        assert_val_eq!(
            vars,
            Value::Number(NumberSource::CollectionSize(vecvar("vec"))),
            "3"
        );
        assert_val_eq!(
            vars,
            Value::Number(NumberSource::StringLength(Reference::Base(
                "var".to_string()
            ))),
            "3"
        );
        assert_val_eq!(
            vars,
            Value::Number(NumberSource::StringLength(Reference::Base(
                "null".to_string()
            ))),
            "0"
        );

        assert_dsl_eq!(vars, vec![], "");
        assert_dsl_eq!(vars, vec![DslPart::String("test".to_string())], "test");
        assert_dsl_eq!(vars, vec![DslPart::Value(string("test"))], "test");
        assert_dsl_eq!(vars, vec![DslPart::Ref(vecvar("var"))], "bar");
        assert_dsl_eq!(vars, vec![DslPart::Ref(vecvar("vec"))], "vec{10, 11, 12}");
        assert_dsl_eq!(
            vars,
            vec![DslPart::Ref(CollectionSource::FilterOperator(Box::new((
                vecvar("vec"),
                Condition::BinaryComparison(
                    num(10.),
                    BinaryComparison::LowerThan,
                    Value::String(it_ref())
                )
            ))))],
            "[11, 12]"
        );
        assert_dsl_eq!(
            vars,
            vec![DslPart::Ref(CollectionSource::FilterOperator(Box::new((
                vecvar("vecvec"),
                Condition::CollectionMatch(
                    CollectionMatch::All,
                    Reference::IteratorVariable,
                    Box::new(Condition::BinaryComparison(
                        num(10.),
                        BinaryComparison::NotEquals,
                        Value::String(it_ref())
                    ))
                )
            ))))],
            "[vec{12}]"
        );
        assert_dsl_eq!(
            vars,
            vec![
                DslPart::String("a zero: ".to_string()),
                DslPart::Ref(vecvar("zero"))
            ],
            "a zero: 0"
        );

        let dsl = &DslString(vec![
            DslPart::Value(Value::String(StringSource::Substring(Box::new((
                strvar("var"),
                NumberSource::Reference(Reference::Base("var".to_string())),
                NumberSource::Number(3.),
            ))))),
            DslPart::String(" - ".to_string()),
            DslPart::Ref(CollectionSource::FilterOperator(Box::new((
                vecvar("var"),
                Condition::Always,
            )))),
            DslPart::String(" - ".to_string()),
            DslPart::Value(strval("var")),
        ]);
        let mut ctx = EvalCtx { variables: &vars };
        let (result, errors) = eval_string(&mut ctx, dsl);
        assert_eq!(result, "UNDEFINED - UNDEFINED - bar");
        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors[0].message,
            "Cannot convert bar to an index (from var)"
        );
        assert_eq!(errors[0].expr, "substring(var, var, 3)");
        assert_eq!(
            errors[1].message,
            "Cannot enumerate non iterable type: var; evaluating to: bar"
        );
        assert_eq!(errors[1].expr, "filter(var, true)");
    }
}
