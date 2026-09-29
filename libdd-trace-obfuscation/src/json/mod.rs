// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! JSON obfuscation with caller-provided value transforms.
//!
//! Obfuscation is a single scanning pass that copies the characters it keeps, so keys come out in
//! the order they were sent and malformed input is still obfuscated up to the point where it
//! breaks. Whitespace between tokens is dropped.
//!
//! [`JsonObfuscator::obfuscate`] covers the ordinary case. Callers that want the values of
//! [`JsonObfuscatorConfig::transform_keys`] rewritten - SQL obfuscation, typically - pass a
//! callback to [`JsonObfuscator::obfuscate_with`] or, on a hot path,
//! [`JsonObfuscator::obfuscate_into`]. The callback is a method-scoped generic, so it can capture
//! whatever runtime configuration the caller has without that configuration having to live in
//! [`JsonObfuscatorConfig`], which is plain serializable data.

use crate::obfuscation_config::JsonObfuscatorConfig;
mod scanner;
use alloc::borrow::Cow;
use core::{convert::Infallible, ops::Range};
use scanner::{Op, Scanner};
use serde::{Deserialize, Serialize};

/// The value written in place of anything that is obfuscated.
const OBFUSCATED_VALUE: &str = "\"?\"";

/// Appended to output that a syntax error cut short.
const TRUNCATION_MARKER: &str = "...";

/// A JSON syntax error found while scanning. The output produced up to the error is still usable
/// and ends in `...`; see [`JsonObfuscator::obfuscate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum JsonScanError {
    /// A character that cannot appear at this point in the grammar. `context` says where the
    /// scanner was, e.g. `looking for beginning of value`.
    #[error("invalid character '{character}' {context}")]
    InvalidCharacter {
        /// The offending character.
        character: char,
        /// Where in the grammar the scanner was when it read `character`.
        context: &'static str,
    },
    /// The input ended in the middle of a value.
    #[error("unexpected end of JSON input at char position {char_position}")]
    UnexpectedEndOfInput {
        /// Number of characters consumed when the input ran out.
        char_position: u64,
    },
}

/// What an obfuscation pass could not do, if anything.
///
/// The two channels are separate because they mean different things: a [`JsonScanError`] says the
/// input was not valid JSON and the output is truncated, while a transform error says a callback
/// the caller supplied failed on one value and the rest of the document is intact. A pass can
/// report both.
///
/// Reporting is feedback, not a failure: the obfuscated output is always usable, and an error here
/// never leaves an unobfuscated value in it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[must_use]
pub struct JsonObfuscationReport<E> {
    /// The syntax error that cut the scan short, if any. The output then ends in `...`.
    pub scan_error: Option<JsonScanError>,
    /// Errors returned by the transform callback, in the order the values were read. Each value
    /// whose transform failed was written as `"?"`, the same as any other obfuscated value.
    pub transform_errors: Vec<E>,
}

// A derived `Default` would ask for `E: Default`, which a caller's error type has no reason to be.
impl<E> Default for JsonObfuscationReport<E> {
    fn default() -> Self {
        Self {
            scan_error: None,
            transform_errors: Vec::new(),
        }
    }
}

impl<E> JsonObfuscationReport<E> {
    /// True when the pass scanned the whole input and every transform succeeded.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.scan_error.is_none() && self.transform_errors.is_empty()
    }
}

/// Output and diagnostics from an allocating JSON obfuscation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[must_use]
pub struct JsonObfuscationOutcome<E> {
    /// The obfuscated JSON.
    pub output: String,
    /// Problems encountered while producing `output`.
    pub report: JsonObfuscationReport<E>,
}

/// Reusable working memory for [`JsonObfuscator::obfuscate_into`].
///
/// A scratch is an optimization, not state: passing a fresh one to every call gives the same
/// output as reusing one. Reuse only avoids the allocations a pass would otherwise make.
///
/// # Retained memory
///
/// A scratch keeps the buffers it grew, so the memory it holds is bounded by the largest input it
/// has ever seen, and nothing shrinks it on its own. A caller that feeds it one unusually large
/// document holds that memory until the scratch is dropped, [`Self::clear`]ed (which keeps
/// capacity) or [`Self::trim_to`]'d (which does not). [`Self::retained_capacity`] reports what is
/// held. The allocating entry points ([`JsonObfuscator::obfuscate`],
/// [`JsonObfuscator::obfuscate_with`]) use a scratch that lives for one call and retain nothing.
#[derive(Debug, Default)]
pub struct JsonObfuscationScratch {
    scanner: Scanner,
    closures: Vec<ClosureKind>,
    /// Holds a transform value that had to be unescaped. Values without escape sequences are
    /// passed to the callback as a slice of the input and never touch this.
    unescaped: String,
}

/// How much memory a [`JsonObfuscationScratch`] is holding, and the limits
/// [`JsonObfuscationScratch::trim_to`] shrinks it to.
///
/// Both counts are approximate: they describe the buffers' capacities, not the allocator's
/// bookkeeping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScratchCapacity {
    /// Capacity of the transform-value unescape buffer, in bytes.
    pub unescape_bytes: usize,
    /// Nesting levels the container stacks can hold.
    pub nesting_slots: usize,
}

impl ScratchCapacity {
    /// A limit pair, for [`JsonObfuscationScratch::trim_to`].
    #[must_use]
    pub const fn new(unescape_bytes: usize, nesting_slots: usize) -> Self {
        Self {
            unescape_bytes,
            nesting_slots,
        }
    }
}

impl JsonObfuscationScratch {
    /// A scratch holding no memory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties the scratch, keeping its allocations for the next pass.
    ///
    /// Passes clear what they use as they go, so this is only needed by a caller that wants a
    /// scratch to hold no live data between passes.
    pub fn clear(&mut self) {
        self.scanner.restart();
        self.closures.clear();
        self.unescaped.clear();
    }

    /// Approximately how much memory the scratch is holding.
    #[must_use]
    pub fn retained_capacity(&self) -> ScratchCapacity {
        ScratchCapacity {
            unescape_bytes: self.unescaped.capacity(),
            // The two stacks grow together, so the larger one is the one that matters.
            nesting_slots: self
                .closures
                .capacity()
                .max(self.scanner.retained_nesting_slots()),
        }
    }

    /// Releases memory beyond `limits`, as far as the allocator allows.
    ///
    /// The scratch stays usable; the next pass simply regrows what it needs. Trimming to
    /// [`ScratchCapacity::default`] asks for everything back.
    pub fn trim_to(&mut self, limits: ScratchCapacity) {
        self.unescaped.shrink_to(limits.unescape_bytes);
        self.closures.shrink_to(limits.nesting_slots);
        self.scanner.trim_nesting_slots_to(limits.nesting_slots);
    }
}

/// Obfuscates a JSON string by replacing every leaf value with `"?"`.
///
/// A value belonging to a key listed in [`JsonObfuscatorConfig::keep_keys`] is kept verbatim,
/// along with everything nested under it.
///
/// String values under a key listed in [`JsonObfuscatorConfig::transform_keys`] are passed to the
/// callback given to [`Self::obfuscate_with`] or [`Self::obfuscate_into`]. Entry points that take
/// no callback obfuscate those values like any other.
///
/// Multiple concatenated JSON objects in the input are each obfuscated independently. On a syntax
/// error the output so far is returned with `...` appended.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct JsonObfuscator {
    config: JsonObfuscatorConfig,
}

#[derive(Clone, Copy, Debug)]
enum ClosureKind {
    Array,
    Object,
}

impl JsonObfuscator {
    /// Builds an obfuscator from its configuration.
    #[must_use]
    pub const fn new(config: JsonObfuscatorConfig) -> Self {
        Self { config }
    }

    /// The configuration this obfuscator was built from.
    #[must_use]
    pub const fn config(&self) -> &JsonObfuscatorConfig {
        &self.config
    }

    /// Obfuscates a JSON string, returning `None` for empty input.
    ///
    /// Any scan error is discarded and a possibly truncated value returned, as with
    /// [`Self::obfuscate`]. This method does not run a transform; values under
    /// [`JsonObfuscatorConfig::transform_keys`] are handled like other unkept values.
    // TODO(APMSP-2764): surface the scan error instead of discarding it.
    #[must_use]
    pub fn obfuscate_opt(&self, input: &str) -> Option<String> {
        if input.is_empty() {
            return None;
        }
        let (res, _err) = self.obfuscate(input);
        Some(res)
    }

    /// Obfuscates a JSON string, returning the syntax error that cut the scan short, if any.
    ///
    /// The returned string is usable either way; on error it is the part of the input that
    /// scanned, with `...` appended. Values under [`JsonObfuscatorConfig::transform_keys`] are
    /// obfuscated like any other value - use [`Self::obfuscate_with`] to transform them.
    #[must_use]
    pub fn obfuscate(&self, input: &str) -> (String, Option<JsonScanError>) {
        let mut out = String::with_capacity(input.len());
        let mut scratch = JsonObfuscationScratch::new();
        let report = self.run(input, &mut out, &mut scratch, NO_TRANSFORM);
        (out, report.scan_error)
    }

    /// Obfuscates a JSON string, passing the value of every key in
    /// [`JsonObfuscatorConfig::transform_keys`] through `transform`.
    ///
    /// `transform` receives the unescaped string value and returns its replacement, borrowed from
    /// its argument, borrowed from elsewhere (a `&'static str` works), or owned. The replacement
    /// is JSON-escaped before it is written. A value whose transform returns `Err` is written as
    /// `"?"` and the error is reported; the pass continues.
    ///
    /// This allocates an output string per call and retains nothing. Use [`Self::obfuscate_into`]
    /// on a hot path.
    ///
    /// # Examples
    ///
    /// ```
    /// use libdd_trace_obfuscation::{
    ///     json::JsonObfuscator,
    ///     obfuscation_config::{DbmsKind, JsonObfuscatorConfig, SqlConfig},
    ///     sql::obfuscate_sql,
    /// };
    /// use std::borrow::Cow;
    ///
    /// let sql_config = SqlConfig::default();
    /// let mut config = JsonObfuscatorConfig::enabled();
    /// config.transform_keys.insert("query".to_owned());
    /// let obfuscator = JsonObfuscator::new(config);
    ///
    /// let outcome = obfuscator
    ///     .obfuscate_with(r#"{"query":"select * from users where id = 42"}"#, |sql| {
    ///         obfuscate_sql(sql, &sql_config, DbmsKind::Generic).map(Cow::Owned)
    ///     });
    ///
    /// assert!(outcome.report.is_clean());
    /// assert_eq!(
    ///     outcome.output,
    ///     r#"{"query":"select * from users where id = ?"}"#
    /// );
    /// ```
    pub fn obfuscate_with<F, E>(&self, input: &str, transform: F) -> JsonObfuscationOutcome<E>
    where
        F: for<'a> FnMut(&'a str) -> Result<Cow<'a, str>, E>,
    {
        let mut output = String::with_capacity(input.len());
        let mut scratch = JsonObfuscationScratch::new();
        let report = self.run(input, &mut output, &mut scratch, Some(transform));
        JsonObfuscationOutcome { output, report }
    }

    /// Obfuscates a JSON string into a caller-owned output string and scratch.
    ///
    /// This is [`Self::obfuscate_with`] without the per-call allocations. `output` is cleared
    /// first, so its capacity is reused, and `scratch` supplies the working memory the pass would
    /// otherwise allocate. Neither is shared or retained by the obfuscator; see
    /// [`JsonObfuscationScratch`] for what a reused scratch holds on to.
    ///
    /// # Examples
    ///
    /// ```
    /// use libdd_trace_obfuscation::{
    ///     json::{JsonObfuscationScratch, JsonObfuscator},
    ///     obfuscation_config::JsonObfuscatorConfig,
    /// };
    /// use std::{borrow::Cow, convert::Infallible};
    ///
    /// let mut config = JsonObfuscatorConfig::enabled();
    /// config.transform_keys.insert("query".to_owned());
    /// let obfuscator = JsonObfuscator::new(config);
    /// let mut output = String::new();
    /// let mut scratch = JsonObfuscationScratch::new();
    ///
    /// for (input, expected) in [
    ///     (r#"{"query":"first"}"#, r#"{"query":"FIRST"}"#),
    ///     (r#"{"query":"second"}"#, r#"{"query":"SECOND"}"#),
    /// ] {
    ///     let report = obfuscator.obfuscate_into(input, &mut output, &mut scratch, |value| {
    ///         Ok::<_, Infallible>(Cow::Owned(value.to_uppercase()))
    ///     });
    ///
    ///     assert!(report.is_clean());
    ///     assert_eq!(output, expected);
    /// }
    /// ```
    pub fn obfuscate_into<F, E>(
        &self,
        input: &str,
        output: &mut String,
        scratch: &mut JsonObfuscationScratch,
        transform: F,
    ) -> JsonObfuscationReport<E>
    where
        F: for<'a> FnMut(&'a str) -> Result<Cow<'a, str>, E>,
    {
        output.clear();
        output.reserve(input.len());
        self.run(input, output, scratch, Some(transform))
    }

    fn run<F, E>(
        &self,
        input: &str,
        out: &mut String,
        scratch: &mut JsonObfuscationScratch,
        transform: Option<F>,
    ) -> JsonObfuscationReport<E>
    where
        F: for<'a> FnMut(&'a str) -> Result<Cow<'a, str>, E>,
    {
        let mut report = JsonObfuscationReport::default();
        if input.is_empty() {
            return report;
        }

        scratch.scanner.restart();
        scratch.closures.clear();
        let mut parser = ParserState {
            config: &self.config,
            input,
            out,
            unescaped: &mut scratch.unescaped,
            closures: &mut scratch.closures,
            transform,
            report: &mut report,
            // The input is one value, so it starts where a value is expected. A key follows only
            // once an object has been opened.
            phase: ParserPhase::AwaitingValue,
        };
        parser.run(&mut scratch.scanner);
        report
    }
}

/// The callback type the no-transform entry points instantiate `run` with.
type NoTransform = for<'a> fn(&'a str) -> Result<Cow<'a, str>, Infallible>;

/// `None`, typed so `run`'s generic parameters can be inferred without a callback.
const NO_TRANSFORM: Option<NoTransform> = None;

/// A value that is kept as sent, with everything nested under it.
///
/// The pass copies characters instead of obfuscating them until it reaches the end of this value.
/// Since one value is one subtree, the depth the pass is at says whether it is still inside: the
/// subtree ends at the first value boundary back at the depth the key was read at.
#[derive(Clone, Copy, Debug)]
struct KeptSubtree {
    /// Nesting depth of the object holding the key that opened the subtree.
    parent_depth: usize,
}

/// What one pass is reading, and what it is doing with the characters.
///
/// Definition: a `key` refers to a JSON key. in `{ "foo": 1 }`, `foo` is a key.
///
/// A phase carries only the state it uses: the byte range of the literal in hand, and the kept
/// subtree the pass sits inside, if any. So a phase that reads no literal cannot see a stale
/// range, an obfuscated value cannot also be kept or transformed, and keeping survives the phases
/// that read a key or transform a value inside the kept subtree.
#[derive(Debug)]
enum ParserPhase {
    /// The next literal is a key.
    AwaitingKey { keep: Option<KeptSubtree> },
    /// Reading a key, whose text decides how its value is handled.
    ReadingKey {
        keep: Option<KeptSubtree>,
        literal: Range<usize>,
    },
    /// The next value is obfuscated.
    AwaitingValue,
    /// The obfuscated value is written, so the rest of its characters are dropped.
    ObfuscatedValue,
    /// Inside a value that is copied as sent.
    KeepingValue(KeptSubtree),
    /// A transform key was read and its value has not started. An object or an array here ends the
    /// attempt, because only a literal is transformed.
    AwaitingTransform { keep: Option<KeptSubtree> },
    /// Reading the literal `transform` rewrites once the value ends.
    TransformingValue {
        keep: Option<KeptSubtree>,
        literal: Range<usize>,
    },
}

impl ParserPhase {
    /// The kept subtree the pass is inside, which outlives the phase that reads one key or value
    /// within it.
    const fn kept_subtree(&self) -> Option<KeptSubtree> {
        match self {
            Self::AwaitingKey { keep }
            | Self::ReadingKey { keep, .. }
            | Self::AwaitingTransform { keep }
            | Self::TransformingValue { keep, .. } => *keep,
            Self::KeepingValue(scope) => Some(*scope),
            Self::AwaitingValue | Self::ObfuscatedValue => None,
        }
    }

    /// The range of the literal being read, for the phases that read one.
    const fn literal_mut(&mut self) -> Option<&mut Range<usize>> {
        match self {
            Self::ReadingKey { literal, .. } | Self::TransformingValue { literal, .. } => {
                Some(literal)
            }
            Self::AwaitingKey { .. }
            | Self::AwaitingValue
            | Self::ObfuscatedValue
            | Self::KeepingValue(_)
            | Self::AwaitingTransform { .. } => None,
        }
    }

    /// Whether the characters of the current literal go to the output as they are read. A
    /// transformed literal is written in one piece when the value ends, and an obfuscated one is
    /// dropped.
    const fn copies_literal(&self) -> bool {
        matches!(self, Self::ReadingKey { .. } | Self::KeepingValue(_))
    }
}

/// One pass over one input.
struct ParserState<'a, F, E> {
    config: &'a JsonObfuscatorConfig,
    input: &'a str,
    out: &'a mut String,
    unescaped: &'a mut String,
    closures: &'a mut Vec<ClosureKind>,
    transform: Option<F>,
    report: &'a mut JsonObfuscationReport<E>,

    phase: ParserPhase,
}

impl<F, E> ParserState<'_, F, E>
where
    F: for<'a> FnMut(&'a str) -> Result<Cow<'a, str>, E>,
{
    fn run(&mut self, scanner: &mut Scanner) {
        for (i, c) in self.input.char_indices() {
            let op = scanner.step(c);
            // The depth before this character is applied, which is the depth of the value or key
            // that just ended.
            let depth = self.closures.len();

            match op {
                Op::BeginObject => {
                    self.closures.push(ClosureKind::Object);
                    // Opening a container drops a pending transform but not the kept subtree.
                    self.begin_next_token(self.phase.kept_subtree());
                }
                Op::BeginArray => {
                    self.closures.push(ClosureKind::Array);
                    self.begin_next_token(self.phase.kept_subtree());
                }
                Op::EndArray | Op::EndObject => {
                    self.closures.pop();
                    let keep = self.finish_value(depth);
                    self.begin_next_token(keep);
                }
                Op::ObjectValue | Op::ArrayValue => {
                    let keep = self.finish_value(depth);
                    self.begin_next_token(keep);
                }
                Op::BeginLiteral | Op::Continue => {
                    // Track the literal's byte range, so it can be read back as a slice of the
                    // input instead of being copied into a buffer character by character.
                    let end = i + c.len_utf8();
                    if op == Op::BeginLiteral {
                        self.begin_literal(i..end);
                    } else if let Some(literal) = self.phase.literal_mut() {
                        literal.end = end;
                    }

                    if !self.phase.copies_literal() {
                        continue;
                    }
                    // A key, or a value being kept: copied through below.
                }
                Op::ObjectKey => self.finish_key(depth),
                Op::SkipSpace => continue,
                Op::Error => {
                    self.out.push_str(TRUNCATION_MARKER);
                    self.report.scan_error = scanner.err;
                    return;
                }
                // Whitespace after a document ended, which is kept.
                Op::End => {}
            }

            self.out.push(c);
        }

        if scanner.eof() == Op::Error {
            self.out.push_str(TRUNCATION_MARKER);
        }
        self.report.scan_error = scanner.err;
    }

    /// Starts the next key or value, inside `keep` if a kept subtree is still open.
    ///
    /// A key follows at the top level and inside an object, but not inside an array.
    fn begin_next_token(&mut self, keep: Option<KeptSubtree>) {
        let expects_key = matches!(self.closures.last(), None | Some(ClosureKind::Object));
        self.phase = if expects_key {
            ParserPhase::AwaitingKey { keep }
        } else if let Some(scope) = keep {
            ParserPhase::KeepingValue(scope)
        } else {
            ParserPhase::AwaitingValue
        };
    }

    /// Starts reading a literal in the phases that care what it says, and writes the obfuscated
    /// value for the ones that do not.
    fn begin_literal(&mut self, literal: Range<usize>) {
        match self.phase {
            ParserPhase::AwaitingKey { keep } => {
                self.phase = ParserPhase::ReadingKey { keep, literal };
            }
            ParserPhase::AwaitingTransform { keep } => {
                self.phase = ParserPhase::TransformingValue { keep, literal };
            }
            ParserPhase::AwaitingValue => {
                self.out.push_str(OBFUSCATED_VALUE);
                self.phase = ParserPhase::ObfuscatedValue;
            }
            // The scanner does not start a literal inside a literal, and a kept value needs no
            // range: it is copied as it is read.
            ParserPhase::ReadingKey { .. }
            | ParserPhase::ObfuscatedValue
            | ParserPhase::KeepingValue(_)
            | ParserPhase::TransformingValue { .. } => {}
        }
    }

    /// Chooses how the value of the key just read is handled.
    fn finish_key(&mut self, depth: usize) {
        let ParserPhase::ReadingKey { keep, literal } = &self.phase else {
            // The scanner reports a key only after its literal, so this is unreachable.
            // Obfuscating is the safe answer if it ever is reached.
            self.phase = ParserPhase::AwaitingValue;
            return;
        };
        let keep = *keep;
        // The scanner guarantees a key is a quoted string, so the quotes are the first and last
        // characters of the literal.
        let key = self.input.get(literal.clone()).unwrap_or_default();
        let key = key.trim_matches('"');
        let transforms = self.transform.is_some() && self.config.transform_keys.contains(key);

        self.phase = match keep {
            // Inside a kept subtree the value is kept whatever the key says, except that a
            // transform key still rewrites its own value.
            Some(scope) if transforms => ParserPhase::AwaitingTransform { keep: Some(scope) },
            Some(scope) => ParserPhase::KeepingValue(scope),
            // A key in both sets is kept rather than transformed.
            None if self.config.keep_keys.contains(key) => ParserPhase::KeepingValue(KeptSubtree {
                parent_depth: depth,
            }),
            None if transforms => ParserPhase::AwaitingTransform { keep: None },
            None => ParserPhase::AwaitingValue,
        };
    }

    /// Handles the end of a value: rewrites it through `transform` if a literal was collected, or
    /// leaves a kept subtree once the pass climbs back out of it. Returns the kept subtree that is
    /// still open.
    fn finish_value(&mut self, depth: usize) -> Option<KeptSubtree> {
        let ParserPhase::TransformingValue { keep, literal } = &self.phase else {
            let keep = self.phase.kept_subtree();
            return keep.filter(|kept| depth > kept.parent_depth);
        };
        let keep = *keep;
        let literal = literal.clone();
        self.transform_value(literal);
        keep
    }

    /// Writes the collected literal through `transform`.
    fn transform_value(&mut self, literal: Range<usize>) {
        let Some(transform) = self.transform.as_mut() else {
            return;
        };

        // A literal arrives here as it was written. A string is unescaped before it is
        // transformed; anything else - a number, `true`, a string `serde_json` will not accept -
        // is passed on verbatim.
        //
        // `literal` is built from `char_indices` offsets of `input`, so it is in bounds and on
        // character boundaries; `unwrap_or_default` only keeps that assumption from panicking.
        let literal = self.input.get(literal).unwrap_or_default();
        let value = match unescape(literal, self.unescaped) {
            Unescaped::Borrowed(s) => s,
            Unescaped::Buffered => self.unescaped.as_str(),
        };

        match transform(value) {
            Ok(replacement) => push_json_string(self.out, &replacement),
            Err(err) => {
                // Fall back to the ordinary obfuscated value: a failed transform must not leak
                // the value it could not rewrite.
                self.out.push_str(OBFUSCATED_VALUE);
                self.report.transform_errors.push(err);
            }
        }
    }
}

fn push_json_string(out: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    out.push('"');
    let mut copied_through = 0;
    for (i, byte) in value.bytes().enumerate() {
        let escape = match byte {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            b'\x08' => Some("\\b"),
            b'\t' => Some("\\t"),
            b'\n' => Some("\\n"),
            b'\x0c' => Some("\\f"),
            b'\r' => Some("\\r"),
            0x00..=0x1f => {
                out.push_str(&value[copied_through..i]);
                out.push_str("\\u00");
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0f)]));
                copied_through = i + 1;
                continue;
            }
            _ => None,
        };
        if let Some(escape) = escape {
            out.push_str(&value[copied_through..i]);
            out.push_str(escape);
            copied_through = i + 1;
        }
    }
    out.push_str(&value[copied_through..]);
    out.push('"');
}

/// Where the unescaped form of a JSON string literal ended up.
enum Unescaped<'a> {
    /// A slice of the literal, because there was nothing to unescape.
    Borrowed(&'a str),
    /// The caller's buffer, because there was.
    Buffered,
}

/// Unescapes the JSON string literal `literal` (quotes included) for the transform callback.
///
/// A literal with no escape sequence is the common case and is returned as a slice of itself,
/// leaving `buffer` untouched. Anything else goes through `serde_json`, into `buffer`. A literal
/// that is not a string, or that `serde_json` rejects, is returned verbatim, quotes and all.
fn unescape<'a>(literal: &'a str, buffer: &mut String) -> Unescaped<'a> {
    let unquoted = literal.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
    match unquoted {
        Some(s) if !s.contains('\\') => Unescaped::Borrowed(s),
        Some(_) => {
            buffer.clear();
            // `serde_json` cannot append to a `&mut String`, so the escaped case - the uncommon
            // one - allocates a string that is then copied into the reusable buffer.
            match serde_json::from_str::<String>(literal) {
                Ok(s) => {
                    buffer.push_str(&s);
                    Unescaped::Buffered
                }
                Err(_) => Unescaped::Borrowed(literal),
            }
        }
        None => Unescaped::Borrowed(literal),
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::Cow;
    use core::cell::Cell;
    use duplicate::duplicate_item;
    use serde_json::json;

    use super::{
        JsonObfuscationOutcome, JsonObfuscationReport, JsonObfuscationScratch, JsonObfuscator,
        JsonScanError, ScratchCapacity,
    };
    use crate::{
        obfuscation_config::{DbmsKind, JsonObfuscatorConfig, SqlConfig, SqlObfuscationMode},
        sql::{obfuscate_sql, SqlObfuscationError, SQL_OBFUSCATION_FAILURE_REPLACEMENT},
    };

    fn obf(keep_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(JsonObfuscatorConfig {
            enabled: true,
            keep_keys: keep_keys
                .iter()
                .map(alloc::string::ToString::to_string)
                .collect(),
            ..Default::default()
        })
    }

    fn obf_keys(keep_keys: &[&str], transform_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(JsonObfuscatorConfig {
            enabled: true,
            keep_keys: keep_keys
                .iter()
                .map(alloc::string::ToString::to_string)
                .collect(),
            transform_keys: transform_keys
                .iter()
                .map(alloc::string::ToString::to_string)
                .collect(),
        })
    }

    /// The SQL transform a production caller writes: obfuscate with a captured runtime config and
    /// let the error reach the report.
    fn sql_transform(
        config: &SqlConfig,
    ) -> impl for<'a> FnMut(&'a str) -> Result<Cow<'a, str>, SqlObfuscationError> + '_ {
        move |sql| obfuscate_sql(sql, config, DbmsKind::Generic).map(Cow::Owned)
    }

    fn obfuscate_sql_values(obfuscator: &JsonObfuscator, input: &str) -> String {
        let config = SqlConfig::default();
        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obfuscator.obfuscate_with(input, sql_transform(&config));
        assert_eq!(report.scan_error, None);
        out
    }

    fn assert_json_eq(result: &str, expected: &str) {
        let result: serde_json::Value =
            serde_json::from_str(result).expect("result is not valid JSON");
        let expected: serde_json::Value =
            serde_json::from_str(expected).expect("expected is not valid JSON");
        assert_eq!(result, expected);
    }

    // Basic obfuscation tests — parametric over (keep_keys, input, expected).
    // Uses assert_json_eq (structural comparison, whitespace-insensitive).
    #[duplicate_item(
        test_name                         keep_keys           input                                                                                                                          expected;
        [test_empty_object]               [&[]]               ["{}"]                                                                                                                         ["{}"];
        [test_empty_array]                [&[]]               ["[]"]                                                                                                                         ["[]"];
        [test_emoji_object]                [&["🐵"]]               [r#"{"🐵":"🙊"}"#]                                                                                                                         [r#"{"🐵":"🙊"}"#];
        [test_nested_empty_objects]       [&[]]               [r#"{"a":{},"b":{"c":{}}}"#]                                                                                                  [r#"{"a":{},"b":{"c":{}}}"#];
        [test_boolean_and_null_obfuscated][&[]]               [r#"{"a":true,"b":false,"c":null}"#]                                                                                          [r#"{"a":"?","b":"?","c":"?"}"#];
        [test_all_values_obfuscated]      [&[]]               [r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#]           [r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["?","?",{"k":"?"}]},"?"]}}}"#];
        [test_numbers_obfuscated]         [&[]]               [r#"{"highlight":{"pre_tags":["<em>"],"post_tags":["</em>"],"index":1}}"#]                                                    [r#"{"highlight":{"pre_tags":["?"],"post_tags":["?"],"index":"?"}}"#];
        [test_keep_key_keeps_entire_value][&["other"]]        [r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#]           [r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["1","2",{"k":"v"}]},"?"]}}}"#];
        [test_keep_key_nested_array_fully_kept][&["fields"]]  [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#]                                                    [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#];
        [test_keep_key_deep_nested]       [&["k"]]            [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#]                                                    [r#"{"fields":["?",{"key":"?","other":["?","?",{"k":"v"}]},"?"]}"#];
        [test_keep_key_in_nested_object]  [&["C"]]            [r#"{"fields":[{"A":1,"B":{"C":3}},"2"]}"#]                                                                                   [r#"{"fields":[{"A":"?","B":{"C":3}},"?"]}"#];
        [test_keep_key_large_nested_structure][&["hits"]]     [r#"{"outer":{"total":2,"max_score":0.9105287,"hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#]                      [r#"{"outer":{"total":"?","max_score":"?","hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#];
        [test_keep_multiple_keys]         [&["_index","title"]][r#"{"hits":[{"_index":"bookdb_index","_type":"book","_score":0.9,"_source":{"summary":"text","title":"ES in Action","publish_date":"2015-12-03"},"highlight":{"title":["ES Action"]}}]}"#] [r#"{"hits":[{"_index":"bookdb_index","_type":"?","_score":"?","_source":{"summary":"?","title":"ES in Action","publish_date":"?"},"highlight":{"title":["ES Action"]}}]}"#];
        [test_keep_key_wallet]            [&["company_wallet_configuration_id"]] [r#"{"email":"dev@datadoghq.com","company_wallet_configuration_id":1}"#] [r#"{"email":"?","company_wallet_configuration_id":1}"#];
    )]
    #[test]
    fn test_name() {
        let (res, err) = obf(keep_keys).obfuscate(input);
        assert_eq!(err, None);
        assert_json_eq(&res, expected);
    }

    // Truncation / error tests — parametric over (input, expected_exact_string).
    #[duplicate_item(
        test_name                           input                                                                    expected                                          expected_error;
        [test_empty_input]                  [""]                                                                     [""]                                              [None];
        [test_invalid_emoji]                ["🤨"]                                                                   ["..."]                                           [Some(JsonScanError::InvalidCharacter { character: '🤨', context: "looking for beginning of value" })];
        [test_invalid_unicode]              ["ჸ"]                                                                    ["..."]                                           [Some(JsonScanError::InvalidCharacter { character: 'ჸ', context: "looking for beginning of value" })];
        [test_invalid_json_appends_ellipsis]["INVALID"]                                                              ["..."]                                           [Some(JsonScanError::InvalidCharacter { character: 'I', context: "looking for beginning of value" })];
        [test_invalid_single_char]          [")"]                                                                    ["..."]                                           [Some(JsonScanError::InvalidCharacter { character: ')', context: "looking for beginning of value" })];
        [test_truncated_open_value_string]  [r#"{"query":""#]                                                        [r#"{"query":"?"..."#]                            [Some(JsonScanError::UnexpectedEndOfInput { char_position: 11 })];
        [test_truncated_multi_json]         [r#"{"first json": "valid"} {"second json": "unfinished"#]               [r#"{"first json":"?"} {"second json":"?"..."#]   [Some(JsonScanError::UnexpectedEndOfInput { char_position: 53 })];
    )]
    #[test]
    fn test_name() {
        let (res, err) = obf(&[]).obfuscate(input);
        assert_eq!(res, expected);
        assert_eq!(err, expected_error);
    }

    /// The error messages are the Agent's, because they reach users through the same channels.
    #[test]
    fn test_scan_error_messages_match_the_agent() {
        let (_, err) = obf(&[]).obfuscate("INVALID");
        assert_eq!(
            err.expect("error").to_string(),
            "invalid character 'I' looking for beginning of value"
        );
        let (_, err) = obf(&[]).obfuscate(r#"{"query":""#);
        assert_eq!(
            err.expect("error").to_string(),
            "unexpected end of JSON input at char position 11"
        );
    }

    /// The obfuscator copies keys in the order it reads them, because it is a scanner rather than a
    /// `serde_json::Map`, which is why this crate does not need `serde_json/preserve_order` - a
    /// feature it cannot enable without enabling it for every crate in a dependent's workspace.
    #[test]
    fn test_key_order_is_the_input_order() {
        let input = r#"{"z":1,"a":{"y":2,"b":3},"m":4}"#;
        let expected = r#"{"z":"?","a":{"y":"?","b":"?"},"m":"?"}"#;
        let (result, err) = obf(&[]).obfuscate(input);
        assert_eq!(err, None);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_multiple_json_objects() {
        // Multiple concatenated JSON objects (elasticsearch bulk API pattern).
        let input = r#"{"index":{"_index":"traces","_type":"trace"}} {"value":1,"name":"test"}"#;
        let (result, err) = obf(&[]).obfuscate(input);
        assert_eq!(err, None);
        let mut stream =
            serde_json::Deserializer::from_str(&result).into_iter::<serde_json::Value>();
        let first = stream
            .next()
            .expect("first value")
            .expect("first value is valid JSON");
        let second = stream
            .next()
            .expect("second value")
            .expect("second value is valid JSON");
        assert_eq!(first, json!({"index":{"_index":"?","_type":"?"}}));
        assert_eq!(second, json!({"value":"?","name":"?"}));
    }

    /// Whitespace between a key and its colon is skipped, so it must not end up inside the key the
    /// lookup is done with.
    #[test]
    fn test_whitespace_around_key_does_not_break_lookup() {
        let input = "{ \"keepme\" : 1 , \"other\" : 2 }";
        let (result, err) = obf(&["keepme"]).obfuscate(input);
        assert_eq!(err, None);
        assert_eq!(result, r#"{"keepme":1,"other":"?"}"#);
    }

    #[test]
    fn test_transform_key_sql_basic() {
        let input = r#"{"query":"select * from table where id = 2","hello":"world","hi":"there"}"#;
        let result = obfuscate_sql_values(&obf_keys(&["hello"], &["query"]), input);

        let val: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(val["hello"], json!("world"));
        assert_eq!(val["hi"], json!("?"));
        assert_eq!(val["query"], json!("select * from table where id = ?"));
    }

    #[test]
    fn test_transform_key_with_object_value_falls_through() {
        let input = r#"{"object":{"not a":"query"}}"#;
        let expected = r#"{"object":{"not a":"?"}}"#;
        assert_json_eq(
            &obfuscate_sql_values(&obf_keys(&[], &["object"]), input),
            expected,
        );
    }

    #[test]
    fn test_transform_key_with_array_value_falls_through() {
        let input = r#"{"object":["not","a","query"]}"#;
        let expected = r#"{"object":["?","?","?"]}"#;
        assert_json_eq(
            &obfuscate_sql_values(&obf_keys(&[], &["object"]), input),
            expected,
        );
    }

    /// A kept subtree is copied as sent, but a transform key inside it still rewrites its own
    /// value, and keeping resumes for the rest of the subtree.
    #[test]
    fn test_transform_key_inside_a_kept_subtree_still_transforms() {
        let input =
            r#"{"kept":{"plain":"sent","query":"select 42","after":"sent"},"outside":"sent"}"#;
        let JsonObfuscationOutcome { output, report } = obf_keys(&["kept"], &["query"])
            .obfuscate_with(input, |v| {
                Ok::<_, SqlObfuscationError>(Cow::Owned(v.to_uppercase()))
            });

        assert!(report.is_clean());
        assert_eq!(
            output,
            r#"{"kept":{"plain":"sent","query":"SELECT 42","after":"sent"},"outside":"?"}"#
        );
    }

    /// A key in both sets is kept, and a kept subtree does not adopt the keep depth of a nested
    /// keep key: the subtree ends where the outer key opened it.
    #[test]
    fn test_keep_wins_over_transform_for_the_same_key() {
        let obfuscator = obf_keys(&["both", "inner"], &["both"]);
        let input = r#"{"both":{"inner":{"deep":1}},"next":"sent"}"#;
        let JsonObfuscationOutcome { output, report } = obfuscator.obfuscate_with(input, |v| {
            Ok::<_, SqlObfuscationError>(Cow::Owned(v.to_uppercase()))
        });

        assert!(report.is_clean());
        assert_eq!(output, r#"{"both":{"inner":{"deep":1}},"next":"?"}"#);
    }

    /// A bare top-level literal is a value, not a key, so it is obfuscated. A literal that follows
    /// a complete document is where the Agent's scanner leaves it: at the top level, read as a key.
    #[test]
    fn test_top_level_literal_is_a_value() {
        for input in ["42", r#""bare""#, "true", "null"] {
            let (output, err) = obf(&[]).obfuscate(input);
            assert_eq!(err, None, "input: {input}");
            assert_eq!(output, r#""?""#, "input: {input}");
        }

        let (output, err) = obf(&[]).obfuscate(r#"{"a":1} 42"#);
        assert_eq!(err, None);
        assert_eq!(output, r#"{"a":"?"} 42"#);
    }

    /// Without a callback, a transform key is just another key. This is the Agent's behavior when
    /// no transformer is configured, and it is why the entry points that take no callback are safe
    /// to use with any config.
    #[test]
    fn test_transform_keys_are_obfuscated_when_no_transform_is_given() {
        let input = r#"{"query":"select * from table where id = 2"}"#;
        let (result, err) = obf_keys(&[], &["query"]).obfuscate(input);
        assert_eq!(err, None);
        assert_eq!(result, r#"{"query":"?"}"#);
        assert_eq!(
            obf_keys(&[], &["query"]).obfuscate_opt(input).as_deref(),
            Some(r#"{"query":"?"}"#)
        );
    }

    /// The transform is a closure, so it can capture configuration that is not - and should not
    /// be - part of the serializable JSON config.
    #[test]
    fn test_transform_captures_runtime_sql_config() {
        let input = r#"{"query":"select col1 from table2"}"#;
        let obfuscator = obf_keys(&[], &["query"]);

        let with_digits = SqlConfig {
            replace_digits: true,
            ..Default::default()
        };
        let JsonObfuscationOutcome {
            output: replaced,
            report,
        } = obfuscator.obfuscate_with(input, sql_transform(&with_digits));
        assert!(report.is_clean());

        let default_config = SqlConfig::default();
        let JsonObfuscationOutcome {
            output: kept,
            report,
        } = obfuscator.obfuscate_with(input, sql_transform(&default_config));
        assert!(report.is_clean());

        assert_eq!(kept, r#"{"query":"select col1 from table2"}"#);
        assert_eq!(
            replaced, r#"{"query":"select col? from table?"}"#,
            "the captured config must reach the SQL obfuscator"
        );
    }

    /// The Agent unescapes a string value before obfuscating it.
    #[test]
    fn test_transform_value_is_unescaped_before_transforming() {
        let input = r#"{"query":"select \"a\" from t"}"#;
        let seen = Cell::new(String::new());
        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obf_keys(&[], &["query"]).obfuscate_with(input, |v| {
            seen.set(v.to_owned());
            Ok::<_, SqlObfuscationError>(Cow::Borrowed("kept"))
        });
        assert!(report.is_clean());
        assert_eq!(seen.into_inner(), r#"select "a" from t"#);
        assert_eq!(out, r#"{"query":"kept"}"#);
    }

    /// A transform value that cannot be unquoted reaches the callback verbatim, quotes included.
    #[test]
    fn test_unquotable_transform_value_is_passed_through_verbatim() {
        // A number: not a quoted string, so there is nothing to unquote.
        let seen = Cell::new(String::new());
        let JsonObfuscationOutcome { report, .. } =
            obf_keys(&[], &["query"]).obfuscate_with(r#"{"query":42}"#, |v| {
                seen.set(v.to_owned());
                Ok::<_, SqlObfuscationError>(Cow::Borrowed("kept"))
            });
        assert!(report.is_clean());
        assert_eq!(seen.into_inner(), "42");

        // A string the scanner accepts but `serde_json` rejects - here a lone surrogate - also
        // reaches the callback verbatim, quotes included.
        let seen = Cell::new(String::new());
        let JsonObfuscationOutcome { report, .. } =
            obf_keys(&[], &["query"]).obfuscate_with(r#"{"query":"\ud800"}"#, |v| {
                seen.set(v.to_owned());
                Ok::<_, SqlObfuscationError>(Cow::Borrowed("kept"))
            });
        assert!(report.is_clean());
        assert_eq!(seen.into_inner(), r#""\ud800""#);
    }

    /// The callback can return a borrow of its argument, a `&'static str`, or an owned string.
    #[test]
    fn test_transform_result_may_be_borrowed_static_or_owned() {
        let input = r#"{"a":"one","b":"two","c":"three"}"#;
        let obfuscator = obf_keys(&[], &["a", "b", "c"]);
        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obfuscator.obfuscate_with(input, |v| {
            Ok::<_, SqlObfuscationError>(match v {
                "one" => Cow::Borrowed(v),
                "two" => Cow::Borrowed(SQL_OBFUSCATION_FAILURE_REPLACEMENT),
                other => Cow::Owned(other.to_uppercase()),
            })
        });
        assert!(report.is_clean());
        assert_eq!(
            out,
            format!(r#"{{"a":"one","b":"{SQL_OBFUSCATION_FAILURE_REPLACEMENT}","c":"THREE"}}"#)
        );
    }

    #[test]
    fn test_transform_result_is_json_escaped() {
        let input = r#"{"query":"replace me"}"#;
        let replacement = "quote: \"; backslash: \\; controls: \0\u{8}\t\n\u{c}\r\u{1f}";
        let expected = serde_json::to_string(&json!({"query": replacement})).unwrap();
        let obfuscator = obf_keys(&[], &["query"]);

        let JsonObfuscationOutcome { output, report } = obfuscator.obfuscate_with(input, |_| {
            Ok::<_, SqlObfuscationError>(Cow::Borrowed(replacement))
        });
        assert!(report.is_clean());
        assert_eq!(output, expected);

        let mut output = String::new();
        let mut scratch = JsonObfuscationScratch::new();
        let report = obfuscator.obfuscate_into(input, &mut output, &mut scratch, |_| {
            Ok::<_, SqlObfuscationError>(Cow::Borrowed(replacement))
        });
        assert!(report.is_clean());
        assert_eq!(output, expected);
    }

    #[test]
    fn test_sql_transform_with_quoted_identifier_is_json_escaped() {
        let input = r#"{"query":"SELECT * FROM \"users\" WHERE id = 1"}"#;
        let config = SqlConfig {
            obfuscation_mode: SqlObfuscationMode::ObfuscateAndNormalize,
            keep_identifier_quotation: true,
            ..Default::default()
        };
        let JsonObfuscationOutcome { output, report } =
            obf_keys(&[], &["query"]).obfuscate_with(input, sql_transform(&config));

        assert!(report.is_clean());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap(),
            json!({"query": "SELECT * FROM \"users\" WHERE id = ?"})
        );
        assert_eq!(
            output,
            r#"{"query":"SELECT * FROM \"users\" WHERE id = ?"}"#
        );
    }

    /// A transform error is reported, the value is obfuscated rather than leaked, and the rest of
    /// the document is still obfuscated.
    #[test]
    fn test_transform_error_is_reported_and_value_is_obfuscated() {
        let input = r#"{"query":"","other":"kept","second":"  "}"#;
        let obfuscator = obf_keys(&["other"], &["query", "second"]);
        let config = SqlConfig::default();
        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obfuscator.obfuscate_with(input, sql_transform(&config));

        assert_eq!(out, r#"{"query":"?","other":"kept","second":"?"}"#);
        assert_eq!(report.scan_error, None);
        assert_eq!(
            report.transform_errors,
            vec![
                SqlObfuscationError::EmptyResult,
                SqlObfuscationError::EmptyResult
            ]
        );
        assert!(!report.is_clean());
    }

    /// A caller can replace failed SQL values and record errors without failing the JSON pass.
    #[test]
    fn test_caller_supplied_sql_failure_replacement() {
        let input = r#"{"query":"","tab":"\t"}"#;
        let obfuscator = obf_keys(&[], &["query", "tab"]);
        let config = SqlConfig::default();
        let mut logged = Vec::new();

        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obfuscator.obfuscate_with(input, |sql| {
            match obfuscate_sql(sql, &config, DbmsKind::Generic) {
                Ok(query) => Ok(Cow::Owned(query)),
                Err(err) => {
                    logged.push(err);
                    Ok::<_, SqlObfuscationError>(Cow::Borrowed(SQL_OBFUSCATION_FAILURE_REPLACEMENT))
                }
            }
        });

        assert!(report.is_clean(), "the caller handled the error itself");
        assert_eq!(
            out,
            format!(
                r#"{{"query":"{SQL_OBFUSCATION_FAILURE_REPLACEMENT}","tab":"{SQL_OBFUSCATION_FAILURE_REPLACEMENT}"}}"#
            )
        );
        assert_eq!(
            logged,
            vec![
                SqlObfuscationError::EmptyResult,
                SqlObfuscationError::EmptyResult
            ]
        );
    }

    /// Malformed JSON still produces partial output through the transforming entry points, and the
    /// scan error is reported next to any transform error.
    #[test]
    fn test_malformed_json_with_transform_reports_both_channels() {
        let input = r#"{"query":"","next":"#;
        let obfuscator = obf_keys(&[], &["query"]);
        let config = SqlConfig::default();
        let JsonObfuscationOutcome {
            output: out,
            report,
        } = obfuscator.obfuscate_with(input, sql_transform(&config));

        assert_eq!(out, r#"{"query":"?","next":..."#);
        assert_eq!(
            report.scan_error,
            Some(JsonScanError::UnexpectedEndOfInput { char_position: 20 })
        );
        assert_eq!(
            report.transform_errors,
            vec![SqlObfuscationError::EmptyResult]
        );
    }

    #[test]
    fn test_obfuscate_into_matches_the_allocating_path_and_reuses_scratch() {
        let obfuscator = obf_keys(&["hello"], &["query"]);
        let config = SqlConfig::default();
        let inputs = [
            r#"{"query":"select * from a where id = 1","hello":"world"}"#,
            r#"{"nested":{"deep":[1,2,{"query":"select 2"}]},"hello":"world"}"#,
            r#"{"query":"","bad":"#,
        ];

        let mut scratch = JsonObfuscationScratch::new();
        let mut out = String::new();
        for input in inputs {
            let JsonObfuscationOutcome {
                output: expected,
                report: expected_report,
            } = obfuscator.obfuscate_with(input, sql_transform(&config));
            let report =
                obfuscator.obfuscate_into(input, &mut out, &mut scratch, sql_transform(&config));
            assert_eq!(out, expected, "input: {input}");
            assert_eq!(report, expected_report, "input: {input}");
        }

        // Running the first input again after an input that failed to scan must give the same
        // answer: a scratch carries no state across passes.
        let report =
            obfuscator.obfuscate_into(inputs[0], &mut out, &mut scratch, sql_transform(&config));
        assert!(report.is_clean());
        assert_eq!(
            out,
            obfuscator
                .obfuscate_with(inputs[0], sql_transform(&config))
                .output
        );
    }

    #[test]
    fn test_scratch_capacity_is_retained_until_trimmed() {
        // A `fn` item rather than a `let`-bound closure: inference does not give a closure
        // stored in a variable the higher-ranked signature the callback needs. It is infallible,
        // but the callback signature is fallible, so the `Result` is not redundant here.
        #[allow(clippy::unnecessary_wraps)]
        fn identity(v: &str) -> Result<Cow<'_, str>, SqlObfuscationError> {
            Ok(Cow::Owned(v.to_owned()))
        }

        let obfuscator = obf_keys(&[], &["query"]);
        let deep = format!(
            "{}{}{}",
            "[".repeat(64),
            r#"{"query":"select \"x\" from a_table_with_a_long_name"}"#,
            "]".repeat(64)
        );
        let mut scratch = JsonObfuscationScratch::new();
        let mut out = String::new();
        let report: JsonObfuscationReport<SqlObfuscationError> =
            obfuscator.obfuscate_into(&deep, &mut out, &mut scratch, identity);
        assert!(report.is_clean());

        let grown = scratch.retained_capacity();
        assert!(grown.nesting_slots >= 64, "retained {grown:?}");
        assert!(grown.unescape_bytes > 0, "retained {grown:?}");

        // Clearing keeps the capacity; that is the point of reuse.
        scratch.clear();
        assert_eq!(scratch.retained_capacity(), grown);

        scratch.trim_to(ScratchCapacity::new(0, 0));
        let trimmed = scratch.retained_capacity();
        assert!(
            trimmed.nesting_slots < grown.nesting_slots,
            "trimmed {trimmed:?}"
        );
        assert!(
            trimmed.unescape_bytes < grown.unescape_bytes,
            "trimmed {trimmed:?}"
        );

        // A trimmed scratch is still a working scratch.
        let report: JsonObfuscationReport<SqlObfuscationError> =
            obfuscator.obfuscate_into(&deep, &mut out, &mut scratch, identity);
        assert!(report.is_clean());
    }

    /// Scratch reuse must survive a pass that ended in an error, including a transform error.
    #[test]
    fn test_scratch_reuse_after_errors() {
        let obfuscator = obf_keys(&[], &["query"]);
        let config = SqlConfig::default();
        let mut scratch = JsonObfuscationScratch::new();
        let mut out = String::new();

        let report = obfuscator.obfuscate_into(
            r#"{"query":"","truncated":"#,
            &mut out,
            &mut scratch,
            sql_transform(&config),
        );
        assert!(report.scan_error.is_some());
        assert_eq!(report.transform_errors.len(), 1);

        let report = obfuscator.obfuscate_into(
            r#"{"query":"select 1"}"#,
            &mut out,
            &mut scratch,
            sql_transform(&config),
        );
        assert!(report.is_clean());
        assert_eq!(out, r#"{"query":"select ?"}"#);
    }

    /// The config is plain data: it round-trips through serde and compares by value, which is what
    /// a caller that fetches it from the Agent needs.
    #[test]
    fn test_config_is_serializable_and_comparable() {
        let obfuscator = obf_keys(&["keepme"], &["query"]);
        let json = serde_json::to_string(&obfuscator).expect("serializes");
        let round_tripped: JsonObfuscator = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, obfuscator);
        assert_eq!(round_tripped.config(), obfuscator.config());
        assert_ne!(round_tripped, obf_keys(&["keepme"], &[]));
    }
}
