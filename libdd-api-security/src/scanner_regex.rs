// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ScannerError;
use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::Infallible;
use regex_syntax::ast::{self, AssertionKind, Ast, ClassPerl, ClassPerlKind, ClassSetItem, Span};

// RE2's Perl classes and word boundaries are ASCII, while the Rust regex
// engine defaults to Unicode. Use the existing syntax parser to distinguish
// escapes, literal backslashes, classes and comments; do not hand-parse regexes.
pub fn normalize(pattern: &str) -> Result<Cow<'_, str>, ScannerError> {
    let ast = ast::parse::Parser::new()
        .parse(pattern)
        .map_err(|error| ScannerError::RegexSyntax(alloc::boxed::Box::new(error)))?;
    let replacements = match ast::visit(&ast, Replacements::default()) {
        Ok(replacements) => replacements,
        Err(impossible) => match impossible {},
    };
    if replacements.is_empty() {
        return Ok(Cow::Borrowed(pattern));
    }
    let mut output = String::with_capacity(pattern.len());
    let mut cursor = 0;
    for (span, replacement) in replacements {
        // Spans refer to token boundaries in this exact parsed input.
        output.push_str(&pattern[cursor..span.start.offset]);
        output.push_str(replacement);
        cursor = span.end.offset;
    }
    output.push_str(&pattern[cursor..]);
    Ok(Cow::Owned(output))
}

#[derive(Default)]
struct Replacements(Vec<(Span, &'static str)>);

impl Replacements {
    fn class(&mut self, class: &ClassPerl) {
        let replacement = match (&class.kind, class.negated) {
            (ClassPerlKind::Digit, false) => "[0-9]",
            (ClassPerlKind::Digit, true) => "[^0-9]",
            (ClassPerlKind::Word, false) => "[A-Za-z0-9_]",
            (ClassPerlKind::Word, true) => "[^A-Za-z0-9_]",
            (ClassPerlKind::Space, false) => r"[\t\n\f\r ]",
            (ClassPerlKind::Space, true) => r"[^\t\n\f\r ]",
        };
        self.0.push((class.span, replacement));
    }
}

impl ast::Visitor for Replacements {
    type Output = Vec<(Span, &'static str)>;
    type Err = Infallible;

    fn finish(mut self) -> Result<Self::Output, Self::Err> {
        self.0.sort_unstable_by_key(|(span, _)| span.start.offset);
        Ok(self.0)
    }

    fn visit_pre(&mut self, node: &Ast) -> Result<(), Self::Err> {
        match node {
            Ast::ClassPerl(class) => self.class(class),
            Ast::Assertion(assertion) => match assertion.kind {
                AssertionKind::WordBoundary => self.0.push((assertion.span, r"(?-u:\b)")),
                AssertionKind::NotWordBoundary => self.0.push((assertion.span, r"(?-u:\B)")),
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn visit_class_set_item_pre(&mut self, item: &ClassSetItem) -> Result<(), Self::Err> {
        if let ClassSetItem::Perl(class) = item {
            self.class(class);
        }
        Ok(())
    }
}
