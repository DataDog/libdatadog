// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

/// Usage:
/// ./dedup_headers <base_header> <child_headers>...
///
/// All type definitions will be removed from the child_headers, and moved to the base_header
/// if they are not already defined in the parent_header
use regex::{Match, Regex, RegexBuilder};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};

fn collect_definitions(header: &str) -> Vec<regex::Match<'_>> {
    lazy_static::lazy_static! {
        static ref HEADER_TYPE_DECL_RE: Regex = RegexBuilder::new(r"^(/\*\*.*?\*/\n)?typedef (struct|enum) [a-zA-Z_0-9]+ +(\{.*?\} )?[a-zA-Z_0-9]+;\n+")
            .multi_line(true)
            .dot_matches_new_line(true)
            .build()
            .unwrap();
    }
    HEADER_TYPE_DECL_RE.find_iter(header).collect()
}

fn read(f: &mut BufReader<&File>) -> String {
    let mut s = Vec::new();
    f.read_to_end(&mut s).unwrap();
    String::from_utf8(s).unwrap()
}

fn write_parts(writer: &mut BufWriter<&File>, parts: &[&str]) -> io::Result<()> {
    writer.get_ref().set_len(0)?;
    writer.rewind()?;
    for part in parts {
        writer.write_all(part.as_bytes())?;
    }
    Ok(())
}

fn content_without_defs<'a>(content: &'a str, defs: &[Match]) -> Vec<&'a str> {
    let mut new_content_parts = Vec::new();
    let mut pos = 0;
    for d in defs {
        new_content_parts.push(&content[pos..d.start()]);
        pos = d.end();
    }
    new_content_parts.push(&content[pos..]);
    new_content_parts
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();

    let mut unique_child_defs: Vec<String> = Vec::new();
    let mut present = HashSet::new();
    for child_def in args[2..].iter().flat_map(|p| {
        let child_header = OpenOptions::new().read(true).write(true).open(p).unwrap();

        let child_header_content = read(&mut BufReader::new(&child_header));
        let child_defs = collect_definitions(&child_header_content);
        let new_content_parts = content_without_defs(&child_header_content, &child_defs);

        write_parts(&mut BufWriter::new(&child_header), &new_content_parts).unwrap();

        child_defs
            .into_iter()
            .map(|m| m.as_str().to_owned())
            .collect::<Vec<_>>()
    }) {
        if present.contains(&child_def) {
            continue;
        }
        unique_child_defs.push(child_def.clone());
        present.insert(child_def);
    }

    let base_header = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&args[1])
        .unwrap();
    let base_header_content = read(&mut BufReader::new(&base_header));
    let base_defs = collect_definitions(&base_header_content);
    let base_defs_set: HashSet<_> = base_defs.iter().map(Match::as_str).collect();

    let mut base_new_parts = vec![&base_header_content[..base_defs.last().unwrap().end()]];
    for child_def in &unique_child_defs {
        if base_defs_set.contains(child_def.as_str()) {
            continue;
        }
        base_new_parts.push(child_def);
    }
    base_new_parts.push(&base_header_content[base_defs.last().unwrap().end()..]);
    write_parts(&mut BufWriter::new(&base_header), &base_new_parts).unwrap();
}
