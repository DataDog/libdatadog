// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum CollectionSource {
    Reference(Reference),
    FilterOperator(Box<(CollectionSource, Condition)>),
}

impl Display for CollectionSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectionSource::Reference(r) => r.fmt(f),
            CollectionSource::FilterOperator(b) => {
                let (source, cond) = &**b;
                write!(f, "filter({source}, {cond})")
            },
        }
    }
}

#[derive(Debug)]
pub enum Reference {
    IteratorVariable,
    Base(String),
    Index(Box<(CollectionSource, Value)>), // i.e. foo[bar]
    Nested(Box<(Reference, Value)>),       // i.e. foo.bar
}

impl Display for Reference {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Reference::IteratorVariable => f.write_str("@it"),
            Reference::Base(s) => s.fmt(f),
            Reference::Index(b) => {
                let (source, index) = &**b;
                write!(f, "{source}[{index}]")
            },
            Reference::Nested(b) => {
                let (source, member) = &**b;
                if let Value::String(StringSource::String(s)) = member {
                    write!(f, "{source}.{s}")
                } else {
                    write!(f, "{source}.{member}")
                }
            },
        }
    }
}

#[derive(Debug)]
pub enum BinaryComparison {
    Equals,
    NotEquals,
    GreaterThan,
    GreaterOrEquals,
    LowerThan,
    LowerOrEquals,
}

impl Display for BinaryComparison {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BinaryComparison::Equals => "==",
            BinaryComparison::NotEquals => "!=",
            BinaryComparison::GreaterThan => ">",
            BinaryComparison::GreaterOrEquals => ">=",
            BinaryComparison::LowerThan => "<",
            BinaryComparison::LowerOrEquals => "<=",
        })
    }
}

#[derive(Debug)]
pub enum StringComparison {
    StartsWith,
    EndsWith,
    Contains,
    Matches,
}

impl Display for StringComparison {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StringComparison::StartsWith => "startsWith",
            StringComparison::EndsWith => "endsWith",
            StringComparison::Contains => "contains",
            StringComparison::Matches => "matches",
        })
    }
}

#[derive(Debug)]
pub enum CollectionMatch {
    All,
    Any,
}

impl Display for CollectionMatch {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CollectionMatch::All => "all",
            CollectionMatch::Any => "any",
        })
    }
}

#[derive(Debug)]
pub enum Condition {
    Always,
    Never,
    Disjunction(Box<(Condition, Condition)>),
    Conjunction(Box<(Condition, Condition)>),
    Negation(Box<Condition>),
    StringComparison(StringComparison, StringSource, String),
    BinaryComparison(Value, BinaryComparison, Value),
    CollectionMatch(CollectionMatch, Reference, Box<Condition>),
    Instanceof(Reference, String),
    IsDefinedReference(Reference),
    IsEmptyReference(Reference),
}

struct NonAssocBoolOp<'a>(&'a Condition, bool);

impl<'a> Display for NonAssocBoolOp<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.1 {
            write!(f, "({})", self.0)
        } else {
            self.0.fmt(f)
        }
    }
}

impl Display for Condition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Condition::Always => f.write_str("true"),
            Condition::Never => f.write_str("false"),
            Condition::Disjunction(b) => {
                let (x, y) = &**b;
                fn is_nonassoc(condition: &Condition) -> NonAssocBoolOp {
                    NonAssocBoolOp(condition, matches!(condition, Condition::Conjunction(_)))
                }
                write!(f, "{} || {}", is_nonassoc(x), is_nonassoc(y))
            }
            Condition::Conjunction(b) => {
                let (x, y) = &**b;
                fn is_nonassoc(condition: &Condition) -> NonAssocBoolOp {
                    NonAssocBoolOp(condition, matches!(condition, Condition::Disjunction(_)))
                }
                write!(f, "{} && {}", is_nonassoc(x), is_nonassoc(y))
            }
            Condition::Negation(b) => write!(f, "!{}", NonAssocBoolOp(b, matches!(**b, Condition::Conjunction(_) | Condition::Disjunction(_) | Condition::BinaryComparison(..)))),
            Condition::StringComparison(cmp, s, v) => write!(f, "{cmp}({s}, {v})"),
            Condition::BinaryComparison(x, cmp, y) => write!(f, "{x} {cmp} {y}"),
            Condition::CollectionMatch(op, s, c) => write!(f, "{op}({s}, {})", **c),
            Condition::IsDefinedReference(r) => write!(f, "isDefined({r})"),
            Condition::IsEmptyReference(r) => write!(f, "isEmpty({r})"),
            Condition::Instanceof(r, class) => write!(f, "instanceof({r}, {class})"),
        }
    }
}

#[derive(Debug)]
pub enum NumberSource {
    Number(f64),
    CollectionSize(CollectionSource),
    StringLength(Reference),
    Reference(Reference),
}

impl Display for NumberSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            NumberSource::Number(n) => n.fmt(f),
            NumberSource::CollectionSize(s) => write!(f, "count({s})"),
            NumberSource::StringLength(s) => write!(f, "len({s})"),
            NumberSource::Reference(r) => r.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum StringSource {
    String(String),
    Substring(Box<(StringSource, NumberSource, NumberSource)>),
    Null,
    Reference(Reference),
}

impl Display for StringSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StringSource::String(s) => write!(f, "\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            StringSource::Substring(b) => {
                let (source, start, end) = &**b;
                write!(f, "substring({source}, {start}, {end})")
            },
            StringSource::Null => f.write_str("null"),
            StringSource::Reference(r) => r.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum Value {
    Bool(Box<Condition>),
    String(StringSource),
    Number(NumberSource),
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => (**b).fmt(f),
            Value::String(s) => s.fmt(f),
            Value::Number(s) => s.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum DslPart {
    Ref(CollectionSource),
    Value(Value),
    String(String),
}

impl Display for DslPart {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DslPart::Ref(r) => write!(f, "{{{r}}}"),
            DslPart::Value(v) => write!(f, "{{{v}}}"),
            DslPart::String(s) => s.fmt(f),
        }
    }
}