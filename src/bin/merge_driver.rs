//! Git merge-driver entry point for astvcs's structural three-way merge.
//!
//! Git invokes a configured merge driver as:
//!
//!   <driver-command> %O %A %B %L %P
//!
//! where %O is the common ancestor (base), %A is the current branch's version
//! (git expects the merge result written back to this path), %B is the other
//! branch's version, %L is the conflict marker size, and %P is the original
//! pathname of the file in the repository (used here to pick the tree-sitter
//! language and to label conflict output).
//!
//! Wiring this up in a repository (see docs/git-integration.md):
//!
//!   # .gitattributes
//!   *.rs merge=astvcs
//!
//!   # .git/config or --global
//!   [merge "astvcs"]
//!       name = astvcs structural merge driver
//!       driver = astvcs-merge-driver %O %A %B %P
//!       recursive = binary
//!
//! Exit code 0 means %A now holds the clean merge result and git stages it.
//! Any nonzero exit code means git treats the path as unmerged. On a
//! structural conflict this driver overwrites %A with a standard
//! `<<<<<<<` / `=======` / `>>>>>>>` marker file (ours then theirs) so the
//! working tree contains standard conflict markers. Binary conflicts leave %A
//! unchanged and still exit nonzero.

use astvcs::frontend::load_working_content;
use astvcs::merge::{ConflictResolutionStyle, MergeOutcome, merge_files};
use astvcs::trace;
use std::fs;
use std::process::ExitCode;

const DEFAULT_MARKER_SIZE: usize = 7;

struct Args {
    base_path: String,
    ours_path: String,
    theirs_path: String,
    marker_size: usize,
    display_path: String,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let base_path = argv
        .next()
        .ok_or("usage: astvcs-merge-driver <%O base> <%A ours> <%B theirs> [%L size] [%P path]")?;
    let ours_path = argv.next().ok_or("missing %A (ours/current) path")?;
    let theirs_path = argv.next().ok_or("missing %B (theirs/other) path")?;

    // Optional %L (digits) then optional %P. Config may pass only %P.
    let mut marker_size = DEFAULT_MARKER_SIZE;
    let mut display_path = ours_path.clone();
    if let Some(fourth) = argv.next() {
        if fourth.chars().all(|c| c.is_ascii_digit()) {
            marker_size = fourth
                .parse()
                .map_err(|_| format!("invalid conflict marker size %L: {fourth}"))?;
            if marker_size == 0 {
                return Err("conflict marker size %L must be >= 1".into());
            }
            if let Some(path) = argv.next() {
                display_path = path;
            }
        } else {
            display_path = fourth;
        }
    }

    Ok(Args {
        base_path,
        ours_path,
        theirs_path,
        marker_size,
        display_path,
    })
}

/// Read a file from disk as merge input. Missing files (added on only one
/// side relative to the merge base) are treated as empty text at this path;
/// git itself only invokes a merge driver when both sides touched the path,
/// but an absent %O is common (add/add) so this must not fail outright.
fn load(path: &str, display_path: &str) -> astvcs::frontend::FileContent {
    match fs::read(path) {
        Ok(bytes) => load_working_content(display_path, bytes),
        Err(_) => load_working_content(display_path, Vec::new()),
    }
}

fn content_to_bytes(
    content: &astvcs::frontend::FileContent,
    path: &str,
) -> Result<Vec<u8>, String> {
    use astvcs::frontend::FileContent;
    match content {
        FileContent::Ast(graph) => Ok(astvcs::unparse(graph).into_bytes()),
        FileContent::Text(text) => Ok(text.content.clone().into_bytes()),
        FileContent::Binary(bin) => Ok(bin.bytes.clone()),
        FileContent::Symlink(link) => Err(format!(
            "{path}: symlink merge produced a target change; re-run ln -sfn {} {path} \
manually (merge drivers cannot rewrite a path's file type)",
            link.target
        )),
    }
}

fn content_to_text(content: &astvcs::frontend::FileContent) -> Option<String> {
    use astvcs::frontend::FileContent;
    match content {
        FileContent::Ast(graph) => Some(astvcs::unparse(graph)),
        FileContent::Text(text) => Some(text.content.clone()),
        FileContent::Binary(_) | FileContent::Symlink(_) => None,
    }
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn format_conflict_markers(ours: &str, theirs: &str, marker_size: usize) -> String {
    let mark = |ch: char| ch.to_string().repeat(marker_size);
    let ours = ensure_trailing_newline(ours.to_string());
    let theirs = ensure_trailing_newline(theirs.to_string());
    format!(
        "{start} ours\n{ours}{sep}\n{theirs}{end} theirs\n",
        start = mark('<'),
        sep = mark('='),
        end = mark('>'),
    )
}

fn run() -> Result<bool, String> {
    let args = parse_args()?;

    let base = load(&args.base_path, &args.display_path);
    let ours = load(&args.ours_path, &args.display_path);
    let theirs = load(&args.theirs_path, &args.display_path);

    match merge_files(&base, &ours, &theirs) {
        MergeOutcome::Merged(content) => {
            let bytes = content_to_bytes(&content, &args.display_path)?;
            fs::write(&args.ours_path, bytes)
                .map_err(|e| format!("failed to write merged result to {}: {e}", args.ours_path))?;
            trace::notice(format!(
                "{}: structural merge clean ({})",
                args.display_path,
                content.display_kind()
            ));
            Ok(true)
        }
        MergeOutcome::Conflict(conflict) => {
            eprintln!(
                "{}",
                conflict.format_focused_report_with_labels(
                    &args.display_path,
                    "ours",
                    "theirs",
                    ConflictResolutionStyle::None,
                )
            );

            match (content_to_text(&ours), content_to_text(&theirs)) {
                (Some(ours_text), Some(theirs_text)) => {
                    let marked =
                        format_conflict_markers(&ours_text, &theirs_text, args.marker_size);
                    fs::write(&args.ours_path, marked).map_err(|e| {
                        format!(
                            "failed to write conflict markers to {}: {e}",
                            args.ours_path
                        )
                    })?;
                    eprintln!(
                        "astvcs: structural merge could not resolve {}; wrote conflict markers to {}",
                        args.display_path, args.ours_path
                    );
                }
                _ => {
                    eprintln!(
                        "astvcs: structural merge could not resolve {}; left {} unchanged (binary or non-text conflict; Git will mark the path unmerged)",
                        args.display_path, args.ours_path
                    );
                }
            }
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(msg) => {
            eprintln!("astvcs-merge-driver: {msg}");
            ExitCode::FAILURE
        }
    }
}
