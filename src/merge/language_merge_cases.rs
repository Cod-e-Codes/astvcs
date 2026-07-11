//! Shared disjoint-edit merge fixtures for every AST frontend.

use crate::frontend::parse_source;
use crate::merge::{MergeOutcome, merge_files};
use crate::unparser::unparse;
use crate::{diff::diff_graphs, frontend::FileContent};

pub struct LanguageMergeCase {
    pub label: &'static str,
    pub path: &'static str,
    pub base: &'static str,
    pub left: &'static str,
    pub right: &'static str,
    pub left_markers: &'static [&'static str],
    pub right_markers: &'static [&'static str],
    pub forbidden: &'static [&'static str],
}

pub fn language_merge_cases() -> &'static [LanguageMergeCase] {
    static CASES: &[LanguageMergeCase] = &[
        LanguageMergeCase {
            label: "rust",
            path: "calc.rs",
            base: "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z\n}\n",
            left: "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c + 1;\n    let z = y - a;\n    z\n}\n",
            right: "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a - 1;\n    z\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "python",
            path: "calc.py",
            base: "def process(a, b, c):\n    x = a + b\n    y = x * c\n    z = y - a\n    return z\n",
            left: "def process(a, b, c):\n    x = a + b\n    y = x * c + 1\n    z = y - a\n    return z\n",
            right: "def process(a, b, c):\n    x = a + b\n    y = x * c\n    z = y - a - 1\n    return z\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "javascript",
            path: "calc.js",
            base: "function process(a, b, c) {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a;\n    return z;\n}\n",
            left: "function process(a, b, c) {\n    const x = a + b;\n    const y = x * c + 1;\n    const z = y - a;\n    return z;\n}\n",
            right: "function process(a, b, c) {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "c",
            path: "calc.c",
            base: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c;\n    int z = y - a;\n    return z;\n}\n",
            left: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c + 1;\n    int z = y - a;\n    return z;\n}\n",
            right: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c;\n    int z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "go",
            path: "calc.go",
            base: "package main\n\nfunc process(a, b, c int) int {\n\tx := a + b\n\ty := x * c\n\tz := y - a\n\treturn z\n}\n",
            left: "package main\n\nfunc process(a, b, c int) int {\n\tx := a + b\n\ty := x*c + 1\n\tz := y - a\n\treturn z\n}\n",
            right: "package main\n\nfunc process(a, b, c int) int {\n\tx := a + b\n\ty := x * c\n\tz := y - a - 1\n\treturn z\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "json",
            path: "calc.json",
            base: "{\n  \"params\": [1, 2, 3],\n  \"x\": 1,\n  \"y\": 10,\n  \"z\": 20\n}\n",
            left: "{\n  \"params\": [1, 2, 3],\n  \"x\": 1,\n  \"y\": 11,\n  \"z\": 20\n}\n",
            right: "{\n  \"params\": [1, 2, 3],\n  \"x\": 1,\n  \"y\": 10,\n  \"z\": 19\n}\n",
            left_markers: &["11"],
            right_markers: &["19"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "json-nested",
            path: "calc.json",
            base: "{\n  \"left\": { \"y\": 10 },\n  \"right\": { \"z\": 20 }\n}\n",
            left: "{\n  \"left\": { \"y\": 11 },\n  \"right\": { \"z\": 20 }\n}\n",
            right: "{\n  \"left\": { \"y\": 10 },\n  \"right\": { \"z\": 19 }\n}\n",
            left_markers: &["11"],
            right_markers: &["19"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "toml",
            path: "calc.toml",
            base: "[calc]\na = 1\nb = 2\nc = 3\nx = 1\ny = 10\nz = 20\n",
            left: "[calc]\na = 1\nb = 2\nc = 3\nx = 1\ny = 11\nz = 20\n",
            right: "[calc]\na = 1\nb = 2\nc = 3\nx = 1\ny = 10\nz = 19\n",
            left_markers: &["y = 11"],
            right_markers: &["z = 19"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "yaml",
            path: "calc.yaml",
            base: "calc:\n  a: 1\n  b: 2\n  c: 3\n  x: 1\n  y: 10\n  z: 20\n",
            left: "calc:\n  a: 1\n  b: 2\n  c: 3\n  x: 1\n  y: 11\n  z: 20\n",
            right: "calc:\n  a: 1\n  b: 2\n  c: 3\n  x: 1\n  y: 10\n  z: 19\n",
            left_markers: &["y: 11"],
            right_markers: &["z: 19"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "typescript",
            path: "calc.ts",
            base: "function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a;\n    return z;\n}\n",
            left: "function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c + 1;\n    const z = y - a;\n    return z;\n}\n",
            right: "function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "tsx",
            path: "calc.tsx",
            base: "export function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a;\n    return z;\n}\n",
            left: "export function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c + 1;\n    const z = y - a;\n    return z;\n}\n",
            right: "export function process(a: number, b: number, c: number): number {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "cpp",
            path: "calc.cpp",
            base: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c;\n    int z = y - a;\n    return z;\n}\n",
            left: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c + 1;\n    int z = y - a;\n    return z;\n}\n",
            right: "int process(int a, int b, int c) {\n    int x = a + b;\n    int y = x * c;\n    int z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "java",
            path: "Calc.java",
            base: "class Calc {\n    static int process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c;\n        int z = y - a;\n        return z;\n    }\n}\n",
            left: "class Calc {\n    static int process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c + 1;\n        int z = y - a;\n        return z;\n    }\n}\n",
            right: "class Calc {\n    static int process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c;\n        int z = y - a - 1;\n        return z;\n    }\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "csharp",
            path: "Calc.cs",
            base: "class Calc {\n    static int Process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c;\n        int z = y - a;\n        return z;\n    }\n}\n",
            left: "class Calc {\n    static int Process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c + 1;\n        int z = y - a;\n        return z;\n    }\n}\n",
            right: "class Calc {\n    static int Process(int a, int b, int c) {\n        int x = a + b;\n        int y = x * c;\n        int z = y - a - 1;\n        return z;\n    }\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "swift",
            path: "calc.swift",
            base: "func process(_ a: Int, _ b: Int, _ c: Int) -> Int {\n    let x = a + b\n    let y = x * c\n    let z = y - a\n    return z\n}\n",
            left: "func process(_ a: Int, _ b: Int, _ c: Int) -> Int {\n    let x = a + b\n    let y = x * c + 1\n    let z = y - a\n    return z\n}\n",
            right: "func process(_ a: Int, _ b: Int, _ c: Int) -> Int {\n    let x = a + b\n    let y = x * c\n    let z = y - a - 1\n    return z\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "kotlin",
            path: "calc.kt",
            base: "fun process(a: Int, b: Int, c: Int): Int {\n    val x = a + b\n    val y = x * c\n    val z = y - a\n    return z\n}\n",
            left: "fun process(a: Int, b: Int, c: Int): Int {\n    val x = a + b\n    val y = x * c + 1\n    val z = y - a\n    return z\n}\n",
            right: "fun process(a: Int, b: Int, c: Int): Int {\n    val x = a + b\n    val y = x * c\n    val z = y - a - 1\n    return z\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "zig",
            path: "calc.zig",
            base: "pub fn process(a: i32, b: i32, c: i32) i32 {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a;\n    return z;\n}\n",
            left: "pub fn process(a: i32, b: i32, c: i32) i32 {\n    const x = a + b;\n    const y = x * c + 1;\n    const z = y - a;\n    return z;\n}\n",
            right: "pub fn process(a: i32, b: i32, c: i32) i32 {\n    const x = a + b;\n    const y = x * c;\n    const z = y - a - 1;\n    return z;\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "sql",
            path: "calc.sql",
            base: "SELECT 10 AS y, 20 AS z;\n",
            left: "SELECT 11 AS y, 20 AS z;\n",
            right: "SELECT 10 AS y, 19 AS z;\n",
            left_markers: &["11"],
            right_markers: &["19"],
            forbidden: &[",,"],
        },
        LanguageMergeCase {
            label: "bash",
            path: "calc.sh",
            base: "process() {\n    local a=$1 b=$2 c=$3\n    local x=$((a + b))\n    local y=$((x * c))\n    local z=$((y - a))\n    echo \"$z\"\n}\n",
            left: "process() {\n    local a=$1 b=$2 c=$3\n    local x=$((a + b))\n    local y=$((x * c + 1))\n    local z=$((y - a))\n    echo \"$z\"\n}\n",
            right: "process() {\n    local a=$1 b=$2 c=$3\n    local x=$((a + b))\n    local y=$((x * c))\n    local z=$((y - a - 1))\n    echo \"$z\"\n}\n",
            left_markers: &["+ 1", "+1"],
            right_markers: &["- 1", "-1"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "gomod",
            path: "go.mod",
            base: "module example.com/demo\n\ngo 1.22\n",
            left: "module example.com/demo\n\ngo 1.23\n",
            right: "module example.com/demo\n\ngo 1.22\n\ntoolchain go1.22.5\n",
            left_markers: &["go 1.23"],
            right_markers: &["toolchain go1.22.5"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "html",
            path: "calc.html",
            base: "<!DOCTYPE html><html><body><div id=\"y\">10</div><div id=\"z\">20</div></body></html>\n",
            left: "<!DOCTYPE html><html><body><div id=\"y\">11</div><div id=\"z\">20</div></body></html>\n",
            right: "<!DOCTYPE html><html><body><div id=\"y\">10</div><div id=\"z\">19</div></body></html>\n",
            left_markers: &[">11<"],
            right_markers: &[">19<"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "css",
            path: "calc.css",
            base: "a { color: red; }\nb { color: blue; }\n",
            left: "a { color: green; }\nb { color: blue; }\n",
            right: "a { color: red; }\nb { color: yellow; }\n",
            left_markers: &["green"],
            right_markers: &["yellow"],
            forbidden: &[],
        },
        LanguageMergeCase {
            label: "css-same-rule",
            path: "calc.css",
            base: ".box {\n    margin-top: 10px;\n    padding-bottom: 20px;\n}\n",
            left: ".box {\n    margin-top: 11px;\n    padding-bottom: 20px;\n}\n",
            right: ".box {\n    margin-top: 10px;\n    padding-bottom: 19px;\n}\n",
            left_markers: &["11px"],
            right_markers: &["19px"],
            forbidden: &[],
        },
    ];
    CASES
}

fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}

pub fn assert_disjoint_language_merge(case: &LanguageMergeCase) {
    let base = parse_source(case.path, case.base)
        .unwrap_or_else(|e| panic!("{}: parse base: {e}", case.label));
    let left = parse_source(case.path, case.left)
        .unwrap_or_else(|e| panic!("{}: parse left: {e}", case.label));
    let right = parse_source(case.path, case.right)
        .unwrap_or_else(|e| panic!("{}: parse right: {e}", case.label));

    let left_diff = diff_graphs(&base, &left);
    let right_diff = diff_graphs(&base, &right);
    for (li, lm) in left_diff.mutations.iter().enumerate() {
        for (ri, rm) in right_diff.mutations.iter().enumerate() {
            if super::mutations_merge_equivalent(lm, rm) {
                assert!(
                    super::overlap_reason(&base, lm, rm).is_none(),
                    "{}: shared merge-equivalent mutations must not overlap: left[{li}] right[{ri}]",
                    case.label
                );
            }
        }
    }

    let outcome = merge_files(
        &FileContent::Ast(base.clone()),
        &FileContent::Ast(left),
        &FileContent::Ast(right),
    );
    let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
        panic!("{}: expected merge, got {outcome:?}", case.label);
    };
    let text = unparse(&merged);
    for forbidden in case.forbidden {
        assert!(
            !text.contains(forbidden),
            "{}: merged source must not contain {forbidden:?}: {text:?}",
            case.label
        );
    }
    parse_source(case.path, &text)
        .unwrap_or_else(|e| panic!("{}: merged source must parse: {e}: {text:?}", case.label));
    assert!(
        contains_any(&text, case.left_markers),
        "{}: merged source missing left edit markers {:?}: {text:?}",
        case.label,
        case.left_markers
    );
    assert!(
        contains_any(&text, case.right_markers),
        "{}: merged source missing right edit markers {:?}: {text:?}",
        case.label,
        case.right_markers
    );
}

#[cfg(test)]
mod tests {
    use super::{assert_disjoint_language_merge, language_merge_cases};

    #[test]
    fn disjoint_body_edits_merge_across_languages() {
        for case in language_merge_cases() {
            assert_disjoint_language_merge(case);
        }
    }
}
