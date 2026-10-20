// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A small document model (headings, paragraphs, tables) with a markdown renderer,
//! so reports are built as data and output shape stays a rendering concern.

use std::fmt::Write as _;

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

pub struct Column {
    pub title: String,
    pub align: Align,
}

impl Column {
    pub fn new(title: &str, align: Align) -> Self {
        Self {
            title: title.to_string(),
            align,
        }
    }
}

pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }
}

pub enum Doc {
    Heading {
        level: u8,
        text: String,
    },
    Para(String),
    Table(Table),
    /// An outline of labeled nodes; rendered as a fenced code block, since the
    /// indentation would not survive markdown collapsing.
    Tree(Vec<Node>),
}

/// A node of a [`Doc::Tree`]: a label, a value rendered right-aligned in the
/// last column, an optional marker after it (e.g. a failure flag), and children.
pub struct Node {
    pub label: String,
    pub value: String,
    pub note: Option<String>,
    pub children: Vec<Node>,
}

impl Node {
    /// Folds `init` over this node and every descendant, depth-first, passing
    /// each node's depth (0 for the root). The function is dyn so recursion
    /// cannot grow the closure's monomorphized type at each level.
    fn fold_depth<T>(&self, depth: usize, init: T, f: &dyn Fn(T, &Self, usize) -> T) -> T {
        let acc = f(init, self, depth);
        self.children
            .iter()
            .fold(acc, |acc, child| child.fold_depth(depth + 1, acc, f))
    }
}

pub fn render_markdown(docs: &[Doc]) -> String {
    docs.iter().map(render_doc).collect()
}

fn render_doc(doc: &Doc) -> String {
    match doc {
        Doc::Heading { level, text } => format!("{} {text}\n", "#".repeat(*level as usize)),
        Doc::Para(text) => format!("\n{text}\n"),
        Doc::Table(table) => format!("\n{}\n", render_table(table)),
        Doc::Tree(roots) => format!("\n```text\n{}```\n", render_tree(roots)),
    }
}

const TEE: &str = "├── ";
const ELBOW: &str = "└── ";
const VERTICAL: &str = "│   ";
const GAP: &str = "    ";
/// Columns per tree level; TEE, ELBOW, VERTICAL and GAP are all this wide.
const INDENT: usize = 4;

/// Renders the outline in `tree`(1) style: TEE/ELBOW connectors, a VERTICAL bar
/// for ancestors with later siblings, values right-aligned in the last column.
fn render_tree(roots: &[Node]) -> String {
    // Both column widths derive from the tree itself: the label column from the
    // longest connector + label, the value column from the longest value.
    let label_width = roots
        .iter()
        .map(|root| {
            root.fold_depth(0, 0, &|max, n, depth| {
                max.max(INDENT * depth + n.label.chars().count())
            })
        })
        .max()
        .unwrap_or(0);
    let value_width = roots
        .iter()
        .map(|root| root.fold_depth(0, 0, &|max, n, _| max.max(n.value.chars().count())))
        .max()
        .unwrap_or(0);
    roots
        .iter()
        .map(|root| render_subtree(root, "", "", true, label_width, value_width))
        .collect()
}

/// Renders one node and its subtree. `bars` is the continuation prefix before
/// the node's connector, `connector` separates it from the label ("" for a
/// root, TEE/ELBOW below it), and `last` tells whether the node is the final
/// child of its parent; together these decide the prefix its children get.
fn render_subtree(
    node: &Node,
    bars: &str,
    connector: &str,
    last: bool,
    label_width: usize,
    value_width: usize,
) -> String {
    let label = format!("{bars}{connector}{}", node.label);
    let note = node
        .note
        .as_deref()
        .map(|note| format!(" {note}"))
        .unwrap_or_default();
    let line = format!("{label:<label_width$} {:>value_width$}{note}\n", node.value);

    // Below a non-root node, its connector's columns become a VERTICAL bar while
    // the node still has later siblings, spaces otherwise; roots leave no margin.
    let child_bars = if connector.is_empty() {
        String::new()
    } else {
        format!("{bars}{}", if last { GAP } else { VERTICAL })
    };
    let children = node
        .children
        .iter()
        .enumerate()
        .map(|(i, child)| {
            let last = i + 1 == node.children.len();
            let connector = if last { ELBOW } else { TEE };
            render_subtree(
                child,
                &child_bars,
                connector,
                last,
                label_width,
                value_width,
            )
        })
        .collect::<String>();
    format!("{line}{children}")
}

fn render_table(table: &Table) -> String {
    let aligns: Vec<Align> = table.columns.iter().map(|c| c.align).collect();
    let mut widths: Vec<usize> = table.columns.iter().map(|c| c.title.len()).collect();
    for row in &table.rows {
        for (cell, width) in row.iter().zip(&mut widths) {
            *width = (*width).max(cell.len());
        }
    }

    let mut out = String::new();
    let titles: Vec<&str> = table.columns.iter().map(|c| c.title.as_str()).collect();
    write_row(&mut out, &titles, &widths, &aligns);
    out.push('|');
    out.push_str(&separator(&widths, &aligns));
    out.push_str("|\n");
    for row in &table.rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        write_row(&mut out, &cells, &widths, &aligns);
    }
    out
}

fn write_row(out: &mut String, cells: &[&str], widths: &[usize], aligns: &[Align]) {
    for ((cell, width), align) in cells.iter().zip(widths.iter().copied()).zip(aligns) {
        match align {
            Align::Left => write!(out, "| {cell:<width$} "),
            Align::Right => write!(out, "| {cell:>width$} "),
        }
        // Writing to a String cannot fail.
        .unwrap();
    }
    out.push_str("|\n");
}

fn separator(widths: &[usize], aligns: &[Align]) -> String {
    widths
        .iter()
        .copied()
        .zip(aligns)
        .map(|(width, align)| match align {
            Align::Left => format!(":{}", "-".repeat(width + 1)),
            Align::Right => format!("{}:", "-".repeat(width + 1)),
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(label: &str, value: &str, note: Option<&str>) -> Node {
        Node {
            label: label.into(),
            value: value.into(),
            note: note.map(Into::into),
            children: Vec::new(),
        }
    }

    #[test]
    fn tree_renders_as_indented_outline() {
        let roots = vec![
            Node {
                label: "alpha".into(),
                value: "3.5s".into(),
                note: None,
                children: vec![
                    Node {
                        label: "mod1".into(),
                        value: "3.0s".into(),
                        note: None,
                        children: vec![leaf("t1", "2.0s", None), leaf("t2", "1.0s", Some("fail"))],
                    },
                    leaf("t3", "0.5s", None),
                ],
            },
            Node {
                label: "beta".into(),
                value: "0.2s".into(),
                note: None,
                children: vec![Node {
                    label: "m".into(),
                    value: "0.2s".into(),
                    note: None,
                    children: vec![leaf("x", "0.2s", None)],
                }],
            },
        ];
        assert_eq!(
            render_markdown(&[Doc::Tree(roots)]),
            concat!(
                "\n```text\n",
                "alpha      3.5s\n",
                "├── mod1   3.0s\n",
                "│   ├── t1 2.0s\n",
                "│   └── t2 1.0s fail\n",
                "└── t3     0.5s\n",
                "beta       0.2s\n",
                "└── m      0.2s\n",
                "    └── x  0.2s\n",
                "```\n",
            )
        );
    }
}
