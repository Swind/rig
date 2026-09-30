//! Unicode-aware message splitting and thread titles.
//!
//! ```
//! let chunks = rig_messaging::format::split_message("hello world", 6);
//! assert_eq!(chunks, ["hello ", "world"]);
//! ```

// Copyright (c) 2026 openabdev. Licensed under MIT; see LICENSE.OpenAB.
use unicode_segmentation::UnicodeSegmentation;

fn codepoint_split_point(s: &str, max_chars: usize) -> usize {
    s.char_indices()
        .nth(max_chars.max(1))
        .map_or(s.len(), |(i, _)| i)
}

fn split_point(s: &str, max_chars: usize, word_wrap: bool) -> usize {
    let mut chars = 0usize;
    let mut byte = 0usize;
    let mut last_ws_byte = 0usize; // byte index just past the last whitespace grapheme
    for (start, g) in s.grapheme_indices(true) {
        let g_chars = g.chars().count();
        if chars + g_chars > max_chars {
            break;
        }
        chars += g_chars;
        byte = start + g.len();
        if g.chars().all(char::is_whitespace) {
            last_ws_byte = byte;
        }
    }
    if word_wrap && byte < s.len() && last_ws_byte > 0 {
        return last_ws_byte;
    }
    byte
}

/// Split at line and grapheme boundaries, counting Unicode scalar values.
/// Reopen fenced code blocks across chunks when the limit permits their overhead.
/// The limit must be positive.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    // A fence wrapper cannot fit below its opener and closing overhead.
    if text
        .lines()
        .any(|line| line.starts_with("```") && line.chars().count().saturating_add(5) >= limit)
    {
        let mut chunks = Vec::new();
        let mut remaining = text;
        while !remaining.is_empty() {
            let boundary = split_point(remaining, limit, false);
            let boundary = if boundary == 0 {
                codepoint_split_point(remaining, limit)
            } else {
                boundary
            };
            let (chunk, rest) = remaining.split_at(boundary);
            chunks.push(chunk.to_owned());
            remaining = rest;
        }
        return chunks;
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;
    let mut fence_opener: Option<String> = None;

    const CLOSE_COST: usize = 4; // '\n' + '`' + '`' + '`'

    for line in text.split('\n') {
        let line_chars = line.chars().count();
        let is_fence_line = line.starts_with("```");

        let close_reserve = if fence_opener.is_some() && !is_fence_line {
            CLOSE_COST
        } else {
            0
        };

        if !current.is_empty() && current_len + 1 + line_chars + close_reserve > limit {
            if let Some(ref opener) = fence_opener {
                if !is_fence_line {
                    current.push_str("\n```");
                }
                chunks.push(std::mem::take(&mut current));
                current.push_str(opener);
                current_len = opener.chars().count();

                if is_fence_line {
                    fence_opener = None;
                    current.push('\n');
                    current_len += 1;
                    current.push_str(line);
                    current_len += line_chars;
                    continue;
                } else if current_len + 1 + line_chars + CLOSE_COST <= limit {
                    current.push('\n');
                    current_len += 1;
                    current.push_str(line);
                    current_len += line_chars;
                    continue;
                }
            } else {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
        }

        if !current.is_empty() {
            current.push('\n');
            current_len += 1;
        }

        if is_fence_line {
            if fence_opener.is_some() {
                fence_opener = None;
            } else {
                fence_opener = Some(line.to_string());
            }
        }

        let effective_avail = if fence_opener.is_some() {
            limit.saturating_sub(current_len + CLOSE_COST)
        } else {
            limit.saturating_sub(current_len)
        };
        if line_chars > effective_avail {
            let overhead = if let Some(ref opener) = fence_opener {
                opener.chars().count() + 1 + CLOSE_COST
            } else {
                0
            };
            let capacity = limit.saturating_sub(overhead);
            if let Some(opener) = fence_opener.as_ref().filter(|_| capacity > 0) {
                let opener_len = opener.chars().count();
                let mut rest = line;

                let avail_first = if current_len > 0 {
                    limit.saturating_sub(current_len + CLOSE_COST)
                } else {
                    capacity
                };
                let cut = split_point(rest, avail_first, false);
                current.push_str(rest.get(..cut).unwrap_or_default());
                current_len += rest.get(..cut).unwrap_or_default().chars().count();
                rest = rest.get(cut..).unwrap_or_default();

                while !rest.is_empty() {
                    current.push_str("\n```");
                    chunks.push(std::mem::take(&mut current));
                    current.push_str(opener);
                    current.push('\n');
                    current_len = opener_len + 1;
                    let mut cut = split_point(rest, capacity, false);
                    if cut == 0 {
                        cut = codepoint_split_point(rest, capacity);
                    }
                    current.push_str(rest.get(..cut).unwrap_or_default());
                    current_len += rest.get(..cut).unwrap_or_default().chars().count();
                    rest = rest.get(cut..).unwrap_or_default();
                }
            } else {
                let mut rest = line;
                while !rest.is_empty() {
                    let avail = limit.saturating_sub(current_len);
                    let mut cut = split_point(rest, avail, true);
                    if cut == 0 {
                        if current.is_empty() {
                            cut = codepoint_split_point(rest, avail);
                        } else {
                            chunks.push(std::mem::take(&mut current));
                            current_len = 0;
                            continue;
                        }
                    }
                    current.push_str(rest.get(..cut).unwrap_or_default());
                    current_len += rest.get(..cut).unwrap_or_default().chars().count();
                    rest = rest.get(cut..).unwrap_or_default();
                    if !rest.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                }
            }
        } else {
            current.push_str(line);
            current_len += line_chars;
        }
    }

    if !current.is_empty() {
        if fence_opener.is_some() {
            current.push_str("\n```");
        }
        chunks.push(current);
    }
    chunks
}

/// Collapse GitHub issue URLs and truncate a thread title to 40 characters plus an ellipsis.
pub fn shorten_thread_name(prompt: &str) -> String {
    use std::sync::LazyLock;
    static GH_RE: LazyLock<Result<regex::Regex, regex::Error>> = LazyLock::new(|| {
        regex::Regex::new(r"https?://github\.com/([^/]+/[^/]+)/(issues|pull)/(\d+)")
    });
    let cleaned = prompt.replace("@(role)", "").replace("@(user)", "");
    let shortened = match GH_RE.as_ref() {
        Ok(regex) => regex.replace_all(cleaned.trim(), "$1#$3"),
        Err(_) => std::borrow::Cow::Borrowed(cleaned.trim()),
    };
    let name: String = shortened.chars().take(40).collect();
    if name.len() < shortened.len() {
        format!("{name}...")
    } else {
        name
    }
}

/// Keep the last `limit` Unicode scalar values of text.
pub fn truncate_chars_tail(s: &str, limit: usize) -> String {
    let total = s.chars().count();
    if total <= limit {
        return s.to_string();
    }
    s.chars().skip(total - limit).collect()
}

#[cfg(test)]
mod tests;
