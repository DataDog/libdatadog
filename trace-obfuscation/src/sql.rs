// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use crate::sql_tokenizer::{SqlTokenizer, SqlTokenizerScanResult, TokenKind};

const QUESTION_MARK: char = '?';

pub fn obfuscate_sql_string(s: &str) -> String {
    let use_literal_escapes = true;
    let mut tokenizer = SqlTokenizer::new(s, use_literal_escapes);
    attempt_sql_obfuscation(tokenizer).unwrap_or("?".to_string())
}

fn attempt_sql_obfuscation(mut tokenizer: SqlTokenizer) -> anyhow::Result<String> {
    let mut result_str = String::new();
    let mut last_token_kind = TokenKind::Char;
    let mut last_token = String::new();

    loop {
        let mut result = tokenizer.scan();
        result.token = result.token.trim().to_string();

        if result.token_kind == TokenKind::LexError && tokenizer.err.is_some() {
            anyhow::bail!(tokenizer.err.unwrap())
        }
        result = discard(result, last_token.as_str(), &last_token_kind)?;
        result = replace(result, last_token.as_str(), &last_token_kind)?;

        let mut grouping_filter = GroupingFilter::new();

        result = grouping_filter.grouping(result, last_token.as_str(), &last_token_kind)?;
        if !result.token.is_empty() {
            if !result_str.is_empty() {
                match result.token.as_str() {
                    "," => {}
                    "=" => {
                        if last_token == ":" {
                            // do not add a space before an equals if a colon was
                            // present before it.
                            break;
                        }
                        result_str.push(' ');
                    }
                    _ => {
                        result_str.push(' ');
                    }
                }
            }
            result_str.push_str(&result.token);
        }
        last_token = result.token;
        last_token_kind = result.token_kind;
        if tokenizer.done {
            break;
        }
    }

    if result_str.is_empty() {
        anyhow::bail!("result is empty")
    }

    Ok(result_str)
}

fn set_result_as_filtered(
    mut result: SqlTokenizerScanResult,
) -> anyhow::Result<SqlTokenizerScanResult> {
    result.token_kind = TokenKind::Filtered;
    result.token = String::new();
    Ok(result)
}

fn set_result_filtered_groupable(mut result: SqlTokenizerScanResult, replacement: Option<char>) -> anyhow::Result<SqlTokenizerScanResult> {
    if result.token == "(" {
        result.token_kind = TokenKind::FilteredGroupableParenthesis;
    } else {
        result.token_kind = TokenKind::FilteredGroupable;
    }
    if let Some(rep) = replacement {
        result.token = rep.to_string();
    } else {
        result.token = String::new()
    }
    Ok(result)
}

// Filter the given result so that the certain tokens are completely skipped
fn discard(
    mut result: SqlTokenizerScanResult,
    last_token: &str,
    last_token_kind: &TokenKind,
) -> anyhow::Result<SqlTokenizerScanResult> {
    if *last_token_kind == TokenKind::As {
        return set_result_as_filtered(result);
    }

    if result.token_kind == TokenKind::Comment {
        return set_result_as_filtered(result);
    }

    if result.token == ";" {
        return set_result_filtered_groupable(result, None);
    }

    if result.token_kind == TokenKind::As {
        result.token = String::new();
        return Ok(result);
    }

    Ok(result)
}

fn replace_digits(mut result: SqlTokenizerScanResult) -> SqlTokenizerScanResult {
    let mut scanning_digit = false;
    let mut result_str = String::new();
    let char_iter = result.token.chars();
    for char in char_iter {
        if char.is_ascii_digit() {
            if scanning_digit {
                continue;
            }
            scanning_digit = true;
            result_str.push(QUESTION_MARK);
            continue;
        }
        scanning_digit = false;
        result_str.push(char);
    }
    result.token = result_str;
    result
}

// Filter the given result so that certain tokens are replaced with '?'
fn replace(
    mut result: SqlTokenizerScanResult,
    last_token: &str,
    last_token_kind: &TokenKind,
) -> anyhow::Result<SqlTokenizerScanResult> {
    if *last_token_kind == TokenKind::Savepoint {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }
    if last_token == "=" && result.token_kind == TokenKind::DoubleQuotedString {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }
    if result.token == "?" {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }

    match result.token_kind {
        TokenKind::String | TokenKind::Number | TokenKind::Null | TokenKind::Variable | TokenKind::BooleanLiteral | TokenKind::EscapeSequence => {
            set_result_filtered_groupable(result, Some(QUESTION_MARK))
        }
        TokenKind::ID => {
            result = replace_digits(result);
            Ok(result)
        }
        _ => {
            Ok(result)
        }
    }
}

struct GroupingFilter {
    consec_dropped_vals: i16, // counts the num of values dropped, e.g. 3 = ?, ?, ?
    consec_dropped_groups: i16, // counts the num of groups dropped, e.g. 2 = (?, ?), (?, ?, ?)
}

impl GroupingFilter {
    fn new() -> GroupingFilter {
        GroupingFilter {
            consec_dropped_vals: 0,
            consec_dropped_groups: 0,
        }
    }

    fn reset(&mut self) {
        self.consec_dropped_vals = 0;
        self.consec_dropped_groups = 0;
    }

    // Filter the given result, discarding grouping patterns.
    // A grouping is composed by items like:
    //   - '( ?, ?, ? )'
    //   - '( ?, ? ), ( ?, ? )'
    fn grouping(
        &mut self,
        mut result: SqlTokenizerScanResult,
        last_token: &str,
        last_token_kind: &TokenKind,
    ) -> anyhow::Result<SqlTokenizerScanResult> {
        // increasing the number of groups means that we're filtering an entire group
	    // because it can be represented with a single '( ? )'
        if (last_token == "(" && is_filtered_groupable(&result.token_kind)) || (result.token == "(" && self.consec_dropped_groups > 0) {
            self.consec_dropped_groups += 1;
        }

        let is_start_of_sub_query = [TokenKind::Select, TokenKind::Delete, TokenKind::Update, TokenKind::ID].contains(&result.token_kind);
        
        if self.consec_dropped_groups > 0 && last_token_kind == &TokenKind::FilteredGroupableParenthesis && is_start_of_sub_query {
            self.reset();
            result.token += "( ";
            return Ok(result);
        }

        if is_filtered_groupable(&result.token_kind) {
            // the previous filter has dropped this token so we should start
            // counting the group filter so that we accept only one '?' for
            // the same group
            self.consec_dropped_groups += 1;

            if self.consec_dropped_groups > 1 {
                return set_result_filtered_groupable(result, None);
            }
        }

        if self.consec_dropped_vals > 0 && (result.token == "," || result.token == "?") {
            return set_result_filtered_groupable(result, None);
        }

        if self.consec_dropped_groups > 1 {
            return set_result_filtered_groupable(result, None);
        }

        if ![",", "(", ")"].contains(&result.token.as_str()) && !is_filtered_groupable(&result.token_kind) {
            self.reset()
        }
        
        Ok(result)
    }
}
fn is_filtered_groupable(token: &TokenKind) -> bool {
    token == &TokenKind::FilteredGroupable || token == &TokenKind::FilteredGroupableParenthesis
}

#[cfg(test)]
mod tests {

    use duplicate::duplicate_item;

    use crate::sql_tokenizer::SqlTokenizer;

    use super::attempt_sql_obfuscation;

    #[duplicate_item(
        [
            test_name   [test_sql_obfuscation_1]
            input       ["SELECT * from table_name"]
            expected    ["SELECT * from table_name"];
        ]
        [
            test_name   [test_sql_obfuscation_2]
            input       ["autovacuum: VACUUM ANALYZE fake.table"]
            expected    ["autovacuum : VACUUM ANALYZE fake.table"];
        ]
        [
            test_name   [test_sql_obfuscation_3]
            input       ["autovacuum: VACUUM fake.big_table (to prevent wraparound)"]
            expected    ["autovacuum : VACUUM fake.big_table ( to prevent wraparound )"];
        ]
    )]
    #[test]
    fn test_name() {
        let tokenizer = SqlTokenizer::new(input, false);
        let result = attempt_sql_obfuscation(tokenizer);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected);
    }
}
