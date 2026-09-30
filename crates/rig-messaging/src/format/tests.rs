// Copyright (c) 2026 openabdev. Licensed under MIT; see LICENSE.OpenAB.
use super::*;

/// Helper: assert every chunk respects the limit.
fn assert_length_invariant(chunks: &[String], limit: usize) {
    for (i, chunk) in chunks.iter().enumerate() {
        let len = chunk.chars().count();
        assert!(
            len <= limit,
            "chunk {i} has {len} chars, exceeds limit {limit}:\n{chunk}"
        );
    }
}

#[test]
fn no_split_under_limit() {
    let text = "hello\nworld";
    let chunks = split_message(text, 100);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], text);
}

#[test]
fn plain_text_split_respects_limit() {
    let text = "aaaa\nbbbb\ncccc\ndddd";
    let chunks = split_message(text, 10);
    assert_length_invariant(&chunks, 10);
    assert!(chunks.len() > 1);
}

#[test]
fn fenced_split_preserves_language_tag() {
    // ```rust\n + 1990 chars of content + \n```  — should split
    let content_line = "x".repeat(1990);
    let text = format!("```rust\n{content_line}\nanother line here\n```");
    let chunks = split_message(&text, 2000);
    assert_length_invariant(&chunks, 2000);
    // First chunk should start with ```rust
    assert!(chunks[0].starts_with("```rust"));
    // If split happened, second chunk should reopen with ```rust
    if chunks.len() > 1 {
        assert!(
            chunks[1].starts_with("```rust"),
            "second chunk should reopen with language tag: {}",
            &chunks[1][..chunks[1].len().min(20)]
        );
    }
}

#[test]
fn fenced_split_close_overhead_budgeted() {
    // Construct a fenced block where content + close marker would overflow
    // without proper budgeting.
    // limit=50, opener="```" (3), close="\n```" (4)
    // Available for content per chunk: 50 - 3 - 1 - 4 = 42 (with opener+newline+close)
    let line1 = "a".repeat(40);
    let line2 = "b".repeat(40);
    let text = format!("```\n{line1}\n{line2}\n```");
    let chunks = split_message(&text, 50);
    assert_length_invariant(&chunks, 50);
}

#[test]
fn reopen_path_no_overflow() {
    // Regression: limit=2000, fenced block with a 1996-char line.
    // Old code would produce 2004-char chunk due to reopen + extra \n.
    let content = "x".repeat(1990);
    let text = format!("```rust\n{content}\nshort\n```");
    let chunks = split_message(&text, 2000);
    assert_length_invariant(&chunks, 2000);
}

#[test]
fn hard_split_fenced_respects_limit() {
    // A single very long line inside a fence.
    let long_line = "x".repeat(100);
    let text = format!("```\n{long_line}\n```");
    let chunks = split_message(&text, 20);
    assert_length_invariant(&chunks, 20);
    // All content should be present
    let total_x: usize = chunks
        .iter()
        .map(|c| c.chars().filter(|&ch| ch == 'x').count())
        .sum();
    assert_eq!(total_x, 100);
}

#[test]
fn hard_split_plain_respects_limit() {
    let long_line = "y".repeat(50);
    let text = format!("before\n{long_line}\nafter");
    let chunks = split_message(&text, 10);
    assert_length_invariant(&chunks, 10);
}

#[test]
fn closing_fence_triggers_split() {
    // The closing ``` itself pushes over the limit.
    let content = "a".repeat(44);
    // "```\n" + 44 chars + "\n```" = 3 + 1 + 44 + 1 + 3 = 52
    let text = format!("```\n{content}\n```");
    let chunks = split_message(&text, 50);
    assert_length_invariant(&chunks, 50);
}

#[test]
fn multi_fence_blocks() {
    let text = "text\n```python\ncode1\ncode2\n```\nmore text\n```js\ncode3\n```";
    let chunks = split_message(text, 25);
    assert_length_invariant(&chunks, 25);
}

#[test]
fn fence_balance_across_chunks() {
    // Every chunk should have balanced fences (even number of ``` lines).
    let content = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("```\n{content}\n```");
    let chunks = split_message(&text, 30);
    assert_length_invariant(&chunks, 30);
    for (i, chunk) in chunks.iter().enumerate() {
        let fence_count = chunk.lines().filter(|l| l.starts_with("```")).count();
        assert!(
            fence_count.is_multiple_of(2),
            "chunk {i} has unbalanced fences ({fence_count}):\n{chunk}"
        );
    }
}

#[test]
fn grapheme_clusters_never_split() {
    use unicode_segmentation::UnicodeSegmentation;
    // multi-codepoint graphemes a char-based split would break: astral emoji,
    // ZWJ family, flag, VS16, astral CJK, plus plain BMP chars.
    let graphemes = ["🎉", "👨‍👩‍👧‍👦", "🇹🇼", "❤️", "𠀀", "a", "你", "🙂"];
    let line: String = graphemes.iter().copied().cycle().take(48).collect();
    // Limits >= the widest grapheme (the 7-scalar ZWJ family) so every grapheme fits
    // and none is split. Graphemes WIDER than the limit are the last-resort codepoint
    // split, covered by `oversized_grapheme_still_respects_limit`.
    for limit in [8, 13, 20] {
        let chunks = split_message(&line, limit);
        // No grapheme split: flattening chunk graphemes reproduces the original
        // grapheme sequence exactly (a split grapheme would re-segment differently).
        let flat: Vec<&str> = chunks.iter().flat_map(|c| c.graphemes(true)).collect();
        let orig: Vec<&str> = line.graphemes(true).collect();
        assert_eq!(flat, orig, "grapheme cluster split at limit {limit}");
    }
}

#[test]
fn plain_hard_split_prefers_whitespace() {
    // A long run of short words: chunks should break at spaces, not mid-word.
    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo";
    let chunks = split_message(line, 20);
    assert_length_invariant(&chunks, 20);
    assert!(chunks.len() > 1);
    let rejoined = chunks
        .iter()
        .map(|c| c.trim())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        rejoined.split_whitespace().collect::<Vec<_>>(),
        line.split_whitespace().collect::<Vec<_>>(),
        "words were broken or lost by the hard-split"
    );
}

#[test]
fn cjk_hard_split_breaks_between_characters() {
    // A long CJK run (no whitespace) must break on codepoint/grapheme bounds and
    // preserve every character.
    let line: String = std::iter::repeat_n('好', 50).collect();
    let chunks = split_message(&line, 10);
    assert_length_invariant(&chunks, 10);
    assert_eq!(chunks.concat(), line);
}

#[test]
fn fenced_hard_split_grapheme_safe() {
    // Emoji inside a code fence: grapheme-safe, all content preserved.
    let content: String = "🎉❤️你".repeat(20);
    let text = format!("```\n{content}\n```");
    let chunks = split_message(&text, 20);
    assert_length_invariant(&chunks, 20);
    let party: usize = chunks.iter().map(|c| c.matches('🎉').count()).sum();
    assert_eq!(party, 20, "emoji lost across fenced hard-split");
}

#[test]
fn oversized_grapheme_still_respects_limit() {
    // A single grapheme wider than the limit must still be bounded to <= limit
    // (last-resort codepoint split) so every chunk stays deliverable; content is
    // preserved byte-exact.
    let family = "👨‍👩‍👧‍👦"; // one grapheme, 7 scalar values
    for limit in [2, 3, 5] {
        let chunks = split_message(family, limit);
        assert_length_invariant(&chunks, limit);
        assert_eq!(chunks.concat(), family, "content lost at limit {limit}");
    }
}

#[test]
fn oversized_grapheme_with_mention_reserve() {
    // Discord path: the caller reduces the limit by a mention-footer reserve before
    // calling split_message; the reduced limit must still hold even when a single
    // grapheme is wider than it, so the footer's reserved capacity is never eaten.
    let text = format!("❤️{fam}{fam}", fam = "👨‍👩‍👧‍👦");
    let limit = 10;
    let reserve = 4;
    let effective = limit - reserve; // 6
    let chunks = split_message(&text, effective);
    assert_length_invariant(&chunks, effective);
    assert_eq!(chunks.concat(), text, "content lost with mention reserve");
}

#[test]
fn small_limits_do_not_overflow_on_fences() {
    for limit in 1..16 {
        let chunks = split_message("```rust\nlet value = 123;\n```", limit);
        assert_length_invariant(&chunks, limit);
    }
}

#[test]
fn title_and_preview_helpers_handle_unicode() {
    assert_eq!(
        shorten_thread_name("https://github.com/rig/rig/issues/42"),
        "rig/rig#42"
    );
    assert_eq!(truncate_chars_tail("你好世界", 2), "世界");
}
