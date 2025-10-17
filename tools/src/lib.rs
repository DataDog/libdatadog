// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod headers {
    use regex_lite::{Regex, RegexBuilder};
    use std::collections::HashSet;
    use std::fs::{File, OpenOptions};
    use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
    use std::sync::LazyLock;

    #[derive(Debug, PartialEq, Eq, Hash)]
    struct Span<'a> {
        start: usize,
        end: usize,
        str: &'a str,
    }

    static ITEM_DEFINITION_HEAD: LazyLock<Regex> = LazyLock::new(|| {
        RegexBuilder::new(
            r"^(?:/\*\*(?:[^*]|\*+[^*/])*\*+/\n)?(?:# *(define [a-zA-Z_0-9]+ [^\n]+)|(typedef))",
        )
        .multi_line(true)
        .dot_matches_new_line(true)
        .build()
        .unwrap()
    });

    /// Gather all top level typedef and #define definitions from a C header file
    fn collect_definitions(header: &str) -> Vec<Span<'_>> {
        let mut items = Vec::new();
        let mut start = 0;

        loop {
            let Some(head) = ITEM_DEFINITION_HEAD.captures_at(header, start) else {
                break;
            };
            start = head.get(0).unwrap().start();
            let end: usize;
            if let Some(capture) = head.get(2) {
                let mut depth: i32 = 0;
                let mut typedef_end = None;
                for (pos, c) in header.bytes().enumerate().skip(capture.end()) {
                    match c {
                        b';' if depth == 0 => {
                            typedef_end = Some(pos + 1);
                            break;
                        }
                        b'{' => {
                            depth += 1;
                        }
                        b'}' => {
                            depth = depth
                                .checked_sub(1)
                                .expect("Unmatched closing brace in typedef");
                        }
                        _ => {}
                    }
                }
                let typedef_end = typedef_end.expect("No closing semicolon found for typedef");
                end = typedef_end
                    + header[typedef_end..]
                        .bytes()
                        .take_while(|c| matches!(c, b'\n' | b'\r' | b' '))
                        .count();
            } else if let Some(capture) = head.get(1) {
                let define_end = capture.end();
                end = define_end
                    + header[define_end..]
                        .bytes()
                        .take_while(|c| matches!(c, b'\n' | b'\r' | b' '))
                        .count();
            } else {
                unreachable!(
                    "the regex should only capture typedef and #define, got {:?}",
                    head
                );
            }

            items.push(Span {
                start,
                end,
                str: &header[start..end],
            });
            start = end;
        }
        items
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

    fn content_without_defs<'a>(content: &'a str, defs: &[Span]) -> Vec<&'a str> {
        let mut new_content_parts = Vec::new();
        let mut pos = 0;
        for d in defs {
            new_content_parts.push(&content[pos..d.start]);
            pos = d.end;
        }
        new_content_parts.push(&content[pos..]);
        new_content_parts
    }

    pub fn dedup_headers(base: &str, headers: &[&str]) {
        let mut unique_child_defs: Vec<String> = Vec::new();
        let mut present = HashSet::new();

        for child_def in headers.iter().flat_map(|p| {
            let child_header = OpenOptions::new().read(true).write(true).open(p).unwrap();

            let child_header_content = read(&mut BufReader::new(&child_header));
            let child_defs = collect_definitions(&child_header_content);
            let new_content_parts = content_without_defs(&child_header_content, &child_defs);

            write_parts(&mut BufWriter::new(&child_header), &new_content_parts).unwrap();

            child_defs
                .into_iter()
                .map(|m| m.str.to_owned())
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
            .open(base)
            .unwrap();

        let base_header_content = read(&mut BufReader::new(&base_header));
        let base_defs = collect_definitions(&base_header_content);
        let base_defs_set: HashSet<_> = base_defs.iter().map(|s| s.str).collect();

        let mut base_new_parts = vec![&base_header_content[..base_defs.last().unwrap().end]];
        for child_def in &unique_child_defs {
            if base_defs_set.contains(child_def.as_str()) {
                continue;
            }
            base_new_parts.push(child_def);
        }
        base_new_parts.push(&base_header_content[base_defs.last().unwrap().end..]);
        write_parts(&mut BufWriter::new(&base_header), &base_new_parts).unwrap();
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[track_caller]
        fn test_regex_match(input: &str, expected: Vec<&str>) {
            let matches = collect_definitions(input);
            assert_eq!(
                matches.len(),
                expected.len(),
                "Expected:\n{expected:#?}\nActual:\n{matches:#?}",
            );
            for (i, m) in matches.iter().enumerate() {
                assert_eq!(m.str, expected[i]);
            }
        }

        #[test]
        fn collect_typedef() {
            let input = "typedef void *Foo;\n";
            let expected = vec!["typedef void *Foo;\n"];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_typedef_comment() {
            let input = r"
/**
 * This is a typedef for a pointer to Foo.
 */
typedef void *Foo;
";
            let expected = vec![
                r"/**
 * This is a typedef for a pointer to Foo.
 */
typedef void *Foo;
",
            ];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_struct_typedef() {
            let input = r"/**
 * This is a typedef for a pointer to a struct.
 */
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
    uintptr_t len;
    uintptr_t capacity;
} ddog_Vec_U8;
";
            let expected = vec![input];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_union_typedef() {
            let input = r"/**
 * This is a typedef for a pointer to a union.
 */
typedef union my_union {
    int a;
    float b;
} my_union;
";
            let expected = vec![input];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_union_nested() {
            let input = r"typedef union ddog_Union_U8 {
    struct inner1 {
        const uint8_t *ptr;
        uintptr_t len;
        uintptr_t capacity;
    } inner;
    struct inner2 {
        const uint8_t *ptr;
        uintptr_t len;
        uintptr_t capacity;
    } inner2;
} ddog_Union_U8;
";
            let expected = vec![input];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_define() {
            let input = r#"#define FOO __attribute__((unused))
"#;
            let expected = vec![input];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_multiple_definitions() {
            let input = r"
/**
 * `QueueId` is a struct that represents a unique identifier for a queue.
 * It contains a single field, `inner`, which is a 64-bit unsigned integer.
 */
typedef uint64_t ddog_QueueId;

void foo() {
}
  
/**
 * Holds the raw parts of a Rust Vec; it should only be created from Rust,
 * never from C.
 **/
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
    uintptr_t len;
    uintptr_t capacity;
} ddog_Vec_U8;
            ";

            let expected = vec![
                r"/**
 * `QueueId` is a struct that represents a unique identifier for a queue.
 * It contains a single field, `inner`, which is a 64-bit unsigned integer.
 */
typedef uint64_t ddog_QueueId;

",
                r"/**
 * Holds the raw parts of a Rust Vec; it should only be created from Rust,
 * never from C.
 **/
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
    uintptr_t len;
    uintptr_t capacity;
} ddog_Vec_U8;
            ",
            ];
            test_regex_match(input, expected);
        }

        #[test]
        fn collect_definitions_comments() {
            let header = r"/** foo */
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
} ddog_Vec_U8;
";
            let matches = collect_definitions(header);

            assert_eq!(matches.len(), 1);
            assert_eq!(
                matches[0].str,
                r"/** foo */
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
} ddog_Vec_U8;
"
            );

            let header = r"/** foo **/ */
typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
} ddog_Vec_U8;
";
            let matches = collect_definitions(header);

            assert_eq!(matches.len(), 1);
            assert_eq!(
                matches[0].str,
                r"typedef struct ddog_Vec_U8 {
    const uint8_t *ptr;
} ddog_Vec_U8;
"
            );
        }
    }
} /* Headers */
