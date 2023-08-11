// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub enum CollectionSource {
    Reference(Reference),
    FilterOperator(Box<(CollectionSource, Condition)>),
}

pub enum Reference {
    Base(String),
    Index(Box<(CollectionSource, Value)>), // i.e. foo[bar]
    Nested(Box<(Reference, Value)>),       // i.e. foo.bar
}

pub enum BinaryComparison {
    Equals,
    NotEquals,
    GreaterThan,
    GreaterOrEquals,
    LessThan,
    LessOrEquals,
}

pub enum StringComparison {
    StartsWith,
    EndsWith,
    Contains,
    Matches,
}

pub enum CollectionMatch {
    All,
    Any,
}

pub enum Condition {
    Always,
    Never,
    Disjunction(Box<(Condition, Condition)>),
    Conjunction(Box<(Condition, Condition)>),
    Negation(Box<Condition>),
    StringComparison(StringComparison, StringSource, String),
    BinaryComparison(Value, BinaryComparison, Value),
    CollectionMatch(CollectionMatch, Reference, Box<Condition>),
    IsUndefinedReference(Reference),
    IsEmptyReference(Reference),
}

pub enum NumberSource {
    Number(f64),
    CollectionSize(CollectionSource),
    StringLength(Reference),
    Reference(Reference),
}

pub enum StringSource {
    String(String),
    Substring(Box<(StringSource, NumberSource, NumberSource)>),
    Null,
    Reference(Reference),
}

pub enum Value {
    Bool(Box<Condition>),
    String(StringSource),
    Number(NumberSource),
}

pub enum DslPart {
    Ref(CollectionSource),
    String(String),
}
