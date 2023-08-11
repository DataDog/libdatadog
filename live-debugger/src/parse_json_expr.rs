use crate::expr_defs::{
    BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
    Reference, StringComparison, StringSource, Value,
};
use crate::parse_json::ParseResult;
use crate::parse_util::get;
use crate::DslString;
use json::JsonValue;

fn parse_string_value(json: &JsonValue) -> ParseResult<StringSource> {
    if let Ok(substring) = get(json, "substring") {
        if substring.is_array() && substring.len() == 3 {
            return Ok(StringSource::Substring(Box::new((
                parse_string_value(&substring[0])?,
                parse_number_value(&substring[1])?,
                parse_number_value(&substring[2])?,
            ))));
        }
    }
    if json.is_string() {
        return Ok(StringSource::String(json.as_str().ok_or(())?.into()));
    }
    if json.is_null() {
        return Ok(StringSource::Null);
    }
    parse_reference(json).map(StringSource::Reference)
}

fn parse_number_value(json: &JsonValue) -> ParseResult<NumberSource> {
    if let Ok(reference) = get(json, "len") {
        return Ok(NumberSource::StringLength(parse_reference(reference)?));
    }
    if let Ok(reference) = get(json, "count") {
        return Ok(NumberSource::CollectionSize(parse_collection_source(
            reference,
        )?));
    }
    if json.is_number() {
        return Ok(NumberSource::Number(json.as_number().unwrap().into()));
    }
    parse_reference(json).map(NumberSource::Reference)
}

fn parse_reference(json: &JsonValue) -> ParseResult<Reference> {
    if let Ok(identifier) = get(json, "ref") {
        return Ok(Reference::Base(identifier.as_str().ok_or(())?.into()));
    }
    if let Ok(index) = get(json, "index") {
        if index.is_array() && index.len() == 2 {
            return Ok(Reference::Index(Box::new((
                parse_collection_source(&index[0])?,
                parse_value(&index[1])?,
            ))));
        }
    }
    if let Ok(index) = get(json, "nested") {
        if index.is_array() && index.len() == 2 {
            return Ok(Reference::Nested(Box::new((
                parse_reference(&index[0])?,
                parse_value(&index[1])?,
            ))));
        }
    }
    Err(())
}

pub fn parse_value(json: &JsonValue) -> ParseResult<Value> {
    parse_string_value(json)
        .map(Value::String)
        .or(parse_number_value(json).map(Value::Number))
        .or(parse_condition(json).map(|c| Value::Bool(Box::new(c))))
}

pub fn parse_condition(json: &JsonValue) -> ParseResult<Condition> {
    for (key, comparer) in [
        ("eq", BinaryComparison::Equals),
        ("ne", BinaryComparison::NotEquals),
        ("gt", BinaryComparison::GreaterThan),
        ("ge", BinaryComparison::GreaterOrEquals),
        ("lt", BinaryComparison::LessThan),
        ("le", BinaryComparison::LessOrEquals),
    ] {
        if let Ok(args) = get(json, key) {
            if args.is_array() && args.len() == 2 {
                return Ok(Condition::BinaryComparison(
                    parse_value(&args[0])?,
                    comparer,
                    parse_value(&args[1])?,
                ));
            }
        }
    }

    if let Ok(args) = get(json, "and") {
        if args.is_array() && args.len() == 2 {
            return Ok(Condition::Disjunction(Box::new((
                parse_condition(&args[0])?,
                parse_condition(&args[1])?,
            ))));
        }
    }

    if let Ok(args) = get(json, "or") {
        if args.is_array() && args.len() == 2 {
            return Ok(Condition::Conjunction(Box::new((
                parse_condition(&args[0])?,
                parse_condition(&args[1])?,
            ))));
        }
    }

    if let Ok(arg) = get(json, "not") {
        return Ok(Condition::Negation(Box::new(parse_condition(arg)?)));
    }

    if let Ok(arg) = get(json, "isEmpty") {
        return Ok(Condition::IsEmptyReference(parse_reference(arg)?));
    }

    if let Ok(arg) = get(json, "isUndefined") {
        return Ok(Condition::IsUndefinedReference(parse_reference(arg)?));
    }

    for (key, comparer) in [("any", CollectionMatch::Any), ("all", CollectionMatch::All)] {
        if let Ok(args) = get(json, key) {
            if args.is_array() && args.len() == 2 {
                return Ok(Condition::CollectionMatch(
                    comparer,
                    parse_reference(&args[0])?,
                    Box::new(parse_condition(&args[1])?),
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
        if let Ok(args) = get(json, key) {
            if args.is_array() && args.len() == 2 && args[1].is_string() {
                return Ok(Condition::StringComparison(
                    comparer,
                    parse_string_value(&args[0])?,
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

    Err(())
}

pub fn parse_collection_source(json: &JsonValue) -> ParseResult<CollectionSource> {
    if let Ok(index) = get(json, "filter") {
        if index.is_array() && index.len() == 2 {
            return Ok(CollectionSource::FilterOperator(Box::new((
                parse_collection_source(&index[0])?,
                parse_condition(&index[1])?,
            ))));
        }
    }

    parse_reference(json).map(CollectionSource::Reference)
}

pub fn parse_segments(json: &JsonValue) -> ParseResult<DslString> {
    if json.is_array() {
        let mut vec = vec![];
        for member in json.members() {
            if let Ok(str) = get(member, "str") {
                vec.push(DslPart::String(str.as_str().ok_or(())?.to_string()));
            } else if let Ok(part) = parse_collection_source(member) {
                vec.push(DslPart::Ref(part));
            } else {
                return Err(());
            }
        }
        return Ok(DslString(vec));
    }
    Err(())
}
