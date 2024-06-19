use anyhow::Context;
use crate::expr_defs::{
    BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
    Reference, StringComparison, StringSource, Value,
};
use crate::parse_util::try_get;
use crate::DslString;
use json::JsonValue;

fn try_parse_string_value(json: &JsonValue) -> anyhow::Result<Option<StringSource>> {
    if let Some(substring) = try_get(json, "substring") {
        if substring.is_array() && substring.len() == 3 {
            return Ok(Some(StringSource::Substring(Box::new((
                parse_string_value(&substring[0]).context("while parsing source string for substring")?,
                parse_number_value(&substring[1]).context("while parsing number for substring")?,
                parse_number_value(&substring[2]).context("while parsing number for substring")?,
            )))));
        }
    }
    if json.is_string() {
        return Ok(Some(StringSource::String(json.as_str().unwrap().into())));
    }
    if json.is_null() {
        return Ok(Some(StringSource::Null));
    }
    Ok(try_parse_reference(json).context("while parsing string reference")?.map(StringSource::Reference))
}

fn parse_string_value(json: &JsonValue) -> anyhow::Result<StringSource> {
    try_parse_string_value(json)?.ok_or_else(|| anyhow::format_err!("Could not find an appropriate operation for a string value"))
}

fn try_parse_number_value(json: &JsonValue) -> anyhow::Result<Option<NumberSource>> {
    if let Some(reference) = try_get(json, "len") {
        return Ok(Some(NumberSource::StringLength(parse_reference(reference).context("while parsing reference for len operation")?)));
    }
    if let Some(reference) = try_get(json, "count") {
        return Ok(Some(NumberSource::CollectionSize(parse_collection_source(
            reference,
        ).context("while parsing collection for size operation")?)));
    }
    if json.is_number() {
        return Ok(Some(NumberSource::Number(json.as_number().unwrap().into())));
    }
    Ok(try_parse_reference(json).context("while parsing number reference")?.map(NumberSource::Reference))
}

fn parse_number_value(json: &JsonValue) -> anyhow::Result<NumberSource> {
    try_parse_number_value(json)?.ok_or_else(|| anyhow::format_err!("Could not find an appropriate operation for a number"))
}

fn try_parse_reference(json: &JsonValue) -> anyhow::Result<Option<Reference>> {
    if let Some(identifier) = try_get(json, "ref") {
        return Ok(Some(Reference::Base(identifier.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from ref"))?.into())));
    }
    if let Some(index) = try_get(json, "index") {
        if index.is_array() && index.len() == 2 {
            return Ok(Some(Reference::Index(Box::new((
                parse_collection_source(&index[0]).context("while parsing collection for index operation")?,
                parse_value(&index[1]).context("while parsing index for index operation")?,
            )))));
        }
    }
    if let Some(index) = try_get(json, "nested") {
        if index.is_array() && index.len() == 2 {
            return Ok(Some(Reference::Nested(Box::new((
                parse_reference(&index[0]).context("while parsing reference for nested operation")?,
                parse_value(&index[1]).context("while parsing key for nested operation")?,
            )))));
        }
    }
    Ok(None)
}

fn parse_reference(json: &JsonValue) -> anyhow::Result<Reference> {
    try_parse_reference(json)?.ok_or_else(|| anyhow::format_err!("Could not find an appropriate operation for a reference"))
}

pub fn parse_value(json: &JsonValue) -> anyhow::Result<Value> {
    Ok(if let Some(str) = try_parse_string_value(json)? {
        Value::String(str)
    } else if let Some(num) = try_parse_number_value(json)? {
        Value::Number(num)
    } else {
        Value::Bool(Box::new(parse_condition(json).context("while parsing arbitrary value")?))
    })
}

pub fn parse_condition(json: &JsonValue) -> anyhow::Result<Condition> {
    for (key, comparer) in [
        ("eq", BinaryComparison::Equals),
        ("ne", BinaryComparison::NotEquals),
        ("gt", BinaryComparison::GreaterThan),
        ("ge", BinaryComparison::GreaterOrEquals),
        ("lt", BinaryComparison::LessThan),
        ("le", BinaryComparison::LessOrEquals),
    ] {
        if let Some(args) = try_get(json, key) {
            if args.is_array() && args.len() == 2 {
                return Ok(Condition::BinaryComparison(
                    parse_value(&args[0]).context("while parsing lhs of binary comparison")?,
                    comparer,
                    parse_value(&args[1]).context("while parsing rhs of binary comparison")?,
                ));
            }
        }
    }

    if let Some(args) = try_get(json, "and") {
        if args.is_array() && args.len() == 2 {
            return Ok(Condition::Disjunction(Box::new((
                parse_condition(&args[0]).context("while parsing lhs of binary and")?,
                parse_condition(&args[1]).context("while parsing rhs of binary and")?,
            ))));
        }
    }

    if let Some(args) = try_get(json, "or") {
        if args.is_array() && args.len() == 2 {
            return Ok(Condition::Conjunction(Box::new((
                parse_condition(&args[0]).context("while parsing lhs of binary or")?,
                parse_condition(&args[1]).context("while parsing rhs of binary or")?,
            ))));
        }
    }

    if let Some(arg) = try_get(json, "not") {
        return Ok(Condition::Negation(Box::new(parse_condition(arg).context("while parsing negation")?)));
    }

    if let Some(arg) = try_get(json, "isEmpty") {
        return Ok(Condition::IsEmptyReference(parse_reference(arg).context("while parsing reference for isEmpty operation")?));
    }

    if let Some(arg) = try_get(json, "isUndefined") {
        return Ok(Condition::IsUndefinedReference(parse_reference(arg).context("while parsing reference for isUndefined operation")?));
    }

    for (key, comparer) in [("any", CollectionMatch::Any), ("all", CollectionMatch::All)] {
        if let Some(args) = try_get(json, key) {
            if args.is_array() && args.len() == 2 {
                return Ok(Condition::CollectionMatch(
                    comparer,
                    parse_reference(&args[0]).context("while parsing collection reference for collection operation")?,
                    Box::new(parse_condition(&args[1]).context("while parsing condition for collection operation")?),
                ));
            }
        }
    }

    for (key, comparer) in [
        ("startsWith", StringComparison::StartsWith),
        ("endsWith", StringComparison::EndsWith),
        ("contains", StringComparison::Contains),
        ("matches", StringComparison::Matches),
    ] {
        if let Some(args) = try_get(json, key) {
            if args.is_array() && args.len() == 2 && args[1].is_string() {
                return Ok(Condition::StringComparison(
                    comparer,
                    parse_string_value(&args[0]).context("While parsing string operand for string comparison")?,
                    args[1].as_str().unwrap().into(),
                ));
            }
        }
    }

    if let Some(bool) = json.as_bool() {
        return Ok(if bool {
            Condition::Always
        } else {
            Condition::Never
        });
    }

    anyhow::bail!("Could not find an appropriate operation for a condition / boolean")
}

pub fn try_parse_collection_source(json: &JsonValue) -> anyhow::Result<Option<CollectionSource>> {
    if let Some(index) = try_get(json, "filter") {
        if index.is_array() && index.len() == 2 {
            return Ok(Some(CollectionSource::FilterOperator(Box::new((
                parse_collection_source(&index[0]).context("while parsing collection source for filter operation")?,
                parse_condition(&index[1]).context("while parsing condition for collection filter operation")?,
            )))));
        }
    }

    Ok(try_parse_reference(json)?.map(CollectionSource::Reference))
}

fn parse_collection_source(json: &JsonValue) -> anyhow::Result<CollectionSource> {
    try_parse_collection_source(json)?.ok_or_else(|| anyhow::format_err!("Could not find an appropriate operation for a collection source"))
}

pub fn parse_segments(json: &JsonValue) -> anyhow::Result<DslString> {
    if json.is_array() {
        let mut vec = vec![];
        for member in json.members() {
            if let Some(str) = try_get(member, "str") {
                vec.push(DslPart::String(str.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from str in segment parsing"))?.to_string()));
            } else if let Some(part) = try_parse_collection_source(member).context("while parsing collection source for segments")? {
                vec.push(DslPart::Ref(part));
            } else {
                anyhow::bail!("Could not find an appropriate key for segment parsing");
            }
        }
        return Ok(DslString(vec));
    }
    anyhow::bail!("segments is not an array")
}
