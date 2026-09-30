//! Markdown table conversion for messaging platforms.
//!
//! ```
//! use rig_messaging::markdown::{convert_tables, TableMode};
//! assert_eq!(convert_tables("hello", TableMode::Code), "hello");
//! ```

// Copyright (c) 2026 openabdev. Licensed under MIT; see LICENSE.OpenAB.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::fmt;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Rendering mode for markdown tables.
pub enum TableMode {
    #[default]
    /// Wrap tables in aligned code blocks.
    Code,
    /// Render table cells as bullet points.
    Bullets,
    /// Keep the original markdown.
    Off,
}

impl fmt::Display for TableMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code => write!(f, "code"),
            Self::Bullets => write!(f, "bullets"),
            Self::Off => write!(f, "off"),
        }
    }
}

struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

enum Segment {
    Text(String),
    Table(Table),
}

/// Convert tables while preserving surrounding text. `Off` returns the original text.
pub fn convert_tables(markdown: &str, mode: TableMode) -> String {
    if mode == TableMode::Off || markdown.is_empty() {
        return markdown.to_string();
    }

    let segments = parse_segments(markdown);

    let mut out = String::with_capacity(markdown.len());
    for seg in segments {
        match seg {
            Segment::Text(t) => out.push_str(&t),
            Segment::Table(table) => match mode {
                TableMode::Code => render_table_code(&table, &mut out),
                TableMode::Bullets => render_table_bullets(&table, &mut out),
                TableMode::Off => out.push_str(markdown),
            },
        }
    }
    out
}

fn parse_segments(markdown: &str) -> Vec<Segment> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);

    let mut segments: Vec<Segment> = Vec::new();
    let mut in_table = false;
    let mut in_head = false;
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut cell_buf = String::new();
    let mut last_table_end: usize = 0;

    let parser_with_offsets = Parser::new_ext(markdown, opts).into_offset_iter();

    for (event, range) in parser_with_offsets {
        match event {
            Event::Start(Tag::Table(_)) => {
                let before = markdown
                    .get(last_table_end..range.start)
                    .unwrap_or_default();
                if !before.is_empty() {
                    push_text(&mut segments, before);
                }
                in_table = true;
                headers.clear();
                rows.clear();
            }
            Event::End(TagEnd::Table) => {
                let table = Table {
                    headers: std::mem::take(&mut headers),
                    rows: std::mem::take(&mut rows),
                };
                segments.push(Segment::Table(table));
                in_table = false;
                last_table_end = range.end;
            }
            Event::Start(Tag::TableHead) => {
                in_head = true;
                current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                headers = std::mem::take(&mut current_row);
                in_head = false;
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) if !in_head => {
                rows.push(std::mem::take(&mut current_row));
            }
            Event::Start(Tag::TableCell) => {
                cell_buf.clear();
            }
            Event::End(TagEnd::TableCell) => {
                current_row.push(cell_buf.trim().to_string());
                cell_buf.clear();
            }
            Event::Text(t) if in_table => {
                cell_buf.push_str(&t);
            }
            Event::Code(t) if in_table => {
                cell_buf.push('`');
                cell_buf.push_str(&t);
                cell_buf.push('`');
            }
            Event::SoftBreak if in_table => {
                cell_buf.push(' ');
            }
            Event::HardBreak if in_table => {
                cell_buf.push(' ');
            }
            Event::Start(Tag::Emphasis)
            | Event::Start(Tag::Strong)
            | Event::Start(Tag::Strikethrough)
            | Event::Start(Tag::Link { .. })
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Link)
                if in_table => {}
            _ => {}
        }
    }

    if last_table_end < markdown.len() {
        let tail = markdown.get(last_table_end..).unwrap_or_default();
        if !tail.is_empty() {
            push_text(&mut segments, tail);
        }
    }

    segments
}

fn push_text(segments: &mut Vec<Segment>, text: &str) {
    if let Some(Segment::Text(prev)) = segments.last_mut() {
        prev.push_str(text);
    } else {
        segments.push(Segment::Text(text.to_string()));
    }
}

fn render_table_code(table: &Table, out: &mut String) {
    let col_count = table
        .headers
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if col_count == 0 {
        return;
    }

    let strip = |s: &str| s.replace('`', "");
    let headers: Vec<String> = table.headers.iter().map(|h| strip(h)).collect();
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| r.iter().map(|c| strip(c)).collect())
        .collect();

    let mut widths = vec![0usize; col_count];
    for (width, h) in widths.iter_mut().zip(&headers) {
        *width = (*width).max(UnicodeWidthStr::width(h.as_str()));
    }
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    for w in &mut widths {
        *w = (*w).max(3);
    }

    out.push_str("```\n");

    write_row(out, &headers, &widths, col_count);
    out.push('|');
    for w in &widths {
        out.push(' ');
        for _ in 0..*w {
            out.push('-');
        }
        out.push_str(" |");
    }
    out.push('\n');
    for row in &rows {
        write_row(out, row, &widths, col_count);
    }

    out.push_str("```\n");
}

fn write_row(out: &mut String, cells: &[String], widths: &[usize], col_count: usize) {
    out.push('|');
    for (i, w) in widths.iter().enumerate().take(col_count) {
        out.push(' ');
        let cell = cells.get(i).map(|s| s.as_str()).unwrap_or("");
        out.push_str(cell);
        let display_width = UnicodeWidthStr::width(cell);
        let pad = w.saturating_sub(display_width);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(" |");
    }
    out.push('\n');
}

fn render_table_bullets(table: &Table, out: &mut String) {
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (i, cell) in row.iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            out.push_str("• ");
            if let Some(h) = table.headers.get(i)
                && !h.is_empty()
            {
                out.push_str(h);
                out.push_str(": ");
            }
            out.push_str(cell);
            out.push('\n');
        }
        if row_idx + 1 < table.rows.len() {
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests;
