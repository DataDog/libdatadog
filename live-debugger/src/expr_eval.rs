// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use crate::expr_defs::{
    BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
    Reference, StringComparison, StringSource, Value,
};
use regex::Regex;
use std::cmp::min;
use std::str::FromStr;
use std::usize;

#[derive(Debug)]
pub struct DslString(pub(crate) Vec<DslPart>);
#[derive(Debug)]
pub struct ProbeValue(pub(crate) Value);
#[derive(Debug)]
pub struct ProbeCondition(pub(crate) Condition);

pub enum IntermediateValue<'a, I> {
    String(Cow<'a, str>),
    Number(f64),
    Bool(bool),
    Null,
    Referenced(&'a I),
}

pub struct Evaluator<C, I> {
    pub equals: for<'a> fn(&'a C, IntermediateValue<'a, I>, IntermediateValue<'a, I>) -> bool,
    pub greater_than: for<'a> fn(&'a C, IntermediateValue<'a, I>, IntermediateValue<'a, I>) -> bool,
    pub greater_or_equals:
        for<'a> fn(&'a C, IntermediateValue<'a, I>, IntermediateValue<'a, I>) -> bool,
    pub fetch_identifier: for<'a> fn(&'a C, &str) -> Option<&'a I>, // special values: @duration, @return, @exception
    pub fetch_index: for<'a> fn(&'a C, &'a I, IntermediateValue<'a, I>) -> Option<&'a I>,
    pub fetch_nested: for<'a> fn(&'a C, &'a I, IntermediateValue<'a, I>) -> Option<&'a I>,
    pub length: for<'a> fn(&'a C, &'a I) -> u64,
    pub try_enumerate: for<'a> fn(&'a C, &'a I) -> Option<Vec<&'a I>>,
    pub stringify: for<'a> fn(&'a C, &'a I) -> Cow<'a, str>,
    pub convert_index: for<'a> fn(&'a C, &'a I) -> Option<usize>,
}

type EvalResult<T> = Result<T, ()>;

struct Eval<'a, I, C> {
    eval: &'a Evaluator<C, I>,
    context: &'a C,
    it: Option<&'a I>,
}

impl<'a, I, C> Eval<'a, I, C> {
    fn value(&mut self, value: &'a Value) -> EvalResult<IntermediateValue<'a, I>> {
        Ok(match value {
            Value::Bool(condition) => IntermediateValue::Bool(self.condition(condition)?),
            Value::String(s) => self.string_source(s)?,
            Value::Number(n) => self.number_source(n)?,
        })
    }

    fn number_source(&mut self, value: &'a NumberSource) -> EvalResult<IntermediateValue<'a, I>> {
        Ok(match value {
            NumberSource::Number(n) => IntermediateValue::Number(*n),
            NumberSource::CollectionSize(collection) => {
                IntermediateValue::Number(match collection {
                    CollectionSource::Reference(reference) => {
                        (self.eval.length)(self.context, self.reference(reference)?.ok_or(())?)
                            as f64
                    }
                    CollectionSource::FilterOperator(_) => {
                        self.collection_source(collection)?.ok_or(())?.len() as f64
                    }
                })
            }
            NumberSource::StringLength(reference) => IntermediateValue::Number((self.eval.length)(
                self.context,
                self.reference(reference)?.ok_or(())?,
            )
                as f64),
            NumberSource::Reference(reference) => {
                IntermediateValue::Referenced(self.reference(reference)?.ok_or(())?)
            }
        })
    }

    fn convert_index(&mut self, value: IntermediateValue<'a, I>) -> EvalResult<usize> {
        Ok(match value {
            IntermediateValue::String(s) => return usize::from_str(&s).map_err(|_| ()),
            IntermediateValue::Number(n) => n as usize,
            IntermediateValue::Bool(_) => return Err(()),
            IntermediateValue::Null => 0,
            IntermediateValue::Referenced(referenced) => {
                (self.eval.convert_index)(self.context, referenced).ok_or(())?
            }
        })
    }

    fn number_to_index(&mut self, value: &'a NumberSource) -> EvalResult<usize> {
        let value = self.number_source(value)?;
        self.convert_index(value)
    }

    fn string_source(&mut self, value: &'a StringSource) -> EvalResult<IntermediateValue<'a, I>> {
        Ok(match value {
            StringSource::String(s) => IntermediateValue::String(Cow::Borrowed(s.as_str())),
            StringSource::Substring(boxed) => {
                let (string, start, end) = &**boxed;
                let str = self.stringify(string)?;
                let start = self.number_to_index(start)?;
                let mut end = self.number_to_index(end)?;
                if start > end || start >= str.len() {
                    return Err(());
                }
                end = min(end, str.len());
                IntermediateValue::String(match str {
                    Cow::Owned(s) => Cow::Owned(s[start..end].to_string()),
                    Cow::Borrowed(s) => Cow::Borrowed(&s[start..end])
                })
            }
            StringSource::Null => IntermediateValue::Null,
            StringSource::Reference(reference) => {
                IntermediateValue::Referenced(self.reference(reference)?.ok_or(())?)
            }
        })
    }

    fn reference_collection(&mut self, reference: &'a Reference) -> EvalResult<Option<Vec<&'a I>>> {
        Ok(self
            .reference(reference)?
            .and_then(|reference| (self.eval.try_enumerate)(self.context, reference)))
    }

    fn reference(&mut self, reference: &'a Reference) -> EvalResult<Option<&'a I>> {
        Ok(match reference {
            Reference::Base(ref identifier) => {
                if identifier == "@it" {
                    self.it
                } else {
                    (self.eval.fetch_identifier)(self.context, identifier.as_str())
                }
            }
            Reference::Index(ref boxed) => {
                let (source, dimension) = &**boxed;
                let dimension = self.value(dimension)?;
                match source {
                    CollectionSource::FilterOperator(_) => {
                        let index = self.convert_index(dimension)?;
                        self.collection_source(source)?.and_then(|vec| {
                            if index < vec.len() {
                                Some(vec[index])
                            } else {
                                None
                            }
                        })
                    }
                    CollectionSource::Reference(ref reference) => self
                        .reference(reference)?
                        .and_then(|base| (self.eval.fetch_index)(self.context, base, dimension)),
                }
            }
            Reference::Nested(ref boxed) => {
                let (source, member) = &**boxed;
                let member = self.value(member)?;
                self.reference(source)?
                    .and_then(|base| (self.eval.fetch_nested)(self.context, base, member))
            }
        })
    }

    fn collection_source(
        &mut self,
        collection: &'a CollectionSource,
    ) -> EvalResult<Option<Vec<&'a I>>> {
        Ok(match collection {
            CollectionSource::Reference(ref reference) => self.reference_collection(reference)?,
            CollectionSource::FilterOperator(ref boxed) => {
                let (source, condition) = &**boxed;
                let mut values = vec![];
                let it = self.it;
                if let Some(source_values) = self.collection_source(source)? {
                    for item in source_values {
                        self.it = Some(item);
                        if self.condition(condition)? {
                            values.push(item);
                        }
                    }
                    self.it = it;
                    Some(values)
                } else {
                    None
                }
            }
        })
    }

    fn stringify_intermediate(&mut self, value: IntermediateValue<'a, I>) -> Cow<'a, str> {
        match value {
            IntermediateValue::String(s) => s,
            IntermediateValue::Number(n) => Cow::Owned(n.to_string()),
            IntermediateValue::Bool(b) => Cow::Owned(b.to_string()),
            IntermediateValue::Null => Cow::Borrowed(""),
            IntermediateValue::Referenced(referenced) => {
                (self.eval.stringify)(self.context, referenced)
            }
        }
    }

    fn stringify(&mut self, value: &'a StringSource) -> EvalResult<Cow<'a, str>> {
        let value = self.string_source(value)?;
        Ok(self.stringify_intermediate(value))
    }

    fn condition(&mut self, condition: &'a Condition) -> EvalResult<bool> {
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
                            .map_err(|_| ())
                            .map(|r| r.is_match(&haystack))
                    }
                }
            }
            Condition::BinaryComparison(a, comparer, b) => {
                let (a, b) = (self.value(a)?, self.value(b)?);
                match comparer {
                    BinaryComparison::Equals => (self.eval.equals)(self.context, a, b),
                    BinaryComparison::NotEquals => !(self.eval.equals)(self.context, a, b),
                    BinaryComparison::GreaterThan => (self.eval.greater_than)(self.context, a, b),
                    BinaryComparison::GreaterOrEquals => {
                        (self.eval.greater_or_equals)(self.context, a, b)
                    }
                    BinaryComparison::LessThan => {
                        !(self.eval.greater_or_equals)(self.context, a, b)
                    }
                    BinaryComparison::LessOrEquals => !(self.eval.greater_than)(self.context, a, b),
                }
            }
            Condition::CollectionMatch(match_type, reference, condition) => {
                let vec = self.reference_collection(reference)?.ok_or(())?;
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
            Condition::IsUndefinedReference(reference) => self.reference(reference).ok().is_none(),
            Condition::IsEmptyReference(reference) => {
                if let Some(value) = self.reference(reference)? {
                    (self.eval.length)(self.context, value) == 0
                } else {
                    return Err(());
                }
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
        })
    }
}

pub fn eval_condition<'a, 'e, 'v, I, C>(
    eval: &'e Evaluator<C, I>,
    condition: &'v ProbeCondition,
    context: &'a C,
) -> bool
where
    'e: 'a,
    'v: 'a,
{
    Eval {
        eval,
        context,
        it: None,
    }
    .condition(&condition.0)
    .unwrap_or(false)
}

pub fn eval_string<'a, 'e, 'v, I, C>(
    eval: &'e Evaluator<C, I>,
    dsl: &'v DslString,
    context: &'a C,
) -> String
where
    'e: 'a,
    'v: 'a,
{
    dsl.0
        .iter()
        .map(|p| match p {
            DslPart::String(ref str) => Cow::Borrowed(str.as_str()),
            DslPart::Ref(ref reference) => {
                let mut eval = Eval {
                    eval,
                    context,
                    it: None,
                };
                match reference {
                    CollectionSource::Reference(reference) => eval
                        .reference(reference)
                        .unwrap_or_default()
                        .map(|referenced| {
                            eval.stringify_intermediate(IntermediateValue::Referenced(referenced))
                        }),
                    CollectionSource::FilterOperator(_) => eval
                        .collection_source(reference)
                        .ok()
                        .unwrap_or_default()
                        .map(|vec| {
                            Cow::Owned(format!(
                                "[{}]",
                                vec.iter()
                                    .map(|referenced| eval.stringify_intermediate(
                                        IntermediateValue::Referenced(referenced)
                                    ))
                                    .collect::<Vec<Cow<'a, str>>>()
                                    .join(", ")
                            ))
                        }),
                }
                .unwrap_or(Cow::Borrowed("UNDEFINED"))
            }
        })
        .collect::<Vec<Cow<'a, str>>>()
        .join("")
}
