// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn is_splitter(b: u8) -> bool {
    matches!(
        b,
        b',' | b'(' | b')' | b'|' | b' ' | b'\t' | b'\n' | b'\r' |
        0xB | // vertical tab 
        0xC // form feed
    )
}

fn is_numeric_litteral_prefix(bytes: &[u8], start: usize) -> bool {
    matches!(
        bytes[start],
        b'0' | b'1' | b'2' | b'3' | b'4' | b'5' | b'6' | b'7' | b'8' | b'9' | b'-' | b'+' | b'.'
    ) && !(start + 1 < bytes.len() && bytes[start] == b'-' && bytes[start + 1] == b'-')
}

fn is_hex_litteral_prefix(bytes: &[u8], start: usize, end: usize) -> bool {
    (bytes[start] | b' ') == b'x' && start + 1 < end && bytes[start + 1] == b'\''
}

fn is_quoted(bytes: &[u8], start: usize, end: usize) -> bool {
    bytes[start] == b'\'' && bytes[end - 1] == b'\''
}

/// Obfuscates an sql string by replacing litterals with '?' chars.
///
/// The algorithm works by finding the places where a litteral could start (so called splitters)
/// and then identifies them by looking at their first few characters.
///
/// It does not attempt at rigorous parsing of the SQL syntax, and does not take any context
/// sensitive decision, contrary to the more exhaustive datadog-agent implementation.
///
/// based off
/// <https://github.com/DataDog/dd-trace-java/blob/36e924eaa/internal-api/src/main/java/datadog/trace/api/normalize/SQLNormalizer.java>
pub fn obfuscate_sql_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut obfuscated = String::new();
    if s.is_empty() {
        return obfuscated;
    }
    let mut start = 0;
    loop {
        if start >= s.len() {
            break;
        }
        let end = next_splitter(bytes, start).unwrap_or(s.len());
        #[allow(clippy::comparison_chain)]
        if start + 1 == end {
            // if the gap is 1 character it can only be a number
            if bytes[start].is_ascii_digit() {
                obfuscated.push('?');
            } else {
                obfuscated.push_str(&s[start..end]);
            }
        } else if start + 1 < end {
            if is_numeric_litteral_prefix(bytes, start)
                || is_quoted(bytes, start, end)
                || is_hex_litteral_prefix(bytes, start, end)
            {
                obfuscated.push('?');
            } else {
                obfuscated.push_str(&s[start..end]);
            }
        }
        if end < s.len() {
            obfuscated.push(bytes[end] as char);
        }
        start = end + 1;
    }
    obfuscated
}

fn next_splitter(s: &[u8], at: usize) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (pos, b) in s.iter().copied().enumerate().skip(at) {
        if b == b'\'' && !escaped {
            quoted = !quoted;
            continue;
        }
        escaped = (b == b'\\') && !escaped;
        if !quoted && is_splitter(b) {
            return Some(pos);
        }
    }
    None
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_sql_obfuscation() {
        let mut panic = None;
        let err = CASES
            .iter()
            .enumerate()
            .filter_map(|(i, (input, output))| {
                let err =
                    match std::panic::catch_unwind(|| test_sql_obfuscation_case(input, output)) {
                        Ok(r) => r,
                        Err(p) => {
                            panic = Some(p);
                            eprintln!("panicked case {i}\n\tinput: {input}\n\n");
                            return None;
                        }
                    }
                    .err()?;
                Some(format!("failed case {i}\n\terr: {err}\n"))
            })
            .collect::<String>();
        if !err.is_empty() {
            if panic.is_none() {
                panic!("{err}")
            } else {
                eprintln!("{err}")
            }
        }
        if let Some(p) = panic {
            std::panic::resume_unwind(p);
        }
    }

    fn test_sql_obfuscation_case(input: &str, output: &str) -> anyhow::Result<()> {
        let got = super::obfuscate_sql_string(input);
        if output != got {
            anyhow::bail!("expected {output}\n\tgot: {got}")
        }
        Ok(())
    }

    const CASES: &[(&str, &str)] = &[
        ("" , ""),
        ("   " , "   "),
        ("         " , "         "),
        ("罿" , "罿"),
        ("罿潯" , "罿潯"),
        ("罿潯罿潯罿潯罿潯罿潯" , "罿潯罿潯罿潯罿潯罿潯"),
        ("'abc1287681964'", "?"),
        ("-- comment", "-- comment"),
        ("---", "---"),
        ("1 - 2", "? - ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc1287681964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc\\'1287681964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = '\\'abc1287681964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc1287681964\\''" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = '\\'abc1287681964\\''" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc\\'1287681\\'964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc\\'1287\\'681\\'964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId = 'abc\\'1287\\'681\\'\\'\\'\\'964'" , "SELECT * FROM TABLE WHERE userId = ?"),
        ("SELECT * FROM TABLE WHERE userId IN (\'a\', \'b\', \'c\')" , "SELECT * FROM TABLE WHERE userId IN (?, ?, ?)"),
        ("SELECT * FROM TABLE WHERE userId IN (\'abc\\'1287681\\'964\', \'abc\\'1287\\'681\\'\\'\\'\\'964\', \'abc\\'1287\\'681\\'964\')" , "SELECT * FROM TABLE WHERE userId IN (?, ?, ?)"),
        ("SELECT * FROM TABLE WHERE userId = 'abc1287681964' ORDER BY FOO DESC" , "SELECT * FROM TABLE WHERE userId = ? ORDER BY FOO DESC"),
        ("SELECT * FROM TABLE WHERE userId = 'abc\\'1287\\'681\\'\\'\\'\\'964' ORDER BY FOO DESC" , "SELECT * FROM TABLE WHERE userId = ? ORDER BY FOO DESC"),
        ("SELECT * FROM TABLE JOIN SOMETHING ON TABLE.foo = SOMETHING.bar" , "SELECT * FROM TABLE JOIN SOMETHING ON TABLE.foo = SOMETHING.bar"),
        ("CREATE TABLE \"VALUE\"" , "CREATE TABLE \"VALUE\""),
        ("INSERT INTO \"VALUE\" (\"column\") VALUES (\'ljahklshdlKASH\')" , "INSERT INTO \"VALUE\" (\"column\") VALUES (?)"),
        ("INSERT INTO \"VALUE\" (\"col1\",\"col2\",\"col3\") VALUES (\'blah\',12983,X'ff')" , "INSERT INTO \"VALUE\" (\"col1\",\"col2\",\"col3\") VALUES (?,?,?)"),
        ("INSERT INTO \"VALUE\" (\"col1\", \"col2\", \"col3\") VALUES (\'blah\',12983,X'ff')" , "INSERT INTO \"VALUE\" (\"col1\", \"col2\", \"col3\") VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES (\'blah\',12983,X'ff')" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES (12983,X'ff',\'blah\')" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'blah\',12983)" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES ('a',\'b\',1)" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1, col2, col3) VALUES ('a',\'b\',1)" , "INSERT INTO VALUE (col1, col2, col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE ( col1, col2, col3 ) VALUES ('a',\'b\',1)" , "INSERT INTO VALUE ( col1, col2, col3 ) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES ('a', \'b\' ,1)" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?, ? ,?)"),
        ("INSERT INTO VALUE (col1, col2, col3) VALUES ('a', \'b\', 1)" , "INSERT INTO VALUE (col1, col2, col3) VALUES (?, ?, ?)"),
        ("INSERT INTO VALUE ( col1, col2, col3 ) VALUES ('a', \'b\', 1)" , "INSERT INTO VALUE ( col1, col2, col3 ) VALUES (?, ?, ?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'罿潯罿潯罿潯罿潯罿潯\',12983)" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'罿\',12983)" , "INSERT INTO VALUE (col1,col2,col3) VALUES (?,?,?)"),
        ("SELECT 3 AS NUCLEUS_TYPE,A0.ID,A0.\"NAME\" FROM \"VALUE\" A0" , "SELECT ? AS NUCLEUS_TYPE,A0.ID,A0.\"NAME\" FROM \"VALUE\" A0"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > .9999" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > 0.9999" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -0.9999" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -1e6" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +1e6" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +255" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +6.34F" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +6f" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +0.5D" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -1d" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > x'ff'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > X'ff'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > 0xff" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'\'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'  \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' \\\' \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' \\\'Бегите, глупцы \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' x \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' x x\'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' x\\\'ab x\'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' \\\' 0xf \'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'5,123\'" , "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?"),
        ("CREATE TABLE S_H2 (id INTEGER not NULL, PRIMARY KEY ( id ))" , "CREATE TABLE S_H2 (id INTEGER not NULL, PRIMARY KEY ( id ))"),
        ("CREATE TABLE S_H2 ( id INTEGER not NULL, PRIMARY KEY ( id ) )" , "CREATE TABLE S_H2 ( id INTEGER not NULL, PRIMARY KEY ( id ) )"),
        ("SELECT * FROM TABLE WHERE name = 'O''Brady'" , "SELECT * FROM TABLE WHERE name = ?"),
        ("INSERT INTO visits VALUES (2, 8, '2013-01-02', 'rabies shot')" , "INSERT INTO visits VALUES (?, ?, ?, ?)"),
        (
            "SELECT
\tcountry.country_name_eng,
\tSUM(CASE WHEN call.id IS NOT NULL THEN 1 ELSE 0 END) AS calls,
\tAVG(ISNULL(DATEDIFF(SECOND, call.start_time, call.end_time),0)) AS avg_difference
FROM country
LEFT JOIN city ON city.country_id = country.id
LEFT JOIN customer ON city.id = customer.city_id
LEFT JOIN call ON call.customer_id = customer.id
GROUP BY
\tcountry.id,
\tcountry.country_name_eng
HAVING AVG(ISNULL(DATEDIFF(SECOND, call.start_time, call.end_time),0)) > (SELECT AVG(DATEDIFF(SECOND, call.start_time, call.end_time)) FROM call)
ORDER BY calls DESC, country.id ASC;",
                "SELECT
\tcountry.country_name_eng,
\tSUM(CASE WHEN call.id IS NOT NULL THEN ? ELSE ? END) AS calls,
\tAVG(ISNULL(DATEDIFF(SECOND, call.start_time, call.end_time),?)) AS avg_difference
FROM country
LEFT JOIN city ON city.country_id = country.id
LEFT JOIN customer ON city.id = customer.city_id
LEFT JOIN call ON call.customer_id = customer.id
GROUP BY
\tcountry.id,
\tcountry.country_name_eng
HAVING AVG(ISNULL(DATEDIFF(SECOND, call.start_time, call.end_time),?)) > (SELECT AVG(DATEDIFF(SECOND, call.start_time, call.end_time)) FROM call)
ORDER BY calls DESC, country.id ASC;"
        ),
        ("DROP VIEW IF EXISTS v_country_all; GO CREATE VIEW v_country_all AS SELECT * FROM country;", "DROP VIEW IF EXISTS v_country_all; GO CREATE VIEW v_country_all AS SELECT * FROM country;"),
        (
            "UPDATE v_country_all /* 1. in-line comment */ SET
/*
    * 2. multi-line comment
    */
country_name = 'Nova1'
-- 3. single-line comment
WHERE id = 8;",
                "UPDATE v_country_all /* ? in-line comment */ SET
/*
    * ? multi-line comment
    */
country_name = ?
-- ? single-line comment
WHERE id = ?"
        ),
        (
            "INSERT INTO country (country_name, country_name_eng, country_code) VALUES ('Deutschland', 'Germany', 'DEU');
INSERT INTO country (country_name, country_name_eng, country_code) VALUES ('Srbija', 'Serbia', 'SRB');
INSERT INTO country (country_name, country_name_eng, country_code) VALUES ('Hrvatska', 'Croatia', 'HRV');
INSERT INTO country (country_name, country_name_eng, country_code) VALUES ('United States of America', 'United States of America', 'USA');
INSERT INTO country (country_name, country_name_eng, country_code) VALUES ('Polska', 'Poland', 'POL');",
                "INSERT INTO country (country_name, country_name_eng, country_code) VALUES (?, ?, ?);
INSERT INTO country (country_name, country_name_eng, country_code) VALUES (?, ?, ?);
INSERT INTO country (country_name, country_name_eng, country_code) VALUES (?, ?, ?);
INSERT INTO country (country_name, country_name_eng, country_code) VALUES (?, ?, ?);
INSERT INTO country (country_name, country_name_eng, country_code) VALUES (?, ?, ?);"
        ),
        ("SELECT * FROM TABLE WHERE userId = ',' and foo=foo.bar", "SELECT * FROM TABLE WHERE userId = ? and foo=foo.bar"),
        ("SELECT * FROM TABLE WHERE userId =     ','||foo.bar", "SELECT * FROM TABLE WHERE userId =     ?||foo.bar"),
        (
            concat!(
            "SELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||', '|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,'YYYYMMDD'),'DD-MON-YYYY'),'DD-MON-YYYY') AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,'YYYYMMDD'),'DD-MON-YYYY'),'DD-MON-YYYY') AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 150 THEN '>150 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 120 THEN '121 to 150 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 90 THEN '91 to 120 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 60 THEN '61 to 90 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 30 THEN '31 to 60 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 0 THEN '1 to 30 Days' ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD')),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||', '||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||', '||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,'YYYYMMDD'),'DD-MON-YYYY') AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||', '||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,'YYYYMMDD'),'DD-MON-YYYY') AS \"c25\", DECODE(bar.c , 1,'N',0, 'Y', bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:protocols)) WHERE RNK = 1) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:protocols) and bar.x IN (:sites)) ) ",
            "SELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||', '|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,'YYYYMMDD'),'DD-MON-YYYY'),'DD-MON-YYYY') AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,'YYYYMMDD'),'DD-MON-YYYY'),'DD-MON-YYYY') AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 150 THEN '>150 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 120 THEN '121 to 150 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 90 THEN '91 to 120 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 60 THEN '61 to 90 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 30 THEN '31 to 60 Days' WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD'))) > 0 THEN '1 to 30 Days' ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,'YYYYMMDD')),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||', '||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||', '||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,'YYYYMMDD'),'DD-MON-YYYY') AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||', '||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,'YYYYMMDD'),'DD-MON-YYYY') AS \"c25\", DECODE(bar.c , 1,'N',0, 'Y', bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:protocols)) WHERE RNK = 1) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:protocols) and bar.x IN (:sites)) )"),
            concat!(
                "SELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||?|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,?),?),?) AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?)),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||?||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||?||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,?),?) AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||?||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,?),?) AS \"c25\", DECODE(bar.c , ?,?,?, ?, bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:protocols)) WHERE RNK = ?) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:protocols) and bar.x IN (:sites)) ) ",
                "SELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||?|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,?),?),?) AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?)),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||?||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||?||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,?),?) AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||?||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,?),?) AS \"c25\", DECODE(bar.c , ?,?,?, ?, bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:protocols)) WHERE RNK = ?) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:protocols) and bar.x IN (:sites)) )"
            )
        ),
    ];
}
