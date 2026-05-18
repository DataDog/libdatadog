// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DbmsKind {
    #[default]
    Generic,
    Mssql,
    Mysql,
    Postgresql,
    Oracle,
}

/// See `DbmsKind` for the list of supported DBMS.
pub struct UnknownDBMSError;

impl TryFrom<&str> for DbmsKind {
    type Error = UnknownDBMSError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let res = match value.to_lowercase().as_str() {
            "" => Self::Generic,
            "mssql" => Self::Mssql,
            "mysql" => Self::Mysql,
            "postgresql" => Self::Postgresql,
            "oracle" => Self::Oracle,
            _ => return Err(UnknownDBMSError),
        };
        Ok(res)
    }
}

#[allow(deprecated)]
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SqlObfuscationMode {
    #[default]
    #[deprecated = "kept for compatibility with agent's obfuscator but has unintuitive behavior"]
    #[serde(alias = "")]
    Unspecified,
    NormalizeOnly,
    ObfuscateOnly,
    ObfuscateAndNormalize,
}

/// Configuration for SQL obfuscation
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SqlObfuscateConfig {
    pub replace_digits: bool,
    pub keep_sql_alias: bool,
    pub dollar_quoted_func: bool,
    pub keep_null: bool,
    pub keep_boolean: bool,
    pub keep_positional_parameter: bool,
    pub keep_trailing_semicolon: bool,
    pub keep_identifier_quotation: bool,
    pub replace_bind_parameter: bool,
    pub remove_space_between_parentheses: bool,
    pub keep_json_path: bool,
    pub obfuscation_mode: SqlObfuscationMode,
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b > 127
}

fn is_ident_char(b: u8) -> bool {
    // Go's scanIdentifier includes '.*$' as continuation chars in addition to alnum/_.
    // '@' is in Go's isLetter (isLeadingLetter) so it continues identifiers too.
    // '.' is handled separately (qualifier), but '*', '$', '@' are included here.
    b.is_ascii_alphanumeric()
        || b == b'_'
        || b == b'$'
        || b == b'#'
        || b == b'*'
        || b == b'@'
        || b > 127
}

/// Replace trailing digit sequences in identifier with `?`
/// e.g., sales_2019_07_01 → sales_?_?_?
///       item1001 → item?
///       ddh19 → ddh?
fn apply_replace_digits(ident: &str) -> String {
    let bytes = ident.as_bytes();
    let mut result = String::with_capacity(ident.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            result.push('?');
        } else {
            // Push char as UTF-8
            let c = ident[i..].chars().next().unwrap_or(' ');
            result.push(c);
            i += c.len_utf8();
        }
    }
    result
}

/// Returns the index just past the closing `'` of a quoted string starting at `start`.
fn find_quoted_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'\'') {
        return None;
    }
    // First: try short close (no backslash escape) — standard SQL uses '' only
    let short_end = {
        let mut i = start + 1;
        let mut result = None;
        while i < bytes.len() {
            if bytes[i] == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2; // '' escape
                    continue;
                } else {
                    result = Some(i + 1);
                    break;
                }
            }
            i += 1;
        }
        result
    };

    // Use the short close if what follows is a SQL word boundary (not alphanumeric/underscore),
    // meaning the string truly ends there. If followed by alphanumeric, the \' was likely
    // an escape and the string continues — try greedy (with backslash escape).
    let short_at_boundary = short_end.is_some_and(|end| {
        !bytes
            .get(end)
            .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_')
    });

    if short_at_boundary {
        return short_end;
    }

    // Greedy: use backslash escape to find a longer match
    let mut i = start + 1;
    let mut escaped = false;
    while i < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[i] == b'\\' {
            escaped = true;
        } else if bytes[i] == b'\'' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 1; // '' escape inside greedy scan
            } else {
                return Some(i + 1);
            }
        }
        i += 1;
    }

    // Greedy found nothing; fall back to short_end (even if not at word boundary)
    short_end
}

/// Find the end of a dollar-quoted string $tag$...$tag$
/// Returns (inner_start, inner_end, outer_end) or None if not a valid dollar quote
fn find_dollar_quote_end(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    let n = bytes.len();
    if start >= n || bytes[start] != b'$' {
        return None;
    }
    // Collect the tag: $<tag>$ — Go allows spaces and other chars in tags
    let mut tag_end = start + 1;
    while tag_end < n && bytes[tag_end] != b'$' {
        if bytes[tag_end] == b'\n' {
            return None; // tags don't span lines
        }
        tag_end += 1;
    }
    if tag_end >= n {
        return None;
    }
    // tag is bytes[start..=tag_end], e.g. $func$ or $$
    let tag = &bytes[start..=tag_end];
    let inner_start = tag_end + 1;

    // Search for closing tag
    let mut i = inner_start;
    while i + tag.len() <= n {
        if bytes[i] == b'$' && bytes[i..].starts_with(tag) {
            return Some((inner_start, i, i + tag.len()));
        }
        i += 1;
    }
    None
}

struct Tokenizer<'a> {
    s: &'a str,
    bytes: &'a [u8],
    pos: usize,
    result: String,
    dbms: DbmsKind,
    config: &'a SqlObfuscateConfig,
    // For alias stripping: length of result before we emitted the most recent ' AS' segment
    before_as_len: Option<usize>,
    // After SAVEPOINT keyword, next token should become ?
    pending_savepoint: bool,
    // True when the last emitted non-space char was a standalone placeholder '?'
    // (as opposed to '?' from replace_digits inside an identifier name)
    last_was_placeholder: bool,
    // When keep_json_path=true, set after -> or ->> to keep next literal as-is
    pending_json_path: bool,
    // True when the last emitted operator was a standalone = (assignment/comparison)
    // Used to detect value context for double-quoted strings
    last_was_assign: bool,
}

impl<'a> Tokenizer<'a> {
    fn new(s: &'a str, config: &'a SqlObfuscateConfig, dbms: DbmsKind) -> Self {
        Self {
            s,
            bytes: s.as_bytes(),
            pos: 0,
            result: String::with_capacity(s.len()),
            dbms,
            config,
            before_as_len: None,
            pending_savepoint: false,
            last_was_placeholder: false,
            pending_json_path: false,
            last_was_assign: false,
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn is_normalize_only(&self) -> bool {
        matches!(
            self.config.obfuscation_mode,
            SqlObfuscationMode::NormalizeOnly
        )
    }

    fn is_obfuscate_only(&self) -> bool {
        matches!(
            self.config.obfuscation_mode,
            SqlObfuscationMode::ObfuscateOnly
        )
    }

    #[allow(deprecated)]
    fn is_unspecified_obfuscate_mode(&self) -> bool {
        matches!(
            self.config.obfuscation_mode,
            SqlObfuscationMode::Unspecified
        )
    }

    fn last_char(&self) -> Option<u8> {
        self.result.as_bytes().last().copied()
    }

    fn last_nonspace_char(&self) -> Option<u8> {
        self.result
            .as_bytes()
            .iter()
            .rev()
            .find(|&&b| b != b' ')
            .copied()
    }

    /// Push a space if result doesn't already end with one (and result is non-empty).
    /// Does NOT add space after `.` (qualifier separator).
    /// When actually pushing a space, resets last_was_placeholder — equivalent to Go's
    /// groupingFilter.Reset() on any non-comma, non-paren, non-FilteredGroupable token.
    fn space(&mut self) {
        if !self.result.is_empty()
            && self.last_char() != Some(b' ')
            && !(self.last_char() == Some(b'.') && {
                // Only suppress space after '.' when it acts as a qualifier separator
                // (preceded by an identifier character like "table.column").
                // For standalone '.' at the start or after operators, add the space.
                let len = self.result.len();
                if len < 2 {
                    false
                } else {
                    let before_dot = self.result.as_bytes()[len - 2];
                    before_dot.is_ascii_alphanumeric()
                        || before_dot == b'_'
                        || before_dot == b'"'
                        || before_dot == b']'
                        || before_dot == b'#'
                        || before_dot == b'?'
                        || before_dot > 127 // Non-ASCII identifier chars
                }
            })
            && !(self.last_char() == Some(b'(')
                && (self.config.remove_space_between_parentheses || self.is_obfuscate_only()))
        {
            // Reset last_was_placeholder when transitioning past a non-placeholder token
            // (e.g., operator or identifier). When last non-space char is '?', preserve
            // the group state so that commas stripped after a literal don't break grouping.
            if self.last_nonspace_char() != Some(b'?') {
                self.last_was_placeholder = false;
            }
            self.result.push(' ');
        } else if self.last_char() == Some(b' ')
            && !matches!(self.last_nonspace_char(), Some(b'?') | Some(b'('))
        {
            // Result already ends in space (e.g. after an operator like '!') and we still need
            // to reset placeholder state. Do NOT reset after '(' — Go's groupingFilter lets
            // last_was_placeholder persist through '(' tokens.
            self.last_was_placeholder = false;
        }
    }

    /// Emit a token, adding a space before it if needed.
    fn emit(&mut self, token: &str) {
        self.space();
        self.result.push_str(token);
        self.last_was_placeholder = false;
        self.last_was_assign = false;
    }

    /// Emit a single char token, adding a space before if needed.
    fn emit_char(&mut self, c: char) {
        self.space();
        self.result.push(c);
        self.last_was_placeholder = c == '?';
        self.last_was_assign = false;
    }

    /// Emit a literal-replacement '?' with consecutive-duplicate suppression.
    /// In legacy mode, Go's groupingFilter suppresses consecutive FilteredGroupable tokens
    /// (groupFilter > 1). If last_was_placeholder is already true, suppress this one.
    fn emit_placeholder(&mut self) {
        if self.is_unspecified_obfuscate_mode() && self.last_was_placeholder {
            // Suppress consecutive placeholder (Go groupFilter > 1 rule)
            return;
        }
        self.emit_char('?');
    }

    fn skip_whitespace(&mut self) {
        while !self.at_end() && is_whitespace(self.bytes[self.pos]) {
            self.pos += 1;
        }
        // Also skip Unicode whitespace (e.g. U+2003 EM SPACE) — Go uses unicode.IsSpace
        while !self.at_end() && self.bytes[self.pos] > 127 {
            if let Some(c) = self.s[self.pos..].chars().next() {
                if c.is_whitespace() {
                    self.pos += c.len_utf8();
                    // There may be ASCII whitespace after, loop again
                    while !self.at_end() && is_whitespace(self.bytes[self.pos]) {
                        self.pos += 1;
                    }
                    continue;
                }
            }
            break;
        }
    }

    fn skip_line_comment(&mut self) {
        while !self.at_end() && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) {
        // We've already consumed '/*', now find '*/'
        while self.pos + 1 < self.bytes.len() {
            if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
        // Malformed - skip to end
        self.pos = self.bytes.len();
    }

    /// Handle a single-quoted string: emit '?'
    fn handle_single_quote(&mut self) {
        let str_start = self.pos;
        if let Some(end) = find_quoted_string_end(self.bytes, self.pos) {
            self.pos = end;
        } else {
            // Unterminated string: consume to end of input, emit ?
            self.pos = self.bytes.len();
        }
        if self.pending_json_path || self.is_normalize_only() {
            self.pending_json_path = false;
            // Keep the string as-is (don't quantize)
            let raw = &self.s[str_start..self.pos].to_string();
            if !self.maybe_consume_alias_next() {
                self.emit(raw);
            }
            return;
        }
        if !self.maybe_consume_alias_next() {
            self.emit_placeholder();
        }
    }

    /// Called when we're about to emit a real token after 'AS'.
    /// If we're in alias-stripping mode, truncate result to before 'AS' and return true (skip
    /// token).
    fn maybe_consume_alias_next(&mut self) -> bool {
        if let Some(before_len) = self.before_as_len.take() {
            // Truncate result to remove the ' AS' we emitted
            self.result.truncate(before_len);
            return true; // caller should skip emitting the token
        }
        false
    }

    /// Emit an identifier token (handles NULL/bool/AS).
    fn emit_identifier(&mut self, ident: &str) {
        let lower = ident.to_ascii_lowercase();

        // If we're in pending_savepoint state, the next token becomes ?
        if self.pending_savepoint {
            self.pending_savepoint = false;
            if self.maybe_consume_alias_next() {
                return;
            }
            self.emit_placeholder();
            return;
        }

        // Handle NULL
        if !self.config.keep_null && !self.is_normalize_only() && lower == "null" {
            if self.maybe_consume_alias_next() {
                return;
            }
            self.emit_placeholder();
            return;
        }

        // Handle boolean literals
        if !self.config.keep_boolean
            && !self.is_normalize_only()
            && (lower == "true" || lower == "false")
        {
            if self.maybe_consume_alias_next() {
                return;
            }
            self.emit_placeholder();
            return;
        }

        // Handle AS keyword for alias stripping
        // Alias stripping applies in legacy mode and normalize modes, but NOT in obfuscate_only
        // (go-sqllexer obfuscator does not strip aliases)
        if !self.config.keep_sql_alias
            && !self.is_normalize_only()
            && !self.is_obfuscate_only()
            && lower == "as"
        {
            // Don't consume alias here - emit AS but remember where to truncate
            // Trim trailing space from result if any
            if self.last_char() == Some(b' ') {
                self.result.pop();
            }
            let before_len = self.result.len();
            self.space();
            self.result.push_str(ident);
            self.before_as_len = Some(before_len);
            // Go's groupingFilter resets when it sees AS (non-groupable, non-paren, non-comma).
            // Reset last_was_placeholder so the comma after `alias` is not stripped.
            self.last_was_placeholder = false;
            return;
        }

        // SQL control-flow keywords should NOT be consumed as aliases
        // (e.g., `CREATE PROCEDURE TestProc AS BEGIN ...` — BEGIN is not an alias)
        // The `AS` is already in self.result; just clear before_as_len to keep it.
        // Go's discardFilter discards the token after AS unconditionally (except '[' which triggers
        // MSSQL bracketed identifier mode). However, SQL block-start keywords like BEGIN should not
        // be consumed as aliases (e.g., CREATE PROCEDURE ... AS BEGIN).
        // We keep the exclusion list minimal: only true SQL block-starters that cannot be aliases.
        if self.before_as_len.is_some()
            && matches!(
                lower.as_str(),
                "begin"
                    | "end"
                    | "select"
                    | "insert"
                    | "update"
                    | "delete"
                    | "from"
                    | "where"
                    | "join"
                    | "on"
                    | "set"
                    | "values"
                    | "into"
                    | "group"
                    | "order"
                    | "having"
                    | "union"
                    | "intersect"
                    | "except"
                    | "limit"
                    | "offset"
                    | "with"
                    | "create"
                    | "drop"
                    | "alter"
                    | "truncate"
            )
        {
            self.before_as_len = None;
        }

        if self.maybe_consume_alias_next() {
            return;
        }

        // After emitting SAVEPOINT, the next identifier/literal becomes ?
        if lower == "savepoint" {
            self.pending_savepoint = true;
        }

        let out = if self.config.replace_digits {
            apply_replace_digits(ident)
        } else {
            ident.to_string()
        };
        self.emit(&out);
    }

    /// After emitting a backtick/double-quote identifier, check if followed by '.' and another
    /// quoted ident.
    fn handle_dot_after_quoted_ident(&mut self) {
        if !self.at_end() && self.bytes[self.pos] == b'.' {
            let next = self.bytes.get(self.pos + 1).copied();
            // In obfuscate_only mode, preserve input spacing (no spaces around dots)
            if self.is_obfuscate_only() {
                self.result.push('.');
                self.pos += 1;
                return;
            }
            match next {
                Some(b'`') | Some(b'"') | Some(b'[') => {
                    self.result.push_str(" . ");
                    self.pos += 1; // skip '.'
                }
                Some(b'*') => {
                    self.result.push_str(".*");
                    self.pos += 2;
                }
                Some(c) if is_ident_start(c) => {
                    self.result.push_str(" . ");
                    self.pos += 1; // skip '.'
                }
                _ => {
                    self.result.push('.');
                    self.pos += 1;
                }
            }
        }
    }

    /// After emitting a bracket identifier, check if followed by '.' and another bracket ident.
    fn handle_dot_after_bracket_ident(&mut self) {
        if !self.at_end() && self.bytes[self.pos] == b'.' {
            let next = self.bytes.get(self.pos + 1).copied();
            if next == Some(b'[') {
                self.result.push_str(" . ");
                self.pos += 1; // skip '.'
            } else {
                self.result.push('.');
                self.pos += 1;
            }
        }
    }

    /// Consume and emit the rest of a numeric literal starting at current pos.
    fn consume_number(&mut self) {
        self.consume_number_inner(false);
    }

    fn consume_number_inner(&mut self, seen_dot: bool) {
        // Consume digits, '.', 'e'/'E', optional sign after 'e', suffix letters.
        // `seen_dot`: true when caller already consumed the leading '.', so don't allow another.
        // This mirrors Go's scanNumber(seenDecimalPoint) which goes straight to `exponent`
        // without looping back to `fraction`, leaving a second '.' as a separate token.
        let mut saw_dot = seen_dot;
        let mut saw_exp = false;
        while !self.at_end() {
            let b = self.bytes[self.pos];
            match b {
                b'0'..=b'9' => {
                    self.pos += 1;
                }
                b'.' if !saw_dot => {
                    saw_dot = true;
                    self.pos += 1;
                }
                b'e' | b'E' if !saw_exp => {
                    saw_exp = true;
                    self.pos += 1;
                    // optional sign
                    if !self.at_end() && matches!(self.bytes[self.pos], b'+' | b'-') {
                        self.pos += 1;
                    }
                }
                // Note: letter suffixes like 'f'/'d'/'l' are NOT consumed here.
                // Go's old SQL tokenizer does not treat them as numeric suffixes,
                // so "0D" parses as number "0" + identifier "D".
                _ => break,
            }
        }
    }

    fn process(&mut self) {
        while !self.at_end() {
            let b = self.bytes[self.pos];

            match b {
                // Whitespace: normalize to single space
                b if is_whitespace(b) => {
                    self.pos += 1;
                    self.skip_whitespace();
                    // Don't push space if we're in alias-stripping mode (waiting for next token)
                    if self.before_as_len.is_none() {
                        self.space();
                    }
                }

                // Line comment: -- ... (also // like Go's old tokenizer)
                b'-' if self.peek(1) == Some(b'-') => {
                    self.pos += 2;
                    self.skip_line_comment();
                }
                b'/' if self.peek(1) == Some(b'/') => {
                    self.pos += 2;
                    self.skip_line_comment();
                }

                // MySQL-style comment: # ...
                // In Go's old tokenizer, # is ALWAYS a comment unless DBMS is SQL Server.
                b'#' => {
                    let next = self.peek(1);
                    let is_sqlserver = matches!(self.dbms, DbmsKind::Mssql);
                    match next {
                        Some(b)
                            if is_sqlserver
                                && (b.is_ascii_alphanumeric() || b == b'_' || b == b'#') =>
                        {
                            // SQL Server temp table identifier like #temp or ##global
                            let start = self.pos;
                            while !self.at_end() && is_ident_char(self.bytes[self.pos]) {
                                self.pos += 1;
                            }
                            let ident = &self.s[start..self.pos];
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            let out = if self.config.replace_digits {
                                apply_replace_digits(ident)
                            } else {
                                ident.to_string()
                            };
                            self.emit(&out);
                        }
                        // PostgreSQL JSON operators: #>, #>>, #-
                        Some(b'>') if matches!(self.dbms, DbmsKind::Postgresql) => {
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            if self.peek(2) == Some(b'>') {
                                self.emit("#>>");
                                self.pos += 3;
                            } else {
                                self.emit("#>");
                                self.pos += 2;
                            }
                            self.space();
                        }
                        Some(b'-') if matches!(self.dbms, DbmsKind::Postgresql) => {
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit("#-");
                            self.pos += 2;
                            self.space();
                        }
                        _ => {
                            // MySQL-style comment: skip to end of line
                            self.pos += 1;
                            self.skip_line_comment();
                        }
                    }
                }

                // Block comment: /* ... */
                b'/' if self.peek(1) == Some(b'*') => {
                    self.pos += 2;
                    self.skip_block_comment();
                }

                // Semicolon
                b';' => {
                    self.pos += 1;
                    // In old tokenizer mode (obfuscation_mode=""), Go ALWAYS strips semicolons.
                    // Go's discardFilter marks ';' as filterable-groupable, so the next '?'
                    // gets grouped/dropped. Set last_was_placeholder to replicate this.
                    if self.is_obfuscate_only()
                        || (!self.is_unspecified_obfuscate_mode()
                            && self.config.keep_trailing_semicolon)
                    {
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        self.result.push(';');
                    } else if self.is_unspecified_obfuscate_mode() {
                        // Mark as "filterable groupable" so next ? is grouped (Go behavior)
                        self.last_was_placeholder = true;
                    }
                }

                // Opening paren
                b'(' => {
                    if self.before_as_len.is_some() && !self.is_unspecified_obfuscate_mode() {
                        // In obfuscate_and_normalize mode: keep AS before ( (CTE body)
                        self.before_as_len = None;
                    } else if self.before_as_len.is_some() {
                        // Legacy mode: Go discards the token immediately after AS, including '('.
                        // Strip AS from result and skip '(' (matching Go's discardFilter behavior).
                        self.maybe_consume_alias_next();
                        self.pos += 1;
                        self.skip_whitespace();
                        continue; // skip emitting '('
                    }
                    self.pending_savepoint = false;
                    self.space();
                    self.result.push('(');
                    self.pos += 1;
                    // In old-tokenizer mode (obfuscation_mode=""), Go always adds a space after '('
                    // regardless of remove_space_between_parentheses. Only suppress in new modes.
                    let add_space = if self.is_unspecified_obfuscate_mode() {
                        !self.is_obfuscate_only()
                    } else {
                        !self.config.remove_space_between_parentheses && !self.is_obfuscate_only()
                    };
                    if add_space {
                        self.skip_whitespace();
                        self.result.push(' ');
                    }
                }

                // Closing paren
                b')' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    // In old-tokenizer mode, Go always adds spaces before ')'
                    let add_close_space = if self.is_unspecified_obfuscate_mode() {
                        !self.is_obfuscate_only()
                    } else {
                        !self.config.remove_space_between_parentheses && !self.is_obfuscate_only()
                    };
                    if add_close_space {
                        // Add space before ) if needed (not after '(' or already spaced)
                        if !matches!(self.last_char(), Some(b'(') | Some(b' ') | None) {
                            self.result.push(' ');
                        }
                    }
                    self.result.push(')');
                    self.pos += 1;
                    // NOTE: do NOT clear last_was_placeholder here.
                    // Go's groupingFilter doesn't reset on ')' either, which means
                    // a comma after `? )` (CTE body ending with a placeholder) is stripped.
                }

                // Comma
                b',' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.pos += 1;
                    // Go behavior: commas that follow a standalone placeholder ? are stripped
                    // Go's groupingFilter comma stripping: when groupFilter > 0 and token == ',',
                    // discard it. This only applies in legacy mode (obfuscation_mode == "").
                    // go-sqllexer modes (obfuscate_only, etc.) do NOT strip commas.
                    if self.last_was_placeholder && self.is_unspecified_obfuscate_mode() {
                        // Remove any trailing space too
                        while self.last_char() == Some(b' ') {
                            self.result.pop();
                        }
                        continue;
                    }
                    // Remove any trailing space before comma (input may have `token ,`)
                    if self.last_char() == Some(b' ') {
                        self.result.pop();
                    }
                    self.result.push(',');
                    // Space after comma handled by next token's space() call
                }

                // Single-quoted string
                b'\'' => {
                    self.handle_single_quote();
                }

                // Backtick identifier (MySQL-style, handle doubled backtick escaping)
                b'`' => {
                    self.pos += 1;
                    let mut ident_buf = String::new();
                    loop {
                        if self.at_end() {
                            break;
                        }
                        if self.bytes[self.pos] == b'`' {
                            if self.bytes.get(self.pos + 1) == Some(&b'`') {
                                // Escaped backtick
                                ident_buf.push('`');
                                self.pos += 2;
                            } else {
                                self.pos += 1; // skip closing backtick
                                break;
                            }
                        } else {
                            let c = self.s[self.pos..].chars().next().unwrap_or(' ');
                            ident_buf.push(c);
                            self.pos += c.len_utf8();
                        }
                    }
                    // Empty/whitespace-only backtick identifiers must keep their delimiters
                    // to avoid producing invalid SQL (matches Go's scanString behavior).
                    let out = if ident_buf.chars().all(char::is_whitespace) {
                        format!("`{ident_buf}`")
                    } else if self.config.replace_digits {
                        apply_replace_digits(&ident_buf)
                    } else {
                        ident_buf.clone()
                    };
                    if self.maybe_consume_alias_next() {
                        // The alias token is consumed
                    } else {
                        self.emit(&out);
                        self.handle_dot_after_quoted_ident();
                    }
                }

                // Double-quoted identifier
                b'"' => {
                    self.pos += 1;
                    // Scan double-quoted identifier, decoding "" escape sequences to single "
                    let mut ident_buf = String::new();
                    while !self.at_end() {
                        if self.bytes[self.pos] == b'"' {
                            if self.bytes.get(self.pos + 1) == Some(&b'"') {
                                ident_buf.push('"'); // "" → one "
                                self.pos += 2;
                            } else {
                                break;
                            }
                        } else {
                            let ch = self.s[self.pos..].chars().next().unwrap_or('\0');
                            ident_buf.push(ch);
                            self.pos += ch.len_utf8();
                        }
                    }
                    let ident_owned = ident_buf;
                    let ident = ident_owned.as_str();
                    if !self.at_end() {
                        self.pos += 1; // skip closing quote
                    }
                    // If last token was = (assignment/comparison), treat double-quoted string
                    // as a value (quantize), not as an identifier.
                    let is_string_value = self.last_was_assign;
                    // If pending SAVEPOINT or empty/whitespace content, treat as literal → ?
                    if self.pending_savepoint
                        || (!ident.is_empty() && ident.chars().all(|c| c.is_whitespace()))
                        || (!self.is_normalize_only() && is_string_value)
                    {
                        self.pending_savepoint = false;
                        if self.maybe_consume_alias_next() {
                            // consumed
                        } else {
                            self.emit_placeholder();
                            self.handle_dot_after_quoted_ident();
                        }
                    } else if (self.config.keep_identifier_quotation
                        && !self.is_unspecified_obfuscate_mode())
                        || self.is_obfuscate_only()
                    {
                        // Keep original double-quote syntax (go-sqllexer obfuscate_only keeps
                        // quotes) In old tokenizer mode,
                        // keep_identifier_quotation is ignored (like Go).
                        let quoted = format!("\"{ident}\"");
                        if self.maybe_consume_alias_next() {
                            // consumed
                        } else {
                            self.emit(&quoted);
                            self.handle_dot_after_quoted_ident();
                        }
                    } else {
                        // For empty identifiers, keep quotes (empty ident without quotes = invalid
                        // SQL) Go's replaceFilter never applies
                        // replace_digits to DoubleQuotedString tokens (only
                        // ID/TableName), so we never digit-replace quoted identifier content.
                        let out = if ident.is_empty() {
                            format!("\"{ident}\"")
                        } else {
                            ident.to_string()
                        };
                        if self.maybe_consume_alias_next() {
                            // consumed
                        } else {
                            self.emit(&out);
                            self.handle_dot_after_quoted_ident();
                        }
                    }
                }

                // Square bracket identifier [...]
                b'[' => {
                    if matches!(self.dbms, DbmsKind::Mssql) {
                        self.pos += 1;
                        let id_start = self.pos;
                        while !self.at_end() && self.bytes[self.pos] != b']' {
                            self.pos += 1;
                        }
                        let ident = &self.s[id_start..self.pos];
                        if !self.at_end() {
                            self.pos += 1; // skip ']'
                        }
                        if self.maybe_consume_alias_next() {
                            // consumed
                        } else {
                            let out = if self.config.replace_digits {
                                apply_replace_digits(ident)
                            } else {
                                ident.to_string()
                            };
                            self.emit(&out);
                            self.handle_dot_after_bracket_ident();
                        }
                    } else {
                        // Non-mssql: emit [ as operator, let content be tokenized normally
                        // But if in alias mode, consume the whole [...] block as the alias
                        if self.before_as_len.is_some() {
                            self.pos += 1; // skip '['
                            while !self.at_end() && self.bytes[self.pos] != b']' {
                                self.pos += 1;
                            }
                            if !self.at_end() {
                                self.pos += 1; // skip ']'
                            }
                            self.maybe_consume_alias_next();
                        } else {
                            self.space();
                            self.result.push('[');
                            self.pos += 1;
                            self.skip_whitespace();
                            self.space();
                        }
                    }
                }

                b']' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if !matches!(self.last_char(), Some(b'[') | Some(b' ') | None) {
                        self.space();
                    }
                    self.result.push(']');
                    self.pos += 1;
                    // If followed by '.', emit ' . ' for chained bracket access
                    if !self.at_end() && self.bytes[self.pos] == b'.' {
                        self.result.push_str(" . ");
                        self.pos += 1; // skip '.'
                    }
                }

                // Dollar sign: positional param, dollar-quoted string, or identifier
                b'$' => {
                    let next = self.peek(1);
                    match next {
                        // Positional param: $1, $2, $?, $09
                        Some(b) if b.is_ascii_digit() || b == b'?' => {
                            let token_start = self.pos;
                            self.pos += 1; // skip '$'
                                           // Go's scanPreparedStatement calls scanNumber which only scans
                                           // decimal digits (not all alphanumeric). Letters like 'C' in "$2C"
                                           // are NOT part of the positional param.
                            while !self.at_end()
                                && (self.bytes[self.pos].is_ascii_digit()
                                    || self.bytes[self.pos] == b'?')
                            {
                                self.pos += 1;
                            }
                            // Go's scanNumber follows the fraction path: a trailing '.' (and any
                            // following digits) is consumed as part of the number, e.g. "$0." → "?"
                            if !self.at_end() && self.bytes[self.pos] == b'.' {
                                self.pos += 1; // consume '.'
                                while !self.at_end() && self.bytes[self.pos].is_ascii_digit() {
                                    self.pos += 1;
                                }
                            }
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            // In old-tokenizer mode (obfuscation_mode=Unspecified), Go always
                            // replaces positional parameters regardless
                            // of keep_positional_parameter.
                            // Only respect keep_positional_parameter in new lexer modes.
                            let keep = (self.config.keep_positional_parameter
                                && !self.is_unspecified_obfuscate_mode())
                                || self.is_normalize_only()
                                || self.is_obfuscate_only();
                            if keep {
                                self.emit(&self.s[token_start..self.pos]);
                            } else {
                                self.emit_placeholder();
                            }
                        }
                        // Dollar-quoted string: $tag$...$tag$ or $$...$$
                        _ if next == Some(b'$')
                            || next.is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') =>
                        {
                            let start = self.pos;
                            if let Some((inner_start, inner_end, outer_end)) =
                                find_dollar_quote_end(self.bytes, start)
                            {
                                if self.maybe_consume_alias_next() {
                                    self.pos = outer_end;
                                    continue;
                                }
                                if self.is_normalize_only() {
                                    // In normalize mode: process inner content with same config
                                    let tag_str = &self.s[start..inner_start];
                                    let inner = &self.s[inner_start..inner_end];
                                    let close_tag = &self.s[inner_end..outer_end];
                                    let normalized_inner =
                                        obfuscate_sql(inner, self.config, self.dbms);
                                    self.space();
                                    self.result.push_str(tag_str);
                                    self.result.push_str(&normalized_inner);
                                    self.result.push_str(close_tag);
                                } else if self.config.dollar_quoted_func {
                                    // Obfuscate the content inside dollar quotes
                                    let tag_str = &self.s[start..inner_start];
                                    let inner = &self.s[inner_start..inner_end];
                                    let close_tag = &self.s[inner_end..outer_end];
                                    let obfuscated_inner =
                                        obfuscate_sql(inner, self.config, self.dbms);
                                    // If inner collapses to just '?' (trivial content), emit ?
                                    // directly
                                    if obfuscated_inner.trim() == "?" {
                                        self.emit_placeholder();
                                    } else {
                                        self.space();
                                        self.result.push_str(tag_str);
                                        self.result.push_str(&obfuscated_inner);
                                        self.result.push_str(close_tag);
                                    }
                                } else {
                                    // Replace whole thing with ?
                                    self.emit_placeholder();
                                }
                                self.pos = outer_end;
                            } else {
                                // Not a valid dollar quote, check if it's an identifier starting
                                // with $
                                self.pos += 1; // skip '$'
                                let id_start_pos = self.pos;
                                while !self.at_end()
                                    && (is_ident_char(self.bytes[self.pos])
                                        || self.bytes[self.pos] == b'$')
                                {
                                    self.pos += 1;
                                }
                                let ident = &self.s[id_start_pos - 1..self.pos]; // include '$'
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.emit(ident);
                            }
                            let _ = b; // suppress unused warning
                        }
                        _ => {
                            // $identifier like $action - keep as-is
                            let start = self.pos;
                            self.pos += 1; // skip '$'
                            while !self.at_end()
                                && (is_ident_char(self.bytes[self.pos])
                                    || self.bytes[self.pos] == b'$')
                            {
                                self.pos += 1;
                            }
                            let token = &self.s[start..self.pos];
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit(token);
                        }
                    }
                }

                // Hex literal: 0x...
                b'0' if matches!(self.peek(1), Some(b'x') | Some(b'X')) => {
                    self.pos += 2; // skip '0x'
                    while !self.at_end() && self.bytes[self.pos].is_ascii_hexdigit() {
                        self.pos += 1;
                    }
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.emit_placeholder();
                }

                // Hex literal: X'...' or x'...'
                b'X' | b'x' if self.peek(1) == Some(b'\'') => {
                    self.pos += 1; // skip 'X'/'x'
                    if let Some(end) = find_quoted_string_end(self.bytes, self.pos) {
                        self.pos = end;
                    } else {
                        self.pos += 1;
                    }
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.emit_placeholder();
                }

                // % bind param: %s, %d, %b, %i, %(name)s
                b'%' => {
                    let next = self.peek(1);
                    match next {
                        Some(b)
                            if b.is_ascii_alphabetic() || b == b'_' || b == b'@' || b == b'#' =>
                        {
                            // Any ASCII letter/underscore/@/# after % is a format parameter
                            // (Go's scanFormatParameter handles all isLetter chars)
                            self.pos += 2;
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit_placeholder();
                        }
                        Some(b'(') => {
                            self.pos += 2; // skip '%('
                            while !self.at_end() && self.bytes[self.pos] != b')' {
                                self.pos += 1;
                            }
                            if !self.at_end() {
                                self.pos += 1;
                            } // skip ')'
                              // Skip the format character
                            if !self.at_end() && self.bytes[self.pos].is_ascii_alphabetic() {
                                self.pos += 1;
                            }
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit_placeholder();
                        }
                        Some(b) if b > 127 => {
                            // Non-ASCII byte: check if it starts a Unicode letter.
                            // Go's old SQL tokenizer treats %<letter> as a format parameter
                            // (Variable token) which gets obfuscated to '?'.
                            let next_char = self.s[self.pos + 1..].chars().next();
                            if let Some(nc) = next_char.filter(|c| c.is_alphabetic() || *c == '_') {
                                let skip = 1 + nc.len_utf8();
                                self.pos += skip;
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.emit_placeholder();
                            } else {
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.space();
                                self.result.push('%');
                                self.pos += 1;
                                self.space();
                            }
                        }
                        _ => {
                            // Just a % sign - emit as operator
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push('%');
                            self.pos += 1;
                            self.space();
                        }
                    }
                }

                // Number starting with '.'
                // Only treat .digit as a float literal if NOT preceded by an identifier char.
                // When preceded by an identifier, '.2' is a qualifier (Go's scanIdentifier includes
                // '.')
                b'.' if self.peek(1).is_some_and(|b| b.is_ascii_digit())
                    && !self
                        .last_char()
                        .is_some_and(|b| is_ident_char(b) || b == b'"' || b == b'`') =>
                {
                    let num_start = self.pos;
                    self.pos += 1; // skip '.'
                                   // Go's scanNumber(seenDecimalPoint=true) doesn't loop back to fraction,
                                   // so a second '.' is NOT consumed (e.g. ".0.x" → number ".0" + dot + "x")
                    self.consume_number_inner(true);
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if self.is_normalize_only() {
                        let raw = self.s[num_start..self.pos].to_string();
                        self.emit(&raw);
                    } else {
                        self.emit_placeholder();
                    }
                }

                // Plain dot (qualifier separator)
                // When preceded by an identifier and followed by digits, Go's scanIdentifier
                // includes '.' in the identifier (see ContainsRune(".*$",...)).
                // We need to handle ident.digit as a single identifier-like token.
                b'.' => {
                    let after_dot_is_digit = self.peek(1).is_some_and(|b| b.is_ascii_digit());
                    let preceded_by_ident = self
                        .last_char()
                        .is_some_and(|b| is_ident_char(b) || b == b'"' || b == b'`');
                    if after_dot_is_digit && preceded_by_ident {
                        // handled below
                    } else if !after_dot_is_digit && !preceded_by_ident {
                        // Standalone dot not after identifier: needs space (Go adds space before
                        // every token)
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        self.space();
                        self.result.push('.');
                        self.pos += 1;
                        continue;
                    }
                    if after_dot_is_digit && preceded_by_ident {
                        // Scan the rest of the identifier (including .digit parts) as one unit
                        let start = self.pos; // start at '.'
                        self.pos += 1; // skip '.'
                        while !self.at_end()
                            && (is_ident_char(self.bytes[self.pos]) || self.bytes[self.pos] == b'.')
                        {
                            if self.bytes[self.pos] == b'.' {
                                if self
                                    .peek(1)
                                    .is_some_and(|b| b.is_ascii_digit() || is_ident_char(b))
                                {
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            } else {
                                self.pos += 1;
                            }
                        }
                        let suffix = &self.s[start..self.pos];
                        // Apply replace_digits if configured, otherwise emit as-is
                        let out = if self.config.replace_digits {
                            apply_replace_digits(suffix)
                        } else {
                            suffix.to_string()
                        };
                        // Append directly to result (already have the identifier prefix)
                        self.result.push_str(&out);
                    } else {
                        self.result.push('.');
                        self.pos += 1;
                    }
                }

                // Numeric literal
                b'0'..=b'9' => {
                    let num_start = self.pos;
                    self.consume_number();
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if self.pending_json_path || self.is_normalize_only() {
                        self.pending_json_path = false;
                        let raw = self.s[num_start..self.pos].to_string();
                        self.emit(&raw);
                    } else {
                        self.emit_placeholder();
                    }
                }

                // Curly braces { ... }
                b'{' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    // Peek at content after { and optional whitespace
                    let mut peek_pos = self.pos + 1;
                    while peek_pos < self.bytes.len() && is_whitespace(self.bytes[peek_pos]) {
                        peek_pos += 1;
                    }
                    // ODBC stored proc: {call ...} — keep outer braces, tokenize content normally
                    let is_call = peek_pos + 4 <= self.bytes.len()
                        && self.bytes[peek_pos..peek_pos + 4].eq_ignore_ascii_case(b"call")
                        && (peek_pos + 4 >= self.bytes.len()
                            || !self.bytes[peek_pos + 4].is_ascii_alphanumeric());
                    if is_call {
                        self.space();
                        self.result.push('{');
                        self.pos += 1;
                        self.skip_whitespace();
                        self.result.push(' ');
                    } else {
                        // Cassandra maps, {fn ...}, etc. → scan to matching } and emit ?
                        let mut depth = 1usize;
                        self.pos += 1; // skip '{'
                        while !self.at_end() && depth > 0 {
                            match self.bytes[self.pos] {
                                b'{' => {
                                    depth += 1;
                                    self.pos += 1;
                                }
                                b'}' => {
                                    depth -= 1;
                                    self.pos += 1;
                                }
                                b'\'' => {
                                    if let Some(end) = find_quoted_string_end(self.bytes, self.pos)
                                    {
                                        self.pos = end;
                                    } else {
                                        self.pos += 1;
                                    }
                                }
                                _ => {
                                    self.pos += 1;
                                }
                            }
                        }
                        self.emit_placeholder();
                    }
                }
                b'}' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    // Unmatched closing brace — emit as operator
                    self.space();
                    self.result.push('}');
                    self.pos += 1;
                }

                // @ - named params (@name, @1, @@var) - keep as-is
                b'@' => {
                    if self.peek(1) == Some(b'@') {
                        // @@global_var
                        let start = self.pos;
                        self.pos += 2; // skip '@@'
                        while !self.at_end() && is_ident_char(self.bytes[self.pos]) {
                            self.pos += 1;
                        }
                        let token = &self.s[start..self.pos];
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        self.emit(token);
                    } else if self.peek(1).is_some_and(|b| {
                        b.is_ascii_alphanumeric()
                            || b == b'_'
                            || b == b'#'
                            || b == b'$'
                            || b == b'*'
                    }) {
                        // @name, @1, @#name, @$name, @*name — all valid Go ident chars (ASCII)
                        let start = self.pos;
                        self.pos += 1; // skip '@'
                        while !self.at_end() && is_ident_char(self.bytes[self.pos]) {
                            self.pos += 1;
                        }
                        let token = self.s[start..self.pos].to_string();
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        // Go's old tokenizer keeps @ prefix and only applies replace_digits.
                        // In new modes (obfuscation_mode!=""), respect replace_bind_parameter.
                        if self.config.replace_digits && token.chars().any(|c| c.is_ascii_digit()) {
                            let replaced = apply_replace_digits(&token);
                            self.emit(&replaced);
                        } else if self.config.replace_bind_parameter
                            && !self.is_unspecified_obfuscate_mode()
                        {
                            self.emit_placeholder();
                        } else {
                            self.emit(&token);
                        }
                    } else if self.peek(1).is_some_and(|b| b > 127) {
                        // @unicodeLetter — Go's isAlphaNumeric includes Unicode letters
                        let next_char = self.s[self.pos + 1..].chars().next();
                        if next_char.is_some_and(|c| c.is_alphabetic() || c == '_') {
                            let start = self.pos;
                            self.pos += 1; // skip '@'
                            while !self.at_end() {
                                if self.bytes[self.pos] == b'#' || self.bytes[self.pos] == b'@' {
                                    self.pos += 1;
                                    continue;
                                }
                                let rest = &self.s[self.pos..];
                                match rest.chars().next() {
                                    Some(c) if c.is_alphanumeric() || c == '_' => {
                                        self.pos += c.len_utf8();
                                    }
                                    _ => break,
                                }
                            }
                            let token = self.s[start..self.pos].to_string();
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            if self.config.replace_digits {
                                self.emit(&apply_replace_digits(&token));
                            } else {
                                self.emit(&token);
                            }
                        } else {
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push('@');
                            self.pos += 1;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                    } else if self.peek(1) == Some(b'>') {
                        // @> operator
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        self.emit("@>");
                        self.pos += 2;
                        self.result.push(' ');
                    } else {
                        // @ as standalone operator
                        if self.maybe_consume_alias_next() {
                            continue;
                        }
                        self.space();
                        self.result.push('@');
                        self.pos += 1;
                        self.last_was_placeholder = false;
                        self.result.push(' ');
                    }
                }

                // Colon: ::, :=, :name, or standalone
                b':' => {
                    match self.peek(1) {
                        Some(b':') => {
                            // :: PostgreSQL cast
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push_str("::");
                            self.pos += 2;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                        Some(b'=') => {
                            // := assignment
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push_str(":=");
                            self.pos += 2;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                        Some(b)
                            if b.is_ascii_alphanumeric() || b == b'_' || b == b'#' || b == b'@' =>
                        {
                            // :name bind parameter - keep as-is; '#' is valid in Go identifiers;
                            // '@' is isLeadingLetter in Go
                            let start = self.pos;
                            self.pos += 1; // skip ':'
                                           // Go's scanBindVar loops while isLetter || isDigit || ch == '.'
                            while !self.at_end()
                                && (is_ident_char(self.bytes[self.pos])
                                    || self.bytes[self.pos] == b'.')
                            {
                                self.pos += 1;
                            }
                            let token = &self.s[start..self.pos];
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit(token);
                            // If the bind var ends with '.', push a trailing space so that
                            // the next token doesn't get its space suppressed by space()'s
                            // qualifier-separator logic (which checks last_char == '.').
                            if token.ends_with('.') {
                                self.result.push(' ');
                            }
                        }
                        Some(b) if b > 127 => {
                            // Non-ASCII byte: check if it starts a Unicode letter.
                            // Go's normalizer emits ':' and a following IDENT without a space
                            // in bind-variable context. Treat ':unicodeword' as a single token.
                            let next_char = self.s[self.pos + 1..].chars().next();
                            if next_char.is_some_and(|c| c.is_alphabetic() || c == '_') {
                                let start = self.pos;
                                self.pos += 1; // skip ':'
                                while !self.at_end() {
                                    // '#' and '@' are valid Go identifier chars (isLetter includes
                                    // them). '.' is also valid:
                                    // Go's scanBindVar loops while isLetter || isDigit || ch == '.'
                                    if self.bytes[self.pos] == b'#'
                                        || self.bytes[self.pos] == b'@'
                                        || self.bytes[self.pos] == b'.'
                                    {
                                        self.pos += 1;
                                        continue;
                                    }
                                    let rest = &self.s[self.pos..];
                                    match rest.chars().next() {
                                        Some(c) if c.is_alphanumeric() || c == '_' => {
                                            self.pos += c.len_utf8();
                                        }
                                        _ => break,
                                    }
                                }
                                let token = &self.s[start..self.pos];
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.emit(token);
                                // If the bind var ends with '.', push a trailing space so that
                                // the next token doesn't get its space suppressed by space()'s
                                // qualifier-separator logic (which checks last_char == '.').
                                if token.ends_with('.') {
                                    self.result.push(' ');
                                }
                            } else {
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.space();
                                self.result.push(':');
                                self.pos += 1;
                                self.last_was_placeholder = false;
                                self.result.push(' ');
                            }
                        }
                        _ => {
                            // Standalone : (e.g., autovacuum:)
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push(':');
                            self.pos += 1;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                    }
                }

                // Minus: --, ->, ->>, or operator, or signed number
                b'-' => {
                    match self.peek(1) {
                        Some(b'-') => {
                            // Already handled above, but just in case
                            self.pos += 2;
                            self.skip_line_comment();
                        }
                        Some(b'>') if self.peek(2) == Some(b'>') => {
                            // ->> operator
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit("->>");
                            self.pos += 3;
                            self.result.push(' ');
                            if self.config.keep_json_path {
                                self.pending_json_path = true;
                            }
                        }
                        Some(b'>') => {
                            // -> operator
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit("->");
                            self.pos += 2;
                            self.result.push(' ');
                            if self.config.keep_json_path {
                                self.pending_json_path = true;
                            }
                        }
                        Some(b) if b.is_ascii_digit() => {
                            // Go's old tokenizer ALWAYS scans -digit as a negative number
                            // regardless of preceding context (tkn.lastChar check is only on next
                            // char).
                            {
                                self.pos += 1; // skip '-'
                                self.consume_number();
                                if self.maybe_consume_alias_next() {
                                    continue;
                                }
                                self.emit_placeholder();
                            }
                        }
                        Some(b'.')
                            if self.peek(2).is_some()
                                && !self.peek(2).is_some_and(|d| d.is_ascii_digit()) =>
                        {
                            // '-' followed by '.' followed by a non-digit non-EOF char:
                            // Go's tokenizer peeks at '.' then backtracks; because off advances
                            // past '.', bytes() captures '-.' as a
                            // single token. Emit '-.' together.
                            // When '.' is at EOF, Go's advance() doesn't change off (EndChar path),
                            // so bytes() only returns '-' — handled by the fallthrough `_` arm.
                            //
                            // Additionally, Go's advance() for the peek also advances off past the
                            // non-digit char ('v' in '-.v5'), making it the first byte of the NEXT
                            // token's bytes() output. Replicate that off-leak inline:
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push_str("-.");
                            self.pos += 2;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                            // Handle the off-leak: the char at pos was advanced past by Go's peek.
                            // It becomes the first byte of the next token in Go's model.
                            if !self.at_end() {
                                let c_len = self.s[self.pos..]
                                    .chars()
                                    .next()
                                    .map_or(1, |c| c.len_utf8());
                                let after_c = self.pos + c_len;
                                if after_c < self.bytes.len()
                                    && self.bytes[after_c].is_ascii_digit()
                                {
                                    // Leaked char + following digits = Number in Go → '?'
                                    self.pos = after_c;
                                    self.consume_number();
                                    self.emit_placeholder();
                                } else {
                                    // Leaked char becomes the bytes of a '.' token in Go → emit
                                    // as-is
                                    let n = c_len.min(self.bytes.len() - self.pos);
                                    let leaked = self.s[self.pos..self.pos + n].to_owned();
                                    self.pos += n;
                                    self.result.push_str(&leaked);
                                    self.result.push(' ');
                                }
                            }
                        }
                        Some(b'.') if self.peek(2).is_some_and(|d| d.is_ascii_digit()) => {
                            // -.digit: Go ALWAYS treats this as a signed float number,
                            // regardless of the preceding token context.
                            self.pos += 2; // skip '-.'
                            self.consume_number();
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit_placeholder();
                        }
                        _ => {
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.space();
                            self.result.push('-');
                            self.pos += 1;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                    }
                }

                // Plus: signed number or operator
                b'+' => {
                    // Go's old SQL tokenizer does NOT consume '+' as part of a signed number.
                    // '+' always stays as a separate operator token (unlike '-').
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('+');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // ? - keep as-is (already a placeholder, or JSONB operator)
                b'?' => {
                    let next = self.peek(1);
                    match next {
                        Some(b'|') if self.peek(2) != Some(b'|') => {
                            // ?| operator (but NOT ?|| which is ? followed by || concatenation)
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit("?|");
                            self.pos += 2;
                            self.result.push(' ');
                        }
                        Some(b'&') => {
                            // ?& operator
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            self.emit("?&");
                            self.pos += 2;
                            self.result.push(' ');
                        }
                        _ => {
                            // Raw ? in input:
                            // - For postgresql: treat as JSONB operator (not FilteredGroupable).
                            //   Don't set last_was_placeholder, so consecutive literals aren't
                            //   suppressed.
                            // - For other dbms: treat as bind parameter (FilteredGroupable). Use
                            //   emit_placeholder so consecutive ?s are suppressed in legacy mode.
                            if self.maybe_consume_alias_next() {
                                continue;
                            }
                            if matches!(self.dbms, DbmsKind::Postgresql) {
                                self.space();
                                self.result.push('?');
                                self.last_was_assign = false;
                                // last_was_placeholder intentionally NOT set to true for PG JSONB ?
                            } else {
                                self.emit_placeholder();
                            }
                            self.pos += 1;
                        }
                    }
                }

                // < operator and <@ / <> / <= operators
                b'<' => {
                    let next = self.peek(1);
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    match next {
                        // <@ containment operator: always PG operator when dbms=postgresql,
                        // but when non-PG and followed by identifier, treat as < @ident
                        Some(b'@') => {
                            let next2_is_ident = self
                                .peek(2)
                                .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_');
                            if matches!(self.dbms, DbmsKind::Postgresql) || !next2_is_ident {
                                self.emit("<@");
                                self.pos += 2;
                                self.result.push(' ');
                            } else {
                                // Non-PG dbms with <@name → emit < then @name handled separately
                                self.space();
                                self.result.push('<');
                                self.pos += 1;
                                self.result.push(' ');
                            }
                        }
                        Some(b'>') => {
                            self.emit("<>");
                            self.pos += 2;
                            self.result.push(' ');
                        }
                        Some(b'=') => {
                            self.emit("<=");
                            self.pos += 2;
                            self.result.push(' ');
                        }
                        _ => {
                            self.space();
                            self.result.push('<');
                            self.pos += 1;
                            self.last_was_placeholder = false;
                            self.result.push(' ');
                        }
                    }
                }

                // > and >= operators
                b'>' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if self.peek(1) == Some(b'=') {
                        self.emit(">=");
                        self.pos += 2;
                    } else {
                        self.space();
                        self.result.push('>');
                        self.pos += 1;
                        self.last_was_placeholder = false;
                    }
                    self.result.push(' ');
                }

                // = operator
                b'=' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('=');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                    self.last_was_assign = true;
                    continue; // skip emit() clearing last_was_assign
                }

                // ! and !=, !~, !~*
                b'!' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if self.peek(1) == Some(b'=') {
                        self.emit("!=");
                        self.pos += 2;
                        self.result.push(' ');
                    } else if self.peek(1) == Some(b'~') {
                        if self.peek(2) == Some(b'*') {
                            self.emit("!~*");
                            self.pos += 3;
                        } else {
                            self.emit("!~");
                            self.pos += 2;
                        }
                        self.result.push(' ');
                    } else {
                        self.space();
                        self.result.push('!');
                        self.pos += 1;
                        self.last_was_placeholder = false;
                        self.result.push(' ');
                    }
                }

                // | and ||
                b'|' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    // Emit single | (Go tokenizes || as two separate | tokens, each with spaces)
                    self.space();
                    self.result.push('|');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // & operator
                b'&' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('&');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // ~ ^ operators (and ~* compound)
                b'~' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    if self.peek(1) == Some(b'*') {
                        self.emit("~*");
                        self.pos += 2;
                    } else {
                        self.space();
                        self.result.push('~');
                        self.pos += 1;
                        self.last_was_placeholder = false;
                    }
                    self.result.push(' ');
                }
                b'^' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('^');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // * operator (or SELECT *)
                b'*' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('*');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // / operator (not /*, which is handled above)
                b'/' => {
                    if self.maybe_consume_alias_next() {
                        continue;
                    }
                    self.space();
                    self.result.push('/');
                    self.pos += 1;
                    self.last_was_placeholder = false;
                    self.result.push(' ');
                }

                // Unicode whitespace (e.g. U+2003 EM SPACE): Go uses unicode.IsSpace
                b if b > 127
                    && self.s[self.pos..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_whitespace()) =>
                {
                    let c = self.s[self.pos..].chars().next().unwrap_or(' ');
                    self.pos += c.len_utf8();
                    self.skip_whitespace();
                    if self.before_as_len.is_none() {
                        self.space();
                    }
                }

                // Identifier or keyword
                _ if is_ident_start(b) || b > 127 => {
                    let start = self.pos;
                    // Go's scanIdentifier includes '.' and '$' in identifiers
                    while !self.at_end() {
                        let b = self.bytes[self.pos];
                        if b > 127 {
                            // Non-ASCII: check if this char is Unicode whitespace — if so, stop.
                            // Go's scanIdentifier stops at unicode.IsSpace chars.
                            let c = self.s[self.pos..].chars().next();
                            if c.is_some_and(|c| c.is_whitespace()) {
                                break;
                            }
                            self.pos += c.map_or(1, |c| c.len_utf8());
                        } else if is_ident_char(b) || b == b'.' {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let token = &self.s[start..self.pos];
                    self.emit_identifier(token);
                    // If identifier ends with '.', push space so next token is separated
                    // (Go includes trailing '.' in identifier but still spaces next token)
                    if token.ends_with('.') && !self.at_end() {
                        self.result.push(' ');
                    }
                }

                // Unknown: emit as-is
                _ => {
                    let c = self.s[self.pos..].chars().next().unwrap_or(' ');
                    if self.maybe_consume_alias_next() {
                        self.pos += c.len_utf8();
                        continue;
                    }
                    self.result.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn finalize(mut self) -> String {
        // Trim trailing whitespace
        while self.result.ends_with(' ') {
            self.result.pop();
        }
        self.result
    }
}

/// Try to match a `( ?, ?, ..., ? )` or `[ ?, ?, ..., ? ]` pattern starting at `i`.
/// Returns Some(k) where k is the index after the closing bracket if matched, else None.
fn try_match_pure_group(bytes: &[u8], open: u8, close: u8, i: usize) -> Option<usize> {
    let n = bytes.len();
    if i >= n || bytes[i] != open {
        return None;
    }
    let mut k = i + 1;
    if k < n && bytes[k] == b' ' {
        k += 1;
    }
    if k >= n || bytes[k] != b'?' {
        return None;
    }
    k += 1;
    loop {
        if k < n && bytes[k] == b' ' {
            k += 1;
        }
        if k >= n {
            return None;
        }
        if bytes[k] == close {
            return Some(k + 1);
        }
        // Accept either `?, ?` or `? ?` (commas may have been stripped)
        if bytes[k] == b',' {
            k += 1;
            if k < n && bytes[k] == b' ' {
                k += 1;
            }
        }
        if k < n && bytes[k] == b'?' {
            k += 1;
        } else {
            return None;
        }
    }
}

/// Collapse `( ?, ?, ..., ? )` into `( ? )`, `[ ?, ?, ..., ? ]` into `[ ? ]`,
/// multi-row `VALUES ( ? ) , ( ? ) , ...` into `VALUES ( ? )`, and `LIMIT ?, ?` into `LIMIT ?`.
fn collapse_grouped_values(s: &str, obfuscation_mode: SqlObfuscationMode) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut result = String::with_capacity(n);
    let mut i = 0;

    while i < n {
        // Try ( ?, ... )
        if bytes[i] == b'(' {
            if let Some(end) = try_match_pure_group(bytes, b'(', b')', i) {
                result.push_str("( ? )");
                i = end;
                continue;
            }
        }
        // Try [ ?, ... ]
        if bytes[i] == b'[' {
            if let Some(end) = try_match_pure_group(bytes, b'[', b']', i) {
                result.push_str("[ ? ]");
                i = end;
                continue;
            }
        }
        // Push next character correctly (multi-byte UTF-8 safe)
        if bytes[i] < 128 {
            result.push(bytes[i] as char);
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
            result.push(c);
            i += c.len_utf8();
        }
    }

    let result = collapse_multi_values(&result);
    #[allow(deprecated)]
    if matches!(obfuscation_mode, SqlObfuscationMode::Unspecified) {
        // FIXME: this being only collapsed on the deprecated mode is unintuitive but follows the
        // weird behavior of the agent's obfuscator Collapse `LIMIT ?, ?` → `LIMIT ?`
        // (MySQL/SQLite LIMIT offset, count syntax)
        collapse_limit_two_args(&result)
    } else {
        result
    }
}

/// Collapse `VALUES ( ? ) , ( ? ) , ...` → `VALUES ( ? )`.
/// Also handles comma-less groups `VALUES ( ? ) ( ? )` (when commas were stripped by placeholder
/// logic).
fn collapse_multi_values(s: &str) -> String {
    // Pattern: "VALUES ( ? )" followed by one or more " , ( ? )" or " ( ? )" groups
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while let Some(c) = remaining.chars().next() {
        const VALUES_KW: &str = "VALUES";
        const VALUES_TAIL: &str = " ( ? )";
        const VALUES_FULL: &str = "VALUES ( ? )";

        // Match "VALUES" case-insensitively, and then match the exact tail " ( ? )"
        let matches_values_pattern = remaining.get(..VALUES_FULL.len()).is_some_and(|head| {
            head.get(..VALUES_KW.len())
                .is_some_and(|kw| kw.eq_ignore_ascii_case(VALUES_KW))
                && head
                    .get(VALUES_KW.len()..)
                    .is_some_and(|tail| tail == VALUES_TAIL)
        });

        if matches_values_pattern {
            // Preceding context: must be start or space/'(' or '\n'
            let prev_ok = result.is_empty()
                || matches!(result.chars().last(), Some(' ') | Some('(') | Some('\n'));

            if prev_ok {
                // Keep the original casing as it appeared in `remaining`
                result.push_str(&remaining[..VALUES_FULL.len()]);
                remaining = &remaining[VALUES_FULL.len()..];

                // Consume trailing groups
                loop {
                    if let Some(rest) = remaining.strip_prefix(", ( ? )") {
                        remaining = rest;
                    } else if let Some(rest) = remaining.strip_prefix(" ( ? )") {
                        remaining = rest;
                    } else if let Some(rest) = remaining.strip_prefix(" ()") {
                        remaining = rest;
                    } else {
                        break;
                    }
                }
                continue;
            }
        }

        result.push(c);
        remaining = &remaining[c.len_utf8()..];
    }
    result
}

/// Collapse `LIMIT ?, ?` → `LIMIT ?`
fn collapse_limit_two_args(s: &str) -> String {
    // Scan for "LIMIT ?, ?" pattern (all ASCII keywords, UTF-8 safe via char iteration)
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while !remaining.is_empty() {
        // Check for LIMIT (case-insensitive) + " ?, ?" or " ? ?"
        if remaining.len() >= 9 {
            let rb = remaining.as_bytes();
            const PREFIX: &[u8] = b"LIMIT ?";
            if rb[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
                // Check " ?, ?" or " ? ?"
                let skip =
                    if remaining.len() >= 10 && rb[7] == b',' && rb[8] == b' ' && rb[9] == b'?' {
                        Some(10) // "LIMIT ?, ?"
                    } else if rb[7] == b' ' && rb[8] == b'?' {
                        Some(9) // "LIMIT ? ?" (comma already stripped)
                    } else {
                        None
                    };
                if let Some(skip_len) = skip {
                    // Word boundary: previous char in result should be space or start
                    let prev_ok = result.is_empty()
                        || matches!(
                            result.as_bytes().last(),
                            Some(b' ') | Some(b'(') | Some(b'\n')
                        );
                    if prev_ok {
                        result.push_str(&remaining[..7]); // "LIMIT ?"
                        remaining = &remaining[skip_len..];
                        continue;
                    }
                }
            }
        }
        let c = remaining.chars().next().unwrap_or(' ');
        result.push(c);
        remaining = &remaining[c.len_utf8()..];
    }
    result
}

/// Obfuscates a SQL string using a proper tokenizer.
pub fn obfuscate_sql(s: &str, config: &SqlObfuscateConfig, dbms: DbmsKind) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut tokenizer = Tokenizer::new(s, config, dbms);
    tokenizer.process();
    let raw = tokenizer.finalize();
    // collapse_grouped_values applies in legacy mode and obfuscate_and_normalize mode.
    // In obfuscate_only and normalize_only modes, values are NOT collapsed.
    #[allow(deprecated)]
    let should_collapse = matches!(
        config.obfuscation_mode,
        SqlObfuscationMode::Unspecified | SqlObfuscationMode::ObfuscateAndNormalize
    );
    if should_collapse {
        collapse_grouped_values(&raw, config.obfuscation_mode)
    } else {
        raw
    }
}

/// Obfuscates a SQL string with default configuration.
pub fn obfuscate_sql_string(s: &str) -> String {
    obfuscate_sql(s, &SqlObfuscateConfig::default(), DbmsKind::Generic)
}

/// SQL obfuscation with Go-compatible whitespace normalization for use in JSON plan obfuscation.
/// Applies obfuscate_sql_string then additional normalizations for JSON plan SQL.
// FIXME: remove these tiny wrappers they provide no value, keep the public api 1 function which
// takes a config
pub fn obfuscate_sql_string_normalized(s: &str) -> String {
    let obfuscated = obfuscate_sql_string(s);
    normalize_plan_sql(&obfuscated)
}

fn normalize_plan_sql(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '`' => {
                // Strip backticks: collect identifier content up to closing backtick
                let identifier: String = chars.by_ref().take_while(|&c| c != '`').collect();
                result.push_str(&identifier);

                // If followed by `.` then another backtick identifier, replace `.` with ` . `
                if chars.peek() == Some(&'.') {
                    chars.next(); // consume `.`
                    result.push_str(if chars.peek() == Some(&'`') {
                        " . "
                    } else {
                        "."
                    });
                }
            }
            '(' => {
                result.push('(');
                if chars.peek().is_some_and(|&c| c != ' ') {
                    result.push(' ');
                }
            }
            ')' => {
                if result.as_bytes().last().is_some_and(|&b| b != b' ') {
                    result.push(' ');
                }
                result.push(')');
            }
            ':' if chars.peek() == Some(&':') => {
                chars.next(); // consume second `:`
                if result.as_bytes().last().is_some_and(|&b| b != b' ') {
                    result.push(' ');
                }
                result.push_str("::");
                if chars.peek().is_some_and(|&c| c != ' ') {
                    result.push(' ');
                }
            }
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{DbmsKind, SqlObfuscateConfig, SqlObfuscationMode};

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
            anyhow::bail!("expected {output:?}\n\tgot:      {got:?}")
        }
        Ok(())
    }

    #[test]
    fn test_sql_obfuscation_normalized() {
        let mut panic = None;
        let err = NORMALIZED_CASES
            .iter()
            .enumerate()
            .filter_map(|(i, (input, output))| {
                let err = match std::panic::catch_unwind(|| {
                    test_sql_obfuscation_normalized_case(input, output)
                }) {
                    Ok(r) => r,
                    Err(p) => {
                        panic = Some(p);
                        eprintln!("panicked normalized case {i}\n\tinput: {input}\n\n");
                        return None;
                    }
                }
                .err()?;
                Some(format!("failed normalized case {i}\n\terr: {err}\n"))
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

    fn test_sql_obfuscation_normalized_case(input: &str, output: &str) -> anyhow::Result<()> {
        let got = super::obfuscate_sql_string_normalized(input);
        if output != got {
            anyhow::bail!("expected {output:?}\n\tgot:      {got:?}")
        }
        Ok(())
    }

    #[test]
    fn test_keep_identifier_quotation() {
        let config = SqlObfuscateConfig {
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT * FROM "users" WHERE id = 1 AND name = 'test'"#,
            &config,
            DbmsKind::Generic,
        );
        // In old tokenizer mode, keep_identifier_quotation is ignored (Go does too).
        let expected = "SELECT * FROM users WHERE id = ? AND name = ?";
        assert_eq!(got, expected, "keep_identifier_quotation: got {got:?}");
    }

    #[test]
    fn test_remove_space_between_parentheses() {
        let config = SqlObfuscateConfig {
            remove_space_between_parentheses: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = ? AND (name = 'test' OR name = 'test2')",
            &config,
            DbmsKind::Generic,
        );
        // In old-tokenizer mode, Go ignores remove_space_between_parentheses and always adds spaces
        let expected = "SELECT * FROM users WHERE id = ? AND ( name = ? OR name = ? )";
        assert_eq!(
            got, expected,
            "remove_space_between_parentheses: got {got:?}"
        );
    }

    #[test]
    fn test_keep_positional_parameter() {
        // When keep_positional_parameter=true, $1/$2 should be kept as-is
        let config = SqlObfuscateConfig {
            keep_positional_parameter: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = ? AND name = $1 and id = $2",
            &config,
            DbmsKind::Generic,
        );
        // In old-tokenizer mode (obfuscation_mode=""), positional params are always replaced
        // regardless of keep_positional_parameter (matches Go's old tokenizer behavior).
        let expected = "SELECT * FROM users WHERE id = ? AND name = ? and id = ?";
        assert_eq!(
            got, expected,
            "keep_positional_parameter: got {got:?}, expected {expected:?}"
        );
    }

    const NORMALIZED_CASES: &[(&str, &str)] = &[
        // 'value'::type fix (in obfuscate_sql_string)
        ("'60'::double precision", "? :: double precision"),
        ("'dogfood'::text", "? :: text"),
        ("'15531'::tid", "? :: tid"),
        ("(query <> 'dogfood'::text)", "( query <> ? :: text )"),
        // normalize_plan_sql — parens spacing
        ("(foo != ?)", "( foo != ? )"),
        ("((a >= ?) AND (b < ?))", "( ( a >= ? ) AND ( b < ? ) )"),
        // normalize_plan_sql — :: spacing
        ("?::double precision", "? :: double precision"),
        ("(query <> ?::text)", "( query <> ? :: text )"),
        // normalize_plan_sql — backtick stripping
        ("`id`", "id"),
        (
            "(`sbtest`.`sbtest1`.`id` between ? and ?)",
            "( sbtest . sbtest1 . id between ? and ? )",
        ),
        // full pipeline (obfuscate_sql_string_normalized)
        (
            "(`sbtest`.`sbtest1`.`id` between 5016 and 5115)",
            "( sbtest . sbtest1 . id between ? and ? )",
        ),
        ("(query <> 'dogfood'::text)", "( query <> ? :: text )"),
        ("'60'::double precision", "? :: double precision"),
    ];

    const CASES: &[(&str, &str)] = &[
        ("", ""),
        ("   ", ""),
        ("         ", ""),
        ("罿", "罿"),
        ("罿潯", "罿潯"),
        ("罿潯罿潯罿潯罿潯罿潯", "罿潯罿潯罿潯罿潯罿潯"),
        ("'abc1287681964'", "?"),
        ("-- comment", ""),
        ("---", ""),
        ("1 - 2", "? - ?"),
        (
            "SELECT * FROM TABLE WHERE userId = 'abc1287681964'",
            "SELECT * FROM TABLE WHERE userId = ?",
        ),
        // Standard SQL uses '' to escape quotes, not backslash
        (
            "SELECT * FROM TABLE WHERE userId = 'it''s a string'",
            "SELECT * FROM TABLE WHERE userId = ?",
        ),
        (
            "SELECT * FROM TABLE WHERE userId IN ('a', 'b', 'c')",
            "SELECT * FROM TABLE WHERE userId IN ( ? )",
        ),
        (
            "SELECT * FROM TABLE WHERE userId = 'abc1287681964' ORDER BY FOO DESC",
            "SELECT * FROM TABLE WHERE userId = ? ORDER BY FOO DESC",
        ),
        // Backslash followed by ' at a SQL word boundary (space follows): string closes there
        // 'backslash\' closes at the ' after \, because ' is followed by space (SQL boundary)
        (
            "SELECT * FROM foo LEFT JOIN bar ON 'backslash\\' = foo.b WHERE foo.name = 'String'",
            "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?",
        ),
        (
            "SELECT * FROM foo LEFT JOIN bar ON 'backslash\\' = foo.b LEFT JOIN bar2 ON 'backslash2\\' = foo.b2 WHERE foo.name = 'String'",
            "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b LEFT JOIN bar2 ON ? = foo.b2 WHERE foo.name = ?",
        ),
        // Backslash followed by ' before more string content (alphanumeric follows): acts as escape
        // 'embedded \'quote\' in string' is ONE string because ' after \ is followed by 'q'
        (
            "SELECT * FROM foo LEFT JOIN bar ON 'embedded \\'quote\\' in string' = foo.b WHERE foo.name = 'String'",
            "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?",
        ),
        (
            "SELECT * FROM TABLE JOIN SOMETHING ON TABLE.foo = SOMETHING.bar",
            "SELECT * FROM TABLE JOIN SOMETHING ON TABLE.foo = SOMETHING.bar",
        ),
        (
            "CREATE TABLE \"VALUE\"",
            "CREATE TABLE VALUE",
        ),
        (
            "INSERT INTO \"VALUE\" (\"column\") VALUES (\'ljahklshdlKASH\')",
            "INSERT INTO VALUE ( column ) VALUES ( ? )",
        ),
        (
            "INSERT INTO \"VALUE\" (\"col1\",\"col2\",\"col3\") VALUES (\'blah\',12983,X'ff')",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO \"VALUE\" (\"col1\", \"col2\", \"col3\") VALUES (\'blah\',12983,X'ff')",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES (\'blah\',12983,X'ff')",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES (12983,X'ff',\'blah\')",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'blah\',12983)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES ('a',\'b\',1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1, col2, col3) VALUES ('a',\'b\',1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ('a',\'b\',1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES ('a', \'b\' ,1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1, col2, col3) VALUES ('a', \'b\', 1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ('a', \'b\', 1)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'罿潯罿潯罿潯罿潯罿潯\',12983)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        (
            "INSERT INTO VALUE (col1,col2,col3) VALUES (X'ff',\'罿\',12983)",
            "INSERT INTO VALUE ( col1, col2, col3 ) VALUES ( ? )",
        ),
        // AS resets groupFilter: comma after alias IS kept (verified against Go)
        (
            "SELECT 3 AS NUCLEUS_TYPE,A0.ID,A0.\"NAME\" FROM \"VALUE\" A0",
            "SELECT ?, A0.ID, A0. NAME FROM VALUE A0",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > .9999",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > 0.9999",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -0.9999",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -1e6",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +1e6",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > + ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +255",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > + ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +6.34F",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > + ? F",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +6f",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > + ? f",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > +0.5D",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > + ? D",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > -1d",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ? d",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > x'ff'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > X'ff'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > 0xff",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 > ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'\'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \' \'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'  \'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        // Standard SQL strings with spaces and regular content
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ' x '",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ' x x'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        (
            "SELECT COUNT(*) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> \'5,123\'",
            "SELECT COUNT ( * ) FROM TABLE_1 JOIN table_2 ON TABLE_1.foo = table_2.bar where col1 <> ?",
        ),
        // comma after ? is stripped: NOT NULL, next_col → NOT ? next_col
        (
            "CREATE TABLE S_H2 (id INTEGER not NULL, PRIMARY KEY ( id ))",
            "CREATE TABLE S_H2 ( id INTEGER not ? PRIMARY KEY ( id ) )",
        ),
        (
            "CREATE TABLE S_H2 ( id INTEGER not NULL, PRIMARY KEY ( id ) )",
            "CREATE TABLE S_H2 ( id INTEGER not ? PRIMARY KEY ( id ) )",
        ),
        (
            "SELECT * FROM TABLE WHERE name = 'O''Brady'",
            "SELECT * FROM TABLE WHERE name = ?",
        ),
        (
            "INSERT INTO visits VALUES (2, 8, '2013-01-02', 'rabies shot')",
            "INSERT INTO visits VALUES ( ? )",
        ),
        (
            "SELECT * FROM TABLE WHERE userId = ',' and foo=foo.bar",
            "SELECT * FROM TABLE WHERE userId = ? and foo = foo.bar",
        ),
        (
            "SELECT * FROM TABLE WHERE userId =     ','||foo.bar",
            "SELECT * FROM TABLE WHERE userId = ? | | foo.bar",
        ),
        // :named bind params kept as-is
        (
            "SELECT * FROM t WHERE y IN (:protocols) AND x IN (:sites)",
            "SELECT * FROM t WHERE y IN ( :protocols ) AND x IN ( :sites )",
        ),
        // multi-row VALUES collapse (quantizer_33)
        (
            "INSERT INTO user (id, username) VALUES ('Fred','Smith'), ('John','Smith'), ('Michael','Smith'), ('Robert','Smith');",
            "INSERT INTO user ( id, username ) VALUES ( ? )",
        ),
        // backtick identifier with regular ident after dot (quantizer_43)
        (
            "INSERT INTO `qual-aa`.issues (alert0, alert1) VALUES (NULL, NULL)",
            "INSERT INTO qual-aa . issues ( alert0, alert1 ) VALUES ( ? )",
        ),
        // !+2 sign handling: + after ! should be an operator (table_finder_23)
        (
            "select !+2",
            "select ! + ?",
        ),
        // keep_positional_parameter handled in test_sql_obfuscation_config below

        // 5*s1: multiplication, not sign prefix (quantizer_49 style)
        (
            "SELECT 5*s1 FROM t4",
            "SELECT ? * s1 FROM t4",
        ),
        (
            "(SELECT 5*s1 FROM t4 UNION SELECT 77 FROM t5)",
            "( SELECT ? * s1 FROM t4 UNION SELECT ? FROM t5 )",
        ),
        // Full quantizer_49 relevant fragment (ROW with = subquery)
        (
            "WHERE ROW(5*t2.s1,77)=(SELECT 5*s1 FROM t4 UNION SELECT 77 FROM (SELECT * FROM t5))",
            "WHERE ROW ( ? * t2.s1, ? ) = ( SELECT ? * s1 FROM t4 UNION SELECT ? FROM ( SELECT * FROM t5 ) )",
        ),
        // comma after ? is stripped (quantizer_10 style)
        (
            "UPDATE user_dash_pref SET json_prefs = %(json_prefs)s, modified = '2015-08-27' WHERE user_id = %(user_id)s AND url = %(url)s",
            "UPDATE user_dash_pref SET json_prefs = ? modified = ? WHERE user_id = ? AND url = ?",
        ),
        // comma after ? in SET list (metadata_create_trigger style)
        (
            "UPDATE t SET a = 1, b = 2, c = 3",
            "UPDATE t SET a = ? b = ? c = ?",
        ),
        // comma after ? in function call (quantizer_81 style): comma after first ? arg stripped
        (
            "SELECT set_config('foo', bar, FALSE)",
            "SELECT set_config ( ? bar, ? )",
        ),
        // fuzzing_792557810: colon followed by unicode letter should not add space
        (":ჸ", ":ჸ"),
        // fuzzing_1113621604: % followed by unicode letter is a format parameter → ?
        ("%ჸ", "?"),
        // fuzzing_4250509562: % followed by any ASCII letter is a format parameter → ?
        ("%C", "?"),
        // fuzzing_1492599371: standalone dot followed by unicode ident needs space
        (".ჸ", ". ჸ"),
        // sql_fuzzing: 0!(2 grouping — operator resets lp, so '2' after '(' gets '?'
        ("0!(2", "? ! ( ?"),
        // sql_fuzzing_3326675327: '(' does NOT reset lp — '$0' grouped with leading '?'
        ("0(($0", "? ( ("),
        // fuzzing_4233627642: digit followed by letter suffix — letter is separate IDENT
        ("0D", "? D"),
        // sql_fuzzing_4064530249: @ followed by unicode ident → no space (bind param)
        ("@ᏤᏤ", "@ᏤᏤ"),
        // fuzzing_4138960753: unicode ident immediately followed by * is one token
        ("ჸ*", "ჸ*"),
        // sql_fuzzing: standalone .* → ". *" but table.* → "table.*"
        (".*", ". *"),
        ("table.*", "table.*"),
        // fuzzing test: ( followed by unicode ident should have space
        ("(ჷ", "( ჷ"),
        ("2%$2", "? % ?"),
    ];

    #[test]
    fn test_normalize_only() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let cases = &[
            // Simple: keep numbers as-is
            (
                "SELECT * FROM users WHERE id = 1",
                "SELECT * FROM users WHERE id = 1",
            ),
            // Keep strings as-is
            (
                "SELECT * FROM users WHERE id = 1 AND name = 'test'",
                "SELECT * FROM users WHERE id = 1 AND name = 'test'",
            ),
            // Strip comments, normalize whitespace
            (
                "-- comment\n/* comment */\nSELECT id as id, name as n FROM users123 WHERE id in (1,2,3)",
                "SELECT id as id, name as n FROM users123 WHERE id in ( 1, 2, 3 )",
            ),
            // WITH CTE: keep AS
            (
                "WITH users AS (SELECT * FROM people) SELECT * FROM users",
                "WITH users AS ( SELECT * FROM people ) SELECT * FROM users",
            ),
            // Keep positional params as-is
            (
                "SELECT * FROM users WHERE id = 1 AND address = $1 and id = $2 AND deleted IS NULL AND active is TRUE",
                "SELECT * FROM users WHERE id = 1 AND address = $1 and id = $2 AND deleted IS NULL AND active is TRUE",
            ),
            // keep_trailing_semicolon ignored — semicolon stripped in normalize mode (same as obfuscate)
            // Actually in normalize mode: keep semicolon when keep_trailing_semicolon=true
            // Here with default (false): strip semicolon
            (
                "SELECT * FROM users WHERE id = 1;",
                "SELECT * FROM users WHERE id = 1",
            ),
        ];
        for (input, expected) in cases {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            assert_eq!(got, *expected, "normalize_only input={input:?}");
        }
    }

    #[test]
    fn test_normalize_only_keep_trailing_semi() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            keep_trailing_semicolon: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = 1 AND name = 'test';",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT * FROM users WHERE id = 1 AND name = 'test';";
        assert_eq!(
            got, expected,
            "normalize_only+keep_trailing_semicolon: {got:?}"
        );
    }

    #[test]
    fn test_normalize_only_keep_identifier_quotation() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT * FROM "users" WHERE id = 1 AND name = 'test'"#,
            &config,
            DbmsKind::Generic,
        );
        let expected = r#"SELECT * FROM "users" WHERE id = 1 AND name = 'test'"#;
        assert_eq!(
            got, expected,
            "normalize_only+keep_identifier_quotation: {got:?}"
        );
    }

    #[test]
    fn test_with_cte_stripping() {
        // In legacy mode (obfuscation_mode=""), WITH T1 AS (SELECT...) → WITH T1 SELECT...
        let config = SqlObfuscateConfig::default();
        let cases = &[
            // Single CTE - strip AS and opening paren, keep closing )
            (
                "WITH sales AS (SELECT x FROM t WHERE id = 1) SELECT * FROM sales",
                "WITH sales SELECT x FROM t WHERE id = ? ) SELECT * FROM sales",
            ),
            // Two CTEs - comma between CTEs stripped too
            (
                "WITH T1 AS (SELECT a FROM t1 WHERE id = 1), T2 AS (SELECT b FROM t2) SELECT * FROM T1",
                "WITH T1 SELECT a FROM t1 WHERE id = ? ) T2 SELECT b FROM t2 ) SELECT * FROM T1",
            ),
        ];
        for (input, expected) in cases {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            assert_eq!(got, *expected, "with_cte_stripping input={input:?}");
        }
    }

    #[test]
    fn test_double_quoted_string_value_quantize() {
        // Double-quoted strings in value context (after =) should be quantized
        let config = SqlObfuscateConfig::default();
        let cases = &[
            // After = in SET clause
            (
                r#"update Orders set created = "2019-05-24 00:26:17", gross = 30.28"#,
                "update Orders set created = ? gross = ?",
            ),
            // After = in SET clause, identifier-like value
            (
                r#"update Orders set payment_type = "eventbrite""#,
                "update Orders set payment_type = ?",
            ),
            // Table identifier (after FROM) — keep
            (
                r#"SELECT * FROM "users" WHERE id = 1"#,
                r#"SELECT * FROM users WHERE id = ?"#,
            ),
        ];
        for (input, expected) in cases {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            assert_eq!(got, *expected, "double_quoted_value input={input:?}");
        }
    }

    #[test]
    fn test_normalize_only_dollar_func() {
        // In normalize_only mode, dollar-quoted strings are normalized (not quantized)
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT $func$INSERT INTO table VALUES ( 'a', 1, 2 )$func$ FROM users";
        assert_eq!(got, expected, "normalize_only dollar_func: {got:?}");
    }

    #[test]
    fn test_dollar_quoted_func_trivial_collapse() {
        // When dollar_quoted_func=true and inner content obfuscates to a single ?, collapse to ?
        let config = SqlObfuscateConfig {
            dollar_quoted_func: true,
            replace_digits: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users123 WHERE id = $tag$1$tag$",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT * FROM users? WHERE id = ?";
        assert_eq!(
            got, expected,
            "dollar_quoted_func trivial collapse: {got:?}"
        );
    }

    #[test]
    fn test_obfuscate_only_keeps_quotes_and_semi() {
        // In obfuscate_only mode: keep double-quoted identifiers, keep $?, keep trailing ;
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT "table"."field" FROM "table" WHERE "table"."otherfield" = $? AND "table"."thirdfield" = $?;"#,
            &config,
            DbmsKind::Generic,
        );
        let expected = r#"SELECT "table"."field" FROM "table" WHERE "table"."otherfield" = $? AND "table"."thirdfield" = $?;"#;
        assert_eq!(got, expected, "obfuscate_only keeps quotes/$/semi: {got:?}");
    }

    #[test]
    fn test_obfuscate_only_dollar_quoted_func_no_collapse() {
        // In obfuscate_only+dollar_quoted_func: VALUES inside func are NOT collapsed
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            dollar_quoted_func: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT $func$INSERT INTO table VALUES (?, ?, ?)$func$ FROM users";
        assert_eq!(
            got, expected,
            "obfuscate_only dollar_quoted_func no collapse: {got:?}"
        );
    }

    #[test]
    fn test_normalize_only_procedure() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END",
            &config,
            DbmsKind::Generic,
        );
        let expected =
            "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END";
        assert_eq!(got, expected, "normalize_only+procedure: {got:?}");
    }

    #[test]
    fn test_q41() {
        let config = SqlObfuscateConfig::default();
        let input = "SELECT * FROM public.table ( array [ ROW ( array [ 'magic', 'foo',";
        // First check raw (pre-collapse) output
        let mut tok = super::Tokenizer::new(input, &config, DbmsKind::Generic);
        tok.process();
        let raw = tok.finalize();
        eprintln!("RAW: {raw:?}");
        let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
        let expected = "SELECT * FROM public.table ( array [ ROW ( array [ ?";
        assert_eq!(got, expected, "q41: {got:?}");
    }

    // --- Integration test cases added for faster iteration ---

    #[test]
    fn test_pg_json_operators_7() {
        // JSONB ? operator followed by string literal — both should be kept as ?
        let got = super::obfuscate_sql(
            "select * from users where user.custom ? 'foo'",
            &SqlObfuscateConfig::default(),
            DbmsKind::Postgresql,
        );
        let expected = "select * from users where user.custom ? ?";
        assert_eq!(got, expected, "pg_json_7: {got:?}");
    }

    #[test]
    fn test_quantizer_90() {
        // Inline comment /*!obfuscation*/ should be stripped; consecutive literals after = reset
        let config = SqlObfuscateConfig::default();
        let got = super::obfuscate_sql(
            "SELECT * FROM dbo.Items WHERE id = 1 or /*!obfuscation*/ 1 = 1",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT * FROM dbo.Items WHERE id = ? or ? = ?";
        assert_eq!(got, expected, "q90: {got:?}");
    }

    #[test]
    fn test_cassandra_nested_dates() {
        // Consecutive ? placeholders inside nested function calls should be suppressed
        let config = SqlObfuscateConfig::default();
        let got = super::obfuscate_sql(
            "SELECT TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) FROM t",
            &config,
            DbmsKind::Generic,
        );
        let expected = "SELECT TO_DATE ( TO_CHAR ( TO_DATE ( bar.h, ? ) ) ) FROM t";
        assert_eq!(got, expected, "cassandra_nested_dates: {got:?}");
    }

    #[test]
    fn test_cassandra_pipe_concat() {
        // || concatenation — Go tokenizes as two separate | tokens with spaces
        let config = SqlObfuscateConfig::default();
        let got = super::obfuscate_sql("SELECT a ||?|| b FROM t", &config, DbmsKind::Generic);
        let expected = "SELECT a | | ? | | b FROM t";
        assert_eq!(got, expected, "cassandra_pipe: {got:?}");
    }

    // Test cases from the agent repo
    const SUITE_CASES: &[(&str, &str)] = &[
        // sql_keep_alias_off
        ("SELECT username AS person FROM users WHERE id=4", "SELECT username FROM users WHERE id = ?"),
        // sql_autovacuum_0
        ("autovacuum: VACUUM ANALYZE fake.table", "autovacuum : VACUUM ANALYZE fake.table"),
        // sql_autovacuum_1
        ("autovacuum: VACUUM ANALYZE fake.table_downtime", "autovacuum : VACUUM ANALYZE fake.table_downtime"),
        // sql_autovacuum_2
        ("autovacuum: VACUUM fake.big_table (to prevent wraparound)", "autovacuum : VACUUM fake.big_table ( to prevent wraparound )"),
        // sql_dollar_quoted_func_off
        ("SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users", "SELECT ? FROM users"),
        // sql_metadata_multiline_comment_select_insert
        ("\n/* Multi-line comment */\nSELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO owners (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT", "SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO owners ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"),
        // sql_metadata_multiline_comment_select_insert_lowercase_limit
        ("\n/* Multi-line comment */\nSELECT * FROM clients WHERE (clients.first_name = 'Andy') limit 1 BEGIN INSERT INTO owners (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT", "SELECT * FROM clients WHERE ( clients.first_name = ? ) limit ? BEGIN INSERT INTO owners ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"),
        // sql_metadata_single_line_comments_grant
        ("\n-- Single line comment\n-- Another single line comment\n-- Another another single line comment\nGRANT USAGE, DELETE ON SCHEMA datadog TO datadog", "GRANT USAGE, DELETE ON SCHEMA datadog TO datadog"),
        // sql_metadata_no_collect
        ("\n/*\nMulti-line comment\nwith line breaks\n*/\n/* Two multi-line comments with\nline breaks */\nSELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = 't'", "SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = ?"),
        // sql_metadata_create_trigger
        ("CREATE TRIGGER dogwatcher SELECT ON w1 BEFORE (UPDATE d1 SET (c1, c2, c3) = (c1 + 1, c2 + 1, c3 + 1))", "CREATE TRIGGER dogwatcher SELECT ON w1 BEFORE ( UPDATE d1 SET ( c1, c2, c3 ) = ( c1 + ? c2 + ? c3 + ? ) )"),
        // sql_metadata_table_value_constructor
        ("\n-- Testing table value constructor SQL expression\nSELECT * FROM (VALUES (1, 'dog')) AS d (id, animal)", "SELECT * FROM ( VALUES ( ? ) ) ( id, animal )"),
        // sql_metadata_alter_table
        ("ALTER TABLE table DROP COLUMN column", "ALTER TABLE table DROP COLUMN column"),
        // sql_metadata_revoke
        ("REVOKE ALL ON SCHEMA datadog FROM datadog", "REVOKE ALL ON SCHEMA datadog FROM datadog"),
        // sql_metadata_truncate
        ("TRUNCATE TABLE datadog", "TRUNCATE TABLE datadog"),
        // sql_metadata_explicit_table
        ("\n-- Testing explicit table SQL expression\nWITH T1 AS (SELECT PNO , PNAME , COLOR , WEIGHT , CITY FROM P WHERE  CITY = 'London'),\nT2 AS (SELECT PNO, PNAME, COLOR, WEIGHT, CITY, 2 * WEIGHT AS NEW_WEIGHT, 'Oslo' AS NEW_CITY FROM T1),\nT3 AS ( SELECT PNO , PNAME, COLOR, NEW_WEIGHT AS WEIGHT, NEW_CITY AS CITY FROM T2),\nT4 AS ( TABLE P EXCEPT CORRESPONDING TABLE T1)\nTABLE T4 UNION CORRESPONDING TABLE T3", "WITH T1 SELECT PNO, PNAME, COLOR, WEIGHT, CITY FROM P WHERE CITY = ? ) T2 SELECT PNO, PNAME, COLOR, WEIGHT, CITY, ? * WEIGHT, ? FROM T1 ), T3 SELECT PNO, PNAME, COLOR, NEW_WEIGHT, NEW_CITY FROM T2 ), T4 TABLE P EXCEPT CORRESPONDING TABLE T1 ) TABLE T4 UNION CORRESPONDING TABLE T3"),
        // sql_utf8_catalan_1
        ("SELECT Codi , Nom_CA AS Nom, Descripció_CAT AS Descripció FROM ProtValAptitud WHERE Vigent=1 ORDER BY Ordre, Codi", "SELECT Codi, Nom_CA, Descripció_CAT FROM ProtValAptitud WHERE Vigent = ? ORDER BY Ordre, Codi"),
        // sql_utf8_catalan_2
        (" SELECT  dbo.Treballadors_ProtCIE_AntecedentsPatologics.IdTreballadorsProtCIE_AntecedentsPatologics,   dbo.ProtCIE.Codi As CodiProtCIE, Treballadors_ProtCIE_AntecedentsPatologics.Año,                              dbo.ProtCIE.Nom_ES, dbo.ProtCIE.Nom_CA  FROM         dbo.Treballadors_ProtCIE_AntecedentsPatologics  WITH (NOLOCK)  INNER JOIN                       dbo.ProtCIE  WITH (NOLOCK)  ON dbo.Treballadors_ProtCIE_AntecedentsPatologics.CodiProtCIE = dbo.ProtCIE.Codi  WHERE Treballadors_ProtCIE_AntecedentsPatologics.IdTreballador =  12345 ORDER BY   Treballadors_ProtCIE_AntecedentsPatologics.Año DESC, dbo.ProtCIE.Codi ", "SELECT dbo.Treballadors_ProtCIE_AntecedentsPatologics.IdTreballadorsProtCIE_AntecedentsPatologics, dbo.ProtCIE.Codi, Treballadors_ProtCIE_AntecedentsPatologics.Año, dbo.ProtCIE.Nom_ES, dbo.ProtCIE.Nom_CA FROM dbo.Treballadors_ProtCIE_AntecedentsPatologics WITH ( NOLOCK ) INNER JOIN dbo.ProtCIE WITH ( NOLOCK ) ON dbo.Treballadors_ProtCIE_AntecedentsPatologics.CodiProtCIE = dbo.ProtCIE.Codi WHERE Treballadors_ProtCIE_AntecedentsPatologics.IdTreballador = ? ORDER BY Treballadors_ProtCIE_AntecedentsPatologics.Año DESC, dbo.ProtCIE.Codi"),
        // sql_utf8_catalan_3
        ("select  top 100 percent  IdTrebEmpresa as [IdTrebEmpresa], CodCli as [Client], NOMEMP as [Nom Client], Baixa as [Baixa], CASE WHEN IdCentreTreball IS NULL THEN '-' ELSE  CONVERT(VARCHAR(8),IdCentreTreball) END as [Id Centre],  CASE WHEN NOMESTAB IS NULL THEN '-' ELSE NOMESTAB END  as [Nom Centre],  TIPUS as [Tipus Lloc], CASE WHEN IdLloc IS NULL THEN '-' ELSE  CONVERT(VARCHAR(8),IdLloc) END  as [Id Lloc],  CASE WHEN NomLlocComplert IS NULL THEN '-' ELSE NomLlocComplert END  as [Lloc Treball],  CASE WHEN DesLloc IS NULL THEN '-' ELSE DesLloc END  as [Descripció], IdLlocTreballUnic as [Id Únic]  From ( SELECT    '-' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP,   dbo.Treb_Empresa.Baixa,                      dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, null AS IdLloc,                        null AS NomLlocComplert, dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     dbo.Treb_Empresa.IdTreballador = 64376 AND Treb_Empresa.IdTecEIRLLlocTreball IS NULL AND IdMedEIRLLlocTreball IS NULL AND IdLlocTreballTemporal IS NULL  UNION ALL SELECT    'AV. RIESGO' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa,                       dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdTecEIRLLlocTreball AS IdLloc,                        dbo.fn_NomLlocComposat(dbo.Treb_Empresa.IdTecEIRLLlocTreball) AS NomLlocComplert, dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     (dbo.Treb_Empresa.IdTreballador = 64376) AND (NOT (dbo.Treb_Empresa.IdTecEIRLLlocTreball IS NULL))  UNION ALL SELECT     'EXTERNA' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP,  dbo.Treb_Empresa.Baixa,                      dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdMedEIRLLlocTreball AS IdLloc,                        dbo.fn_NomMedEIRLLlocComposat(dbo.Treb_Empresa.IdMedEIRLLlocTreball) AS NomLlocComplert,  dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     (dbo.Treb_Empresa.IdTreballador = 64376) AND (Treb_Empresa.IdTecEIRLLlocTreball IS NULL) AND (NOT (dbo.Treb_Empresa.IdMedEIRLLlocTreball IS NULL))  UNION ALL SELECT     'TEMPORAL' AS TIPUS,  dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa,                       dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdLlocTreballTemporal AS IdLloc,                       dbo.Lloc_Treball_Temporal.NomLlocTreball AS NomLlocComplert,  dbo.Treb_Empresa.DataInici,                        dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS NULL THEN '' ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM         dbo.Clients  WITH (NOLOCK) INNER JOIN                       dbo.Treb_Empresa  WITH (NOLOCK) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli INNER JOIN                       dbo.Lloc_Treball_Temporal  WITH (NOLOCK) ON dbo.Treb_Empresa.IdLlocTreballTemporal = dbo.Lloc_Treball_Temporal.IdLlocTreballTemporal LEFT OUTER JOIN                       dbo.Cli_Establiments  WITH (NOLOCK) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND                        dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE     dbo.Treb_Empresa.IdTreballador = 64376 AND Treb_Empresa.IdTecEIRLLlocTreball IS NULL AND IdMedEIRLLlocTreball IS NULL ) as taula  Where 1=0 ", "select top ? percent IdTrebEmpresa, CodCli, NOMEMP, Baixa, CASE WHEN IdCentreTreball IS ? THEN ? ELSE CONVERT ( VARCHAR ( ? ) IdCentreTreball ) END, CASE WHEN NOMESTAB IS ? THEN ? ELSE NOMESTAB END, TIPUS, CASE WHEN IdLloc IS ? THEN ? ELSE CONVERT ( VARCHAR ( ? ) IdLloc ) END, CASE WHEN NomLlocComplert IS ? THEN ? ELSE NomLlocComplert END, CASE WHEN DesLloc IS ? THEN ? ELSE DesLloc END, IdLlocTreballUnic From ( SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, ?, ?, dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE dbo.Treb_Empresa.IdTreballador = ? AND Treb_Empresa.IdTecEIRLLlocTreball IS ? AND IdMedEIRLLlocTreball IS ? AND IdLlocTreballTemporal IS ? UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdTecEIRLLlocTreball, dbo.fn_NomLlocComposat ( dbo.Treb_Empresa.IdTecEIRLLlocTreball ), dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE ( dbo.Treb_Empresa.IdTreballador = ? ) AND ( NOT ( dbo.Treb_Empresa.IdTecEIRLLlocTreball IS ? ) ) UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdMedEIRLLlocTreball, dbo.fn_NomMedEIRLLlocComposat ( dbo.Treb_Empresa.IdMedEIRLLlocTreball ), dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE ( dbo.Treb_Empresa.IdTreballador = ? ) AND ( Treb_Empresa.IdTecEIRLLlocTreball IS ? ) AND ( NOT ( dbo.Treb_Empresa.IdMedEIRLLlocTreball IS ? ) ) UNION ALL SELECT ?, dbo.Treb_Empresa.IdTrebEmpresa, dbo.Treb_Empresa.IdTreballador, dbo.Treb_Empresa.CodCli, dbo.Clients.NOMEMP, dbo.Treb_Empresa.Baixa, dbo.Treb_Empresa.IdCentreTreball, dbo.Cli_Establiments.NOMESTAB, dbo.Treb_Empresa.IdLlocTreballTemporal, dbo.Lloc_Treball_Temporal.NomLlocTreball, dbo.Treb_Empresa.DataInici, dbo.Treb_Empresa.DataFi, CASE WHEN dbo.Treb_Empresa.DesLloc IS ? THEN ? ELSE dbo.Treb_Empresa.DesLloc END DesLloc, dbo.Treb_Empresa.IdLlocTreballUnic FROM dbo.Clients WITH ( NOLOCK ) INNER JOIN dbo.Treb_Empresa WITH ( NOLOCK ) ON dbo.Clients.CODCLI = dbo.Treb_Empresa.CodCli INNER JOIN dbo.Lloc_Treball_Temporal WITH ( NOLOCK ) ON dbo.Treb_Empresa.IdLlocTreballTemporal = dbo.Lloc_Treball_Temporal.IdLlocTreballTemporal LEFT OUTER JOIN dbo.Cli_Establiments WITH ( NOLOCK ) ON dbo.Cli_Establiments.Id_ESTAB_CLI = dbo.Treb_Empresa.IdCentreTreball AND dbo.Cli_Establiments.CODCLI = dbo.Treb_Empresa.CodCli WHERE dbo.Treb_Empresa.IdTreballador = ? AND Treb_Empresa.IdTecEIRLLlocTreball IS ? AND IdMedEIRLLlocTreball IS ? ) Where ? = ?"),
        // sql_utf8_catalan_4
        ("select  IdHistLabAnt as [IdHistLabAnt], IdTreballador as [IdTreballador], Empresa as [Professió], Anys as [Anys],  Riscs as [Riscos], Nom_CA AS [Prot CNO], Nom_ES as [Prot CNO Altre Idioma]   From ( SELECT     dbo.Treb_HistAnt.IdHistLabAnt, dbo.Treb_HistAnt.IdTreballador,           dbo.Treb_HistAnt.Empresa, dbo.Treb_HistAnt.Anys, dbo.Treb_HistAnt.Riscs, dbo.Treb_HistAnt.CodiProtCNO,           dbo.ProtCNO.Nom_ES, dbo.ProtCNO.Nom_CA  FROM     dbo.Treb_HistAnt  WITH (NOLOCK) LEFT OUTER JOIN                       dbo.ProtCNO  WITH (NOLOCK) ON dbo.Treb_HistAnt.CodiProtCNO = dbo.ProtCNO.Codi  Where  dbo.Treb_HistAnt.IdTreballador = 12345 ) as taula ", "select IdHistLabAnt, IdTreballador, Empresa, Anys, Riscs, Nom_CA, Nom_ES From ( SELECT dbo.Treb_HistAnt.IdHistLabAnt, dbo.Treb_HistAnt.IdTreballador, dbo.Treb_HistAnt.Empresa, dbo.Treb_HistAnt.Anys, dbo.Treb_HistAnt.Riscs, dbo.Treb_HistAnt.CodiProtCNO, dbo.ProtCNO.Nom_ES, dbo.ProtCNO.Nom_CA FROM dbo.Treb_HistAnt WITH ( NOLOCK ) LEFT OUTER JOIN dbo.ProtCNO WITH ( NOLOCK ) ON dbo.Treb_HistAnt.CodiProtCNO = dbo.ProtCNO.Codi Where dbo.Treb_HistAnt.IdTreballador = ? )"),
        // sql_utf8_catalan_5
        ("SELECT     Cli_Establiments.CODCLI, Cli_Establiments.Id_ESTAB_CLI As [Código Centro Trabajo], Cli_Establiments.CODIGO_CENTRO_AXAPTA As [Código C. Axapta],  Cli_Establiments.NOMESTAB As [Nombre],                                 Cli_Establiments.ADRECA As [Dirección], Cli_Establiments.CodPostal As [Código Postal], Cli_Establiments.Poblacio as [Población], Cli_Establiments.Provincia,                                Cli_Establiments.TEL As [Tel],  Cli_Establiments.EMAIL As [EMAIL],                                Cli_Establiments.PERS_CONTACTE As [Contacto], Cli_Establiments.PERS_CONTACTE_CARREC As [Cargo Contacto], Cli_Establiments.NumTreb As [Plantilla],                                Cli_Establiments.Localitzacio As [Localización], Tipus_Activitat.CNAE, Tipus_Activitat.Nom_ES As [Nombre Actividad], ACTIVO AS [Activo]                        FROM         Cli_Establiments LEFT OUTER JOIN                                    Tipus_Activitat ON Cli_Establiments.Id_ACTIVITAT = Tipus_Activitat.IdActivitat                        Where CODCLI = '01234' AND CENTRE_CORRECTE = 3 AND ACTIVO = 5                        ORDER BY Cli_Establiments.CODIGO_CENTRO_AXAPTA ", "SELECT Cli_Establiments.CODCLI, Cli_Establiments.Id_ESTAB_CLI, Cli_Establiments.CODIGO_CENTRO_AXAPTA, Cli_Establiments.NOMESTAB, Cli_Establiments.ADRECA, Cli_Establiments.CodPostal, Cli_Establiments.Poblacio, Cli_Establiments.Provincia, Cli_Establiments.TEL, Cli_Establiments.EMAIL, Cli_Establiments.PERS_CONTACTE, Cli_Establiments.PERS_CONTACTE_CARREC, Cli_Establiments.NumTreb, Cli_Establiments.Localitzacio, Tipus_Activitat.CNAE, Tipus_Activitat.Nom_ES, ACTIVO FROM Cli_Establiments LEFT OUTER JOIN Tipus_Activitat ON Cli_Establiments.Id_ACTIVITAT = Tipus_Activitat.IdActivitat Where CODCLI = ? AND CENTRE_CORRECTE = ? AND ACTIVO = ? ORDER BY Cli_Establiments.CODIGO_CENTRO_AXAPTA"),
        // sql_utf8_dollar_field
        ("select * from dollarField$ as df from some$dollar$filled_thing$$;", "select * from dollarField$ from some$dollar$filled_thing$$"),
        // sql_utf8_backtick_jp
        ("select * from `構わない`;", "select * from 構わない"),
        // sql_utf8_replacement_chars_1
        ("select * from names where name like '�����';", "select * from names where name like ?"),
        // sql_utf8_replacement_chars_2
        ("select replacement from table where replacement = 'i�n�t�e��rspersed';", "select replacement from table where replacement = ?"),
        // sql_utf8_replacement_char_3
        ("SELECT ('�');", "SELECT ( ? )"),
        // sql_replace_digits_off_0
        ("REPLACE INTO sales_2019_07_01 (`itemID`, `date`, `qty`, `price`) VALUES ((SELECT itemID FROM item1001 WHERE `sku` = [sku]), CURDATE(), [qty], 0.00)", "REPLACE INTO sales_2019_07_01 ( itemID, date, qty, price ) VALUES ( ( SELECT itemID FROM item1001 WHERE sku = [ sku ] ), CURDATE ( ), [ qty ], ? )"),
        // sql_replace_digits_off_1
        ("SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = 2 AND ddh19.name = 'datadog'", "SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = ? AND ddh19.name = ?"),
        // sql_replace_digits_off_2
        ("SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk", "SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk"),
        // sql_replace_digits_off_3
        ("SELECT daily_values1529.*, LEAST((5040000 - @runtot), value1830) AS value1830,\n(@runtot := @runtot + daily_values1529.value1830) AS total\nFROM (SELECT @runtot:=0) AS n,\ndaily_values1529 WHERE daily_values1529.subject_id = 12345 AND daily_values1592.subject_type = 'Skippity'\nAND (daily_values1529.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date", "SELECT daily_values1529.*, LEAST ( ( ? - @runtot ), value1830 ), ( @runtot := @runtot + daily_values1529.value1830 ) FROM ( SELECT @runtot := ? ), daily_values1529 WHERE daily_values1529.subject_id = ? AND daily_values1592.subject_type = ? AND ( daily_values1529.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"),
        // sql_replace_digits_off_4
        ("WITH sales AS\n(SELECT sf2.*\n\tFROM gosalesdw28391.sls_order_method_dim AS md,\n\t\tgosalesdw1920.sls_product_dim391 AS pd190,\n\t\tgosalesdw3819.emp_employee_dim AS ed,\n\t\tgosalesdw3919.sls_sales_fact3819 AS sf2\n\tWHERE pd190.product_key = sf2.product_key\n\tAND pd190.product_number381 > 10000\n\tAND pd190.base_product_key > 30\n\tAND md.order_method_key = sf2.order_method_key8319\n\tAND md.order_method_code > 5\n\tAND ed.employee_key = sf2.employee_key\n\tAND ed.manager_code1 > 20),\ninventory3118 AS\n(SELECT if.*\n\tFROM gosalesdw1592.go_branch_dim AS bd3221,\n\tgosalesdw.dist_inventory_fact AS if\n\tWHERE if.branch_key = bd3221.branch_key\n\tAND bd3221.branch_code > 20)\nSELECT sales1828.product_key AS PROD_KEY,\nSUM(CAST (inventory3118.quantity_shipped AS BIGINT)) AS INV_SHIPPED3118,\nSUM(CAST (sales1828.quantity AS BIGINT)) AS PROD_QUANTITY,\nRANK() OVER ( ORDER BY SUM(CAST (sales1828.quantity AS BIGINT)) DESC) AS PROD_RANK\nFROM sales1828, inventory3118\nWHERE sales1828.product_key = inventory3118.product_key\nGROUP BY sales1828.product_key", "WITH sales SELECT sf2.* FROM gosalesdw28391.sls_order_method_dim, gosalesdw1920.sls_product_dim391, gosalesdw3819.emp_employee_dim, gosalesdw3919.sls_sales_fact3819 WHERE pd190.product_key = sf2.product_key AND pd190.product_number381 > ? AND pd190.base_product_key > ? AND md.order_method_key = sf2.order_method_key8319 AND md.order_method_code > ? AND ed.employee_key = sf2.employee_key AND ed.manager_code1 > ? ) inventory3118 SELECT if.* FROM gosalesdw1592.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd3221.branch_key AND bd3221.branch_code > ? ) SELECT sales1828.product_key, SUM ( CAST ( inventory3118.quantity_shipped ) ), SUM ( CAST ( sales1828.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales1828.quantity ) ) DESC ) FROM sales1828, inventory3118 WHERE sales1828.product_key = inventory3118.product_key GROUP BY sales1828.product_key"),
        // sql_quantizer_0
        ("SELECT \"table\".\"field\" FROM \"table\" WHERE \"table\".\"otherfield\" = $? AND \"table\".\"thirdfield\" = $?;", "SELECT table . field FROM table WHERE table . otherfield = ? AND table . thirdfield = ?"),
        // sql_quantizer_1
        ("select * from users where id = 42", "select * from users where id = ?"),
        // sql_quantizer_2
        ("select * from users where float = .43422", "select * from users where float = ?"),
        // sql_quantizer_3
        ("SELECT host, status FROM ec2_status WHERE org_id = 42", "SELECT host, status FROM ec2_status WHERE org_id = ?"),
        // sql_quantizer_4
        ("SELECT host, status FROM ec2_status WHERE org_id=42", "SELECT host, status FROM ec2_status WHERE org_id = ?"),
        // sql_quantizer_5
        ("-- get user \n--\n select * \n   from users \n    where\n       id = 214325346", "select * from users where id = ?"),
        // sql_quantizer_6
        ("SELECT * FROM `host` WHERE `id` IN (42, 43) /*comment with parameters,host:localhost,url:controller#home,id:FF005:00CAA*/", "SELECT * FROM host WHERE id IN ( ? )"),
        // sql_quantizer_7
        ("SELECT `host`.`address` FROM `host` WHERE org_id=42", "SELECT host . address FROM host WHERE org_id = ?"),
        // sql_quantizer_8
        ("SELECT \"host\".\"address\" FROM \"host\" WHERE org_id=42", "SELECT host . address FROM host WHERE org_id = ?"),
        // sql_quantizer_9
        ("SELECT * FROM host WHERE id IN (42, 43) /*\n\t\t\tmultiline comment with parameters,\n\t\t\thost:localhost,url:controller#home,id:FF005:00CAA\n\t\t\t*/", "SELECT * FROM host WHERE id IN ( ? )"),
        // sql_quantizer_10
        ("UPDATE user_dash_pref SET json_prefs = %(json_prefs)s, modified = '2015-08-27 22:10:32.492912' WHERE user_id = %(user_id)s AND url = %(url)s", "UPDATE user_dash_pref SET json_prefs = ? modified = ? WHERE user_id = ? AND url = ?"),
        // sql_quantizer_11
        ("SELECT DISTINCT host.id AS host_id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = %(org_id_1)s AND host.name NOT IN (%(name_1)s) AND host.name IN (%(name_2)s, %(name_3)s, %(name_4)s, %(name_5)s)", "SELECT DISTINCT host.id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = ? AND host.name NOT IN ( ? ) AND host.name IN ( ? )"),
        // sql_quantizer_12
        ("SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = %(org_id)s AND metric_key = ANY(array[75])", "SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( array [ ? ] )"),
        // sql_quantizer_13
        ("SELECT org_id, metric_key   FROM metrics_metadata   WHERE org_id = %(org_id)s AND metric_key = ANY(array[21, 25, 32])", "SELECT org_id, metric_key FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( array [ ? ] )"),
        // sql_quantizer_14
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_lowercase_limit
        ("SELECT articles.* FROM articles WHERE articles.id = 1 limit 1", "SELECT articles.* FROM articles WHERE articles.id = ? limit ?"),
        // sql_quantizer_15
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1, 20", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_16
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1, 20;", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_limit_two_arguments_lowercase
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1, 20;", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_17
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 15,20;", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_18
        ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1;", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
        // sql_quantizer_19
        ("SELECT articles.* FROM articles WHERE (articles.created_at BETWEEN '2016-10-31 23:00:00.000000' AND '2016-11-01 23:00:00.000000')", "SELECT articles.* FROM articles WHERE ( articles.created_at BETWEEN ? AND ? )"),
        // sql_quantizer_20
        ("SELECT articles.* FROM articles WHERE (articles.created_at BETWEEN $1 AND $2)", "SELECT articles.* FROM articles WHERE ( articles.created_at BETWEEN ? AND ? )"),
        // sql_quantizer_21
        ("SELECT articles.* FROM articles WHERE (articles.published != true)", "SELECT articles.* FROM articles WHERE ( articles.published != ? )"),
        // sql_quantizer_22
        ("SELECT articles.* FROM articles WHERE (title = 'guides.rubyonrails.org')", "SELECT articles.* FROM articles WHERE ( title = ? )"),
        // sql_quantizer_23
        ("SELECT articles.* FROM articles WHERE ( title = ? ) AND ( author = ? )", "SELECT articles.* FROM articles WHERE ( title = ? ) AND ( author = ? )"),
        // sql_quantizer_24
        ("SELECT articles.* FROM articles WHERE ( title = :title )", "SELECT articles.* FROM articles WHERE ( title = :title )"),
        // sql_quantizer_25
        ("SELECT articles.* FROM articles WHERE ( title = @title )", "SELECT articles.* FROM articles WHERE ( title = @title )"),
        // sql_quantizer_26
        ("SELECT date(created_at) as ordered_date, sum(price) as total_price FROM orders GROUP BY date(created_at) HAVING sum(price) > 100", "SELECT date ( created_at ), sum ( price ) FROM orders GROUP BY date ( created_at ) HAVING sum ( price ) > ?"),
        // sql_quantizer_27
        ("SELECT * FROM articles WHERE id > 10 ORDER BY id asc LIMIT 20", "SELECT * FROM articles WHERE id > ? ORDER BY id asc LIMIT ?"),
        // sql_quantizer_28
        ("SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = 't'", "SELECT clients.* FROM clients INNER JOIN posts ON posts.author_id = author.id AND posts.published = ?"),
        // sql_quantizer_29
        ("SELECT articles.* FROM articles WHERE articles.id IN (1, 3, 5)", "SELECT articles.* FROM articles WHERE articles.id IN ( ? )"),
        // sql_quantizer_30
        ("SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO clients (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT", "SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO clients ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"),
        // sql_quantizer_31
        ("SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 15, 25 BEGIN INSERT INTO clients (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT", "SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO clients ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"),
        // sql_quantizer_32
        ("SAVEPOINT \"s139956586256192_x1\"", "SAVEPOINT ?"),
        // sql_quantizer_33
        ("INSERT INTO user (id, username) VALUES ('Fred','Smith'), ('John','Smith'), ('Michael','Smith'), ('Robert','Smith');", "INSERT INTO user ( id, username ) VALUES ( ? )"),
        // sql_quantizer_34
        ("CREATE KEYSPACE Excelsior WITH replication = {'class': 'SimpleStrategy', 'replication_factor' : 3};", "CREATE KEYSPACE Excelsior WITH replication = ?"),
        // sql_quantizer_35
        ("SELECT \"webcore_page\".\"id\" FROM \"webcore_page\" WHERE \"webcore_page\".\"slug\" = %s ORDER BY \"webcore_page\".\"path\" ASC LIMIT 1", "SELECT webcore_page . id FROM webcore_page WHERE webcore_page . slug = ? ORDER BY webcore_page . path ASC LIMIT ?"),
        // sql_quantizer_36
        ("SELECT server_table.host AS host_id FROM table#.host_tags as server_table WHERE server_table.host_id = 50", "SELECT server_table.host FROM table#.host_tags WHERE server_table.host_id = ?"),
        // sql_quantizer_37
        ("INSERT INTO delayed_jobs (attempts, created_at, failed_at, handler, last_error, locked_at, locked_by, priority, queue, run_at, updated_at) VALUES (0, '2016-12-04 17:09:59', NULL, '--- !ruby/object:Delayed::PerformableMethod\nobject: !ruby/object:Item\n  store:\n  - a simple string\n  - an \\'escaped \\' string\n  - another \\'escaped\\' string\n  - 42\n  string: a string with many \\\\\\\\\\'escapes\\\\\\\\\\'\nmethod_name: :show_store\nargs: []\n', NULL, NULL, NULL, 0, NULL, '2016-12-04 17:09:59', '2016-12-04 17:09:59')", "INSERT INTO delayed_jobs ( attempts, created_at, failed_at, handler, last_error, locked_at, locked_by, priority, queue, run_at, updated_at ) VALUES ( ? )"),
        // sql_quantizer_38
        ("SELECT name, pretty_print(address) FROM people;", "SELECT name, pretty_print ( address ) FROM people"),
        // sql_quantizer_39
        ("* SELECT * FROM fake_data(1, 2, 3);", "* SELECT * FROM fake_data ( ? )"),
        // sql_quantizer_40
        ("CREATE FUNCTION add(integer, integer) RETURNS integer\n AS 'select $1 + $2;'\n LANGUAGE SQL\n IMMUTABLE\n RETURNS NULL ON NULL INPUT;", "CREATE FUNCTION add ( integer, integer ) RETURNS integer LANGUAGE SQL IMMUTABLE RETURNS ? ON ? INPUT"),
        // sql_quantizer_41
        ("SELECT * FROM public.table ( array [ ROW ( array [ 'magic', 'foo',", "SELECT * FROM public.table ( array [ ROW ( array [ ?"),
        // sql_quantizer_42
        ("SELECT pg_try_advisory_lock (123) AS t46eef3f025cc27feb31ca5a2d668a09a", "SELECT pg_try_advisory_lock ( ? )"),
        // sql_quantizer_43
        ("INSERT INTO `qual-aa`.issues (alert0 , alert1) VALUES (NULL, NULL)", "INSERT INTO qual-aa . issues ( alert0, alert1 ) VALUES ( ? )"),
        // sql_quantizer_44
        ("INSERT INTO user (id, email, name) VALUES (null, ?, ?)", "INSERT INTO user ( id, email, name ) VALUES ( ? )"),
        // sql_quantizer_45
        ("select * from users where id = 214325346     # This comment continues to the end of line", "select * from users where id = ?"),
        // sql_quantizer_46
        ("select * from users where id = 214325346     -- This comment continues to the end of line", "select * from users where id = ?"),
        // sql_quantizer_47
        ("SELECT * FROM /* this is an in-line comment */ users;", "SELECT * FROM users"),
        // sql_quantizer_48
        ("SELECT /*! STRAIGHT_JOIN */ col1 FROM table1", "SELECT col1 FROM table1"),
        // sql_quantizer_49
        ("DELETE FROM t1\n\t\t\tWHERE s11 > ANY\n\t\t\t(SELECT COUNT(*) /* no hint */ FROM t2\n\t\t\tWHERE NOT EXISTS\n\t\t\t(SELECT * FROM t3\n\t\t\tWHERE ROW(5*t2.s1,77)=\n\t\t\t(SELECT 50,11*s1 FROM t4 UNION SELECT 50,77 FROM\n\t\t\t(SELECT * FROM t5) AS t5)));", "DELETE FROM t1 WHERE s11 > ANY ( SELECT COUNT ( * ) FROM t2 WHERE NOT EXISTS ( SELECT * FROM t3 WHERE ROW ( ? * t2.s1, ? ) = ( SELECT ? * s1 FROM t4 UNION SELECT ? FROM ( SELECT * FROM t5 ) ) ) )"),
        // sql_quantizer_50
        ("SET @g = 'POLYGON((0 0,10 0,10 10,0 10,0 0),(5 5,7 5,7 7,5 7, 5 5))';", "SET @g = ?"),
        // sql_quantizer_51
        ("SELECT daily_values.*,\n                    LEAST((5040000 - @runtot), value) AS value,\n                    (@runtot := @runtot + daily_values.value) AS total FROM (SELECT @runtot:=0) AS n, `daily_values`  WHERE `daily_values`.`subject_id` = 12345 AND `daily_values`.`subject_type` = 'Skippity' AND (daily_values.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date", "SELECT daily_values.*, LEAST ( ( ? - @runtot ), value ), ( @runtot := @runtot + daily_values.value ) FROM ( SELECT @runtot := ? ), daily_values WHERE daily_values . subject_id = ? AND daily_values . subject_type = ? AND ( daily_values.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"),
        // sql_quantizer_52
        ("    SELECT\n      t1.userid,\n      t1.fullname,\n      t1.firm_id,\n      t2.firmname,\n      t1.email,\n      t1.location,\n      t1.state,\n      t1.phone,\n      t1.url,\n      DATE_FORMAT( t1.lastmod, \"%m/%d/%Y %h:%i:%s\" ) AS lastmod,\n      t1.lastmod AS lastmod_raw,\n      t1.user_status,\n      t1.pw_expire,\n      DATE_FORMAT( t1.pw_expire, \"%m/%d/%Y\" ) AS pw_expire_date,\n      t1.addr1,\n      t1.addr2,\n      t1.zipcode,\n      t1.office_id,\n      t1.default_group,\n      t3.firm_status,\n      t1.title\n    FROM\n           userdata      AS t1\n      LEFT JOIN lawfirm_names AS t2 ON t1.firm_id = t2.firm_id\n      LEFT JOIN lawfirms      AS t3 ON t1.firm_id = t3.firm_id\n    WHERE\n      t1.userid = 'jstein'\n\n  ", "SELECT t1.userid, t1.fullname, t1.firm_id, t2.firmname, t1.email, t1.location, t1.state, t1.phone, t1.url, DATE_FORMAT ( t1.lastmod, %m/%d/%Y %h:%i:%s ), t1.lastmod, t1.user_status, t1.pw_expire, DATE_FORMAT ( t1.pw_expire, %m/%d/%Y ), t1.addr1, t1.addr2, t1.zipcode, t1.office_id, t1.default_group, t3.firm_status, t1.title FROM userdata LEFT JOIN lawfirm_names ON t1.firm_id = t2.firm_id LEFT JOIN lawfirms ON t1.firm_id = t3.firm_id WHERE t1.userid = ?"),
        // sql_quantizer_53
        ("SELECT [b].[BlogId], [b].[Name]\nFROM [Blogs] AS [b]\nORDER BY [b].[Name]", "SELECT [ b ] . [ BlogId ], [ b ] . [ Name ] FROM [ Blogs ] ORDER BY [ b ] . [ Name ]"),
        // sql_quantizer_54
        ("SELECT * FROM users WHERE firstname=''", "SELECT * FROM users WHERE firstname = ?"),
        // sql_quantizer_55
        ("SELECT * FROM users WHERE firstname=' '", "SELECT * FROM users WHERE firstname = ?"),
        // sql_quantizer_56
        ("SELECT * FROM users WHERE firstname=\"\"", "SELECT * FROM users WHERE firstname = ?"),
        // sql_quantizer_57
        ("SELECT * FROM users WHERE lastname=\" \"", "SELECT * FROM users WHERE lastname = ?"),
        // sql_quantizer_58
        ("SELECT * FROM users WHERE lastname=\"\t \"", "SELECT * FROM users WHERE lastname = ?"),
        // sql_quantizer_59
        ("SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id = ? AND visitor_id IS ? UNION SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id IS ? AND visitor_id = \"AA0DKTGEM6LRN3WWPZ01Q61E3J7ROX7O\" ORDER BY customer_id DESC", "SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id = ? AND visitor_id IS ? UNION SELECT customer_item_list_id, customer_id FROM customer_item_list WHERE type = wishlist AND customer_id IS ? AND visitor_id = ? ORDER BY customer_id DESC"),
        // sql_quantizer_60
        ("update Orders set created = \"2019-05-24 00:26:17\", gross = 30.28, payment_type = \"eventbrite\", mg_fee = \"3.28\", fee_collected = \"3.28\", event = 59366262, status = \"10\", survey_type = 'direct', tx_time_limit = 480, invite = \"\", ip_address = \"69.215.148.82\", currency = 'USD', gross_USD = \"30.28\", tax_USD = 0.00, journal_activity_id = 4044659812798558774, eb_tax = 0.00, eb_tax_USD = 0.00, cart_uuid = \"160b450e7df511e9810e0a0c06de92f8\", changed = '2019-05-24 00:26:17' where id = ?", "update Orders set created = ? gross = ? payment_type = ? mg_fee = ? fee_collected = ? event = ? status = ? survey_type = ? tx_time_limit = ? invite = ? ip_address = ? currency = ? gross_USD = ? tax_USD = ? journal_activity_id = ? eb_tax = ? eb_tax_USD = ? cart_uuid = ? changed = ? where id = ?"),
        // sql_quantizer_61
        ("update Attendees set email = '626837270@qq.com', first_name = \"贺新春送猪福加企鹅１０５４９４８０００领９８綵斟\", last_name = '王子１９８４４２ｃｏｍ体验猪多优惠', journal_activity_id = 4246684839261125564, changed = \"2019-05-24 00:26:22\" where id = 123", "update Attendees set email = ? first_name = ? last_name = ? journal_activity_id = ? changed = ? where id = ?"),
        // sql_quantizer_62
        ("SELECT\r\n\t                CodiFormacio\r\n\t                ,DataInici\r\n\t                ,DataFi\r\n\t                ,Tipo\r\n\t                ,CodiTecnicFormador\r\n\t                ,p.nombre AS TutorNombre\r\n\t                ,p.mail AS TutorMail\r\n\t                ,Sessions.Direccio\r\n\t                ,Sessions.NomEmpresa\r\n\t                ,Sessions.Telefon\r\n                FROM\r\n                ----------------------------\r\n                (SELECT\r\n\t                CodiFormacio\r\n\t                ,case\r\n\t                   when ModalitatSessio = '1' then 'Presencial'--Teoria\r\n\t                   when ModalitatSessio = '2' then 'Presencial'--Practica\r\n\t                   when ModalitatSessio = '3' then 'Online'--Tutoria\r\n                       when ModalitatSessio = '4' then 'Presencial'--Examen\r\n\t                   ELSE 'Presencial'\r\n\t                end as Tipo\r\n\t                ,ModalitatSessio\r\n\t                ,DataInici\r\n\t                ,DataFi\r\n                     ,NomEmpresa\r\n\t                ,Telefon\r\n\t                ,CodiTecnicFormador\r\n\t                ,CASE\r\n\t                   WHEn EsAltres = 1 then FormacioLlocImparticioDescripcio\r\n\t                   else Adreca + ' - ' + CodiPostal + ' ' + Poblacio\r\n\t                end as Direccio\r\n\t\r\n                FROM Consultas.dbo.View_AsActiva__FormacioSessions_InfoLlocImparticio) AS Sessions\r\n                ----------------------------------------\r\n                LEFT JOIN Consultas.dbo.View_AsActiva_Operari AS o\r\n\t                ON o.CodiOperari = Sessions.CodiTecnicFormador\r\n                LEFT JOIN MainAPP.dbo.persona AS p\r\n\t                ON 'preven\\' + o.codioperari = p.codi\r\n                WHERE Sessions.CodiFormacio = 'F00000017898'", "SELECT CodiFormacio, DataInici, DataFi, Tipo, CodiTecnicFormador, p.nombre, p.mail, Sessions.Direccio, Sessions.NomEmpresa, Sessions.Telefon FROM ( SELECT CodiFormacio, case when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? when ModalitatSessio = ? then ? ELSE ? end, ModalitatSessio, DataInici, DataFi, NomEmpresa, Telefon, CodiTecnicFormador, CASE WHEn EsAltres = ? then FormacioLlocImparticioDescripcio else Adreca + ? + CodiPostal + ? + Poblacio end FROM Consultas.dbo.View_AsActiva__FormacioSessions_InfoLlocImparticio ) LEFT JOIN Consultas.dbo.View_AsActiva_Operari ON o.CodiOperari = Sessions.CodiTecnicFormador LEFT JOIN MainAPP.dbo.persona ON ? + o.codioperari = p.codi WHERE Sessions.CodiFormacio = ?"),
        // sql_quantizer_63
        ("SELECT * FROM foo LEFT JOIN bar ON 'backslash\\' = foo.b WHERE foo.name = 'String'", "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"),
        // sql_quantizer_64
        ("SELECT * FROM foo LEFT JOIN bar ON 'backslash\\' = foo.b LEFT JOIN bar2 ON 'backslash2\\' = foo.b2 WHERE foo.name = 'String'", "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b LEFT JOIN bar2 ON ? = foo.b2 WHERE foo.name = ?"),
        // sql_quantizer_65
        ("SELECT * FROM foo LEFT JOIN bar ON 'embedded ''quote'' in string' = foo.b WHERE foo.name = 'String'", "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"),
        // sql_quantizer_66
        ("SELECT * FROM foo LEFT JOIN bar ON 'embedded \\'quote\\' in string' = foo.b WHERE foo.name = 'String'", "SELECT * FROM foo LEFT JOIN bar ON ? = foo.b WHERE foo.name = ?"),
        // sql_quantizer_67
        ("SELECT org_id,metric_key,metric_type,interval FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY(ARRAY[?,?,?,?,?])", "SELECT org_id, metric_key, metric_type, interval FROM metrics_metadata WHERE org_id = ? AND metric_key = ANY ( ARRAY [ ? ] )"),
        // sql_quantizer_68
        ("SELECT wp_woocommerce_order_items.order_id As No_Commande\n\t\t\tFROM  wp_woocommerce_order_items\n\t\t\tLEFT JOIN\n\t\t\t\t(\n\t\t\t\t\tSELECT meta_value As Prenom\n\t\t\t\t\tFROM wp_postmeta\n\t\t\t\t\tWHERE meta_key = '_shipping_first_name'\n\t\t\t\t) AS a\n\t\t\tON wp_woocommerce_order_items.order_id = a.post_id\n\t\t\tWHERE  wp_woocommerce_order_items.order_id =2198", "SELECT wp_woocommerce_order_items.order_id FROM wp_woocommerce_order_items LEFT JOIN ( SELECT meta_value FROM wp_postmeta WHERE meta_key = ? ) ON wp_woocommerce_order_items.order_id = a.post_id WHERE wp_woocommerce_order_items.order_id = ?"),
        // sql_quantizer_69
        ("SELECT a :: VARCHAR(255) FROM foo WHERE foo.name = 'String'", "SELECT a :: VARCHAR ( ? ) FROM foo WHERE foo.name = ?"),
        // sql_quantizer_70
        ("SELECT MIN(`scoped_49a39c4cc9ae4fdda07bcf49e99f8224`.`scoped_8720d2c0e0824ec2910ab9479085839c`) AS `MIN_BECR_DATE_CREATED` FROM (SELECT `49a39c4cc9ae4fdda07bcf49e99f8224`.`submittedOn` AS `scoped_8720d2c0e0824ec2910ab9479085839c`, `49a39c4cc9ae4fdda07bcf49e99f8224`.`domain` AS `scoped_847e4dcfa1c54d72aad6dbeb231c46de`, `49a39c4cc9ae4fdda07bcf49e99f8224`.`eventConsumer` AS `scoped_7b2f7b8da15646d1b75aa03901460eb2`, `49a39c4cc9ae4fdda07bcf49e99f8224`.`eventType` AS `scoped_77a1b9308b384a9391b69d24335ba058` FROM (`SorDesignTime`.`businessEventConsumerRegistry_947a74dad4b64be9847d67f466d26f5e` AS `49a39c4cc9ae4fdda07bcf49e99f8224`) WHERE (`49a39c4cc9ae4fdda07bcf49e99f8224`.`systemData.ClientID`) = ('35c1ccc0-a83c-4812-a189-895e9d4dd223')) AS `scoped_49a39c4cc9ae4fdda07bcf49e99f8224` WHERE ((`scoped_49a39c4cc9ae4fdda07bcf49e99f8224`.`scoped_847e4dcfa1c54d72aad6dbeb231c46de`) = ('Benefits') AND ((`scoped_49a39c4cc9ae4fdda07bcf49e99f8224`.`scoped_7b2f7b8da15646d1b75aa03901460eb2`) = ('benefits') AND (`scoped_49a39c4cc9ae4fdda07bcf49e99f8224`.`scoped_77a1b9308b384a9391b69d24335ba058`) = ('DMXSync'))); ", "SELECT MIN ( scoped_49a39c4cc9ae4fdda07bcf49e99f8224 . scoped_8720d2c0e0824ec2910ab9479085839c ) FROM ( SELECT 49a39c4cc9ae4fdda07bcf49e99f8224 . submittedOn, 49a39c4cc9ae4fdda07bcf49e99f8224 . domain, 49a39c4cc9ae4fdda07bcf49e99f8224 . eventConsumer, 49a39c4cc9ae4fdda07bcf49e99f8224 . eventType FROM ( SorDesignTime . businessEventConsumerRegistry_947a74dad4b64be9847d67f466d26f5e ) WHERE ( 49a39c4cc9ae4fdda07bcf49e99f8224 . systemData.ClientID ) = ( ? ) ) WHERE ( ( scoped_49a39c4cc9ae4fdda07bcf49e99f8224 . scoped_847e4dcfa1c54d72aad6dbeb231c46de ) = ( ? ) AND ( ( scoped_49a39c4cc9ae4fdda07bcf49e99f8224 . scoped_7b2f7b8da15646d1b75aa03901460eb2 ) = ( ? ) AND ( scoped_49a39c4cc9ae4fdda07bcf49e99f8224 . scoped_77a1b9308b384a9391b69d24335ba058 ) = ( ? ) ) )"),
        // sql_quantizer_71
        ("{call px_cu_se_security_pg.sps_get_my_accounts_count(?, ?, ?, ?)}", "{ call px_cu_se_security_pg.sps_get_my_accounts_count ( ? ) }"),
        // sql_quantizer_72
        ("{call px_cu_se_security_pg.sps_get_my_accounts_count(1, 2, 'one', 'two')};", "{ call px_cu_se_security_pg.sps_get_my_accounts_count ( ? ) }"),
        // sql_quantizer_73
        ("{call curly_fun('{{', '}}', '}', '}')};", "{ call curly_fun ( ? ) }"),
        // sql_quantizer_74
        ("SELECT id, name FROM emp WHERE name LIKE {fn UCASE('Smith')}", "SELECT id, name FROM emp WHERE name LIKE ?"),
        // sql_quantizer_75
        ("select users.custom #- '{a,b}' from users", "select users.custom"),
        // sql_quantizer_76
        ("select users.custom #> '{a,b}' from users", "select users.custom"),
        // sql_quantizer_77
        ("select users.custom #>> '{a,b}' from users", "select users.custom"),
        // sql_quantizer_78
        ("SELECT a FROM foo WHERE value<@name", "SELECT a FROM foo WHERE value < @name"),
        // sql_quantizer_79
        ("SELECT @@foo", "SELECT @@foo"),
        // sql_quantizer_80
        ("DROP TABLE IF EXISTS django_site;\nDROP TABLE IF EXISTS knowledgebase_article;\n\nCREATE TABLE django_site (\n    id integer PRIMARY KEY,\n    domain character varying(100) NOT NULL,\n    name character varying(50) NOT NULL,\n    uuid uuid NOT NULL,\n    disabled boolean DEFAULT false NOT NULL\n);\n\nCREATE TABLE knowledgebase_article (\n    id integer PRIMARY KEY,\n    title character varying(255) NOT NULL,\n    site_id integer NOT NULL,\n    CONSTRAINT knowledgebase_article_site_id_fkey FOREIGN KEY (site_id) REFERENCES django_site(id)\n);\n\nINSERT INTO django_site(id, domain, name, uuid, disabled) VALUES (1, 'foo.domain', 'Foo', 'cb4776c1-edf3-4041-96a8-e152f5ae0f91', false);\nINSERT INTO knowledgebase_article(id, title, site_id) VALUES(1, 'title', 1);", "DROP TABLE IF EXISTS django_site DROP TABLE IF EXISTS knowledgebase_article CREATE TABLE django_site ( id integer PRIMARY KEY, domain character varying ( ? ) NOT ? name character varying ( ? ) NOT ? uuid uuid NOT ? disabled boolean DEFAULT ? NOT ? ) CREATE TABLE knowledgebase_article ( id integer PRIMARY KEY, title character varying ( ? ) NOT ? site_id integer NOT ? CONSTRAINT knowledgebase_article_site_id_fkey FOREIGN KEY ( site_id ) REFERENCES django_site ( id ) ) INSERT INTO django_site ( id, domain, name, uuid, disabled ) VALUES ( ? ) INSERT INTO knowledgebase_article ( id, title, site_id ) VALUES ( ? )"),
        // sql_quantizer_81
        ("\nSELECT set_config('foo.bar', (SELECT foo.bar FROM sometable WHERE sometable.uuid = %(some_id)s)::text, FALSE);\nSELECT\n    othertable.id,\n    othertable.title\nFROM othertable\nINNER JOIN sometable ON sometable.id = othertable.site_id\nWHERE\n    sometable.uuid = %(some_id)s\nLIMIT 1\n;", "SELECT set_config ( ? ( SELECT foo.bar FROM sometable WHERE sometable.uuid = ? ) :: text, ? ) SELECT othertable.id, othertable.title FROM othertable INNER JOIN sometable ON sometable.id = othertable.site_id WHERE sometable.uuid = ? LIMIT ?"),
        // sql_quantizer_82
        ("CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert(OUT created boolean, OUT primary_key text) AS $func$ BEGIN INSERT INTO \"school\" (\"id\",\"organization_id\",\"name\",\"created_at\",\"updated_at\") VALUES ('dc4e9444-d7c9-40a9-bcef-68e4cc594e61','ec647f56-f27a-49a1-84af-021ad0a19f21','Test','2021-03-31 16:30:43.915 +00:00','2021-03-31 16:30:43.915 +00:00'); created := true; EXCEPTION WHEN unique_violation THEN UPDATE \"school\" SET \"id\"='dc4e9444-d7c9-40a9-bcef-68e4cc594e61',\"organization_id\"='ec647f56-f27a-49a1-84af-021ad0a19f21',\"name\"='Test',\"updated_at\"='2021-03-31 16:30:43.915 +00:00' WHERE (\"id\" = 'dc4e9444-d7c9-40a9-bcef-68e4cc594e61'); created := false; END; $func$ LANGUAGE plpgsql; SELECT * FROM pg_temp.sequelize_upsert();", "CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert ( OUT created boolean, OUT primary_key text ) LANGUAGE plpgsql SELECT * FROM pg_temp.sequelize_upsert ( )"),
        // sql_quantizer_83
        ("INSERT INTO table (field1, field2) VALUES (1, $$someone's string123$with other things$$)", "INSERT INTO table ( field1, field2 ) VALUES ( ? )"),
        // sql_quantizer_84
        ("INSERT INTO table (field1) VALUES ($some tag$this text confuses$some other text$some ta not quite$some tag$)", "INSERT INTO table ( field1 ) VALUES ( ? )"),
        // sql_quantizer_85
        ("INSERT INTO table (field1) VALUES ($tag$random \\wqejks \"sadads' text$tag$)", "INSERT INTO table ( field1 ) VALUES ( ? )"),
        // sql_quantizer_86
        ("SELECT nspname FROM pg_class where nspname !~ '.*toIgnore.*'", "SELECT nspname FROM pg_class where nspname !~ ?"),
        // sql_quantizer_87
        ("SELECT nspname FROM pg_class where nspname !~* '.*toIgnoreInsensitive.*'", "SELECT nspname FROM pg_class where nspname !~* ?"),
        // sql_quantizer_88
        ("SELECT nspname FROM pg_class where nspname ~ '.*matching.*'", "SELECT nspname FROM pg_class where nspname ~ ?"),
        // sql_quantizer_89
        ("SELECT nspname FROM pg_class where nspname ~* '.*matchingInsensitive.*'", "SELECT nspname FROM pg_class where nspname ~* ?"),
        // sql_quantizer_90
        ("SELECT * FROM dbo.Items WHERE id = 1 or /*!obfuscation*/ 1 = 1", "SELECT * FROM dbo.Items WHERE id = ? or ? = ?"),
        // sql_quantizer_91
        ("SELECT * FROM Items WHERE id = -1 OR id = -01 OR id = -108 OR id = -.018 OR id = -.08 OR id = -908129", "SELECT * FROM Items WHERE id = ? OR id = ? OR id = ? OR id = ? OR id = ? OR id = ?"),
        // sql_quantizer_92
        ("USING $09 SELECT", "USING ? SELECT"),
        // sql_quantizer_93
        ("USING - SELECT", "USING - SELECT"),
        // sql_cassandra_0
        ("select key, status, modified from org_check_run where org_id = %s and check in (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)", "select key, status, modified from org_check_run where org_id = ? and check in ( ? )"),
        // sql_cassandra_1
        ("select key, status, modified from org_check_run where org_id = %s and check in (%s, %s, %s)", "select key, status, modified from org_check_run where org_id = ? and check in ( ? )"),
        // sql_cassandra_2
        ("select key, status, modified from org_check_run where org_id = %s and check in (%s , %s , %s )", "select key, status, modified from org_check_run where org_id = ? and check in ( ? )"),
        // sql_cassandra_3
        ("select key, status, modified from org_check_run where org_id = %s and check = %s", "select key, status, modified from org_check_run where org_id = ? and check = ?"),
        // sql_cassandra_4
        ("SELECT timestamp, processes FROM process_snapshot.minutely WHERE org_id = ? AND host = ? AND timestamp >= ? AND timestamp <= ?", "SELECT timestamp, processes FROM process_snapshot.minutely WHERE org_id = ? AND host = ? AND timestamp >= ? AND timestamp <= ?"),
        // sql_cassandra_5
        ("SELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||?|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,?),?),?) AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?)),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||?||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||?||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,?),?) AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||?||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,?),?) AS \"c25\", DECODE(bar.c , ?,?,?, ?, bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:p)) WHERE RNK = ?) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:p) and bar.x IN (:x)) )\nSELECT count(*) AS totcount FROM (SELECT \"c1\", \"c2\",\"c3\",\"c4\",\"c5\",\"c6\",\"c7\",\"c8\", \"c9\", \"c10\",\"c11\",\"c12\",\"c13\",\"c14\", \"c15\",\"c16\",\"c17\",\"c18\", \"c19\",\"c20\",\"c21\",\"c22\",\"c23\", \"c24\",\"c25\",\"c26\", \"c27\" FROM (SELECT bar.y AS \"c2\", foo.x AS \"c3\", foo.z AS \"c4\", DECODE(foo.a, NULL,NULL, foo.a ||?|| foo.b) AS \"c5\" , foo.c AS \"c6\", bar.d AS \"c1\", bar.e AS \"c7\", bar.f AS \"c8\", bar.g AS \"c9\", TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) AS \"c10\", TO_DATE(TO_CHAR(TO_DATE(bar.i,?),?),?) AS \"c11\", CASE WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? WHEN DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?))) > ? THEN ? ELSE NULL END AS \"c12\", DECODE(bar.j, NULL, TRUNC(SYSDATE) - TRUNC(TO_DATE(bar.h,?)),NULL) as \"c13\", bar.k AS \"c14\", bar.l ||?||bar.m AS \"c15\", DECODE(bar.n, NULL, NULL,bar.n ||?||bar.o) AS \"c16\", bar.p AS \"c17\", bar.q AS \"c18\", bar.r AS \"c19\", bar.s AS \"c20\", qux.a AS \"c21\", TO_CHAR(TO_DATE(qux.b,?),?) AS \"c22\", DECODE(qux.l,NULL,NULL, qux.l ||?||qux.m) AS \"c23\", bar.a AS \"c24\", TO_CHAR(TO_DATE(bar.j,?),?) AS \"c25\", DECODE(bar.c , ?,?,?, ?, bar.c ) AS \"c26\", bar.y AS y, bar.d, bar.d AS \"c27\" FROM blort.bar , ( SELECT * FROM (SELECT a,a,l,m,b,c, RANK() OVER (PARTITION BY c ORDER BY b DESC) RNK FROM blort.d WHERE y IN (:p)) WHERE RNK = ?) qux, blort.foo WHERE bar.c = qux.c(+) AND bar.x = foo.x AND bar.y IN (:p) and bar.x IN (:x)) )", "SELECT count ( * ) FROM ( SELECT c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15, c16, c17, c18, c19, c20, c21, c22, c23, c24, c25, c26, c27 FROM ( SELECT bar.y, foo.x, foo.z, DECODE ( foo.a, ? foo.a | | ? | | foo.b ), foo.c, bar.d, bar.e, bar.f, bar.g, TO_DATE ( TO_CHAR ( TO_DATE ( bar.h, ? ) ) ), TO_DATE ( TO_CHAR ( TO_DATE ( bar.i, ? ) ) ), CASE WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? ELSE ? END, DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ), bar.k, bar.l | | ? | | bar.m, DECODE ( bar.n, ? bar.n | | ? | | bar.o ), bar.p, bar.q, bar.r, bar.s, qux.a, TO_CHAR ( TO_DATE ( qux.b, ? ) ), DECODE ( qux.l, ? qux.l | | ? | | qux.m ), bar.a, TO_CHAR ( TO_DATE ( bar.j, ? ) ), DECODE ( bar.c, ? bar.c ), bar.y, bar.d, bar.d FROM blort.bar, ( SELECT * FROM ( SELECT a, a, l, m, b, c, RANK ( ) OVER ( PARTITION BY c ORDER BY b DESC ) RNK FROM blort.d WHERE y IN ( :p ) ) WHERE RNK = ? ) qux, blort.foo WHERE bar.c = qux.c ( + ) AND bar.x = foo.x AND bar.y IN ( :p ) and bar.x IN ( :x ) ) ) SELECT count ( * ) FROM ( SELECT c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15, c16, c17, c18, c19, c20, c21, c22, c23, c24, c25, c26, c27 FROM ( SELECT bar.y, foo.x, foo.z, DECODE ( foo.a, ? foo.a | | ? | | foo.b ), foo.c, bar.d, bar.e, bar.f, bar.g, TO_DATE ( TO_CHAR ( TO_DATE ( bar.h, ? ) ) ), TO_DATE ( TO_CHAR ( TO_DATE ( bar.i, ? ) ) ), CASE WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? WHEN DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ) > ? THEN ? ELSE ? END, DECODE ( bar.j, ? TRUNC ( SYSDATE ) - TRUNC ( TO_DATE ( bar.h, ? ) ) ), bar.k, bar.l | | ? | | bar.m, DECODE ( bar.n, ? bar.n | | ? | | bar.o ), bar.p, bar.q, bar.r, bar.s, qux.a, TO_CHAR ( TO_DATE ( qux.b, ? ) ), DECODE ( qux.l, ? qux.l | | ? | | qux.m ), bar.a, TO_CHAR ( TO_DATE ( bar.j, ? ) ), DECODE ( bar.c, ? bar.c ), bar.y, bar.d, bar.d FROM blort.bar, ( SELECT * FROM ( SELECT a, a, l, m, b, c, RANK ( ) OVER ( PARTITION BY c ORDER BY b DESC ) RNK FROM blort.d WHERE y IN ( :p ) ) WHERE RNK = ? ) qux, blort.foo WHERE bar.c = qux.c ( + ) AND bar.x = foo.x AND bar.y IN ( :p ) and bar.x IN ( :x ) ) )"),
        // sql_parse_number_1234
        ("1234", "?"),
        // sql_parse_number_-1234
        ("-1234", "?"),
        // sql_parse_number_1234e12
        ("1234e12", "?"),
        // sql_parse_number_0xfa
        ("0xfa", "?"),
        // sql_parse_number_01234567
        ("01234567", "?"),
        // sql_parse_number_09
        ("09", "?"),
        // sql_parse_number_-01234567
        ("-01234567", "?"),
        // sql_parse_number_-012345678
        ("-012345678", "?"),
    ];

    #[test]
    fn test_sql_obfuscation_suite() {
        let mut errors = String::new();
        for (i, (input, expected)) in SUITE_CASES.iter().enumerate() {
            let got = super::obfuscate_sql_string(input);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'keep_sql_alias': True}
    #[test]
    #[allow(deprecated)]
    fn test_suite_keep_sql_alias() {
        let config = SqlObfuscateConfig {
            keep_sql_alias: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sql_keep_alias_on
            (
                "SELECT username AS person FROM users WHERE id=4",
                "SELECT username AS person FROM users WHERE id = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'dollar_quoted_func': True}
    #[test]
    #[allow(deprecated)]
    fn test_suite_dollar_quoted_func() {
        let config = SqlObfuscateConfig {
            dollar_quoted_func: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sql_dollar_quoted_func_on
            (
                "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
                "SELECT $func$INSERT INTO table VALUES ( ? )$func$ FROM users",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'keep_sql_alias': True, 'dollar_quoted_func': True}
    #[test]
    #[allow(deprecated)]
    fn test_suite_keep_sql_alias_dollar_quoted_func() {
        let config = SqlObfuscateConfig {
            keep_sql_alias: true,
            dollar_quoted_func: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sql_dollar_quoted_func_as
            ("CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert(OUT created boolean, OUT primary_key text) AS $func$ BEGIN INSERT INTO \"school\" (\"id\",\"organization_id\",\"name\",\"created_at\",\"updated_at\") VALUES ('dc4e9444-d7c9-40a9-bcef-68e4cc594e61','ec647f56-f27a-49a1-84af-021ad0a19f21','Test','2021-03-31 16:30:43.915 +00:00','2021-03-31 16:30:43.915 +00:00'); created := true; EXCEPTION WHEN unique_violation THEN UPDATE \"school\" SET \"id\"='dc4e9444-d7c9-40a9-bcef-68e4cc594e61',\"organization_id\"='ec647f56-f27a-49a1-84af-021ad0a19f21',\"name\"='Test',\"updated_at\"='2021-03-31 16:30:43.915 +00:00' WHERE (\"id\" = 'dc4e9444-d7c9-40a9-bcef-68e4cc594e61'); created := false; END; $func$ LANGUAGE plpgsql; SELECT * FROM pg_temp.sequelize_upsert();", "CREATE OR REPLACE FUNCTION pg_temp.sequelize_upsert ( OUT created boolean, OUT primary_key text ) AS $func$BEGIN INSERT INTO school ( id, organization_id, name, created_at, updated_at ) VALUES ( ? ) created := ? EXCEPTION WHEN unique_violation THEN UPDATE school SET id = ? organization_id = ? name = ? updated_at = ? WHERE ( id = ? ) created := ? END$func$ LANGUAGE plpgsql SELECT * FROM pg_temp.sequelize_upsert ( )"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'replace_digits': True}
    #[test]
    #[allow(deprecated)]
    fn test_suite_replace_digits() {
        let config = SqlObfuscateConfig {
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sql_metadata_complex_with_replace_digits
            ("\n/* Multi-line comment\nwith line breaks */\nWITH sales AS\n(SELECT sf2.*\n\tFROM gosalesdw28391.sls_order_method_dim AS md,\n\t\tgosalesdw1920.sls_product_dim391 AS pd190,\n\t\tgosalesdw3819.emp_employee_dim AS ed,\n\t\tgosalesdw3919.sls_sales_fact3819 AS sf2\n\tWHERE pd190.product_key = sf2.product_key\n\tAND pd190.product_number381 > 10000\n\tAND pd190.base_product_key > 30\n\tAND md.order_method_key = sf2.order_method_key8319\n\tAND md.order_method_code > 5\n\tAND ed.employee_key = sf2.employee_key\n\tAND ed.manager_code1 > 20),\ninventory3118 AS\n(SELECT if.*\n\tFROM gosalesdw1592.go_branch_dim AS bd3221,\n\tgosalesdw.dist_inventory_fact AS if\n\tWHERE if.branch_key = bd3221.branch_key\n\tAND bd3221.branch_code > 20)\nSELECT sales1828.product_key AS PROD_KEY,\nSUM(CAST (inventory3118.quantity_shipped AS BIGINT)) AS INV_SHIPPED3118,\nSUM(CAST (sales1828.quantity AS BIGINT)) AS PROD_QUANTITY,\nRANK() OVER ( ORDER BY SUM(CAST (sales1828.quantity AS BIGINT)) DESC) AS PROD_RANK\nFROM sales1828, inventory3118\nWHERE sales1828.product_key = inventory3118.product_key\nGROUP BY sales1828.product_key", "WITH sales SELECT sf?.* FROM gosalesdw?.sls_order_method_dim, gosalesdw?.sls_product_dim?, gosalesdw?.emp_employee_dim, gosalesdw?.sls_sales_fact? WHERE pd?.product_key = sf?.product_key AND pd?.product_number? > ? AND pd?.base_product_key > ? AND md.order_method_key = sf?.order_method_key? AND md.order_method_code > ? AND ed.employee_key = sf?.employee_key AND ed.manager_code? > ? ) inventory? SELECT if.* FROM gosalesdw?.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd?.branch_key AND bd?.branch_code > ? ) SELECT sales?.product_key, SUM ( CAST ( inventory?.quantity_shipped ) ), SUM ( CAST ( sales?.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales?.quantity ) ) DESC ) FROM sales?, inventory? WHERE sales?.product_key = inventory?.product_key GROUP BY sales?.product_key"),
            // sql_replace_digits_on_0
            ("REPLACE INTO sales_2019_07_01 (`itemID`, `date`, `qty`, `price`) VALUES ((SELECT itemID FROM item1001 WHERE `sku` = [sku]), CURDATE(), [qty], 0.00)", "REPLACE INTO sales_?_?_? ( itemID, date, qty, price ) VALUES ( ( SELECT itemID FROM item? WHERE sku = [ sku ] ), CURDATE ( ), [ qty ], ? )"),
            // sql_replace_digits_on_1
            ("SELECT ddh19.name, ddt.tags FROM dd91219.host ddh19, dd21916.host_tags ddt WHERE ddh19.id = ddt.host_id AND ddh19.org_id = 2 AND ddh19.name = 'datadog'", "SELECT ddh?.name, ddt.tags FROM dd?.host ddh?, dd?.host_tags ddt WHERE ddh?.id = ddt.host_id AND ddh?.org_id = ? AND ddh?.name = ?"),
            // sql_replace_digits_on_2
            ("SELECT ddu2.name, ddo.id10, ddk.app_key52 FROM dd3120.user ddu2, dd1931.orgs55 ddo, dd53819.keys ddk", "SELECT ddu?.name, ddo.id?, ddk.app_key? FROM dd?.user ddu?, dd?.orgs? ddo, dd?.keys ddk"),
            // sql_replace_digits_on_3
            ("SELECT daily_values1529.*, LEAST((5040000 - @runtot), value1830) AS value1830,\n(@runtot := @runtot + daily_values1529.value1830) AS total\nFROM (SELECT @runtot:=0) AS n,\ndaily_values1529 WHERE daily_values1529.subject_id = 12345 AND daily_values1592.subject_type = 'Skippity'\nAND (daily_values1529.date BETWEEN '2018-05-09' AND '2018-06-19') HAVING value >= 0 ORDER BY date", "SELECT daily_values?.*, LEAST ( ( ? - @runtot ), value? ), ( @runtot := @runtot + daily_values?.value? ) FROM ( SELECT @runtot := ? ), daily_values? WHERE daily_values?.subject_id = ? AND daily_values?.subject_type = ? AND ( daily_values?.date BETWEEN ? AND ? ) HAVING value >= ? ORDER BY date"),
            // sql_replace_digits_on_4
            ("WITH\nsales AS\n(SELECT sf2.*\n\tFROM gosalesdw28391.sls_order_method_dim AS md,\n\t\tgosalesdw1920.sls_product_dim391 AS pd190,\n\t\tgosalesdw3819.emp_employee_dim AS ed,\n\t\tgosalesdw3919.sls_sales_fact3819 AS sf2\n\tWHERE pd190.product_key = sf2.product_key\n\tAND pd190.product_number381 > 10000\n\tAND pd190.base_product_key > 30\n\tAND md.order_method_key = sf2.order_method_key8319\n\tAND md.order_method_code > 5\n\tAND ed.employee_key = sf2.employee_key\n\tAND ed.manager_code1 > 20),\ninventory3118 AS\n(SELECT if.*\n\tFROM gosalesdw1592.go_branch_dim AS bd3221,\n\tgosalesdw.dist_inventory_fact AS if\n\tWHERE if.branch_key = bd3221.branch_key\n\tAND bd3221.branch_code > 20)\nSELECT sales1828.product_key AS PROD_KEY,\nSUM(CAST (inventory3118.quantity_shipped AS BIGINT)) AS INV_SHIPPED3118,\nSUM(CAST (sales1828.quantity AS BIGINT)) AS PROD_QUANTITY,\nRANK() OVER ( ORDER BY SUM(CAST (sales1828.quantity AS BIGINT)) DESC) AS PROD_RANK\nFROM sales1828, inventory3118\nWHERE sales1828.product_key = inventory3118.product_key\nGROUP BY sales1828.product_key", "WITH sales SELECT sf?.* FROM gosalesdw?.sls_order_method_dim, gosalesdw?.sls_product_dim?, gosalesdw?.emp_employee_dim, gosalesdw?.sls_sales_fact? WHERE pd?.product_key = sf?.product_key AND pd?.product_number? > ? AND pd?.base_product_key > ? AND md.order_method_key = sf?.order_method_key? AND md.order_method_code > ? AND ed.employee_key = sf?.employee_key AND ed.manager_code? > ? ) inventory? SELECT if.* FROM gosalesdw?.go_branch_dim, gosalesdw.dist_inventory_fact WHERE if.branch_key = bd?.branch_key AND bd?.branch_code > ? ) SELECT sales?.product_key, SUM ( CAST ( inventory?.quantity_shipped ) ), SUM ( CAST ( sales?.quantity ) ), RANK ( ) OVER ( ORDER BY SUM ( CAST ( sales?.quantity ) ) DESC ) FROM sales?, inventory? WHERE sales?.product_key = inventory?.product_key GROUP BY sales?.product_key"),
            // sql_table_finder_replace_digits_0
            ("select * from users where id = 42", "select * from users where id = ?"),
            // sql_table_finder_replace_digits_1
            ("select * from `backslashes` where id = 42", "select * from backslashes where id = ?"),
            // sql_table_finder_replace_digits_2
            ("select * from \"double-quotes\" where id = 42", "select * from double-quotes where id = ?"),
            // sql_table_finder_replace_digits_3
            ("SELECT host, status FROM ec2_status WHERE org_id = 42", "SELECT host, status FROM ec?_status WHERE org_id = ?"),
            // sql_table_finder_replace_digits_4
            ("SELECT * FROM (SELECT * FROM nested_table)", "SELECT * FROM ( SELECT * FROM nested_table )"),
            // sql_table_finder_replace_digits_5
            ("   -- get user \n--\n select * \n   from users \n    where\n       id = 214325346    ", "select * from users where id = ?"),
            // sql_table_finder_replace_digits_6
            ("SELECT articles.* FROM articles WHERE articles.id = 1 LIMIT 1, 20", "SELECT articles.* FROM articles WHERE articles.id = ? LIMIT ?"),
            // sql_table_finder_replace_digits_7
            ("UPDATE user_dash_pref SET json_prefs = %(json_prefs)s, modified = '2015-08-27 22:10:32.492912' WHERE user_id = %(user_id)s AND url = %(url)s", "UPDATE user_dash_pref SET json_prefs = ? modified = ? WHERE user_id = ? AND url = ?"),
            // sql_table_finder_replace_digits_8
            ("SELECT DISTINCT host.id AS host_id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = %(org_id_1)s AND host.name NOT IN (%(name_1)s) AND host.name IN (%(name_2)s, %(name_3)s, %(name_4)s, %(name_5)s)", "SELECT DISTINCT host.id FROM host JOIN host_alias ON host_alias.host_id = host.id WHERE host.org_id = ? AND host.name NOT IN ( ? ) AND host.name IN ( ? )"),
            // sql_table_finder_replace_digits_9
            ("update Orders set created = \"2019-05-24 00:26:17\", gross = 30.28, payment_type = \"eventbrite\", mg_fee = \"3.28\", fee_collected = \"3.28\", event = 59366262, status = \"10\", survey_type = 'direct', tx_time_limit = 480, invite = \"\", ip_address = \"69.215.148.82\", currency = 'USD', gross_USD = \"30.28\", tax_USD = 0.00, journal_activity_id = 4044659812798558774, eb_tax = 0.00, eb_tax_USD = 0.00, cart_uuid = \"160b450e7df511e9810e0a0c06de92f8\", changed = '2019-05-24 00:26:17' where id = ?", "update Orders set created = ? gross = ? payment_type = ? mg_fee = ? fee_collected = ? event = ? status = ? survey_type = ? tx_time_limit = ? invite = ? ip_address = ? currency = ? gross_USD = ? tax_USD = ? journal_activity_id = ? eb_tax = ? eb_tax_USD = ? cart_uuid = ? changed = ? where id = ?"),
            // sql_table_finder_replace_digits_10
            ("SELECT * FROM clients WHERE (clients.first_name = 'Andy') LIMIT 1 BEGIN INSERT INTO clients (created_at, first_name, locked, orders_count, updated_at) VALUES ('2011-08-30 05:22:57', 'Andy', 1, NULL, '2011-08-30 05:22:57') COMMIT", "SELECT * FROM clients WHERE ( clients.first_name = ? ) LIMIT ? BEGIN INSERT INTO clients ( created_at, first_name, locked, orders_count, updated_at ) VALUES ( ? ) COMMIT"),
            // sql_table_finder_replace_digits_11
            ("DELETE FROM table WHERE table.a=1", "DELETE FROM table WHERE table.a = ?"),
            // sql_table_finder_replace_digits_12
            ("SELECT wp_woocommerce_order_items.order_id FROM wp_woocommerce_order_items LEFT JOIN ( SELECT meta_value FROM wp_postmeta WHERE meta_key = ? ) ON wp_woocommerce_order_items.order_id = a.post_id WHERE wp_woocommerce_order_items.order_id = ?", "SELECT wp_woocommerce_order_items.order_id FROM wp_woocommerce_order_items LEFT JOIN ( SELECT meta_value FROM wp_postmeta WHERE meta_key = ? ) ON wp_woocommerce_order_items.order_id = a.post_id WHERE wp_woocommerce_order_items.order_id = ?"),
            // sql_table_finder_replace_digits_13
            ("REPLACE INTO sales_2019_07_01 (`itemID`, `date`, `qty`, `price`) VALUES ((SELECT itemID FROM item1001 WHERE `sku` = [sku]), CURDATE(), [qty], 0.00)", "REPLACE INTO sales_?_?_? ( itemID, date, qty, price ) VALUES ( ( SELECT itemID FROM item? WHERE sku = [ sku ] ), CURDATE ( ), [ qty ], ? )"),
            // sql_table_finder_replace_digits_14
            ("SELECT name FROM people WHERE person_id = -1", "SELECT name FROM people WHERE person_id = ?"),
            // sql_table_finder_replace_digits_15
            ("select * from test where !is_good;", "select * from test where ! is_good"),
            // sql_table_finder_replace_digits_16
            ("select * from test where ! is_good;", "select * from test where ! is_good"),
            // sql_table_finder_replace_digits_17
            ("select * from test where !45;", "select * from test where ! ?"),
            // sql_table_finder_replace_digits_18
            ("select * from test where !(select is_good from good_things);", "select * from test where ! ( select is_good from good_things )"),
            // sql_table_finder_replace_digits_19
            ("select * from test where !'weird_query'", "select * from test where ! ?"),
            // sql_table_finder_replace_digits_20
            ("select * from test where !\"weird_query\"", "select * from test where ! weird_query"),
            // sql_table_finder_replace_digits_21
            ("select * from test where !`weird_query`", "select * from test where ! weird_query"),
            // sql_table_finder_replace_digits_22
            ("select !- 2", "select ! - ?"),
            // sql_table_finder_replace_digits_23
            ("select !+2", "select ! + ?"),
            // sql_table_finder_replace_digits_24
            ("select * from test where !- 2", "select * from test where ! - ?"),
            // sql_table_finder_replace_digits_25
            ("select count(*) as `count(*)` from test", "select count ( * ) from test"),
            // sql_table_finder_replace_digits_26
            ("SELECT age as `age}` FROM profile", "SELECT age FROM profile"),
            // sql_table_finder_replace_digits_27
            ("SELECT age as `age``}` FROM profile", "SELECT age FROM profile"),
            // sql_table_finder_replace_digits_28
            ("SELECT * from users where user_id =:0_USER", "SELECT * from users where user_id = :0_USER"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'keep_sql_alias': True, 'dollar_quoted_func': True, 'keep_null': True, 'keep_boolean': True,
    // 'keep_positional_parameter': True, 'keep_trailing_semicolon': True,
    // 'keep_identifier_quotation': True, 'replace_bind_parameter': True,
    // 'remove_space_between_parentheses': True, 'keep_json_path': True, 'replace_digits': True}
    #[test]
    #[allow(deprecated)]
    fn test_suite_all_flags() {
        let config = SqlObfuscateConfig {
            keep_sql_alias: true,
            dollar_quoted_func: true,
            keep_null: true,
            keep_boolean: true,
            keep_positional_parameter: true,
            keep_trailing_semicolon: true,
            keep_identifier_quotation: true,
            replace_bind_parameter: true,
            remove_space_between_parentheses: true,
            keep_json_path: true,
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sql_fuzzing_1230223853
            ("$2", "?"),
            // sql_fuzzing_3056568399
            ("(2", "( ?"),
            // sql_fuzzing_2600047278
            (";ჸ", "ჸ"),
            // sql_fuzzing_1323053175
            (";ჸ", "ჸ"),
            // sql_fuzzing_726138257
            ("@2", "@?"),
            // sql_fuzzing_3590332207
            ("@C", "@C"),
            // sql_fuzzing_572710742
            ("\"\"\"\"", "\""),
            // sql_fuzzing_3189077130
            ("@ჸ2", "@ჸ?"),
            // sql_fuzzing_832034588
            ("\"0\"", "0"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'dbms': 'mssql'}
    #[test]
    #[allow(deprecated)]
    fn test_suite_mssql() {
        let config = SqlObfuscateConfig::default();
        let cases: &[(&str, &str)] = &[
            // sql_single_dollar_identifier_merge
            ("\n\tMERGE INTO Employees AS target\n\tUSING EmployeeUpdates AS source\n\tON (target.EmployeeID = source.EmployeeID)\n\tWHEN MATCHED THEN\n\t\tUPDATE SET\n\t\t\ttarget.Name = source.Name\n\tWHEN NOT MATCHED BY TARGET THEN\n\t\tINSERT (EmployeeID, Name)\n\t\tVALUES (source.EmployeeID, source.Name)\n\tWHEN NOT MATCHED BY SOURCE THEN\n\t\tDELETE\n\tOUTPUT $action, inserted.*, deleted.*;\n\t", "MERGE INTO Employees USING EmployeeUpdates ON ( target.EmployeeID = source.EmployeeID ) WHEN MATCHED THEN UPDATE SET target.Name = source.Name WHEN NOT MATCHED BY TARGET THEN INSERT ( EmployeeID, Name ) VALUES ( source.EmployeeID, source.Name ) WHEN NOT MATCHED BY SOURCE THEN DELETE OUTPUT $action, inserted.*, deleted.*"),
            // sql_dbms_sqlserver_global_temp
            ("select * from ##ThisIsAGlobalTempTable where id = 1", "select * from ##ThisIsAGlobalTempTable where id = ?"),
            // sql_dbms_sqlserver_temp
            ("select * from dbo.#ThisIsATempTable where id = 1", "select * from dbo.#ThisIsATempTable where id = ?"),
            // sql_dbms_sqlserver_brackets
            ("SELECT * from [db_users] where [id] = @1", "SELECT * from db_users where id = @1"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Mssql);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'dbms': 'postgresql'}
    #[test]
    #[allow(deprecated)]
    fn test_suite_postgresql() {
        let config = SqlObfuscateConfig::default();
        let cases: &[(&str, &str)] = &[
            // sql_pg_json_operators_0
            (
                "select users.custom #> '{a,b}' from users",
                "select users.custom #> ? from users",
            ),
            // sql_pg_json_operators_1
            (
                "select users.custom #>> '{a,b}' from users",
                "select users.custom #>> ? from users",
            ),
            // sql_pg_json_operators_2
            (
                "select users.custom #- '{a,b}' from users",
                "select users.custom #- ? from users",
            ),
            // sql_pg_json_operators_3
            (
                "select users.custom -> 'foo' from users",
                "select users.custom -> ? from users",
            ),
            // sql_pg_json_operators_4
            (
                "select users.custom ->> 'foo' from users",
                "select users.custom ->> ? from users",
            ),
            // sql_pg_json_operators_5
            (
                "select * from users where user.custom @> '{a,b}'",
                "select * from users where user.custom @> ?",
            ),
            // sql_pg_json_operators_6
            (
                "SELECT a FROM foo WHERE value<@name",
                "SELECT a FROM foo WHERE value <@ name",
            ),
            // sql_pg_json_operators_7
            (
                "select * from users where user.custom ? 'foo'",
                "select * from users where user.custom ? ?",
            ),
            // sql_pg_json_operators_8
            (
                "select * from users where user.custom ?| array [ '1', '2' ]",
                "select * from users where user.custom ?| array [ ? ]",
            ),
            // sql_pg_json_operators_9
            (
                "select * from users where user.custom ?& array [ '1', '2' ]",
                "select * from users where user.custom ?& array [ ? ]",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Postgresql);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'normalize_only'}
    #[test]
    fn test_suite_normalize_only() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_norm_simple
            ("SELECT * FROM users WHERE id = 1", "SELECT * FROM users WHERE id = 1"),
            // sqllexer_norm_dollar_func
            ("SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users", "SELECT $func$INSERT INTO table VALUES ( 'a', 1, 2 )$func$ FROM users"),
            // sqllexer_norm_procedure_on
            ("CREATE PROCEDURE TestProc AS BEGIN SELECT * FROM users WHERE id = 1 END", "CREATE PROCEDURE TestProc AS BEGIN SELECT * FROM users WHERE id = 1 END"),
            // sqllexer_norm_procedure_off
            ("CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END", "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END"),
            // sqllexer_norm_null_bool_pos
            ("SELECT * FROM users WHERE id = 1 AND address = $1 and id = $2 AND deleted IS NULL AND active is TRUE", "SELECT * FROM users WHERE id = 1 AND address = $1 and id = $2 AND deleted IS NULL AND active is TRUE"),
            // sqllexer_norm_cte
            ("WITH users AS (SELECT * FROM people) SELECT * FROM users", "WITH users AS ( SELECT * FROM people ) SELECT * FROM users"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'normalize_only', 'keep_sql_alias': True}
    #[test]
    fn test_suite_normalize_only_keep_sql_alias() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            keep_sql_alias: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_norm_comments_alias
            ("\n\t\t\t-- comment\n\t\t\t/* comment */\n\t\t\tSELECT id as id, name as n FROM users123 WHERE id in (1,2,3)", "SELECT id as id, name as n FROM users123 WHERE id in ( 1, 2, 3 )"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'normalize_only', 'remove_space_between_parentheses': True}
    #[test]
    fn test_suite_normalize_only_remove_space_between_parentheses() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            remove_space_between_parentheses: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_norm_remove_space_parens
            (
                "SELECT * FROM users WHERE id = 1 AND (name = 'test' OR name = 'test2')",
                "SELECT * FROM users WHERE id = 1 AND (name = 'test' OR name = 'test2')",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'normalize_only', 'keep_trailing_semicolon': True}
    #[test]
    fn test_suite_normalize_only_keep_trailing_semicolon() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            keep_trailing_semicolon: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_norm_keep_trailing_semi
            (
                "SELECT * FROM users WHERE id = 1 AND name = 'test';",
                "SELECT * FROM users WHERE id = 1 AND name = 'test';",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'normalize_only', 'keep_identifier_quotation': True}
    #[test]
    fn test_suite_normalize_only_keep_identifier_quotation() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::NormalizeOnly,
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_norm_keep_ident_quot
            (
                "SELECT * FROM \"users\" WHERE id = 1 AND name = 'test'",
                "SELECT * FROM \"users\" WHERE id = 1 AND name = 'test'",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize'}
    #[test]
    fn test_suite_obfuscate_and_normalize() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_simple
            ("SELECT * FROM users WHERE id = 1", "SELECT * FROM users WHERE id = ?"),
            // sqllexer_obn_dollar_func_off
            ("SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users", "SELECT ? FROM users"),
            // sqllexer_obn_procedure_on
            ("CREATE PROCEDURE TestProc AS BEGIN SELECT * FROM users WHERE id = 1 END", "CREATE PROCEDURE TestProc AS BEGIN SELECT * FROM users WHERE id = ? END"),
            // sqllexer_obn_procedure_off
            ("CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END", "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = ? WHERE id = ? END"),
            // sqllexer_obn_null_bool_pos_param
            ("SELECT * FROM users WHERE id = 1 AND address = $1 and id = $2 AND deleted IS NULL AND active is TRUE", "SELECT * FROM users WHERE id = ? AND address = ? and id = ? AND deleted IS ? AND active is ?"),
            // sqllexer_obn_create_table
            ("CREATE TABLE IF NOT EXISTS users (id INT, name VARCHAR(255))", "CREATE TABLE IF NOT EXISTS users ( id INT, name VARCHAR ( ? ) )"),
            // sqllexer_obn_replace_bind_off
            ("SELECT * FROM users WHERE id = @P1 AND name = @P2", "SELECT * FROM users WHERE id = @P1 AND name = @P2"),
            // sqllexer_obn_pg_only
            ("SELECT * FROM ONLY users WHERE id = 1", "SELECT * FROM ONLY users WHERE id = ?"),
            // sqllexer_obn_cte
            ("WITH users AS (SELECT * FROM people) SELECT * FROM users where id = 1", "WITH users AS ( SELECT * FROM people ) SELECT * FROM users where id = ?"),
            // sqllexer_obn_json_path_off
            ("SELECT * FROM users WHERE id = 1 AND name->'first' = 'test'", "SELECT * FROM users WHERE id = ? AND name -> ? = ?"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'replace_digits': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_replace_digits() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_replace_digits
            (
                "SELECT * FROM users123 WHERE id = 1",
                "SELECT * FROM users? WHERE id = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_sql_alias': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_sql_alias() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_sql_alias: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_comments_alias
            ("\n\t\t\t-- comment\n\t\t\t/* comment */\n\t\t\tSELECT id as id, name as n FROM users123 WHERE id in (1,2,3)", "SELECT id as id, name as n FROM users123 WHERE id in ( ? )"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'dollar_quoted_func': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_dollar_quoted_func() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            dollar_quoted_func: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_dollar_func_on
            (
                "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
                "SELECT $func$INSERT INTO table VALUES ( ? )$func$ FROM users",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'dollar_quoted_func': True, 'replace_digits': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_dollar_quoted_func_replace_digits() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            dollar_quoted_func: true,
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_dollar_func_digits
            (
                "SELECT * FROM users123 WHERE id = $tag$1$tag$",
                "SELECT * FROM users? WHERE id = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'remove_space_between_parentheses': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_remove_space_between_parentheses() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            remove_space_between_parentheses: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_remove_space_parens
            (
                "SELECT * FROM users WHERE id = 1 AND (name = 'test' OR name = 'test2')",
                "SELECT * FROM users WHERE id = ? AND (name = ? OR name = ?)",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_null': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_null() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_null: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_keep_null
            (
                "SELECT * FROM users WHERE id = 1 AND name IS NULL",
                "SELECT * FROM users WHERE id = ? AND name IS NULL",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_boolean': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_boolean() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_boolean: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_keep_boolean
            (
                "SELECT * FROM users WHERE id = 1 AND name is TRUE",
                "SELECT * FROM users WHERE id = ? AND name is TRUE",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_positional_parameter': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_positional_parameter() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_positional_parameter: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_keep_pos_param
            (
                "SELECT * FROM users WHERE id = 1 AND name = $1 and id = $2",
                "SELECT * FROM users WHERE id = ? AND name = $1 and id = $2",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_trailing_semicolon': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_trailing_semicolon() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_trailing_semicolon: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_keep_trailing_semi
            (
                "SELECT * FROM users WHERE id = 1 AND name = 'test';",
                "SELECT * FROM users WHERE id = ? AND name = ?;",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_identifier_quotation': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_identifier_quotation() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_keep_ident_quot
            (
                "SELECT * FROM \"users\" WHERE id = 1 AND name = 'test'",
                "SELECT * FROM \"users\" WHERE id = ? AND name = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'replace_bind_parameter': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_replace_bind_parameter() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            replace_bind_parameter: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_replace_bind_on
            (
                "SELECT * FROM users WHERE id = @P1 AND name = @P2",
                "SELECT * FROM users WHERE id = ? AND name = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_and_normalize', 'keep_json_path': True}
    #[test]
    fn test_suite_obfuscate_and_normalize_keep_json_path() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_json_path: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obn_json_path_arrow
            (
                "SELECT * FROM users WHERE id = 1 AND name->'first' = 'test'",
                "SELECT * FROM users WHERE id = ? AND name -> 'first' = ?",
            ),
            // sqllexer_obn_json_path_double_arrow
            (
                "SELECT * FROM users WHERE id = 1 AND name->>2 = 'test'",
                "SELECT * FROM users WHERE id = ? AND name ->> 2 = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_only'}
    #[test]
    fn test_suite_obfuscate_only() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obf_simple
            ("SELECT * FROM users WHERE id = 1", "SELECT * FROM users WHERE id = ?"),
            // sqllexer_obf_dollar_question
            ("SELECT \"table\".\"field\" FROM \"table\" WHERE \"table\".\"otherfield\" = $? AND \"table\".\"thirdfield\" = $?;", "SELECT \"table\".\"field\" FROM \"table\" WHERE \"table\".\"otherfield\" = $? AND \"table\".\"thirdfield\" = $?;"),
            // sqllexer_obf_replace_digits_off
            ("SELECT * FROM users123 WHERE id = 1", "SELECT * FROM users123 WHERE id = ?"),
            // sqllexer_obf_dollar_quoted_func_off
            ("SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users", "SELECT ? FROM users"),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_only', 'replace_digits': True}
    #[test]
    fn test_suite_obfuscate_only_replace_digits() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obf_replace_digits_on
            (
                "SELECT * FROM users123 WHERE id = 1",
                "SELECT * FROM users? WHERE id = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_only', 'dollar_quoted_func': True}
    #[test]
    fn test_suite_obfuscate_only_dollar_quoted_func() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            dollar_quoted_func: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obf_dollar_quoted_func_on
            (
                "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
                "SELECT $func$INSERT INTO table VALUES (?, ?, ?)$func$ FROM users",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // {'mode': 'obfuscate_only', 'dollar_quoted_func': True, 'replace_digits': True}
    #[test]
    fn test_suite_obfuscate_only_dollar_quoted_func_replace_digits() {
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateOnly,
            dollar_quoted_func: true,
            replace_digits: true,
            ..Default::default()
        };
        let cases: &[(&str, &str)] = &[
            // sqllexer_obf_dollar_func_and_digits
            (
                "SELECT * FROM users123 WHERE id = $tag$1$tag$",
                "SELECT * FROM users? WHERE id = ?",
            ),
        ];
        let mut errors = String::new();
        for (i, (input, expected)) in cases.iter().enumerate() {
            let got = super::obfuscate_sql(input, &config, DbmsKind::Generic);
            if got != *expected {
                errors.push_str(&format!(
                    "case {i} ({input:?}):\n  expected {expected:?}\n  got      {got:?}\n"
                ));
            }
        }
        if !errors.is_empty() {
            panic!("{errors}");
        }
    }

    // Test that collapse_limit_two_args handles LIMIT case-insensitively.
    // In the deprecated mode, the grouping filter is inactive, so
    // collapse_limit_two_args is the sole mechanism for both cases.
    #[test]
    fn test_collapse_limit_case_insensitive() {
        #[allow(deprecated)]
        let config = SqlObfuscateConfig {
            obfuscation_mode: SqlObfuscationMode::Unspecified,
            ..Default::default()
        };
        let got_upper =
            super::obfuscate_sql("SELECT * FROM t LIMIT 5, 10", &config, DbmsKind::Generic);
        assert_eq!(
            got_upper, "SELECT * FROM t LIMIT ?",
            "uppercase LIMIT should be collapsed: {got_upper:?}"
        );
        // eq_ignore_ascii_case fix: lowercase limit must also be collapsed.
        let got_lower =
            super::obfuscate_sql("SELECT * FROM t limit 5, 10", &config, DbmsKind::Generic);
        assert_eq!(
            got_lower, "SELECT * FROM t limit ?",
            "lowercase limit should also be collapsed: {got_lower:?}"
        );
    }
}
