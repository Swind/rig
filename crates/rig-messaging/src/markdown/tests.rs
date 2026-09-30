// Copyright (c) 2026 openabdev. Licensed under MIT; see LICENSE.OpenAB.
use super::*;

const TABLE_MD: &str = "\
Some text before.

| Name  | Age |
|-------|-----|
| Alice | 30  |
| Bob   | 25  |

Some text after.
";

#[test]
fn off_mode_passes_through() {
    let result = convert_tables(TABLE_MD, TableMode::Off);
    assert_eq!(result, TABLE_MD);
}

#[test]
fn code_mode_wraps_in_codeblock() {
    let result = convert_tables(TABLE_MD, TableMode::Code);
    assert!(result.contains("```\n"));
    assert!(result.contains("| Alice"));
    assert!(result.contains("Some text before."));
    assert!(result.contains("Some text after."));
}

#[test]
fn bullets_mode_converts_to_bullets() {
    let result = convert_tables(TABLE_MD, TableMode::Bullets);
    assert!(result.contains("• Name: Alice"));
    assert!(result.contains("• Age: 30"));
    assert!(!result.contains("```"));
}

#[test]
fn no_table_passes_through() {
    let plain = "Hello world\nNo tables here.";
    let result = convert_tables(plain, TableMode::Code);
    assert_eq!(result, plain);
}

#[test]
fn code_mode_strips_backticks_from_code_cells() {
    let md = "| col |\n|-----|\n| `value` |\n";
    let result = convert_tables(md, TableMode::Code);
    // The table is inside a ``` block — backtick wrapping must be stripped.
    assert!(result.contains("value"), "cell content should be present");
    // Only the fence markers themselves should contain backticks.
    let inner = result.trim_start_matches("```\n").trim_end_matches("```\n");
    assert!(
        !inner.contains('`'),
        "no backticks should appear inside the code fence: {result:?}"
    );
}

#[test]
fn bullets_mode_keeps_backticks_in_code_cells() {
    let md = "| col |\n|-----|\n| `value` |\n";
    let result = convert_tables(md, TableMode::Bullets);
    assert!(
        result.contains("`value`"),
        "backticks should be kept in bullets mode"
    );
}
