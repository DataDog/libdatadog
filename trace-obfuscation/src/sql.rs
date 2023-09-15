// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use crate::sql_tokenizer::{SqlTokenizer, SqlTokenizerScanResult, TokenKind};

const QUESTION_MARK: char = '?';

pub fn obfuscate_sql_string(s: &str) -> String {
    let use_literal_escapes = true;
    let tokenizer = SqlTokenizer::new(s, use_literal_escapes);
    attempt_sql_obfuscation(tokenizer).unwrap_or("?".to_string())
}

fn attempt_sql_obfuscation(mut tokenizer: SqlTokenizer) -> anyhow::Result<String> {
    let mut result_str = String::new();
    let mut last_token_kind = TokenKind::Char;
    let mut last_token = String::new();

    let mut grouping_filter = GroupingFilter::new();

    loop {
        let mut result = tokenizer.scan();
        result.token = result.token.trim().to_string();

        if result.token_kind == TokenKind::LexError && tokenizer.err.is_some() {
            anyhow::bail!(tokenizer.err.unwrap())
        }
        result = discard(result, &last_token_kind)?;
        result = replace(result, last_token.as_str(), &last_token_kind)?;

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
        TokenKind::DollarQuotedString | TokenKind::String | TokenKind::Number | TokenKind::Null | TokenKind::Variable | TokenKind::BooleanLiteral | TokenKind::EscapeSequence => {
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
        } else if is_filtered_groupable(&result.token_kind) {
            // the previous filter has dropped this token so we should start
            // counting the group filter so that we accept only one '?' for
            // the same group
            self.consec_dropped_vals += 1;

            if self.consec_dropped_vals > 1 {
                return set_result_filtered_groupable(result, None);
            }
        } else if (self.consec_dropped_vals > 0 && (result.token == "," || result.token == "?")) || self.consec_dropped_groups > 1 {
            return set_result_filtered_groupable(result, None);
        } else if ![",", "(", ")"].contains(&result.token.as_str()) && !is_filtered_groupable(&result.token_kind) {
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
            test_name   [test_sql_obfuscation_remove_alias]
            input       ["SELECT username AS person FROM users WHERE id=4"]
            expected    ["SELECT username FROM users WHERE id = ?"];
        ]
        [
            test_name   [test_sql_obfuscation_dollar_quoted_string_1]
            input       ["SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users"]
            expected    ["SELECT ? FROM users"];
        ]
        [
            test_name   [test_sql_obfuscation_dollar_quoted_string_2]
            input       ["SELECT $$INSERT INTO table VALUES ('a', 1, 2)$$ FROM users"]
            expected    ["SELECT ? FROM users"];
        ]
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
        [
            test_name   [test_sql_obfuscation_4]
            input       [r#"/* Multi-line comment */
SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO owners (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT"#]
            expected    ["SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO owners ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"];
        ]
        [
            test_name   [test_sql_obfuscation_5]
            input       [r#"/* Multi-line comment
with line breaks */
WITH sales AS
(SELECT sf2.*
    FROM gosalesdw28391.sls_order_method_dim AS md,
        gosalesdw1920.sls_product_dim391 AS pd190,
        gosalesdw3819.emp_employee_dim AS ed,
        gosalesdw3919.sls_sales_fact3819 AS sf2
    WHERE pd190.product_key = sf2.product_key
    AND pd190.product_number381 > 10000
    AND pd190.base_product_key > 30
    AND md.order_method_key = sf2.order_method_key8319
    AND md.order_method_code > 5
    AND ed.employee_key = sf2.employee_key
    AND ed.manager_code1 > 20),
inventory3118 AS
(SELECT if.*
    FROM gosalesdw1592.go_branch_dim AS bd3221,
    gosalesdw.dist_inventory_fact AS if
    WHERE if.branch_key = bd3221.branch_key
    AND bd3221.branch_code > 20)
SELECT sales1828.product_key AS PROD_KEY,
SUM(CAST (inventory3118.quantity_shipped AS BIGINT)) AS INV_SHIPPED3118,
SUM(CAST (sales1828.quantity AS BIGINT)) AS PROD_QUANTITY,
RANK() OVER ( ORDER BY SUM(CAST (sales1828.quantity AS BIGINT)) DESC) AS PROD_RANK
FROM sales1828, inventory3118
WHERE sales1828.product_key = inventory3118.product_key
GROUP BY sales1828.product_key"#]
            expected    ["WITH sales SELECT sf?.* FROM gosalesdw?.sls_order_method_dim, gosalesdw?.sls_product_dim?, gosalesdw?.emp_employee_dim, gosalesdw?.sls_sales_fact? WHERE pd?.product_key = sf?.product_key AND pd?.product_number? > ? AND pd?.base_product_key > ? AND md.order_method_key = sf?.order_method_key? AND md.order_method_code > ? AND ed.employee_key = sf?.employee_key AND ed.manager_code? > ? ) inventory? SELECT if.* FROM gosalesdw?.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd?.branch_key AND bd?.branch_code > ? ) SELECT sales?.product_key, SUM ( CAST ( inventory?.quantity_shipped ) ), SUM ( CAST ( sales?.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales?.quantity ) ) DESC ) FROM sales?, inventory? WHERE sales?.product_key = inventory?.product_key GROUP BY sales?.product_key"];
        ]
        [
            test_name   [test_sql_obfuscation_6]
            input       [r#"/*
Multi-line comment
with line breaks
*/
/* Two multi-line comments with
line breaks */
SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = 't'"#]
            expected    ["SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = ?"];
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
