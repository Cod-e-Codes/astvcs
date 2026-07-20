//! Git external-diff entry point for astvcs's structural diff.
//!
//! Set as GIT_EXTERNAL_DIFF or per-path via .gitattributes + a diff
//! driver in git config, git invokes this as:
//!
//!   <driver-command> path old-file old-hex old-mode new-file new-hex new-mode [rename info]
//!
//! old-file/new-file are paths to blobs git already extracted to a temp
//! location (or /dev/null / NUL when the path was added or deleted). This
//! driver reports astvcs's compact structural edit intents instead of a raw
//! line diff, falling back to a normal unified line diff for anything
//! astvcs cannot parse (or when one side is absent).
//!
//! Wiring this up (see docs/git-integration.md):
//!
//!   # .gitattributes
//!   *.rs diff=astvcs
//!
//!   # .git/config or --global
//!   [diff "astvcs"]
//!       command = astvcs-diff-driver

use astvcs::diff::{TextEdit, diff_text};
use astvcs::frontend::{FileContent, load_working_content};
use astvcs::intent::format_intent_lines_compact;
use std::fs;
use std::process::ExitCode;

fn format_text_edit(edit: &TextEdit) -> String {
    match edit {
        TextEdit::ReplaceLine { line, old, new } => {
            format!("  ~ line {}: -{old} +{new}", line + 1)
        }
        TextEdit::DeleteLine { line, content } => format!("  - line {}: {content}", line + 1),
        TextEdit::InsertLine { line, content } => format!("  + line {}: {content}", line + 1),
    }
}

struct Args {
    path: String,
    old_file: String,
    new_file: String,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let path = argv
        .next()
        .ok_or("missing path argument (git passes this as $1)")?;
    let old_file = argv
        .next()
        .ok_or("missing old-file argument (git passes this as $2)")?;
    // $3 is old-hex, $4 is old-mode: unused here, skip them.
    let _old_hex = argv.next();
    let _old_mode = argv.next();
    let new_file = argv
        .next()
        .ok_or("missing new-file argument (git passes this as $5)")?;
    // Remaining args (new-hex, new-mode, rename info) are not needed.
    Ok(Args {
        path,
        old_file,
        new_file,
    })
}

/// Git passes /dev/null (Unix) or NUL (Windows) for an added or deleted side.
fn is_null_device(file: &str) -> bool {
    file == "/dev/null" || file.eq_ignore_ascii_case("NUL")
}

fn read_side(file: &str) -> Vec<u8> {
    if is_null_device(file) {
        return Vec::new();
    }
    fs::read(file).unwrap_or_default()
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let old_bytes = read_side(&args.old_file);
    let new_bytes = read_side(&args.new_file);

    let old_content = load_working_content(&args.path, old_bytes.clone());
    let new_content = load_working_content(&args.path, new_bytes.clone());

    println!("astvcs diff: {}", args.path);

    match (&old_content, &new_content) {
        (FileContent::Ast(old_graph), FileContent::Ast(new_graph)) => {
            let mutations = astvcs::diff_graphs(old_graph, new_graph).mutations;
            if mutations.is_empty() {
                println!("  (no structural change)");
            } else {
                for line in format_intent_lines_compact(Some(old_graph), &mutations) {
                    println!("{line}");
                }
            }
        }
        _ => {
            // Text fallback, binary content, symlink change, or a type change
            // between the two sides (e.g. text -> binary): fall back to a
            // conventional line diff over whatever UTF-8 text is available,
            // same behavior as astvcs's own CLI on non-AST paths.
            let old_text = String::from_utf8_lossy(&old_bytes);
            let new_text = String::from_utf8_lossy(&new_bytes);
            if old_content.is_binary() || new_content.is_binary() {
                println!("  (binary file - content diff omitted)");
            } else {
                let edits = diff_text(&old_text, &new_text);
                if edits.is_empty() {
                    println!("  (no textual change)");
                } else {
                    for edit in &edits {
                        println!("{}", format_text_edit(edit));
                    }
                }
            }
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("astvcs-diff-driver: {msg}");
            ExitCode::FAILURE
        }
    }
}
