// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use crate::sql_tokenizer::{SqlTokenizer, SqlTokenizerScanResult, TokenKind};

const QUESTION_MARK: char = '?';
const QUESTION_MARK_STR: &str = "?";

pub fn obfuscate_sql_string(s: &str, replace_digits: bool) -> String {
    let use_literal_escapes = false;
    let mut tokenizer = SqlTokenizer::new(s, use_literal_escapes);
    let result = attempt_sql_obfuscation(tokenizer, replace_digits);
    if result.error.is_none() || !result.seen_escape{
        return result.obfuscated_string.unwrap_or_default()
    }
    println!("retrying");

    tokenizer = SqlTokenizer::new(s, !use_literal_escapes);
    let second_attempt_result = attempt_sql_obfuscation(tokenizer, replace_digits);
    second_attempt_result.obfuscated_string.unwrap_or_default() 
}

struct AttemptSqlObfuscationResult {
    obfuscated_string: Option<String>,
    error: Option<anyhow::Error>,
    seen_escape: bool
}

fn return_attempt_sql_obfuscation_result(obfuscated_string: Option<String>, error: Option<anyhow::Error>, seen_escape: bool) -> AttemptSqlObfuscationResult {
    return AttemptSqlObfuscationResult {
        obfuscated_string,
        error,
        seen_escape,
    }
}

fn attempt_sql_obfuscation(mut tokenizer: SqlTokenizer, replace_digits: bool) -> AttemptSqlObfuscationResult {
    // TODO: Support replace digits in specific tables
    let mut result_str = String::new();
    let mut last_token_kind = TokenKind::Char;
    let mut last_token = String::new();

    let mut grouping_filter = GroupingFilter::new();

    loop {
        let mut result = tokenizer.scan();
        result.token = result.token.trim().to_string();

        if result.token_kind == TokenKind::LexError && tokenizer.err.is_some() {
            return return_attempt_sql_obfuscation_result(None, tokenizer.err, tokenizer.seen_escape);
        }
        result = match discard(result, &last_token_kind) {
            Ok(res) => res,
            Err(err) => return return_attempt_sql_obfuscation_result(None, Some(err), tokenizer.seen_escape)
        };
        result = match replace(result, replace_digits, last_token.as_str(), &last_token_kind)  {
            Ok(res) => res,
            Err(err) => return return_attempt_sql_obfuscation_result(None, Some(err), tokenizer.seen_escape)
        };

        result = match grouping_filter.grouping(result, last_token.as_str(), &last_token_kind)  {
            Ok(res) => res,
            Err(err) => return return_attempt_sql_obfuscation_result(None, Some(err), tokenizer.seen_escape)
        };

        if !result.token.is_empty() {
            if !result_str.is_empty() {
                match result.token.as_str() {
                    "," => {}
                    "=" => {
                        if last_token != ":" {
                            // do not add a space before an equals if a colon was
                            // present before it.
                            result_str.push(' ');
                        }
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
        return return_attempt_sql_obfuscation_result(None, Some(anyhow::anyhow!("result is empty")), tokenizer.seen_escape)
    }

    return_attempt_sql_obfuscation_result(Some(result_str), tokenizer.err, tokenizer.seen_escape)
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
    if *last_token_kind == TokenKind::FilteredBracketedIdentifier {
        if result.token != "]" {
            // we haven't found the closing bracket yet, keep going
            if result.token_kind != TokenKind::ID {
                // the token between the brackets *must* be an identifier,
                // otherwise the query is invalid.
                anyhow::bail!("expected identifier in bracketed filter, got {}", result.token)
            }
            result.token_kind = TokenKind::FilteredBracketedIdentifier;
            result.token = String::new();
            return Ok(result)
        } else {
            return set_result_as_filtered(result);
        }
    }

    if *last_token_kind == TokenKind::As {
        if result.token == "[" {
            result.token_kind = TokenKind::FilteredBracketedIdentifier
        } else {
            result.token_kind = TokenKind::Filtered;
        }
        result.token = String::new();
        return Ok(result);
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
    should_replace_digits: bool,
    last_token: &str,
    last_token_kind: &TokenKind,
) -> anyhow::Result<SqlTokenizerScanResult> {
    if *last_token_kind == TokenKind::Savepoint {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }
    if last_token == "=" && result.token_kind == TokenKind::DoubleQuotedString {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }
    if result.token == QUESTION_MARK_STR {
        return set_result_filtered_groupable(result, Some(QUESTION_MARK));
    }

    match result.token_kind {
        TokenKind::DollarQuotedString | TokenKind::String | TokenKind::Number | TokenKind::Null | TokenKind::Variable | TokenKind::BooleanLiteral | TokenKind::EscapeSequence => {
            set_result_filtered_groupable(result, Some(QUESTION_MARK))
        }
        TokenKind::ID => {
            if should_replace_digits {
                result = replace_digits(result);
            }
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
            result.token = format!("( {}", result.token);
            return Ok(result);
        } else if is_filtered_groupable(&result.token_kind) {
            // the previous filter has dropped this token so we should start
            // counting the group filter so that we accept only one '?' for
            // the same group
            self.consec_dropped_vals += 1;

            if self.consec_dropped_vals > 1 {
                return set_result_filtered_groupable(result, None);
            }
        } else if (self.consec_dropped_vals > 0 && (result.token == "," || result.token == QUESTION_MARK_STR)) || self.consec_dropped_groups > 1 {
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

    use crate::{sql_tokenizer::SqlTokenizer, sql::obfuscate_sql_string};

    use super::attempt_sql_obfuscation;

    #[duplicate_item(
        [
            test_name       [test_sql_obfuscation_remove_alias]
            replace_digits  [true]
            input           ["SELECT username AS person FROM users WHERE id=4"]
            expected        ["SELECT username FROM users WHERE id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_dollar_quoted_string_1]
            replace_digits  [true]
            input           ["SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users"]
            expected        ["SELECT ? FROM users"];
        ]
        [
            test_name       [test_sql_obfuscation_dollar_quoted_string_2]
            replace_digits  [true]
            input           ["SELECT $$INSERT INTO table VALUES ('a', 1, 2)$$ FROM users"]
            expected        ["SELECT ? FROM users"];
        ]
        [
            test_name       [test_sql_obfuscation_1]
            replace_digits  [true]
            input           ["SELECT * from table_name"]
            expected        ["SELECT * from table_name"];
        ]
        [
            test_name       [test_sql_obfuscation_2]
            replace_digits  [true]
            input           ["autovacuum: VACUUM ANALYZE fake.table"]
            expected        ["autovacuum : VACUUM ANALYZE fake.table"];
        ]
        [
            test_name       [test_sql_obfuscation_3]
            replace_digits  [true]
            input           ["autovacuum: VACUUM fake.big_table (to prevent wraparound)"]
            expected        ["autovacuum : VACUUM fake.big_table ( to prevent wraparound )"];
        ]
        [
            test_name       [test_sql_obfuscation_4]
            replace_digits  [true]
            input           [r#"/* Multi-line comment */
SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO owners (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT"#]
            expected        ["SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO owners ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"];
        ]
        [
            test_name       [test_sql_obfuscation_5]
            replace_digits  [true]
            input           [r#"/* Multi-line comment
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
            expected        ["WITH sales SELECT sf?.* FROM gosalesdw?.sls_order_method_dim, gosalesdw?.sls_product_dim?, gosalesdw?.emp_employee_dim, gosalesdw?.sls_sales_fact? WHERE pd?.product_key = sf?.product_key AND pd?.product_number? > ? AND pd?.base_product_key > ? AND md.order_method_key = sf?.order_method_key? AND md.order_method_code > ? AND ed.employee_key = sf?.employee_key AND ed.manager_code? > ? ) inventory? SELECT if.* FROM gosalesdw?.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd?.branch_key AND bd?.branch_code > ? ) SELECT sales?.product_key, SUM ( CAST ( inventory?.quantity_shipped ) ), SUM ( CAST ( sales?.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales?.quantity ) ) DESC ) FROM sales?, inventory? WHERE sales?.product_key = inventory?.product_key GROUP BY sales?.product_key"];
        ]
        [
            test_name       [test_sql_obfuscation_6]
            replace_digits  [true]
            input           [r#"/*
Multi-line comment
with line breaks
*/
/* Two multi-line comments with
line breaks */
SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = 't'"#]
            expected        ["SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_7]
            replace_digits  [false]
            input           ["CREATE TRIGGER dogwatcher SELECT ON w1 BEFORE (UPDATE d1 SET (c1, c2, c3) = (c1 + 1, c2 + 1, c3 + 1))"]
            expected        ["CREATE TRIGGER dogwatcher SELECT ON w1 BEFORE ( UPDATE d1 SET ( c1, c2, c3 ) = ( c1 + ? c2 + ? c3 + ? ) )"];
        ]
        [
            test_name       [test_sql_obfuscation_8]
            replace_digits  [true]
            input           ["SELECT * FROM (VALUES (1, 'dog')) AS d (id, animal)"]
            expected        ["SELECT * FROM ( VALUES ( ? ) ) ( id, animal )"];
        ]
        [
            test_name       [test_sql_obfuscation_9]
            replace_digits  [true]
            input           ["ALTER TABLE table DROP COLUMN column"]
            expected        ["ALTER TABLE table DROP COLUMN column"];
        ]
        [
            test_name       [test_sql_obfuscation_10]
            replace_digits  [true]
            input           ["REVOKE ALL ON SCHEMA datadog FROM datadog"]
            expected        ["REVOKE ALL ON SCHEMA datadog FROM datadog"];
        ]
        [
            test_name       [test_sql_obfuscation_11]
            replace_digits  [false]
            input           [r#"WITH T1 AS (SELECT PNO , PNAME , COLOR , WEIGHT , CITY FROM P WHERE  CITY = 'London'),
T2 AS (SELECT PNO, PNAME, COLOR, WEIGHT, CITY, 2 * WEIGHT AS NEW_WEIGHT, 'Oslo' AS NEW_CITY FROM T1),
T3 AS ( SELECT PNO , PNAME, COLOR, NEW_WEIGHT AS WEIGHT, NEW_CITY AS CITY FROM T2),
T4 AS ( TABLE P EXCEPT CORRESPONDING TABLE T1)
TABLE T4 UNION CORRESPONDING TABLE T3"#]
            expected        ["WITH T1 SELECT PNO, PNAME, COLOR, WEIGHT, CITY FROM P WHERE CITY = ? ) T2 SELECT PNO, PNAME, COLOR, WEIGHT, CITY, ? * WEIGHT, ? FROM T1 ), T3 SELECT PNO, PNAME, COLOR, NEW_WEIGHT, NEW_CITY FROM T2 ), T4 TABLE P EXCEPT CORRESPONDING TABLE T1 ) TABLE T4 UNION CORRESPONDING TABLE T3"];
        ]
        [
            test_name       [test_sql_obfuscation_12]
            replace_digits  [true]
            input           ["SELECT Codi , Nom_CA AS Nom, Descripció_CAT AS Descripció FROM ProtValAptitud WHERE Vigent=1 ORDER BY Ordre, Codi"]
            expected        ["SELECT Codi, Nom_CA, Descripció_CAT FROM ProtValAptitud WHERE Vigent = ? ORDER BY Ordre, Codi"];
        ]
        [
            test_name       [test_sql_obfuscation_13]
            replace_digits  [true]
            input           [" SELECT  dbo.Treballadors_ProtCIE_AntecedentsPatologics.IdTreballadorsProtCIE_AntecedentsPatologics,   dbo.ProtCIE.Codi As CodiProtCIE, Treballadors_ProtCIE_AntecedentsPatologics.Año,                              dbo.ProtCIE.Nom_ES, dbo.ProtCIE.Nom_CA  FROM         dbo.Treballadors_ProtCIE_AntecedentsPatologics  WITH (NOLOCK)  INNER JOIN                       dbo.ProtCIE  WITH (NOLOCK)  ON dbo.Treballadors_ProtCIE_AntecedentsPatologics.CodiProtCIE = dbo.ProtCIE.Codi  WHERE Treballadors_ProtCIE_AntecedentsPatologics.IdTreballador =  12345 ORDER BY   Treballadors_ProtCIE_AntecedentsPatologics.Año DESC, dbo.ProtCIE.Codi "]
            expected        ["SELECT dbo.Treballadors_ProtCIE_AntecedentsPatologics.IdTreballadorsProtCIE_AntecedentsPatologics, dbo.ProtCIE.Codi, Treballadors_ProtCIE_AntecedentsPatologics.Año, dbo.ProtCIE.Nom_ES, dbo.ProtCIE.Nom_CA FROM dbo.Treballadors_ProtCIE_AntecedentsPatologics WITH ( NOLOCK ) INNER JOIN dbo.ProtCIE WITH ( NOLOCK ) ON dbo.Treballadors_ProtCIE_AntecedentsPatologics.CodiProtCIE = dbo.ProtCIE.Codi WHERE Treballadors_ProtCIE_AntecedentsPatologics.IdTreballador = ? ORDER BY Treballadors_ProtCIE_AntecedentsPatologics.Año DESC, dbo.ProtCIE.Codi"];
        ]
        [
            test_name       [test_sql_obfuscation_14]
            replace_digits  [true]
            input           ["select  top 100 percent  IdTrebEmpresa as [IdTrebEmpresa], CodCli as [Client], NOMEMP as [Nom Client], Baixa as [Baixa], CASE WHEN IdCentreTreball IS NULL THEN '-' ELSE  CONVERT(VARCHAR(8),IdCentreTreball) END as [Id Centre],  CASE WHEN NOMESTAB IS NULL THEN '-' ELSE NOMESTAB END  as [Nom Centre],  TIPUS as [Tipus Lloc], CASE WHEN IdLloc IS NULL THEN '-' ELSE  CONVERT(VARCHAR(8),IdLloc) END  as [Id Lloc],  CASE WHEN NomLlocComplert IS NULL THEN '-' ELSE NomLlocComplert END  as [Lloc Treball],  CASE WHEN DesLloc IS NULL THEN '-' ELSE DesLloc END  as [Descripció], IdLlocTreballUnic as [Id Únic]  From ( SELECT    '-' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP,   dbo.Treb_Empresa.Baixa,                      dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, null AS IdLloc,                        null AS NomLlocComplert, dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     dbo.Treb_Empresa.IdTreballador = 64376 AND Treb_Empresa.IdTecEIRLLlocTreball IS NULL AND IdMedEIRLLlocTreball IS NULL AND IdLlocTreballTemporal IS NULL  UNION ALL SELECT    'AV. RIESGO' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa,                       dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdTecEIRLLlocTreball AS IdLloc,                        dbo.fn_NomLlocComposat(dbo.Treb_Empresa.IdTecEIRLLlocTreball) AS NomLlocComplert, dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     (dbo.Treb_Empresa.IdTreballador = 64376) AND (NOT (dbo.Treb_Empresa.IdTecEIRLLlocTreball IS NULL))  UNION ALL SELECT     'EXTERNA' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP,  dbo.Treb_Empresa.Baixa,                      dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdMedEIRLLlocTreball AS IdLloc,                        dbo.fn_NomMedEIRLLlocComposat(dbo.Treb_Empresa.IdMedEIRLLlocTreball) AS NomLlocComplert,  dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     (dbo.Treb_Empresa.IdTreballador = 64376) AND (Treb_Empresa.IdTecEIRLLlocTreball IS NULL) AND (NOT (dbo.Treb_Empresa.IdMedEIRLLlocTreball IS NULL))  UNION ALL SELECT     'TEMPORAL' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa,                       dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdLlocTreballTemporal AS IdLloc,                       dbo.Lloc_Treball_Temporal.NomLlocTreball AS NomLlocComplert,  dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli INNER JOIN                       dbo.Lloc_Treball_Temporal  WITH (NOLOCK) ON dbo.Treb_Empresa.IdLlocTreballTemporal = dbo.Lloc_Treball_Temporal.IdLlocTreballTemporal LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     dbo.Treb_Empresa.IdTreballador = 64376 AND Treb_Empresa.IdTecEIRLLlocTreball IS NULL AND IdMedEIRLLlocTreball IS NULL ) as taula  Where 1=0 "]
            expected        ["select top ? percent IdTrebEmpresa, CodCli, NOMEMP, Baixa, CASE WHEN IdCentreTreball IS ? THEN ? ELSE CONVERT ( VARCHAR ( ? ) IdCentreTreball ) END, CASE WHEN NOMESTAB IS ? THEN ? ELSE NOMESTAB END, TIPUS, CASE WHEN IdLloc IS ? THEN ? ELSE CONVERT ( VARCHAR ( ? ) IdLloc ) END, CASE WHEN NomLlocComplert IS ? THEN ? ELSE NomLlocComplert END, CASE WHEN DesLloc IS ? THEN ? ELSE DesLloc END, IdLlocTreballUnic From ( SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, ?, ?, dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE dbo.Treb_Empresa.IdTreballador = ? AND Treb_Empresa.IdTecEIRLLlocTreball IS ? AND IdMedEIRLLlocTreball IS ? AND IdLlocTreballTemporal IS ? UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdTecEIRLLlocTreball, dbo.fn_NomLlocComposat ( dbo.Treb_Empresa.IdTecEIRLLlocTreball ), dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE ( dbo.Treb_Empresa.IdTreballador = ? ) AND ( NOT ( dbo.Treb_Empresa.IdTecEIRLLlocTreball IS ? ) ) UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdMedEIRLLlocTreball, dbo.fn_NomMedEIRLLlocComposat ( dbo.Treb_Empresa.IdMedEIRLLlocTreball ), dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE ( dbo.Treb_Empresa.IdTreballador = ? ) AND ( Treb_Empresa.IdTecEIRLLlocTreball IS ? ) AND ( NOT ( dbo.Treb_Empresa.IdMedEIRLLlocTreball IS ? ) ) UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdLlocTreballTemporal, dbo.Lloc_Treball_Temporal.NomLlocTreball, dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli INNER JOIN dbo.Lloc_Treball_Temporal WITH ( NOLOCK ) ON dbo.Treb_Empresa.IdLlocTreballTemporal = dbo.Lloc_Treball_Temporal.IdLlocTreballTemporal LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE dbo.Treb_Empresa.IdTreballador = ? AND Treb_Empresa.IdTecEIRLLlocTreball IS ? AND IdMedEIRLLlocTreball IS ? ) Where ? = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_15]
            replace_digits  [true]
            input           ["select  IdHistLabAnt as [IdHistLabAnt], IdTreballador as [IdTreballador], Empresa as [Professió], Anys as [Anys],  Riscs as [Riscos], Nom_CA AS [Prot CNO], Nom_ES as [Prot CNO Altre Idioma]   From ( SELECT     dbo.Treb_HistAnt.IdHistLabAnt, dbo.Treb_HistAnt.IdTreballador,           dbo.Treb_HistAnt.Empresa, dbo.Treb_HistAnt.Anys, dbo.Treb_HistAnt.Riscs, dbo.Treb_HistAnt.CodiProtCNO,           dbo.ProtCNO.Nom_ES, dbo.ProtCNO.Nom_CA  FROM     dbo.Treb_HistAnt  WITH (NOLOCK) LEFT OUTER JOIN                       dbo.ProtCNO  WITH (NOLOCK) ON dbo.Treb_HistAnt.CodiProtCNO = dbo.ProtCNO.Codi  Where  dbo.Treb_HistAnt.IdTreballador = 12345 ) as taula "]
            expected        ["select IdHistLabAnt, IdTreballador, Empresa, Anys, Riscs, Nom_CA, Nom_ES From ( SELECT dbo.Treb_HistAnt.IdHistLabAnt, dbo.Treb_HistAnt.IdTreballador, dbo.Treb_HistAnt.Empresa, dbo.Treb_HistAnt.Anys, dbo.Treb_HistAnt.Riscs, dbo.Treb_HistAnt.CodiProtCNO, dbo.ProtCNO.Nom_ES, dbo.ProtCNO.Nom_CA FROM dbo.Treb_HistAnt WITH ( NOLOCK ) LEFT OUTER JOIN dbo.ProtCNO WITH ( NOLOCK ) ON dbo.Treb_HistAnt.CodiProtCNO = dbo.ProtCNO.Codi Where dbo.Treb_HistAnt.IdTreballador = ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_16]
            replace_digits  [true]
            input           ["SELECT     Cli_Establiments.CODCLI, Cli_Establiments.Id_ESTAB_CLI As [Código Centro Trabajo], Cli_Establiments.CODIGO_CENTRO_AXAPTA As [Código C. Axapta],  Cli_Establiments.NOMESTAB As [Nombre],                                 Cli_Establiments.ADRECA As [Dirección], Cli_Establiments.CodPostal As [Código Postal], Cli_Establiments.Poblacio as [Población], Cli_Establiments.Provincia,                                Cli_Establiments.TEL As [Tel],  Cli_Establiments.EMAIL As [EMAIL],                                Cli_Establiments.PERS_CONTACTE As [Contacto], Cli_Establiments.PERS_CONTACTE_CARREC As [Cargo Contacto], Cli_Establiments.NumTreb As [Plantilla],                                Cli_Establiments.Localitzacio As [Localización], Tipus_Activitat.CNAE, Tipus_Activitat.Nom_ES As [Nombre Actividad], ACTIVO AS [Activo]                        FROM         Cli_Establiments LEFT OUTER JOIN                                    Tipus_Activitat ON Cli_Establiments.Id_ACTIVITAT = Tipus_Activitat.IdActivitat                        Where CODCLI = '01234' AND CENTRE_CORRECTE = 3 AND ACTIVO = 5                        ORDER BY Cli_Establiments.CODIGO_CENTRO_AXAPTA "]
            expected        ["SELECT Cli_Establiments.CODCLI, Cli_Establiments.Id_ESTAB_CLI, Cli_Establiments.CODIGO_CENTRO_AXAPTA, Cli_Establiments.NOMESTAB, Cli_Establiments.ADRECA, Cli_Establiments.CodPostal, Cli_Establiments.Poblacio, Cli_Establiments.Provincia, Cli_Establiments.TEL, Cli_Establiments.EMAIL, Cli_Establiments.PERS_CONTACTE, Cli_Establiments.PERS_CONTACTE_CARREC, Cli_Establiments.NumTreb, Cli_Establiments.Localitzacio, Tipus_Activitat.CNAE, Tipus_Activitat.Nom_ES, ACTIVO FROM Cli_Establiments LEFT OUTER JOIN Tipus_Activitat ON Cli_Establiments.Id_ACTIVITAT = Tipus_Activitat.IdActivitat Where CODCLI = ? AND CENTRE_CORRECTE = ? AND ACTIVO = ? ORDER BY Cli_Establiments.CODIGO_CENTRO_AXAPTA"];
        ]
        [
            test_name       [test_sql_obfuscation_17]
            replace_digits  [true]
            input           ["select * from dollarField$ as df from some$dollar$filled_thing$$;"]
            expected        ["select * from dollarField$ from some$dollar$filled_thing$$"];
        ]
        [
            test_name       [test_sql_obfuscation_18]
            replace_digits  [true]
            input           ["select * from `構わない`;"]
            expected        ["select * from 構わない"];
        ]
        [
            test_name       [test_sql_obfuscation_19]
            replace_digits  [true]
            input           ["select * from names where name like '�����';"]
            expected        ["select * from names where name like ?"];
        ]
        [
            test_name       [test_sql_obfuscation_20]
            replace_digits  [true]
            input           ["select replacement from table where replacement = 'i�n�t�e��rspersed';"]
            expected        ["select replacement from table where replacement = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_21]
            replace_digits  [true]
            input           ["SELECT ('\\ufffd');"]
            expected        ["SELECT ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_on_1]
            replace_digits  [true]
            input           ["REPLACE INTO sales_2019_07_01 (`itemID`, `date`, `qty`, `price`) VALUES ((SELECT itemID FROM item1001 WHERE `sku` = [sku]), CURDATE(), [qty], 0.00)"]
            expected        ["REPLACE INTO sales_?_?_? ( itemID, date, qty, price ) VALUES ( ( SELECT itemID FROM item? WHERE sku = [ sku ] ), CURDATE ( ), [ qty ], ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_on_2]
            replace_digits  [true]
            input           ["SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = 2 AND ddh19.name = 'datadog'"]
            expected        ["SELECT ddh?.name, ddt.tags FROM dd?.host ddh?, dd?.host_tags ddt WHERE ddh?.id = ddt.host_id AND ddh?.org_id = ? AND ddh?.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_on_3]
            replace_digits  [true]
            input           ["SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk"]
            expected        ["SELECT ddu?.name, ddo.id?, ddk.app_key? FROM dd?.user ddu?, dd?.orgs? ddo, dd?.keys ddk"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_on_4]
            replace_digits  [true]
            input           [r#"SELECT daily_values1529.*, LEAST((5040000 - @runtot), value1830) AS value1830,
(@runtot := @runtot + daily_values1529.value1830) AS total
FROM (SELECT @runtot:=0) AS n,
daily_values1529 WHERE daily_values1529.subject_id = 12345 AND daily_values1592.subject_type = 'Skippity'
AND (daily_values1529.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date"#]                 
            expected        ["SELECT daily_values?.*, LEAST ( ( ? - @runtot ), value? ), ( @runtot := @runtot + daily_values?.value? ) FROM ( SELECT @runtot := ? ), daily_values? WHERE daily_values?.subject_id = ? AND daily_values?.subject_type = ? AND ( daily_values?.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_on_5]
            replace_digits  [true]
            input           [r#"WITH
sales AS
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
            expected        ["WITH sales SELECT sf?.* FROM gosalesdw?.sls_order_method_dim, gosalesdw?.sls_product_dim?, gosalesdw?.emp_employee_dim, gosalesdw?.sls_sales_fact? WHERE pd?.product_key = sf?.product_key AND pd?.product_number? > ? AND pd?.base_product_key > ? AND md.order_method_key = sf?.order_method_key? AND md.order_method_code > ? AND ed.employee_key = sf?.employee_key AND ed.manager_code? > ? ) inventory? SELECT if.* FROM gosalesdw?.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd?.branch_key AND bd?.branch_code > ? ) SELECT sales?.product_key, SUM ( CAST ( inventory?.quantity_shipped ) ), SUM ( CAST ( sales?.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales?.quantity ) ) DESC ) FROM sales?, inventory? WHERE sales?.product_key = inventory?.product_key GROUP BY sales?.product_key"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_off_1]
            replace_digits  [false]
            input           ["REPLACE INTO sales_2019_07_01 (`itemID`, `date`, `qty`, `price`) VALUES ((SELECT itemID FROM item1001 WHERE `sku` = [sku]), CURDATE(), [qty], 0.00)"]
            expected        ["REPLACE INTO sales_2019_07_01 ( itemID, date, qty, price ) VALUES ( ( SELECT itemID FROM item1001 WHERE sku = [ sku ] ), CURDATE ( ), [ qty ], ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_off_2]
            replace_digits  [false]
            input           ["SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = 2 AND ddh19.name = 'datadog'"]
            expected        ["SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = ? AND ddh19.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_off_3]
            replace_digits  [false]
            input           ["SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk"]
            expected        ["SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_off_4]
            replace_digits  [false]
            input           ["SELECT daily_values1529.*, LEAST((5040000 - @runtot), value1830) AS value1830,
(@runtot := @runtot + daily_values1529.value1830) AS total
FROM (SELECT @runtot:=0) AS n,
daily_values1529 WHERE daily_values1529.subject_id = 12345 AND daily_values1592.subject_type = 'Skippity'
AND (daily_values1529.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date"]
            expected        ["SELECT daily_values1529.*, LEAST ( ( ? - @runtot ), value1830 ), ( @runtot := @runtot + daily_values1529.value1830 ) FROM ( SELECT @runtot := ? ), daily_values1529 WHERE daily_values1529.subject_id = ? AND daily_values1592.subject_type = ? AND ( daily_values1529.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"];
        ]
        [
            test_name       [test_sql_obfuscation_replace_digits_off_5]
            replace_digits  [false]
            input           ["WITH sales AS
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
GROUP BY sales1828.product_key"]
            expected        ["WITH sales SELECT sf2.* FROM gosalesdw28391.sls_order_method_dim, gosalesdw1920.sls_product_dim391, gosalesdw3819.emp_employee_dim, gosalesdw3919.sls_sales_fact3819 WHERE pd190.product_key = sf2.product_key AND pd190.product_number381 > ? AND pd190.base_product_key > ? AND md.order_method_key = sf2.order_method_key8319 AND md.order_method_code > ? AND ed.employee_key = sf2.employee_key AND ed.manager_code1 > ? ) inventory3118 SELECT if.* FROM gosalesdw1592.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd3221.branch_key AND bd3221.branch_code > ? ) SELECT sales1828.product_key, SUM ( CAST ( inventory3118.quantity_shipped ) ), SUM ( CAST ( sales1828.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales1828.quantity ) ) DESC ) FROM sales1828, inventory3118 WHERE sales1828.product_key = inventory3118.product_key GROUP BY sales1828.product_key"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_1]
            replace_digits  [false]
            input           ["select * from users where id = 42"]
            expected        ["select * from users where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_2]
            replace_digits  [false]
            input           ["select * from users where float = .43422"]
            expected        ["select * from users where float = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_3]
            replace_digits  [false]
            input           ["SELECT host, status FROM ec2_status WHERE org_id = 42"]
            expected        ["SELECT host, status FROM ec2_status WHERE org_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_4]
            replace_digits  [false]
            input           ["SELECT host, status FROM ec2_status WHERE org_id=42"]
            expected        ["SELECT host, status FROM ec2_status WHERE org_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_5]
            replace_digits  [false]
            input           ["-- get user \n--\n select * \n   from users \n    where\n       id = 214325346"]
            expected        ["select * from users where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_6]
            replace_digits  [false]
            input           ["SELECT * FROM `host` WHERE `id` IN (42, 43) /*comment with parameters,host:localhost,url:controller#home,id:FF005:00CAA*/"]
            expected        ["SELECT * FROM host WHERE id IN ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_7]
            replace_digits  [false]
            input           ["SELECT `host`.`address` FROM `host` WHERE org_id=42"]
            expected        ["SELECT host . address FROM host WHERE org_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_8]
            replace_digits  [false]
            input           [r#"SELECT "host"."address" FROM "host" WHERE org_id=42"#]
            expected        ["SELECT host . address FROM host WHERE org_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_9]
            replace_digits  [false]
            input           [r#"SELECT * FROM host WHERE id IN (42, 43) /*
multiline comment with parameters,
host:localhost,url:controller#home,id:FF005:00CAA
*/"#]
            expected        ["SELECT * FROM host WHERE id IN ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_10]
            replace_digits  [false]
            input           ["UPDATE user_dash_pref SET json_prefs = %(json_prefs)s, modified = '2015-08-27 22:10:32.492912' WHERE user_id = %(user_id)s AND url = %(url)s"]
            expected        ["UPDATE user_dash_pref SET json_prefs = ? modified = ? WHERE user_id = ? AND url = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_11]
            replace_digits  [false]
            input           ["SELECT DISTINCT host.id AS host_id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = %(org_id_1)s AND host.name NOT IN (%(name_1)s) AND host.name IN (%(name_2)s, %(name_3)s, %(name_4)s, %(name_5)s)"]
            expected        ["SELECT DISTINCT host.id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = ? AND host.name NOT IN ( ? ) AND host.name IN ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_12]
            replace_digits  [false]
            input           ["SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = %(org_id)s AND metric_key = ANY(array[75])"]
            expected        ["SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( array [ ? ] )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_13]
            replace_digits  [false]
            input           ["SELECT org_id, metric_key   FROM metrics_metadata   WHERE org_id = %(org_id)s AND metric_key = ANY(array[21, 25, 32])"]
            expected        ["SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( array [ ? ] )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_14]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1"]
            expected        ["SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_15]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1, 20"]
            expected        ["SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_16]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 15,20;"]
            expected        ["SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_17]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1;"]
            expected        ["SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_18]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE (articles.created_at BETWEEN '2016-10-31 23:00:00.000000' AND '2016-11-01 23:00:00.000000')"]
            expected        ["SELECT articles.* FROM articles WHERE ( articles.created_at BETWEEN ? AND ? )"];
        ]
        // postgres specific testcase, single dollar sign variables 
        // [
        //     test_name       [test_sql_obfuscation_quantization_19]
        //     replace_digits  [false]
        //     input           ["SELECT articles.* FROM articles WHERE (articles.created_at BETWEEN $1 AND $2)"]
        //     expected        ["SELECT articles.* FROM articles WHERE ( articles.created_at BETWEEN ? AND ? )"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_20]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE (articles.published != true)"]
            expected        ["SELECT articles.* FROM articles WHERE ( articles.published != ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_21]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE (title = 'guides.rubyonrails.org')"]
            expected        ["SELECT articles.* FROM articles WHERE ( title = ? )"];
        ]
        // postgres specific testcase, '?' only shows up in postgres
        // [
        //     test_name       [test_sql_obfuscation_quantization_22]
        //     replace_digits  [false]
        //     input           ["SELECT articles.* FROM articles WHERE ( title = ? ) AND ( author = ? )"]
        //     expected        ["SELECT articles.* FROM articles WHERE ( title = ? ) AND ( author = ? )"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_23]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE ( title = :title )"]
            expected        ["SELECT articles.* FROM articles WHERE ( title = :title )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_24]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE ( title = @title )"]
            expected        ["SELECT articles.* FROM articles WHERE ( title = @title )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_25]
            replace_digits  [false]
            input           ["SELECT date(created_at) as ordered_date, sum(price) as total_price FROM orders GROUP BY date(created_at) HAVING sum(price) > 100"]
            expected        ["SELECT date ( created_at ), sum ( price ) FROM orders GROUP BY date ( created_at ) HAVING sum ( price ) > ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_26]
            replace_digits  [false]
            input           ["SELECT * FROM articles WHERE id > 10 ORDER BY id asc LIMIT 20"]
            expected        ["SELECT * FROM articles WHERE id > ? ORDER BY id asc LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_27]
            replace_digits  [false]
            input           ["SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = 't'"]
            expected        ["SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_28]
            replace_digits  [false]
            input           ["SELECT articles.* FROM articles WHERE articles.id IN (1, 3, 5)"]
            expected        ["SELECT articles.* FROM articles WHERE articles.id IN ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_29]
            replace_digits  [false]
            input           ["SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO clients (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT"]
            expected        ["SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO clients ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_30]
            replace_digits  [false]
            input           ["SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 15, 25 BEGIN INSERT INTO clients (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT"]
            expected        ["SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO clients ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_31]
            replace_digits  [false]
            input           ["SAVEPOINT \"s139956586256192_x1\""]
            expected        ["SAVEPOINT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_32]
            replace_digits  [false]
            input           ["INSERT INTO user (id, username) VALUES ('Fred','Smith'), ('John','Smith'), ('Michael','Smith'), ('Robert','Smith');"]
            expected        ["INSERT INTO user ( id, username ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_33]
            replace_digits  [false]
            input           ["CREATE KEYSPACE Excelsior WITH replication = {'class': 'SimpleStrategy', 'replication_factor' : 3};"]
            expected        ["CREATE KEYSPACE Excelsior WITH replication = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_34]
            replace_digits  [false]
            input           [r#"SELECT "webcore_page"."id" FROM "webcore_page" WHERE "webcore_page"."slug" = %s ORDER BY "webcore_page"."path" ASC LIMIT 1"#]
            expected        ["SELECT webcore_page . id FROM webcore_page WHERE webcore_page . slug = ? ORDER BY webcore_page . path ASC LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_35]
            replace_digits  [false]
            input           ["SELECT server_table.host AS host_id FROM table#.host_tags as server_table WHERE server_table.host_id = 50"]
            expected        ["SELECT server_table.host FROM table#.host_tags WHERE server_table.host_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_36]
            replace_digits  [false]
            input           [r#"INSERT INTO delayed_jobs (attempts, created_at, failed_at, handler, last_error, locked_at, locked_by, priority, queue, run_at, updated_at) VALUES (0, '2016-12-04 17:09:59', NULL, '--- !ruby/object:Delayed::PerformableMethod\nobject: !ruby/object:Item\n  store:\n  - a simple string\n  - an \'escaped \' string\n  - another \'escaped\' string\n  - 42\n  string: a string with many \\\\\'escapes\\\\\'\nmethod_name: :show_store\nargs: []\n', NULL, NULL, NULL, 0, NULL, '2016-12-04 17:09:59', '2016-12-04 17:09:59')"#]
            expected        ["INSERT INTO delayed_jobs ( attempts, created_at, failed_at, handler, last_error, locked_at, locked_by, priority, queue, run_at, updated_at ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_37]
            replace_digits  [false]
            input           ["SELECT name, pretty_print(address) FROM people;"]
            expected        ["SELECT name, pretty_print ( address ) FROM people"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_38]
            replace_digits  [false]
            input           ["* SELECT * FROM fake_data(1, 2, 3);"]
            expected        ["* SELECT * FROM fake_data ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_39]
            replace_digits  [false]
            input           ["CREATE FUNCTION add(integer, integer) RETURNS integer\n AS 'select $1 + $2;'\n LANGUAGE SQL\n IMMUTABLE\n RETURNS NULL ON NULL INPUT;"]
            expected        ["CREATE FUNCTION add ( integer, integer ) RETURNS integer LANGUAGE SQL IMMUTABLE RETURNS ? ON ? INPUT"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_40]
            replace_digits  [false]
            input           ["SELECT * FROM public.table ( array [ ROW ( array [ 'magic', 'foo',"]
            expected        ["SELECT * FROM public.table ( array [ ROW ( array [ ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_41]
            replace_digits  [false]
            input           ["SELECT pg_try_advisory_lock (123) AS t46eef3f025cc27feb31ca5a2d668a09a"]
            expected        ["SELECT pg_try_advisory_lock ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_42]
            replace_digits  [false]
            input           ["INSERT INTO `qual-aa`.issues (alert0 , alert1) VALUES (NULL, NULL)"]
            expected        ["INSERT INTO qual-aa . issues ( alert0, alert1 ) VALUES ( ? )"];
        ]
        // postgres specific testcase, '?' only shows up in postgres
        // [
        //     test_name       [test_sql_obfuscation_quantization_43]
        //     replace_digits  [false]
        //     input           ["INSERT INTO user (id, email, name) VALUES (null, ?, ?)"]
        //     expected        ["INSERT INTO user ( id, email, name ) VALUES ( ? )"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_44]
            replace_digits  [false]
            input           ["select * from users where id = 214325346     # This comment continues to the end of line"]
            expected        ["select * from users where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_45]
            replace_digits  [false]
            input           ["select * from users where id = 214325346     -- This comment continues to the end of line"]
            expected        ["select * from users where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_46]
            replace_digits  [false]
            input           ["SELECT * FROM /* this is an in-line comment */ users;"]
            expected        ["SELECT * FROM users"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_47]
            replace_digits  [false]
            input           ["SELECT /*! STRAIGHT_JOIN */ col1 FROM table1"]
            expected        ["SELECT col1 FROM table1"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_48]
            replace_digits  [false]
            input           [r#"DELETE FROM t1
WHERE s11 > ANY
(SELECT COUNT(*) /* no hint */ FROM t2
WHERE NOT EXISTS
(SELECT * FROM t3
WHERE ROW(5*t2.s1,77)=
(SELECT 50,11*s1 FROM t4 UNION SELECT 50,77 FROM
(SELECT * FROM t5) AS t5)));"#]
            expected        ["DELETE FROM t1 WHERE s11 > ANY ( SELECT COUNT ( * ) FROM t2 WHERE NOT EXISTS ( SELECT * FROM t3 WHERE ROW ( ? * t2.s1, ? ) = ( SELECT ? * s1 FROM t4 UNION SELECT ? FROM ( SELECT * FROM t5 ) ) ) )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_49]
            replace_digits  [false]
            input           ["SET @g = 'POLYGON((0 0,10 0,10 10,0 10,0 0),(5 5,7 5,7 7,5 7, 5 5))';"]
            expected        ["SET @g = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_50]
            replace_digits  [false]
            input           [r#"SELECT daily_values.*,
LEAST((5040000 - @runtot), value) AS value, (@runtot := @runtot + daily_values.value) AS total FROM (SELECT @runtot:=0) AS n, `daily_values`  WHERE `daily_values`.`subject_id` = 12345 AND `daily_values`.`subject_type` = 'Skippity' AND (daily_values.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date"#]
            expected        [r#"SELECT daily_values.*, LEAST ( ( ? - @runtot ), value ), ( @runtot := @runtot + daily_values.value ) FROM ( SELECT @runtot := ? ), daily_values WHERE daily_values . subject_id = ? AND daily_values . subject_type = ? AND ( daily_values.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_51]
            replace_digits  [false]
            input           [r#"    SELECT
t1.userid,
t1.fullname,
t1.firm_id,
t2.firmname,
t1.email,
t1.location,
t1.state,
t1.phone,
t1.url,
DATE_FORMAT( t1.lastmod, "%m/%d/%Y %h:%i:%s" ) AS lastmod,
t1.lastmod AS lastmod_raw,
t1.user_status,
t1.pw_expire,
DATE_FORMAT( t1.pw_expire, "%m/%d/%Y" ) AS pw_expire_date,
t1.addr1,
t1.addr2,
t1.zipcode,
t1.office_id,
t1.default_group,
t3.firm_status,
t1.title
FROM
	userdata      AS t1
LEFT JOIN lawfirm_names AS t2 ON t1.firm_id = t2.firm_id
LEFT JOIN lawfirms      AS t3 ON t1.firm_id = t3.firm_id
WHERE
t1.userid = 'jstein'

"#]
            expected        [r#"SELECT t1.userid, t1.fullname, t1.firm_id, t2.firmname, t1.email, t1.location, t1.state, t1.phone, t1.url, DATE_FORMAT ( t1.lastmod, %m/%d/%Y %h:%i:%s ), t1.lastmod, t1.user_status, t1.pw_expire, DATE_FORMAT ( t1.pw_expire, %m/%d/%Y ), t1.addr1, t1.addr2, t1.zipcode, t1.office_id, t1.default_group, t3.firm_status, t1.title FROM userdata LEFT JOIN lawfirm_names ON t1.firm_id = t2.firm_id LEFT JOIN lawfirms ON t1.firm_id = t3.firm_id WHERE t1.userid = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_52]
            replace_digits  [false]
            input           [r#"SELECT [b].[BlogId], [b].[Name]
FROM [Blogs] AS [b]
ORDER BY [b].[Name]"#]
            expected        [r#"SELECT [ b ] . [ BlogId ], [ b ] . [ Name ] FROM [ Blogs ] ORDER BY [ b ] . [ Name ]"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_53]
            replace_digits  [false]
            input           [r#"SELECT * FROM users WHERE firstname=''"#]
            expected        [r#"SELECT * FROM users WHERE firstname = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_54]
            replace_digits  [false]
            input           [r#"SELECT * FROM users WHERE firstname=' '"#]
            expected        [r#"SELECT * FROM users WHERE firstname = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_55]
            replace_digits  [false]
            input           [r#"SELECT * FROM users WHERE firstname="""#]
            expected        [r#"SELECT * FROM users WHERE firstname = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_56]
            replace_digits  [false]
            input           [r#"SELECT * FROM users WHERE lastname=" ""#]
            expected        [r#"SELECT * FROM users WHERE lastname = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_57]
            replace_digits  [false]
            input           [r#"SELECT * FROM users WHERE lastname="	 ""#]
            expected        [r#"SELECT * FROM users WHERE lastname = ?"#];
        ]
        // postgres specific testcase, '?' only shows up in postgres
        // [
        //     test_name       [test_sql_obfuscation_quantization_58]
        //     replace_digits  [false]
        //     input           [r#"SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id = ? AND visitor_id IS ? UNION SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id IS ? AND visitor_id = "AA0DKTGEM6LRN3WWPZ01Q61E3J7ROX7O" ORDER BY customer_id DESC"#]
        //     expected        ["SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id = ? AND visitor_id IS ? UNION SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id IS ? AND visitor_id = ? ORDER BY customer_id DESC"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_59]
            replace_digits  [false]
            input           [r#"update Orders set created = "2019-05-24 00:26:17", gross = 30.28, payment_type = "eventbrite", mg_fee = "3.28", fee_collected = "3.28", event = 59366262, status = "10", survey_type = 'direct', tx_time_limit = 480, invite = "", ip_address = "69.215.148.82", currency = 'USD', gross_USD = "30.28", tax_USD = 0.00, journal_activity_id = 4044659812798558774, eb_tax = 0.00, eb_tax_USD = 0.00, cart_uuid = "92f8", changed = '2019-05-24 00:26:17' where id = ?"#]
	        expected        ["update Orders set created = ? gross = ? payment_type = ? mg_fee = ? fee_collected = ? event = ? status = ? survey_type = ? tx_time_limit = ? invite = ? ip_address = ? currency = ? gross_USD = ? tax_USD = ? journal_activity_id = ? eb_tax = ? eb_tax_USD = ? cart_uuid = ? changed = ? where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_60]
            replace_digits  [false]
            input           [r#"update Attendees set email = '626837270@qq.com', first_name = "贺新春送猪福加企鹅1054948000领98綵斤", last_name = '王子198442com体验猪多优惠', journal_activity_id = 4246684839261125564, changed = "2019-05-24 00:26:22" where id = 123"#]
	        expected        ["update Attendees set email = ? first_name = ? last_name = ? journal_activity_id = ? changed = ? where id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_61]
            replace_digits  [false]
            input           ["SELECT\r\n\t                CodiFormacio\r\n\t                ,DataInici\r\n\t                ,DataFi\r\n\t                ,Tipo\r\n\t                ,CodiTecnicFormador\r\n\t                ,p.nombre AS TutorNombre\r\n\t                ,p.mail AS TutorMail\r\n\t                ,Sessions.Direccio\r\n\t                ,Sessions.NomEmpresa\r\n\t                ,Sessions.Telefon\r\n                FROM\r\n                ----------------------------\r\n                (SELECT\r\n\t                CodiFormacio\r\n\t                ,case\r\n\t                   when ModalitatSessio = '1' then 'Presencial'--Teoria\r\n\t                   when ModalitatSessio = '2' then 'Presencial'--Practica\r\n\t                   when ModalitatSessio = '3' then 'Online'--Tutoria\r\n                       when ModalitatSessio = '4' then 'Presencial'--Examen\r\n\t                   ELSE 'Presencial'\r\n\t                end as Tipo\r\n\t                ,ModalitatSessio\r\n\t                ,DataInici\r\n\t                ,DataFi\r\n                     ,NomEmpresa\r\n\t                ,Telefon\r\n\t                ,CodiTecnicFormador\r\n\t                ,CASE\r\n\t                   WHEn EsAltres = 1 then FormacioLlocImparticioDescripcio\r\n\t                   else Adreca + ' - ' + CodiPostal + ' ' + Poblacio\r\n\t                end as Direccio\r\n\t\r\n                FROM Consultas.dbo.View_AsActiva__FormacioSessions_InfoLlocImparticio) AS Sessions\r\n                ----------------------------------------\r\n                LEFT JOIN Consultas.dbo.View_AsActiva_Operari AS o\r\n\t                ON o.CodiOperari = Sessions.CodiTecnicFormador\r\n                LEFT JOIN MainAPP.dbo.persona AS p\r\n\t                ON 'preven\\' + o.codioperari = p.codi\r\n                WHERE Sessions.CodiFormacio = 'F00000017898'"]
	        expected        [r#"SELECT CodiFormacio, DataInici, DataFi, Tipo, CodiTecnicFormador, p.nombre, p.mail, Sessions.Direccio, Sessions.NomEmpresa, Sessions.Telefon FROM ( SELECT CodiFormacio, case when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? ELSE ? end, ModalitatSessio, DataInici, DataFi, NomEmpresa, Telefon, CodiTecnicFormador, CASE WHEn EsAltres = ? then FormacioLlocImparticioDescripcio else Adreca + ? + CodiPostal + ? + Poblacio end FROM Consultas.dbo.View_AsActiva__FormacioSessions_InfoLlocImparticio ) LEFT JOIN Consultas.dbo.View_AsActiva_Operari ON o.CodiOperari = Sessions.CodiTecnicFormador LEFT JOIN MainAPP.dbo.persona ON ? + o.codioperari = p.codi WHERE Sessions.CodiFormacio = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_62]
            replace_digits  [false]
            input           [r#"SELECT * FROM foo LEFT JOIN bar ON 'backslash\' = foo.b WHERE foo.name = 'String'"#]
            expected        ["SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_63]
            replace_digits  [false]
            input           [r#"SELECT * FROM foo LEFT JOIN bar ON 'backslash\' = foo.b LEFT JOIN bar2 ON 'backslash2\' = foo.b2 WHERE foo.name = 'String'"#]
            expected        ["SELECT * FROM foo LEFT JOIN bar ON ? = foo.b LEFT JOIN bar2 ON ? = foo.b2 WHERE foo.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_64]
            replace_digits  [false]
            input           ["SELECT * FROM foo LEFT JOIN bar ON 'embedded ''quote'' in string' = foo.b WHERE foo.name = 'String'"]
            expected        ["SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_65]
            replace_digits  [false]
            input           [r#"SELECT * FROM foo LEFT JOIN bar ON 'embedded \'quote\' in string' = foo.b WHERE foo.name = 'String'"#]
            expected        ["SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"];
        ]
        // postgres specific testcase, '?' only shows up in postgres
        // [
        //     test_name       [test_sql_obfuscation_quantization_66]
        //     replace_digits  [false]
        //     input           ["SELECT org_id,metric_key,metric_type,interval FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY(ARRAY[?,?,?,?,?])"]
        //     expected        ["SELECT org_id, metric_key, metric_type, interval FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( ARRAY [ ? ] )"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_67]
            replace_digits  [false]
            input           [r#"SELECT wp_woocommerce_order_items.order_id As No_Commande
FROM  wp_woocommerce_order_items
LEFT JOIN
    (
        SELECT meta_value As Prenom
        FROM wp_postmeta
        WHERE meta_key = '_shipping_first_name'
    ) AS a
ON wp_woocommerce_order_items.order_id = a.post_id
WHERE  wp_woocommerce_order_items.order_id =2198"#]
            expected        ["SELECT wp_woocommerce_order_items.order_id FROM wp_woocommerce_order_items LEFT JOIN ( SELECT meta_value FROM wp_postmeta WHERE meta_key = ? ) ON wp_woocommerce_order_items.order_id = a.post_id WHERE wp_woocommerce_order_items.order_id = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_68]
            replace_digits  [false]
            input           ["SELECT a :: VARCHAR(255) FROM foo WHERE foo.name = 'String'"]
	        expected        ["SELECT a :: VARCHAR ( ? ) FROM foo WHERE foo.name = ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_69]
            replace_digits  [false]
            input           ["SELECT MIN(`scoped_49a`.`scoped_872`) AS `MIN_BECR_DATE_CREATED` FROM (SELECT `49a`.`submittedOn` AS `scoped_872`, `49a`.`domain` AS `scoped_847e4dcfa1c54d72aad6dbeb231c46de`, `49a`.`eventConsumer` AS `scoped_7b2f7b8da15646d1b75aa03901460eb2`, `49a`.`eventType` AS `scoped_77a1b9308b384a9391b69d24335ba058` FROM (`SorDesignTime`.`businessEventConsumerRegistry_947a74dad4b64be9847d67f466d26f5e` AS `49a`) WHERE (`49a`.`systemData.ClientID`) = ('35c1ccc0-a83c-4812-a189-895e9d4dd223')) AS `scoped_49a` WHERE ((`scoped_49a`.`scoped_847e4dcfa1c54d72aad6dbeb231c46de`) = ('Benefits') AND ((`scoped_49a`.`scoped_7b2f7b8da15646d1b75aa03901460eb2`) = ('benefits') AND (`scoped_49a`.`scoped_77a1b9308b384a9391b69d24335ba058`) = ('DMXSync'))); "]
            expected        ["SELECT MIN ( scoped_49a . scoped_872 ) FROM ( SELECT 49a . submittedOn, 49a . domain, 49a . eventConsumer, 49a . eventType FROM ( SorDesignTime . businessEventConsumerRegistry_947a74dad4b64be9847d67f466d26f5e ) WHERE ( 49a . systemData.ClientID ) = ( ? ) ) WHERE ( ( scoped_49a . scoped_847e4dcfa1c54d72aad6dbeb231c46de ) = ( ? ) AND ( ( scoped_49a . scoped_7b2f7b8da15646d1b75aa03901460eb2 ) = ( ? ) AND ( scoped_49a . scoped_77a1b9308b384a9391b69d24335ba058 ) = ( ? ) ) )"];
        ]
        // postgres specific testcase, '?' only shows up in postgres
        // [
        //     test_name       [test_sql_obfuscation_quantization_70]
        //     replace_digits  [false]
        //     input           ["{call px_cu_se_security_pg.sps_get_my_accounts_count(?, ?, ?, ?)}"]
        //     expected        ["{ call px_cu_se_security_pg.sps_get_my_accounts_count ( ? ) }"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_71]
            replace_digits  [false]
            input           [r#"{call px_cu_se_security_pg.sps_get_my_accounts_count(1, 2, 'one', 'two')};"#]
            expected        ["{ call px_cu_se_security_pg.sps_get_my_accounts_count ( ? ) }"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_72]
            replace_digits  [false]
            input           [r#"{call curly_fun('{{', '}}', '}', '}')};"#]
            expected        ["{ call curly_fun ( ? ) }"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_73]
            replace_digits  [false]
            input           ["SELECT id, name FROM emp WHERE name LIKE {fn UCASE('Smith')}"]
	        expected        ["SELECT id, name FROM emp WHERE name LIKE ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_74]
            replace_digits  [false]
            input           ["select users.custom #- '{a,b}' from users"]
            expected        ["select users.custom"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_75]
            replace_digits  [false]
            input           ["select users.custom #> '{a,b}' from users"]
            expected        ["select users.custom"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_76]
            replace_digits  [false]
            input           ["select users.custom #>> '{a,b}' from users"]
            expected        ["select users.custom"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_77]
            replace_digits  [false]
            input           ["SELECT a FROM foo WHERE value<@name"]
	        expected        ["SELECT a FROM foo WHERE value < @name"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_78]
            replace_digits  [false]
            input           ["SELECT @@foo"]
            expected        ["SELECT @@foo"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_79]
            replace_digits  [false]
            input           [r#"DROP TABLE IF EXISTS django_site;
DROP TABLE IF EXISTS knowledgebase_article;

CREATE TABLE django_site (
id integer PRIMARY KEY,
domain character varying(100) NOT NULL,
name character varying(50) NOT NULL,
uuid uuid NOT NULL,
disabled boolean DEFAULT false NOT NULL
);

CREATE TABLE knowledgebase_article (
id integer PRIMARY KEY,
title character varying(255) NOT NULL,
site_id integer NOT NULL,
CONSTRAINT knowledgebase_article_site_id_fkey FOREIGN KEY (site_id) REFERENCES django_site(id)
);

INSERT INTO django_site(id, domain, name, uuid, disabled) VALUES (1, 'foo.domain', 'Foo', 'cb4776c1-edf3-4041-96a8-e152f5ae0f91', false);
INSERT INTO knowledgebase_article(id, title, site_id) VALUES(1, 'title', 1);"#]
            expected        ["DROP TABLE IF EXISTS django_site DROP TABLE IF EXISTS knowledgebase_article CREATE TABLE django_site ( id integer PRIMARY KEY, domain character varying ( ? ) NOT ? name character varying ( ? ) NOT ? uuid uuid NOT ? disabled boolean DEFAULT ? NOT ? ) CREATE TABLE knowledgebase_article ( id integer PRIMARY KEY, title character varying ( ? ) NOT ? site_id integer NOT ? CONSTRAINT knowledgebase_article_site_id_fkey FOREIGN KEY ( site_id ) REFERENCES django_site ( id ) ) INSERT INTO django_site ( id, domain, name, uuid, disabled ) VALUES ( ? ) INSERT INTO knowledgebase_article ( id, title, site_id ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_80]
            replace_digits  [false]
            input           [r#"
SELECT set_config('foo.bar', (SELECT foo.bar FROM sometable WHERE sometable.uuid = %(some_id)s)::text, FALSE);
SELECT
othertable.id,
othertable.title
FROM othertable
INNER JOIN sometable ON sometable.id = othertable.site_id
WHERE
sometable.uuid = %(some_id)s
LIMIT 1
;"#]
            expected        ["SELECT set_config ( ? ( SELECT foo.bar FROM sometable WHERE sometable.uuid = ? ) :: text, ? ) SELECT othertable.id, othertable.title FROM othertable INNER JOIN sometable ON sometable.id = othertable.site_id WHERE sometable.uuid = ? LIMIT ?"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_81]
            replace_digits  [false]
            input           [r#"CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert(OUT created boolean, OUT primary_key text) AS $func$ BEGIN INSERT INTO "school" ("id","organization_id","name","created_at","updated_at") VALUES ('dc4e9444-d7c9-40a9-bcef-68e4cc594e61','ec647f56-f27a-49a1-84af-021ad0a19f21','Test','2021-03-31 16:30:43.915 +00:00','2021-03-31 16:30:43.915 +00:00'); created := true; EXCEPTION WHEN unique_violation THEN UPDATE "school" SET "id"='dc4e9444-d7c9-40a9-bcef-68e4cc594e61',"organization_id"='ec647f56-f27a-49a1-84af-021ad0a19f21',"name"='Test',"updated_at"='2021-03-31 16:30:43.915 +00:00' WHERE ("id" = 'dc4e9444-d7c9-40a9-bcef-68e4cc594e61'); created := false; END; $func$ LANGUAGE plpgsql; SELECT * FROM pg_temp.sequelize_upsert();"#]
	        expected        ["CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert ( OUT created boolean, OUT primary_key text ) LANGUAGE plpgsql SELECT * FROM pg_temp.sequelize_upsert ( )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_82]
            replace_digits  [false]
            input           [r#"INSERT INTO table (field1, field2) VALUES (1, $$someone's string123$with other things$$)"#]
	        expected        ["INSERT INTO table ( field1, field2 ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_83]
            replace_digits  [false]
            input           ["INSERT INTO table (field1) VALUES ($some tag$this text confuses$some other text$some ta not quite$some tag$)"]
	        expected        ["INSERT INTO table ( field1 ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_84]
            replace_digits  [false]
            input           [r#"INSERT INTO table (field1) VALUES ($tag$random \wqejks "sadads' text$tag$)"#]
	        expected        ["INSERT INTO table ( field1 ) VALUES ( ? )"];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_85]
            replace_digits  [false]
            input           [r#"SELECT nspname FROM pg_class where nspname !~ '.*toIgnore.*'"#]
	        expected        [r#"SELECT nspname FROM pg_class where nspname !~ ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_86]
            replace_digits  [false]
            input           [r#"SELECT nspname FROM pg_class where nspname !~* '.*toIgnoreInsensitive.*'"#]
	        expected        [r#"SELECT nspname FROM pg_class where nspname !~* ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_87]
            replace_digits  [false]
            input           [r#"SELECT nspname FROM pg_class where nspname ~ '.*matching.*'"#]
	        expected        [r#"SELECT nspname FROM pg_class where nspname ~ ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_88]
            replace_digits  [false]
            input           [r#"SELECT nspname FROM pg_class where nspname ~* '.*matchingInsensitive.*'"#]
	        expected        [r#"SELECT nspname FROM pg_class where nspname ~* ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_89]
            replace_digits  [false]
            input           [r#"SELECT * FROM dbo.Items WHERE id = 1 or /*!obfuscation*/ 1 = 1"#]
	        expected        [r#"SELECT * FROM dbo.Items WHERE id = ? or ? = ?"#];
        ]
        [
            test_name       [test_sql_obfuscation_quantization_90]
            replace_digits  [false]
            input           [r#"SELECT * FROM Items WHERE id = -1 OR id = -01 OR id = -108 OR id = -.018 OR id = -.08 OR id = -908129"#]
	        expected        [r#"SELECT * FROM Items WHERE id = ? OR id = ? OR id = ? OR id = ? OR id = ? OR id = ?"#];
        ]
        // postgres specific testcase, single dollar sign variables
        // [
        //     test_name       [test_sql_obfuscation_quantization_91]
        //     replace_digits  [false]
        //     input           ["USING $09 SELECT"]
	    //     expected        ["USING ? SELECT"];
        // ]
        [
            test_name       [test_sql_obfuscation_quantization_92]
            replace_digits  [false]
            input           ["USING - SELECT"]
	        expected        ["USING - SELECT"];
		]
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_sql_string(input, replace_digits);
        // assert!(result.is_ok());
        assert_eq!(result, expected);
    }
}
