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
    Heading { level: u8, text: String },
    Para(String),
    Table(Table),
}

pub fn render_markdown(docs: &[Doc]) -> String {
    docs.iter().map(render_doc).collect()
}

fn render_doc(doc: &Doc) -> String {
    match doc {
        Doc::Heading { level, text } => format!("{} {text}\n", "#".repeat(*level as usize)),
        Doc::Para(text) => format!("\n{text}\n"),
        Doc::Table(table) => format!("\n{}\n", render_table(table)),
    }
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
