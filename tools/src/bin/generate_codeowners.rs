// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generate .github/CODEOWNERS from .github/CODEOWNERS.in.
//!
//! CODEOWNERS cannot merge owners across rules (last match wins and
//! replaces), so the co-ownership policy is expanded into explicit
//! per-crate lines. This tool owns that expansion; the .in file only
//! declares data:
//!
//!   team <alias> <@handle>        team aliases used everywhere below
//!   supporting <kinds...>         file kinds co-owned with the libdatadog team
//!   manifest <kinds...>           supporting kinds that also carry the release team
//!   co-owners <teams...>           the libdatadog team co-owning supporting files
//!   release <teams...>            teams added on manifest kinds
//!   infra-default <teams...>      default owners for infra blocks without teams
//!
//!   infra [teams...]:             repo-wide files; indented lines carry
//!                                 patterns, several per line, emitted verbatim
//!   crate [teams...]:             crates; code owned by the teams, supporting
//!                                 files co-owned per policy. A team prefixed
//!                                 with "+" (e.g. +crev) owns code only and is
//!                                 left off the supporting-file co-ownership
//!   exception [teams...]:         overrides, emitted last
//!
//! Emission order (in CODEOWNERS, order is semantics):
//!
//!   1. infra patterns containing "/" (rooted file and directory patterns)
//!   2. crate code rules, except crates living under a supporting directory (tests/wasm,
//!      tests/spawn_from_lib): the general tests/ rule would strip their owners, so their code
//!      rules move below it
//!   3. infra patterns without "/": they match at any depth, so they must come after the crate
//!      rules (crate-level LICENSE/NOTICE stay centralized)
//!   4. general supporting rules: co-owners, plus the release team on manifest kinds. Crates whose
//!      owners are a subset of the co-owners (the core-owned ones) need nothing more, so no family
//!      block is emitted
//!   5. code rules for crates under a supporting directory, then one family block per remaining
//!      crate: owners + co-owners, plus release on manifest kinds
//!   6. exceptions

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use nom::IResult;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_while, take_while1};
use nom::combinator::{all_consuming, map, rest};
use nom::multi::{many0, separated_list1};
use nom::sequence::{preceded, tuple};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::process::ExitCode;

const PAD: usize = 47;

#[derive(Debug, Parser)]
#[command(
    name = "generate_codeowners",
    about = "Generate .github/CODEOWNERS from .github/CODEOWNERS.in"
)]
struct Args {
    /// Verify .github/CODEOWNERS is up to date instead of rewriting it
    #[arg(long)]
    check: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Infra,
    Crate,
    Exception,
}

impl BlockKind {
    fn name(self) -> &'static str {
        match self {
            BlockKind::Infra => "infra",
            BlockKind::Crate => "crate",
            BlockKind::Exception => "exception",
        }
    }
}

#[derive(Debug)]
struct Block {
    kind: BlockKind,
    teams: Vec<String>,
    patterns: Vec<String>,
}

#[derive(Debug, Default)]
struct Model {
    teams: BTreeMap<String, String>,
    supporting: Vec<String>,
    manifest: HashSet<String>,
    co_owners: Vec<String>,
    release: Vec<String>,
    infra_default: Vec<String>,
    blocks: Vec<Block>,
}

/// One parsed line of the .in file. The grammar is deliberately tiny:
///
/// ```text
/// line        := blank | comment | patterns | header | directive
/// blank       := [whitespace]*
/// comment     := [whitespace]* "#" [anything]
/// patterns    := [whitespace]+ token+
/// header      := word+ ":"          (first word is the block kind)
/// directive   := word [whitespace]+ token*
/// word        := [non-whitespace non-colon]+
/// token       := [non-whitespace]+
/// ```
///
/// Words exclude ":" so a block header's terminating colon is unambiguous;
/// patterns keep colons available. `parse_line` must consume the whole line.
#[derive(Debug, PartialEq)]
enum Line<'a> {
    /// Blank or comment line; closes the currently open block.
    Blank,
    /// Indented line of patterns for the current block.
    Patterns(Vec<&'a str>),
    /// `kind teams...:` block header; the first word is the block kind.
    Header(Vec<&'a str>),
    /// `name args...` directive.
    Directive(&'a str, Vec<&'a str>),
}

fn ws(i: &str) -> IResult<&str, &str> {
    take_while(|c: char| c.is_whitespace())(i)
}

fn ws1(i: &str) -> IResult<&str, &str> {
    take_while1(|c: char| c.is_whitespace())(i)
}

fn token(i: &str) -> IResult<&str, &str> {
    take_while1(|c: char| !c.is_whitespace())(i)
}

fn word(i: &str) -> IResult<&str, &str> {
    take_while1(|c: char| !c.is_whitespace() && c != ':')(i)
}

fn parse_line(input: &str) -> IResult<&str, Line<'_>> {
    all_consuming(alt((
        // blank line: whitespace only (self-delimiting so alt can backtrack)
        map(all_consuming(ws), |_| Line::Blank),
        // comment: optional whitespace, '#', rest of the line
        map(tuple((ws, tag("#"), rest)), |_| Line::Blank),
        // indented pattern line: at least one leading space, then patterns
        map(
            tuple((ws1, separated_list1(ws1, token), ws)),
            |(_, toks, _)| Line::Patterns(toks),
        ),
        // block header: words terminated by ':'
        map(
            tuple((separated_list1(ws1, word), tag(":"), ws)),
            |(toks, _, _)| Line::Header(toks),
        ),
        // directive: leading word, then whitespace-separated arguments
        map(
            tuple((word, many0(preceded(ws1, token)), ws)),
            |(name, args, _)| Line::Directive(name, args),
        ),
    )))(input)
}

fn parse(text: &str) -> Result<Model> {
    let mut m = Model::default();
    let mut cur: Option<usize> = None;
    for (idx, raw) in text.lines().enumerate() {
        let n = idx + 1;
        let (_, line) =
            parse_line(raw).map_err(|_| anyhow::anyhow!("line {n}: could not parse {raw:?}"))?;
        match line {
            Line::Blank => cur = None,
            Line::Patterns(pats) => {
                let i = cur.ok_or_else(|| anyhow::anyhow!("line {n}: pattern outside a block"))?;
                m.blocks[i]
                    .patterns
                    .extend(pats.into_iter().map(str::to_owned));
            }
            Line::Header(toks) => {
                let kind = match toks.first().copied().unwrap_or_default() {
                    "infra" => BlockKind::Infra,
                    "crate" => BlockKind::Crate,
                    "exception" => BlockKind::Exception,
                    other => bail!("line {n}: unknown block {other:?}"),
                };
                let mut teams: Vec<String> = toks[1..].iter().map(|t| t.to_string()).collect();
                if teams.is_empty() {
                    if kind != BlockKind::Infra || m.infra_default.is_empty() {
                        bail!("line {n}: {} block needs owners", kind.name());
                    }
                    teams.clone_from(&m.infra_default);
                }
                m.blocks.push(Block {
                    kind,
                    teams,
                    patterns: Vec::new(),
                });
                cur = Some(m.blocks.len() - 1);
            }
            Line::Directive(name, args) => {
                cur = None;
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                match name {
                    "team" => {
                        ensure!(args.len() == 2, "line {n}: team needs alias and handle");
                        m.teams.insert(args[0].clone(), args[1].clone());
                    }
                    "infra-default" => m.infra_default = args,
                    "supporting" => m.supporting = args,
                    "manifest" => m.manifest = args.into_iter().collect(),
                    "co-owners" => m.co_owners = args,
                    "release" => m.release = args,
                    other => bail!("line {n}: unknown directive {other:?}"),
                }
            }
        }
    }
    validate(&m)?;
    Ok(m)
}

fn validate(m: &Model) -> Result<()> {
    ensure!(!m.supporting.is_empty(), "no supporting kinds declared");
    let check = |name: &str, owner: &str| -> Result<()> {
        let key = name.strip_prefix('+').unwrap_or(name);
        ensure!(m.teams.contains_key(key), "{owner}: unknown team {key:?}");
        Ok(())
    };
    for b in &m.blocks {
        for t in &b.teams {
            check(t, &format!("{} block", b.kind.name()))?;
        }
    }
    for (label, teams) in [
        ("policy co-owners", &m.co_owners),
        ("policy release", &m.release),
    ] {
        for t in teams {
            check(t, label)?;
        }
    }
    let unknown: BTreeSet<_> = m
        .manifest
        .iter()
        .filter(|k| !m.supporting.contains(k))
        .collect();
    ensure!(
        unknown.is_empty(),
        "manifest kinds not in supporting: {unknown:?}"
    );
    let mut seen = HashSet::new();
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Crate) {
        for p in &b.patterns {
            ensure!(seen.insert(p.as_str()), "duplicate crate pattern {p:?}");
        }
    }
    Ok(())
}

/// Expand a pattern + team aliases into a CODEOWNERS rule line. A "+"-prefixed
/// team is a code-only reviewer and resolves like any other alias here; the
/// family-block emission filters those out before calling this.
fn rule(m: &Model, pattern: &str, teams: &[&str]) -> Result<String> {
    let mut owners = Vec::with_capacity(teams.len());
    for t in teams {
        let key = t.strip_prefix('+').unwrap_or(t);
        let handle = m
            .teams
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("unknown team {key:?}"))?;
        owners.push(handle.as_str());
    }
    Ok(format!("{pattern:<PAD$} {}", owners.join(" "))
        .trim_end()
        .to_string())
}

fn emit(m: &Model) -> Result<String> {
    let co = &m.co_owners;
    let co_set: HashSet<&str> = co.iter().map(String::as_str).collect();
    let supporting = &m.supporting;
    let dir_kinds: HashSet<&str> = supporting
        .iter()
        .filter_map(|k| k.strip_suffix('/'))
        .collect();

    let mut lines: Vec<String> = [
        "# CODEOWNERS — generated from .github/CODEOWNERS.in, do not edit.",
        "# Regenerate with `cargo run -p tools --bin generate_codeowners`;",
        "# edit the .in file and commit both. The last matching rule wins and",
        "# its owner list replaces every earlier match — owners are never",
        "# merged. Sections are ordered so that later rules deliberately",
        "# override earlier ones.",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    lines.push("# ── 1. Repo-wide infrastructure ──".to_string());
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Infra) {
        let ts: Vec<&str> = b.teams.iter().map(String::as_str).collect();
        for p in b.patterns.iter().filter(|p| p.contains('/')) {
            lines.push(rule(m, p, &ts)?);
        }
    }

    lines.push(String::new());
    lines.push("# ── 2. Crate code — owning teams only ──".to_string());
    let late_crate = |p: &str| dir_kinds.contains(p.split('/').next().unwrap_or_default());
    let mut early = Vec::new();
    let mut late = Vec::new();
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Crate) {
        for p in &b.patterns {
            let slot = if late_crate(p) { &mut late } else { &mut early };
            slot.push((b, p.as_str()));
        }
    }
    for (b, p) in &early {
        let ts: Vec<&str> = b.teams.iter().map(String::as_str).collect();
        lines.push(rule(m, &format!("/{p}/"), &ts)?);
    }

    lines.push(String::new());
    lines.push("# ── Infra patterns matching at any depth (after the crate rules, so".to_string());
    lines.push("#    crate-level LICENSE/NOTICE stay centralized) ──".to_string());
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Infra) {
        let ts: Vec<&str> = b.teams.iter().map(String::as_str).collect();
        for p in b.patterns.iter().filter(|p| !p.contains('/')) {
            lines.push(rule(m, p, &ts)?);
        }
    }

    lines.push(String::new());
    lines.push("# ── 3. Supporting architecture — co-owned by the libdatadog team ──".to_string());
    for kind in supporting {
        let mut ts: Vec<&str> = co.iter().map(String::as_str).collect();
        if m.manifest.contains(kind.as_str()) {
            ts.extend(m.release.iter().map(String::as_str));
        }
        lines.push(rule(m, kind, &ts)?);
    }

    lines.push(String::new());
    lines.push("# ── Crate code rules under a supporting directory (below the".to_string());
    lines.push(
        "#    general tests/ rule), then family blocks restoring crate owners ──".to_string(),
    );
    for (b, p) in &late {
        let ts: Vec<&str> = b.teams.iter().map(String::as_str).collect();
        lines.push(rule(m, &format!("/{p}/"), &ts)?);
    }
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Crate) {
        let owners: Vec<&str> = b
            .teams
            .iter()
            .map(String::as_str)
            .filter(|t| !t.starts_with('+'))
            .collect();
        if owners.iter().all(|t| co_set.contains(t)) {
            continue; // general supporting rules already cover these owners
        }
        for p in &b.patterns {
            for kind in supporting {
                let mut ts = owners.clone();
                ts.extend(co.iter().map(String::as_str));
                if m.manifest.contains(kind.as_str()) {
                    ts.extend(m.release.iter().map(String::as_str));
                }
                lines.push(rule(m, &format!("/{p}/{kind}"), &ts)?);
            }
        }
    }

    lines.push(String::new());
    lines.push("# ── 4. Exceptions ──".to_string());
    for b in m.blocks.iter().filter(|b| b.kind == BlockKind::Exception) {
        let ts: Vec<&str> = b.teams.iter().map(String::as_str).collect();
        for p in &b.patterns {
            lines.push(rule(m, p, &ts)?);
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

fn run(check: bool) -> Result<()> {
    // tools/ sits at the repository root, next to .github/
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR is the tools/ directory, which has a parent");
    let in_path = root.join(".github/CODEOWNERS.in");
    let out_path = root.join(".github/CODEOWNERS");
    let text = std::fs::read_to_string(&in_path)
        .with_context(|| format!("reading {}", in_path.display()))?;
    let model = parse(&text)?;
    let generated = emit(&model)?;
    if check {
        let current = std::fs::read_to_string(&out_path)
            .with_context(|| format!("reading {}", out_path.display()))?;
        ensure!(
            current == generated,
            "{} is stale; run `cargo run -p tools --bin generate_codeowners`",
            out_path.display()
        );
        println!("{} is up to date", out_path.display());
        return Ok(());
    }
    std::fs::write(&out_path, &generated)
        .with_context(|| format!("writing {}", out_path.display()))?;
    println!(
        "wrote {} ({} lines)",
        out_path.display(),
        generated.lines().count()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args.check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("generate_codeowners: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = "team a @x/a
team b @x/b
team c @x/c
supporting Cargo.toml tests/
manifest Cargo.toml
co-owners a
release b
infra-default a

infra:
  /docs/ root-file

crate a:
  core-thing

crate b +c:
  domain-thing tests/nested-thing
";

    #[test]
    fn line_grammar() {
        for input in ["", "   ", "# comment", "  # indented comment"] {
            let (rest, line) = parse_line(input).expect("blank or comment");
            assert_eq!(rest, "");
            assert_eq!(line, Line::Blank);
        }
        let (_, line) = parse_line("  libdd-a libdd-b").unwrap();
        assert_eq!(line, Line::Patterns(vec!["libdd-a", "libdd-b"]));
        let (_, line) = parse_line("crate php +crev:").unwrap();
        assert_eq!(line, Line::Header(vec!["crate", "php", "+crev"]));
        let (_, line) = parse_line("infra:").unwrap();
        assert_eq!(line, Line::Header(vec!["infra"]));
        let (_, line) = parse_line("team core @DataDog/x").unwrap();
        assert_eq!(line, Line::Directive("team", vec!["core", "@DataDog/x"]));
        // a colon outside the header terminator is a parse error
        assert!(parse_line("crate: trailing").is_err());
    }

    #[test]
    fn expands_policy() {
        let model = parse(MINI).expect("mini model parses");
        let out = emit(&model).expect("mini model emits");
        // collapse the alignment padding so assertions stay readable
        let rules: Vec<String> = out
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let has = |l: &str| rules.iter().any(|r| r == l);

        // infra split: rooted patterns first, any-depth patterns after crates
        assert!(has("/docs/ @x/a"));
        assert!(has("root-file @x/a"));
        let i_docs = rules.iter().position(|l| l.starts_with("/docs/")).unwrap();
        let i_root = rules
            .iter()
            .position(|l| l.starts_with("root-file"))
            .unwrap();
        let i_core = rules
            .iter()
            .position(|l| l.starts_with("/core-thing/"))
            .unwrap();
        assert!(i_docs < i_core && i_core < i_root);

        // crate code rules carry the +reviewer team
        assert!(has("/domain-thing/ @x/b @x/c"));

        // general supporting rules
        assert!(has("Cargo.toml @x/a @x/b"));
        assert!(has("tests/ @x/a"));

        // late crate under tests/: code rule below the general tests/ rule
        assert!(has("/tests/nested-thing/ @x/b @x/c"));
        let i_tests = rules.iter().position(|l| l.starts_with("tests/ ")).unwrap();
        let i_nested = rules
            .iter()
            .position(|l| l.starts_with("/tests/nested-thing/"))
            .unwrap();
        assert!(i_tests < i_nested);

        // family blocks: owners + co-owners (+ release on manifest kinds),
        // without the +reviewer team
        assert!(has("/domain-thing/Cargo.toml @x/b @x/a @x/b"));
        assert!(has("/domain-thing/tests/ @x/b @x/a"));
        assert!(has("/tests/nested-thing/Cargo.toml @x/b @x/a @x/b"));
        assert!(has("/tests/nested-thing/tests/ @x/b @x/a"));

        // core-owned crate needs no family block
        assert!(!out.contains("/core-thing/Cargo.toml"));
    }

    #[test]
    fn rejects_duplicate_crate_pattern() {
        let text = "team a @x/a
supporting Cargo.toml
co-owners a

crate a:
  thing
crate a:
  thing
";
        let err = parse(text).expect_err("duplicate must be rejected");
        assert!(err.to_string().contains("duplicate crate pattern"));
    }

    #[test]
    fn rejects_unknown_team() {
        let text = "team a @x/a
supporting Cargo.toml
co-owners a

crate nope:
  thing
";
        let err = parse(text).expect_err("unknown team must be rejected");
        assert!(err.to_string().contains("unknown team"));
    }

    #[test]
    fn rejects_manifest_kind_outside_supporting() {
        let text = "team a @x/a
supporting Cargo.toml
manifest build.rs
co-owners a
";
        let err = parse(text).expect_err("manifest subset must be checked");
        assert!(err.to_string().contains("manifest kinds not in supporting"));
    }
}
