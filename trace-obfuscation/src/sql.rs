// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use crate::sql_tokenizer::{SqlTokenizer, SqlTokenizerScanResult, TokenKind};

const QUESTION_MARK: char = '?';
const QUESTION_MARK_STR: &str = "?";

pub fn obfuscate_sql_string(s: &str, replace_digits: bool) -> String {
    let use_literal_escapes = true;
    let tokenizer = SqlTokenizer::new(s, use_literal_escapes);
    attempt_sql_obfuscation(tokenizer, replace_digits).unwrap_or(QUESTION_MARK_STR.to_string())
}

fn attempt_sql_obfuscation(mut tokenizer: SqlTokenizer, replace_digits: bool) -> anyhow::Result<String> {
    // TODO: Support replace digits in specific tables
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
        result = replace(result, replace_digits, last_token.as_str(), &last_token_kind)?;

        result = grouping_filter.grouping(result, last_token.as_str(), &last_token_kind)?;
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

    use crate::sql_tokenizer::SqlTokenizer;

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
    )]
    #[test]
    fn test_name() {
        let tokenizer = SqlTokenizer::new(input, false);
        let result = attempt_sql_obfuscation(tokenizer, replace_digits);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected);
    }
}
