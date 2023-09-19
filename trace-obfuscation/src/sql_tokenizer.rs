// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::str::FromStr;

const ESCAPE_CHARACTER: char = '\\';

#[derive(Debug, PartialEq)]
pub enum TokenKind {
    LexError,

    Done,

    Char,

    ID,
    Limit,
    Null,
    String,
    DoubleQuotedString,
    Number,
    BooleanLiteral,
    ValueArg,
    ListArg,
    Comment,
    Variable,
    Savepoint,
    EscapeSequence,
    NullSafeEqual,
    LE,
    GE,
    NE,
    Not,
    As,
    Alter,
    Drop,
    Create,
    Grant,
    Revoke,
    Commit,
    Begin,
    Truncate,
    Select,
    From,
    Update,
    Delete,
    Insert,
    Into,
    Join,
    ColonCast,

    // Filtered specifies that the token is a comma and was discarded by one
    // of the filters.
    Filtered,

    // FilteredGroupableParenthesis is a parenthesis marked as filtered groupable. It is the
	// beginning of either a group of values ('(') or a nested query. We track is as
	// a special case for when it may start a nested query as opposed to just another
	// value group to be obfuscated.
	FilteredGroupableParenthesis,

    // FilteredGroupable specifies that the given token has been discarded by one of the
	// token filters and that it is groupable together with consecutive FilteredGroupable
	// tokens.
	FilteredGroupable,

    // FilteredBracketedIdentifier specifies that we are currently discarding
	// a bracketed identifier (MSSQL).
	// See issue https://github.com/DataDog/datadog-trace-agent/issues/475.
	FilteredBracketedIdentifier,

    DollarQuotedString,
}

impl FromStr for TokenKind {
    type Err = anyhow::Error;
    fn from_str(input: &str) -> Result<TokenKind, anyhow::Error> {
        match input {
            "NULL" => Ok(TokenKind::Null),
            "TRUE" => Ok(TokenKind::BooleanLiteral),
            "FALSE" => Ok(TokenKind::BooleanLiteral),
            "SAVEPOINT" => Ok(TokenKind::Savepoint),
            "LIMIT" => Ok(TokenKind::Limit),
            "AS" => Ok(TokenKind::As),
            "ALTER" => Ok(TokenKind::Alter),
            "CREATE" => Ok(TokenKind::Create),
            "GRANT" => Ok(TokenKind::Grant),
            "REVOKE" => Ok(TokenKind::Revoke),
            "COMMIT" => Ok(TokenKind::Commit),
            "BEGIN" => Ok(TokenKind::Begin),
            "TRUNCATE" => Ok(TokenKind::Truncate),
            "DROP" => Ok(TokenKind::Drop),
            "SELECT" => Ok(TokenKind::Select),
            "FROM" => Ok(TokenKind::From),
            "UPDATE" => Ok(TokenKind::Update),
            "DELETE" => Ok(TokenKind::Delete),
            "INSERT" => Ok(TokenKind::Insert),
            "INTO" => Ok(TokenKind::Into),
            "JOIN" => Ok(TokenKind::Join),
            "STRING" => Ok(TokenKind::String),
            "LEXERROR" => Ok(TokenKind::LexError),
            "COLONCAST" => Ok(TokenKind::ColonCast),
            _ => Err(anyhow::anyhow!("Error creating TokenKind from string")),
        }
    }
}

pub struct SqlTokenizerScanResult {
    pub token_kind: TokenKind,
    pub token: String,
}

pub struct SqlTokenizer {
    cur_char: char,        // the current char
    offset: Option<usize>, // the index of the current char
    index_of_last_read: usize,
    query: Vec<char>,               // the sql query we are parsing
    pub err: Option<anyhow::Error>, // any errors that occurred while reading
    curlys: i32, // number of active open curly braces in top-level sql escape sequences
    literal_escapes: bool, // indicates we should not treat backslashes as escape characters
    pub seen_escape: bool, // indicated whether this tokenizer has seen an escape character within a string
    pub done: bool,
}

impl SqlTokenizer {
    pub fn new(query: &str, literal_escapes: bool) -> SqlTokenizer {
        SqlTokenizer {
            cur_char: ' ',
            query: query.trim().chars().collect(),
            offset: None,
            index_of_last_read: 0,
            err: None,
            curlys: 0,
            literal_escapes,
            seen_escape: false,
            done: false,
        }
    }

    pub fn set_literal_escapes(&mut self, literal_escapes: bool) {
        self.literal_escapes = literal_escapes
    }

    pub fn scan(&mut self) -> SqlTokenizerScanResult {
        if self.offset.is_none() {
            self.next();
            if self.done {
                return SqlTokenizerScanResult {
                    token_kind: TokenKind::Done,
                    token: String::new(),
                };
            }
        }
        self.skip_blank();

        if self.is_leading_letter(self.cur_char) {
            // TODO: add is_dbms_postgres specific logic
            return self.scan_identifier();
        }
        if self.cur_char.is_ascii_digit() {
            return self.scan_number(false);
        }

        let prev_char = self.cur_char;

        self.next();

        if self.done && self.err.is_some() {
            return SqlTokenizerScanResult {
                token_kind: TokenKind::LexError,
                token: String::new(),
            };
        }

        match prev_char {
            // TODO: Add postgres specific behavior for '$' and '@' match cases (which are omitted)
            //       in addition to the other postgres TODOs in included match cases.
            ':' => {
                if self.cur_char == ':' {
                    self.next();
                    self.get_advanced_chars();
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::ColonCast,
                        token: "::".to_string(),
                    };
                }
                if self.cur_char.is_whitespace() {
                    // example scenario: "autovacuum: VACUUM ANALYZE fake.table"
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::Char,
                        token: self.get_advanced_chars(),
                    };
                }
                if self.cur_char != '=' {
                    return self.scan_bind_var();
                }
                self.set_unexpected_char_error_and_return()
            }
            '~' => {
                if self.cur_char == '*' {
                    self.next();
                    self.get_advanced_chars();
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::Char,
                        token: "~*".to_string(),
                    };
                }
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '?' => {
                // TODO: add dbms postgres specific logic
                self.set_unexpected_char_error_and_return()
            }
            '=' | ',' | ';' | '(' | ')' | '+' | '*' | '&' | '|' | '^' | ']' => {
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '[' => {
                // TODO: add dbms postgres specific logic
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '.' => {
                if self.cur_char.is_ascii_digit() {
                    return self.scan_number(true);
                }
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '/' => match self.cur_char {
                '/' => {
                    self.next();
                    self.scan_comment_type_1()
                }
                '*' => {
                    self.next();
                    self.scan_comment_type_2()
                }
                _ => SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                },
            },
            '-' => {
                // TODO: add dbms postgres specific logic
                if self.cur_char == '-' {
                    self.next();
                    return self.scan_comment_type_1();
                }
                if self.cur_char.is_ascii_digit() {
                    return self.scan_number(false);
                }
                if self.cur_char == '.' {
                    self.next();
                    if self.cur_char.is_ascii_digit() {
                        return self.scan_number(true);
                    }
                    // if the next char after a period is not a digit, revert back a character
                    self.cur_char = '.';
                    self.offset = Some(self.offset.unwrap() - 1);
                }
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '#' => {
                // TODO: add dbms postgres specific logic
                self.next();
                self.scan_comment_type_1()
            }
            '<' => match self.cur_char {
                '>' => {
                    self.next();
                    self.get_advanced_chars();
                    SqlTokenizerScanResult {
                        token_kind: TokenKind::NE,
                        token: "<>".to_string(),
                    }
                }
                '=' => {
                    self.next();
                    if self.cur_char == '>' {
                        self.next();
                        self.get_advanced_chars();
                        return SqlTokenizerScanResult {
                            token_kind: TokenKind::NullSafeEqual,
                            token: "<=>".to_string(),
                        };
                    }
                    self.get_advanced_chars();
                    SqlTokenizerScanResult {
                        token_kind: TokenKind::LE,
                        token: "<=".to_string(),
                    }
                }
                _ => SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                },
            },
            '>' => {
                if self.cur_char == '=' {
                    self.next();
                    self.get_advanced_chars();
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::GE,
                        token: ">=".to_string(),
                    };
                }
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '!' => match self.cur_char {
                '=' => {
                    self.next();
                    self.get_advanced_chars();
                    SqlTokenizerScanResult {
                        token_kind: TokenKind::NE,
                        token: "!=".to_string(),
                    }
                }
                '~' => {
                    self.next();
                    if self.cur_char == '*' {
                        self.next();
                        self.get_advanced_chars();
                        return SqlTokenizerScanResult {
                            token_kind: TokenKind::NE,
                            token: "!~*".to_string(),
                        };
                    }
                    self.get_advanced_chars();
                    SqlTokenizerScanResult {
                        token_kind: TokenKind::NE,
                        token: "!~".to_string(),
                    }
                }
                _ => {
                    if self.is_valid_char_after_operator(self.cur_char) {
                        return SqlTokenizerScanResult {
                            token_kind: TokenKind::Not,
                            token: self.get_advanced_chars(),
                        };
                    }
                    self.set_error(&format!(
                        "unexpected char \"{}\" after \"!\"",
                        self.cur_char
                    ));
                    SqlTokenizerScanResult {
                        token_kind: TokenKind::LexError,
                        token: self.get_advanced_chars(),
                    }
                }
            },
            '\'' => self.scan_string(prev_char, TokenKind::String),
            '"' => self.scan_string(prev_char, TokenKind::DoubleQuotedString),
            '`' => self.scan_string(prev_char, TokenKind::ID),
            '%' => {
                if self.cur_char == '(' {
                    return self.scan_variable_identifier();
                }
                if self.is_letter(self.cur_char) {
                    // format parameter (e.g. '%s')
                    return self.scan_format_identifier();
                }
                // modulo operator (e.g. 'id % 8')
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '{' => {
                if self.offset.unwrap_or_default() == 1 || self.curlys > 0 {
                    // A closing curly brace has no place outside an in-progress top-level SQL escape sequence
                    // started by the '{' switch-case.
                    self.curlys += 1;
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::Char,
                        token: self.get_advanced_chars(),
                    };
                }
                self.scan_escape_sequence()
            }
            '}' => {
                if self.curlys == 0 {
                    self.set_error(&format!("unexptected char \"{}\"", self.cur_char));
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::LexError,
                        token: self.get_advanced_chars(),
                    };
                }
                self.curlys -= 1;
                SqlTokenizerScanResult {
                    token_kind: TokenKind::Char,
                    token: self.get_advanced_chars(),
                }
            }
            '$' => {
                // TODO: Handle SQLServer strings starting with a single '$'
                // For example in SQLServer, you can have "MG..... OUTPUT $action, inserted.*"
                // $action in the OUTPUT clause of a MERGE statement is a special identifier
                // that returns one of three values for each row: 'INSERT', 'UPDATE', or 'DELETE'.
                // See: https://docs.microsoft.com/en-us/sql/t-sql/statements/merge-transact-sql?view=sql-server-ver15
                let result = self.scan_dollar_quoted_string();
                self.get_advanced_chars();
                result
            }
            _ => {
                self.set_error(&format!("unexpected char \"{}\"", self.cur_char));
                SqlTokenizerScanResult {
                    token_kind: TokenKind::LexError,
                    token: self.get_advanced_chars(),
                }
            }
        }
    }

    fn set_error(&mut self, err: &str) {
        let pos = self.offset.unwrap_or_default();
        println!("setting error: at position {:?}: {}", pos, err);
        self.err = Some(anyhow::anyhow!("at position {:?}: {}", pos, err));
    }

    fn set_unexpected_char_error_and_return(&mut self) -> SqlTokenizerScanResult {
        self.err = Some(anyhow::anyhow!("unexpected char: {}", self.cur_char));
        SqlTokenizerScanResult {
            token_kind: TokenKind::Char,
            token: self.get_advanced_chars(),
        }
    }

    fn skip_blank(&mut self) {
        while self.cur_char.is_whitespace() && !self.done {
            self.next();
        }
    }

    fn scan_format_identifier(&mut self) -> SqlTokenizerScanResult {
        self.next();
        SqlTokenizerScanResult {
            token_kind: TokenKind::Variable,
            token: self.get_advanced_chars(),
        }
    }

    fn scan_identifier(&mut self) -> SqlTokenizerScanResult {
        self.next();
        while !self.done && (self.is_letter(self.cur_char)
            || self.cur_char.is_ascii_digit()
            || ".*$".contains(self.cur_char))
        {
            self.next();
        }

        let token = self.get_advanced_chars().trim().to_string();

        if let Ok(token_kind) = TokenKind::from_str(&token.to_uppercase()) {
            return SqlTokenizerScanResult { token_kind, token };
        }

        SqlTokenizerScanResult {
            token_kind: TokenKind::ID,
            token,
        }
    }

    fn scan_variable_identifier(&mut self) -> SqlTokenizerScanResult {
        while self.cur_char != ')' && !self.done {
            self.next();
        }
        self.next();
        if !self.is_letter(self.cur_char) {
            self.set_error(&format!(
                "invalid character after variable identifier: \"{}\"",
                self.cur_char
            ));
            return SqlTokenizerScanResult {
                token_kind: TokenKind::LexError,
                token: self.get_advanced_chars(),
            };
        }
        self.next();
        SqlTokenizerScanResult {
            token_kind: TokenKind::Variable,
            token: self.get_advanced_chars(),
        }
    }

    fn scan_escape_sequence(&mut self) -> SqlTokenizerScanResult {
        while self.cur_char != '}' && !self.done {
            self.next();
        }

        // we've reached the end of the string without finding closing curly braces
        if self.done {
            self.set_error("unexpected EOF in escape sequence");
            return SqlTokenizerScanResult {
                token_kind: TokenKind::LexError,
                token: self.get_advanced_chars(),
            };
        }

        self.next();
        SqlTokenizerScanResult {
            token_kind: TokenKind::EscapeSequence,
            token: self.get_advanced_chars(),
        }
    }

    fn scan_bind_var(&mut self) -> SqlTokenizerScanResult {
        let mut token_kind = TokenKind::ValueArg;
        if self.cur_char == ':' {
            token_kind = TokenKind::ListArg;
            self.next();
        }
        if !self.is_letter(self.cur_char) && !self.cur_char.is_ascii_digit() {
            self.set_error(&format!(
                "bind variables should start with letters or digits, got \"{}\"",
                self.cur_char
            ));
            return SqlTokenizerScanResult {
                token_kind: TokenKind::LexError,
                token: self.get_advanced_chars(),
            };
        }
        while self.is_letter(self.cur_char)
            || self.cur_char.is_ascii_digit()
            || self.cur_char == '.'
        {
            self.next();
        }
        SqlTokenizerScanResult {
            token_kind,
            token: self.get_advanced_chars(),
        }
    }

    fn scan_number(&mut self, seen_decimal_point: bool) -> SqlTokenizerScanResult {
        if seen_decimal_point {
            self.scan_mantissa(10);
            self.scan_exponent();
            return self.finish_number_scan();
        }

        if self.cur_char == '0' {
            self.next();
            if self.cur_char == 'x' || self.cur_char == 'X' {
                // hexadecimel int
                self.next();
                self.scan_mantissa(16);
            } else {
                // octal int or float
                self.scan_mantissa(8);
                if self.cur_char == '8' || self.cur_char == '9' {
                    self.scan_mantissa(10);
                }
                if self.cur_char == '.' {
                    self.scan_fraction();
                }
                if self.cur_char == 'e' || self.cur_char == 'E' {
                    self.scan_exponent();
                }
            }
            return self.finish_number_scan();
        }

        self.scan_mantissa(10);
        self.scan_fraction();
        self.scan_exponent();
        self.finish_number_scan()
    }

    fn scan_fraction(&mut self) {
        if self.cur_char != '.' {
            return;
        }
        self.next();
        self.scan_mantissa(10);
    }

    fn scan_exponent(&mut self) {
        if self.cur_char != 'e' && self.cur_char != 'E' {
            return;
        }
        self.next();
        if self.cur_char == '+' || self.cur_char == '-' {
            self.next();
        }
        self.scan_mantissa(10);
    }

    fn finish_number_scan(&mut self) -> SqlTokenizerScanResult {
        let s = self.get_advanced_chars();
        if s.is_empty() {
            self.err = Some(anyhow::anyhow!(
                "Parse error: ended up with a zero-length number."
            ));
            return SqlTokenizerScanResult {
                token_kind: TokenKind::LexError,
                token: s,
            };
        }
        SqlTokenizerScanResult {
            token_kind: TokenKind::Number,
            token: s,
        }
    }

    fn scan_mantissa(&mut self, base: u32) {
        while !self.done && self.digit_val(self.cur_char) < base {
            self.next()
        }
    }

    fn digit_val(&mut self, c: char) -> u32 {
        if c.is_ascii_digit() {
            return c.to_digit(10).unwrap();
        }
        if ('a'..='f').contains(&c) {
            return c as u32 - 'a' as u32 + 10;
        }
        if ('A'..='F').contains(&c) {
            return c as u32 - 'A' as u32 + 10;
        }
        16
    }

    fn scan_string(&mut self, delim: char, kind: TokenKind) -> SqlTokenizerScanResult {
        let s = &mut String::new();
        loop {
            let mut prev_char = self.cur_char;
            self.next();
            if prev_char == delim {
                if self.cur_char == delim && !self.done {
                    // doubling the delimiter is the default way to embed the delimiter within a string
                    self.next();
                } else if self.done && self.offset.unwrap() > self.query.len() {
                    // edge case where we start scanning for a string at the very end of the query
                    self.set_error("unexpected EOF in string");
                    return SqlTokenizerScanResult {
                        token_kind: TokenKind::LexError,
                        token: s.to_string(),
                    };
                } else {
                    // a single delimiter denotes the end of the string
                    break;
                }
            } else if prev_char == ESCAPE_CHARACTER {
                self.seen_escape = true;

                if !self.literal_escapes {
                    // treat as an escape character
                    prev_char = self.cur_char;
                    self.next();
                }
            }
            s.push(prev_char);
            if self.done {
                self.set_error("unexpected EOF in string");
                return SqlTokenizerScanResult {
                    token_kind: TokenKind::LexError,
                    token: s.to_string(),
                };
            }
        }
        self.get_advanced_chars();
        if kind == TokenKind::ID && s.is_empty() || s.chars().all(|c| c.is_whitespace()) {
            return SqlTokenizerScanResult {
                token_kind: kind,
                token: format!("{delim}{delim}"),
            };
        }
        SqlTokenizerScanResult {
            token_kind: kind,
            token: s.to_string(),
        }
    }

    fn scan_dollar_quoted_string(&mut self) -> SqlTokenizerScanResult {
        let mut result = self.scan_string('$', TokenKind::String);
        if result.token_kind == TokenKind::LexError {
            result.token = self.get_advanced_chars();
            return result;
        }
        let s = &mut String::new();
        let mut delim_index = 0;
        let delim: Vec<char> = match result.token.as_str() {
            "$$" => {
                result.token.chars().collect()
            }
            _ => {
                format!("${}$", result.token).chars().collect()
            }
        };
        loop {
            let c = self.cur_char;
            self.next();
            if self.done {
                self.err = Some(anyhow::anyhow!("unexpected EOF in dollar-quoted string"));
                return SqlTokenizerScanResult {
                    token_kind: TokenKind::LexError,
                    token: s.to_string()
                };
            }
            if c == delim[delim_index] {
                delim_index += 1;
                if delim_index == delim.len() {
                    break;
                }
                continue;
            }
            if delim_index > 0 {
                let seen_delim_substr: String = (delim[0..delim_index]).iter().collect();
                s.push_str(&seen_delim_substr);
                delim_index = 0;
            }
            s.push(c);
        }
        SqlTokenizerScanResult {
            token_kind: TokenKind::DollarQuotedString,
            token: s.to_string()
        }

    }

    fn scan_comment_type_1(&mut self) -> SqlTokenizerScanResult {
        while !self.done {
            if self.cur_char == '\n' {
                self.next();
                break;
            }
            self.next();
        }
        SqlTokenizerScanResult {
            token_kind: TokenKind::Comment,
            token: self.get_advanced_chars(),
        }
    }

    fn scan_comment_type_2(&mut self) -> SqlTokenizerScanResult {
        let mut token_kind = TokenKind::Comment;
        loop {
            if self.cur_char == '*' {
                self.next();
                if self.cur_char == '/' {
                    self.next();
                    break;
                }
                continue;
            }
            if self.done {
                self.set_error("unexpected EOF in comment");
                token_kind = TokenKind::LexError;
                break;
            }
            self.next();
        }
        SqlTokenizerScanResult {
            token_kind,
            token: self.get_advanced_chars(),
        }
    }

    // gets the substring of the query that were advanced since the last time this function
    // was called
    fn get_advanced_chars(&mut self) -> String {
        if self.offset.is_none() {
            return String::new();
        }
        let end_index = self.offset.unwrap();

        if end_index > self.query.len() {
            return String::new();
        }

        let return_val: String = self.query[self.index_of_last_read..end_index]
            .iter()
            .collect();

        self.index_of_last_read = self.offset.unwrap();
        return_val
    }

    fn next(&mut self) {
        if let Some(offset) = self.offset {
            self.offset = Some(offset + 1);
        } else {
            self.offset = Some(0);
        }
        let offset = self.offset.unwrap();
        if offset < self.query.len() {
            self.cur_char = self.query[offset];
            return;
        }
        self.done = true;
    }

    fn is_leading_letter(&mut self, c: char) -> bool {
        char::is_alphabetic(c) || c == '_' || c == '@'
    }

    fn is_letter(&mut self, c: char) -> bool {
        self.is_leading_letter(c) || c == '#'
    }

    fn is_valid_char_after_operator(&mut self, c: char) -> bool {
        c == '('
            || c == '`'
            || c == '\''
            || c == '"'
            || c == '+'
            || c == '-'
            || c.is_whitespace()
            || c.is_ascii_digit()
            || self.is_letter(c)
    }
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use duplicate::duplicate_item;

    use crate::sql_tokenizer::TokenKind;

    use super::SqlTokenizer;

    #[test]
    fn test_tokenizer_empty_query() {
        let query = "";
        let expected = [""];
        let mut tokenizer = SqlTokenizer::new(query, false);
        for expected_val in expected {
            let result = tokenizer.scan();
            assert_eq!(result.token.trim(), expected_val)
        }
        assert!(tokenizer.done);
    }

    #[test]
    fn test_tokenizer_simple_query() {
        let query = "SELECT username AS         person FROM (SELECT * FROM users) WHERE id=4";
        let expected = [
            "SELECT", "username", "AS", "person", "FROM", "(", "SELECT", "*", "FROM", "users", ")",
            "WHERE", "id", "=", "4",
        ];
        let mut tokenizer = SqlTokenizer::new(query, false);
        for expected_val in expected {
            let result = tokenizer.scan();
            assert_eq!(result.token.trim(), expected_val)
        }
        assert!(tokenizer.done);
    }

    #[test]
    fn test_tokenizer_single_line_comment_dashes() {
        let query = r#"
-- Single line comment
-- Another single line comment
-- Another another single line comment
GRANT USAGE, DELETE ON SCHEMA datadog TO datadog"#;
        let expected = [
            "-- Single line comment",
            "-- Another single line comment",
            "-- Another another single line comment",
            "GRANT",
            "USAGE",
            ",",
            "DELETE",
            "ON",
            "SCHEMA",
            "datadog",
            "TO",
            "datadog",
        ];
        let mut tokenizer = SqlTokenizer::new(query, false);
        for expected_val in expected {
            let result = tokenizer.scan();
            assert_eq!(result.token.trim(), expected_val)
        }
        assert!(tokenizer.done);
    }

    #[test]
    fn test_tokenizer_single_line_comment_slash() {
        let query = r#"
// Single line comment
// Another single line comment
// Another another single line comment
GRANT USAGE, DELETE ON SCHEMA datadog TO datadog"#;
        let expected = [
            "// Single line comment",
            "// Another single line comment",
            "// Another another single line comment",
            "GRANT",
            "USAGE",
            ",",
            "DELETE",
            "ON",
            "SCHEMA",
            "datadog",
            "TO",
            "datadog",
        ];
        let mut tokenizer = SqlTokenizer::new(query, false);
        for expected_val in expected {
            let result = tokenizer.scan();
            assert_eq!(result.token.trim(), expected_val)
        }
        assert!(tokenizer.done);
    }

    #[test]
    fn test_tokenizer_multi_line_comment() {
        let query = r#"SELECT * FROM host /*
multiline comment with parameters,
host:localhost,url:controller#home,id:FF005:00CAA
*/"#;
        let expected = [
            "SELECT", "*", "FROM", "host", "/*\nmultiline comment with parameters,\nhost:localhost,url:controller#home,id:FF005:00CAA\n*/",
        ];
        let mut tokenizer = SqlTokenizer::new(query, false);
        for expected_val in expected {
            let result = tokenizer.scan();
            assert_eq!(result.token.trim(), expected_val)
        }
        assert!(tokenizer.done);
    }

    #[duplicate_item(
        test_name                           number_value;
        [test_tokenize_int_strings_1]       ["123456789"];
        [test_tokenize_int_strings_2]       ["0"];
        [test_tokenize_int_strings_3]       ["-1"];
        [test_tokenize_int_strings_4]       ["-2018"];
        [test_tokenize_int_strings_5]       [i64::MIN.to_string().as_str()];
        [test_tokenize_int_strings_6]       [i64::MAX.to_string().as_str()];
        [test_tokenize_int_strings_7]       ["39"];
        [test_tokenize_int_strings_8]       ["7"];
        [test_tokenize_int_strings_9]       ["-83"];
        [test_tokenize_int_strings_10]      ["-9223372036854775807"];
        [test_tokenize_int_strings_11]      ["9"];
        [test_tokenize_int_strings_12]      ["-108"];
        [test_tokenize_int_strings_13]      ["-71"];
        [test_tokenize_int_strings_14]      ["-71"];
        [test_tokenize_int_strings_15]      ["-9223372036854775675"];
        [test_tokenize_float_strings_1]     ["0"];
        [test_tokenize_float_strings_2]     ["0.123456789"];
        [test_tokenize_float_strings_3]     ["-0.123456789"];
        [test_tokenize_float_strings_4]     ["12.3456789"];
        [test_tokenize_float_strings_5]     ["-12.3456789"];
        [test_tokenize_only_decimal_1]      [".001"];
        [test_tokenize_decimal_only_2]      [".21341"];
        [test_tokenize_decimal_only_3]      ["-.1234"];
        [test_tokenize_decimal_only_4]      ["-.0003"];
        [test_tokenize_hex_number_1]        ["0x6400"];
        [test_tokenize_decimal_exponent_1]  ["2.5E+01"];
        [test_tokenize_decimal_exponent_2]  ["2.5e+01"];
        [test_tokenize_decimal_exponent_3]  ["9.99999E+05"];
        [test_tokenize_decimal_exponent_4]  ["9.99999e+05"];
        [test_tokenize_decimal_exponent_5]  ["0E+00"];
        [test_tokenize_decimal_exponent_6]  ["0e+00"];
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = SqlTokenizer::new(number_value, false);
        let result = tokenizer.scan();
        assert!(tokenizer.done);
        assert_eq!(result.token, number_value);
        assert_eq!(result.token_kind, TokenKind::Number);
    }

    #[duplicate_item(
        test_name                               input               expected;
        [test_tokenize_dollar_quoted_str_1]  ["$tag$abc$tag$"]   ["abc"];
        [test_tokenize_dollar_quoted_str_2]  ["$func$abc$func$"]   ["abc"];
        [test_tokenize_dollar_quoted_str_3]  [r#"$tag$textwith\n\rnewlinesand\r\\\$tag$"#]   [r#"textwith\n\rnewlinesand\r\\\"#];
        [test_tokenize_dollar_quoted_str_4]  ["$tag$ab$tactac$tx$tag$"]   ["ab$tactac$tx"];
        [test_tokenize_dollar_quoted_str_5]  ["$$abc$$"]   ["abc"];
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = SqlTokenizer::new(input, false);
        let result = tokenizer.scan();
        assert!(tokenizer.done);
        assert_eq!(result.token, expected);
    }

    #[duplicate_item(
        test_name                               input               expected;
        [test_tokenize_dollar_quoted_str_err_1]  ["$$abc"]   ["abc"];
        [test_tokenize_dollar_quoted_str_err_2]  ["$$abc$"]   ["abc"];
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = SqlTokenizer::new(input, false);
        let result = tokenizer.scan();
        assert!(tokenizer.done);
        assert_eq!(result.token_kind, TokenKind::LexError);
        assert_eq!(tokenizer.err.unwrap().to_string(), "unexpected EOF in dollar-quoted string");
    }

    #[duplicate_item(
        [
            test_name       [test_tokenize_literal_escapes_false_1]
            input           [r#"'Simple string'"#]
            expected        ["Simple string"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_false_2]
            input           [r#"'Simple string'"#]
            expected        ["Simple string"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_3]
            input           [r#"'String with backslash at end \'"#]
            expected        ["String with backslash at end '"]
			token_kind_str  ["LEXERROR"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_4]
            input           [r#"'String with backslash \ in the middle'"#]
            expected        ["String with backslash  in the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_5]
            input           [r#"'String with double-backslash at end \\'"#]
            expected        ["String with double-backslash at end \\"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_6]
            input           [r#"'String with double-backslash \\ in the middle'"#]
            expected        ["String with double-backslash \\ in the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_7]
            input           [r#"'String with backslash-escaped quote at end \''"#]
            expected        ["String with backslash-escaped quote at end '"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_8]
            input           [r#"'String with backslash-escaped quote \' in middle'"#]
            expected        ["String with backslash-escaped quote ' in middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_9]
            input           [r#"'String with backslash-escaped embedded string \'foo\' in the middle'"#]
            expected        ["String with backslash-escaped embedded string 'foo' in the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_10]
            input           [r#"'String with backslash-escaped embedded string at end \'foo\''"#]
            expected        ["String with backslash-escaped embedded string at end 'foo'"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_11]
            input           [r#"'String with double-backslash-escaped embedded string at the end \\'foo\\''"#]
            expected        ["String with double-backslash-escaped embedded string at the end \\"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_12]
            input           [r#"'String with double-backslash-escaped embedded string \\'foo\\' in the middle'"#]
            expected        ["String with double-backslash-escaped embedded string \\"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_13]
            input           [r#"'String with backslash-escaped embedded string \'foo\' in the middle followed by one at the end \'"#]
            expected        ["String with backslash-escaped embedded string 'foo' in the middle followed by one at the end '"]
			token_kind_str  ["LEXERROR"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_14]
            input           [r#"'String with embedded string at end ''foo'''"#]
            expected        ["String with embedded string at end 'foo'"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_15]
            input           [r#"'String with embedded string ''foo'' in the middle'"#]
            expected        ["String with embedded string 'foo' in the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_16]
            input           [r#"'String with tab at end	'"#]
            expected        ["String with tab at end\t"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_17]
            input           [r#"'String with tab	in the middle'"#]
            expected        ["String with tab\tin the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_18]
            input           [r#"'String with newline at the end
'"#]
            expected        ["String with newline at the end\n"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_19]
            input           [r#"'String with newline
in the middle'"#]
            expected        ["String with newline\nin the middle"]
			token_kind_str  ["STRING"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_20]
            input           [r#"'Simple string missing closing quote"#]
            expected        ["Simple string missing closing quote"]
			token_kind_str  ["LEXERROR"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_21]
            input           [r#"'String missing closing quote with backslash at end \"#]
            expected        ["String missing closing quote with backslash at end \\"]
			token_kind_str  ["LEXERROR"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_22]
            input           [r#"'String with backslash \ in the middle missing closing quote"#]
            expected        ["String with backslash  in the middle missing closing quote"]
			token_kind_str  ["LEXERROR"];
		]
		[
            test_name       [test_tokenize_literal_escapes_false_23]
            input           ["::"]
			expected        ["::"]
			token_kind_str  ["COLONCAST"];
        ]
		// The following case will treat the final quote as unescaped
		[
            test_name       [test_tokenize_literal_escapes_false_24]
            input           [r#"'String missing closing quote with backslash-escaped quote at end \'"#]
            expected        ["String missing closing quote with backslash-escaped quote at end '"]
			token_kind_str  ["LEXERROR"];
        ]
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = SqlTokenizer::new(input, false);
        let result = tokenizer.scan();
        assert_eq!(result.token_kind, TokenKind::from_str(token_kind_str).unwrap());
        assert_eq!(result.token, expected);
    }

    #[duplicate_item(
        [
            test_name       [test_tokenize_literal_escapes_true_1]
            input           [r#"'Simple string'"#]
            expected        ["Simple string"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_2]
            input           [r#"'String with backslash at end \'"#]
            expected        ["String with backslash at end \\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_3]
            input           [r#"'String with backslash \ in the middle'"#]
            expected        ["String with backslash \\ in the middle"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_4]
            input           [r#"'String with double-backslash at end \\'"#]
            expected        ["String with double-backslash at end \\\\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_5]
            input           [r#"'String with double-backslash \\ in the middle'"#]
            expected        ["String with double-backslash \\\\ in the middle"]
            token_kind_str  ["STRING"];
        ]
        // The following case will treat backslash as literal and double single quote as a single quote
        // thus missing the final single quote
        [
            test_name       [test_tokenize_literal_escapes_true_6]
            input           [r#"'String with backslash-escaped quote at end \''"#]
            expected        ["String with backslash-escaped quote at end \\'"]
            token_kind_str  ["LEXERROR"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_7]
            input           [r#"'String with backslash-escaped quote \' in middle'"#]
            expected        ["String with backslash-escaped quote \\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_8]
            input           [r#"'String with backslash-escaped embedded string at the end \'foo\''"#]
            expected        ["String with backslash-escaped embedded string at the end \\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_9]
            input           [r#"'String with backslash-escaped embedded string \'foo\' in the middle'"#]
            expected        ["String with backslash-escaped embedded string \\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_10]
            input           [r#"'String with double-backslash-escaped embedded string at end \\'foo\\''"#]
            expected        ["String with double-backslash-escaped embedded string at end \\\\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_11]
            input           [r#"'String with double-backslash-escaped embedded string \\'foo\\' in the middle'"#]
            expected        ["String with double-backslash-escaped embedded string \\\\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_12]
            input           [r#"'String with backslash-escaped embedded string \'foo\' in the middle followed by one at the end \'"#]
            expected        ["String with backslash-escaped embedded string \\"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_13]
            input           [r#"'String with embedded string at end ''foo'''"#]
            expected        ["String with embedded string at end 'foo'"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_14]
            input           [r#"'String with embedded string ''foo'' in the middle'"#]
            expected        ["String with embedded string 'foo' in the middle"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_15]
            input           [r#"'String with tab at end	'"#]
            expected        ["String with tab at end\t"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_16]
            input           [r#"'String with tab	in the middle'"#]
            expected        ["String with tab\tin the middle"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_17]
            input           [r#"'String with newline at the end
'"#]
            expected        ["String with newline at the end\n"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_18]
            input           [r#"'String with newline
in the middle'"#]
            expected        ["String with newline\nin the middle"]
            token_kind_str  ["STRING"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_19]
            input           [r#"'Simple string missing closing quote"#]
            expected        ["Simple string missing closing quote"]
            token_kind_str  ["LEXERROR"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_20]
            input           [r#"'String missing closing quote with backslash at end \"#]
            expected        ["String missing closing quote with backslash at end \\"]
            token_kind_str  ["LEXERROR"];
        ]
        [
            test_name       [test_tokenize_literal_escapes_true_21]
            input           [r#"'String with backslash \ in the middle missing closing quote"#]
            expected        ["String with backslash \\ in the middle missing closing quote"]
            token_kind_str  ["LEXERROR"];
        ]
        // The following case will treat the final quote as unescaped
        [
            test_name       [test_tokenize_literal_escapes_true_22]
            input           [r#"'String missing closing quote with backslash-escaped quote at end \'"#]
            expected        ["String missing closing quote with backslash-escaped quote at end \\"]
            token_kind_str  ["STRING"];
        ]
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = SqlTokenizer::new(input, true);
        let result = tokenizer.scan();
        assert_eq!(result.token_kind, TokenKind::from_str(token_kind_str).unwrap());
        assert_eq!(result.token, expected);
    }
}
