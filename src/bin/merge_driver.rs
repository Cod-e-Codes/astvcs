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
//! Any nonzero exit code means git treats the path as conflicted: it leaves
//! %A as a standard git conflict-marker file (git's own fallback merge,
//! already written to %A before the driver ever runs) so git status and
//! manual resolution still work exactly like they do without this driver.

use astvcs::frontend::load_working_content;
use astvcs::merge::{ConflictResolutionStyle, MergeOutcome, merge_files};
use astvcs::trace;
use std::fs;
use std::process::ExitCode;

struct Args {
    base_path: String,
    ours_path: String,
    theirs_path: String,
    display_path: String,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let base_path = argv
        .next()
        .ok_or("usage: astvcs-merge-driver <%O base> <%A ours> <%B theirs> [%P path]")?;
    let ours_path = argv.next().ok_or("missing %A (ours/current) path")?;
    let theirs_path = argv.next().ok_or("missing %B (theirs/other) path")?;
    // %P is optional: git always passes it per the setup instructions above,
    // but a person invoking this by hand for testing may not.
    let display_path = argv.next().unwrap_or_else(|| ours_path.clone());
    Ok(Args {
        base_path,
        ours_path,
        theirs_path,
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
            // Leave %A untouched: git already wrote its own conflict-marker
            // version there before invoking the driver, so a non-clean exit
            // preserves normal `git status` / manual-resolution behavior.
            eprintln!(
                "{}",
                conflict.format_focused_report_with_labels(
                    &args.display_path,
                    "ours",
                    "theirs",
                    ConflictResolutionStyle::None,
                )
            );
            eprintln!(
                "astvcs: structural merge could not resolve {}; git's own conflict markers in {} are unchanged",
                args.display_path, args.ours_path
            );
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
