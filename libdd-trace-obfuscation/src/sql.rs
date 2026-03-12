// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbmsKind {
    #[default]
    Generic,
    Mssql,
    Mysql,
    Postgresql,
    Oracle,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SqlObfuscationMode {
    #[default]
    #[deprecated = "kept for compatibility with agent's obfuscator but has unintuitive behavior"]
    #[allow(deprecated)]
    Unspecified,
    NormalizeOnly,
    ObfuscateOnly,
    ObfuscateAndNormalize,
}

/// Configuration for SQL obfuscation
#[derive(Debug, Default, Clone)]
pub struct SqlObfuscateConfig {
    pub dbms: DbmsKind,
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
    fn new(s: &'a str, config: &'a SqlObfuscateConfig) -> Self {
        Self {
            s,
            bytes: s.as_bytes(),
            pos: 0,
            result: String::with_capacity(s.len()),
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
                    let is_sqlserver = matches!(self.config.dbms, DbmsKind::Mssql);
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
                        Some(b'>') if matches!(self.config.dbms, DbmsKind::Postgresql) => {
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
                        Some(b'-') if matches!(self.config.dbms, DbmsKind::Postgresql) => {
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
                    if matches!(self.config.dbms, DbmsKind::Mssql) {
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
                                    let normalized_inner = obfuscate_sql(inner, self.config);
                                    self.space();
                                    self.result.push_str(tag_str);
                                    self.result.push_str(&normalized_inner);
                                    self.result.push_str(close_tag);
                                } else if self.config.dollar_quoted_func {
                                    // Obfuscate the content inside dollar quotes
                                    let tag_str = &self.s[start..inner_start];
                                    let inner = &self.s[inner_start..inner_end];
                                    let close_tag = &self.s[inner_end..outer_end];
                                    let obfuscated_inner = obfuscate_sql(inner, self.config);
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
                            if matches!(self.config.dbms, DbmsKind::Postgresql) {
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
                            if matches!(self.config.dbms, DbmsKind::Postgresql) || !next2_is_ident {
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
fn collapse_grouped_values(s: &str) -> String {
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

    // Collapse multi-row VALUES: `VALUES ( ? ) , ( ? ) , ...` → `VALUES ( ? )`
    let result = collapse_multi_values(&result);
    // Collapse `LIMIT ?, ?` → `LIMIT ?` (MySQL/SQLite LIMIT offset, count syntax)
    collapse_limit_two_args(&result)
}

/// Collapse `VALUES ( ? ) , ( ? ) , ...` → `VALUES ( ? )`.
/// Also handles comma-less groups `VALUES ( ? ) ( ? )` (when commas were stripped by placeholder
/// logic).
fn collapse_multi_values(s: &str) -> String {
    // Pattern: "VALUES ( ? )" followed by one or more " , ( ? )" or " ( ? )" groups
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while !remaining.is_empty() {
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

        // Fallback: consume one char
        let c = remaining.chars().next().unwrap(); // safe because remaining not empty
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
            if rb.starts_with(b"LIMIT ?") {
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
pub fn obfuscate_sql(s: &str, config: &SqlObfuscateConfig) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut tokenizer = Tokenizer::new(s, config);
    tokenizer.process();
    let raw = tokenizer.finalize();
    // collapse_grouped_values applies in legacy mode and obfuscate_and_normalize mode.
    // In obfuscate_only and normalize_only modes, values are NOT collapsed.
    let should_collapse = matches!(
        config.obfuscation_mode,
        SqlObfuscationMode::Unspecified | SqlObfuscationMode::ObfuscateAndNormalize
    );
    if should_collapse {
        collapse_grouped_values(&raw)
    } else {
        raw
    }
}

/// Obfuscates a SQL string with default configuration.
pub fn obfuscate_sql_string(s: &str) -> String {
    obfuscate_sql(s, &SqlObfuscateConfig::default())
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
        let config = super::SqlObfuscateConfig {
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT * FROM "users" WHERE id = 1 AND name = 'test'"#,
            &config,
        );
        // In old tokenizer mode, keep_identifier_quotation is ignored (Go does too).
        let expected = "SELECT * FROM users WHERE id = ? AND name = ?";
        assert_eq!(got, expected, "keep_identifier_quotation: got {got:?}");
    }

    #[test]
    fn test_remove_space_between_parentheses() {
        let config = super::SqlObfuscateConfig {
            remove_space_between_parentheses: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = ? AND (name = 'test' OR name = 'test2')",
            &config,
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
        let config = super::SqlObfuscateConfig {
            keep_positional_parameter: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = ? AND name = $1 and id = $2",
            &config,
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
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: super::SqlObfuscationMode::NormalizeOnly,
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
            let got = super::obfuscate_sql(input, &config);
            assert_eq!(got, *expected, "normalize_only input={input:?}");
        }
    }

    #[test]
    fn test_normalize_only_keep_trailing_semi() {
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: super::SqlObfuscationMode::NormalizeOnly,
            keep_trailing_semicolon: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT * FROM users WHERE id = 1 AND name = 'test';",
            &config,
        );
        let expected = "SELECT * FROM users WHERE id = 1 AND name = 'test';";
        assert_eq!(
            got, expected,
            "normalize_only+keep_trailing_semicolon: {got:?}"
        );
    }

    #[test]
    fn test_normalize_only_keep_identifier_quotation() {
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: super::SqlObfuscationMode::NormalizeOnly,
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT * FROM "users" WHERE id = 1 AND name = 'test'"#,
            &config,
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
        let config = super::SqlObfuscateConfig::default();
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
            let got = super::obfuscate_sql(input, &config);
            assert_eq!(got, *expected, "with_cte_stripping input={input:?}");
        }
    }

    #[test]
    fn test_double_quoted_string_value_quantize() {
        // Double-quoted strings in value context (after =) should be quantized
        let config = super::SqlObfuscateConfig::default();
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
            let got = super::obfuscate_sql(input, &config);
            assert_eq!(got, *expected, "double_quoted_value input={input:?}");
        }
    }

    #[test]
    fn test_normalize_only_dollar_func() {
        // In normalize_only mode, dollar-quoted strings are normalized (not quantized)
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: super::SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
            &config,
        );
        let expected = "SELECT $func$INSERT INTO table VALUES ( 'a', 1, 2 )$func$ FROM users";
        assert_eq!(got, expected, "normalize_only dollar_func: {got:?}");
    }

    #[test]
    fn test_dollar_quoted_func_trivial_collapse() {
        // When dollar_quoted_func=true and inner content obfuscates to a single ?, collapse to ?
        let config = super::SqlObfuscateConfig {
            dollar_quoted_func: true,
            replace_digits: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql("SELECT * FROM users123 WHERE id = $tag$1$tag$", &config);
        let expected = "SELECT * FROM users? WHERE id = ?";
        assert_eq!(
            got, expected,
            "dollar_quoted_func trivial collapse: {got:?}"
        );
    }

    #[test]
    fn test_obfuscate_only_keeps_quotes_and_semi() {
        // In obfuscate_only mode: keep double-quoted identifiers, keep $?, keep trailing ;
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: crate::sql::SqlObfuscationMode::ObfuscateOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            r#"SELECT "table"."field" FROM "table" WHERE "table"."otherfield" = $? AND "table"."thirdfield" = $?;"#,
            &config,
        );
        let expected = r#"SELECT "table"."field" FROM "table" WHERE "table"."otherfield" = $? AND "table"."thirdfield" = $?;"#;
        assert_eq!(got, expected, "obfuscate_only keeps quotes/$/semi: {got:?}");
    }

    #[test]
    fn test_obfuscate_only_dollar_quoted_func_no_collapse() {
        // In obfuscate_only+dollar_quoted_func: VALUES inside func are NOT collapsed
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: crate::sql::SqlObfuscationMode::ObfuscateOnly,
            dollar_quoted_func: true,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "SELECT $func$INSERT INTO table VALUES ('a', 1, 2)$func$ FROM users",
            &config,
        );
        let expected = "SELECT $func$INSERT INTO table VALUES (?, ?, ?)$func$ FROM users";
        assert_eq!(
            got, expected,
            "obfuscate_only dollar_quoted_func no collapse: {got:?}"
        );
    }

    #[test]
    fn test_normalize_only_procedure() {
        let config = super::SqlObfuscateConfig {
            obfuscation_mode: super::SqlObfuscationMode::NormalizeOnly,
            ..Default::default()
        };
        let got = super::obfuscate_sql(
            "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END",
            &config,
        );
        let expected =
            "CREATE PROCEDURE TestProc AS BEGIN UPDATE users SET name = 'test' WHERE id = 1 END";
        assert_eq!(got, expected, "normalize_only+procedure: {got:?}");
    }

    #[test]
    fn test_q41() {
        let config = super::SqlObfuscateConfig::default();
        let input = "SELECT * FROM public.table ( array [ ROW ( array [ 'magic', 'foo',";
        // First check raw (pre-collapse) output
        let mut tok = super::Tokenizer::new(input, &config);
        tok.process();
        let raw = tok.finalize();
        eprintln!("RAW: {raw:?}");
        let got = super::obfuscate_sql(input, &config);
        let expected = "SELECT * FROM public.table ( array [ ROW ( array [ ?";
        assert_eq!(got, expected, "q41: {got:?}");
    }

    // --- Integration test cases added for faster iteration ---

    #[test]
    fn test_pg_json_operators_7() {
        // JSONB ? operator followed by string literal — both should be kept as ?
        let config = super::SqlObfuscateConfig {
            dbms: super::DbmsKind::Postgresql,
            ..Default::default()
        };
        let got = super::obfuscate_sql("select * from users where user.custom ? 'foo'", &config);
        let expected = "select * from users where user.custom ? ?";
        assert_eq!(got, expected, "pg_json_7: {got:?}");
    }

    #[test]
    fn test_quantizer_90() {
        // Inline comment /*!obfuscation*/ should be stripped; consecutive literals after = reset
        let config = super::SqlObfuscateConfig::default();
        let got = super::obfuscate_sql(
            "SELECT * FROM dbo.Items WHERE id = 1 or /*!obfuscation*/ 1 = 1",
            &config,
        );
        let expected = "SELECT * FROM dbo.Items WHERE id = ? or ? = ?";
        assert_eq!(got, expected, "q90: {got:?}");
    }

    #[test]
    fn test_cassandra_nested_dates() {
        // Consecutive ? placeholders inside nested function calls should be suppressed
        let config = super::SqlObfuscateConfig::default();
        let got = super::obfuscate_sql(
            "SELECT TO_DATE(TO_CHAR(TO_DATE(bar.h,?),?),?) FROM t",
            &config,
        );
        let expected = "SELECT TO_DATE ( TO_CHAR ( TO_DATE ( bar.h, ? ) ) ) FROM t";
        assert_eq!(got, expected, "cassandra_nested_dates: {got:?}");
    }

    #[test]
    fn test_cassandra_pipe_concat() {
        // || concatenation — Go tokenizes as two separate | tokens with spaces
        let config = super::SqlObfuscateConfig::default();
        let got = super::obfuscate_sql("SELECT a ||?|| b FROM t", &config);
        let expected = "SELECT a | | ? | | b FROM t";
        assert_eq!(got, expected, "cassandra_pipe: {got:?}");
    }
}
