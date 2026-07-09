use astvcs::diff::{open_in_browser, write_diff_view_html};
use astvcs::network::{
    add_remote, clone_repo, fetch, list_remotes, push, remove_remote, serve_repo,
};
use astvcs::store::{
    ChangeColumn, CommitOptions, FileStatus, FsckOptions, Repo, RepoError, RepoResult, ScanOptions,
    configured_identity, import_git_snapshot, parse_merge_resolutions, set_identity,
};
use astvcs::trace;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "astvcs",
    version = env!("CARGO_PKG_VERSION"),
    about = "Local version control with AST structural diff"
)]
struct Cli {
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// Print operational detail (notice:) to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Emit structured JSON errors on failure (stderr).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Status {
        /// Walk every tracked directory instead of using the scan cache.
        #[arg(long)]
        full_scan: bool,
    },
    Add {
        /// Stage modifications and deletions to tracked paths only (no new untracked files).
        #[arg(short = 'u', long = "update")]
        update: bool,
        /// Stage all changes (tracked modifications, deletions, and untracked files).
        #[arg(short = 'A', long = "all")]
        all: bool,
        paths: Vec<String>,
    },
    Diff(DiffArgs),
    Commit {
        #[arg(short, long)]
        message: String,
        /// Walk every tracked directory instead of using the scan cache.
        #[arg(long)]
        full_scan: bool,
        /// Skip client hooks (pre-commit, commit-msg).
        #[arg(long)]
        no_verify: bool,
    },
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },
    Tag {
        #[command(subcommand)]
        action: TagAction,
    },
    Merge(MergeArgs),
    MergeBase {
        left: String,
        right: String,
    },
    Checkout {
        #[arg(long, group = "target")]
        branch: Option<String>,
        #[arg(long, group = "target")]
        state: Option<String>,
        /// Allow checkout when the working tree has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    Reset {
        reference: String,
        /// Move the ref only; leave the working tree and staging unchanged.
        #[arg(long)]
        soft: bool,
        /// Move the ref and sync index.json to the target; clear staging; leave disk unchanged.
        #[arg(long)]
        mixed: bool,
        /// Allow hard reset when the working tree has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    Revert {
        reference: String,
        #[arg(short, long)]
        message: String,
        /// Simulate revert and print conflicts without changing the repository.
        #[arg(long)]
        dry_run: bool,
        /// Allow revert when the working tree has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    Rebase(RebaseArgs),
    CherryPick {
        reference: String,
        #[arg(short, long)]
        message: String,
        /// Allow cherry-pick when the working tree has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    Log {
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    Blame {
        path: String,
    },
    Bisect {
        #[command(subcommand)]
        action: BisectAction,
    },
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    Fetch {
        remote: String,
        #[arg(long)]
        branch: Option<String>,
        /// Limit fetched ancestor history to N timeline entries from each tip.
        #[arg(long)]
        depth: Option<usize>,
        /// Skip TLS certificate verification (dangerous; local dev only).
        #[arg(long)]
        insecure: bool,
    },
    Push {
        remote: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        force: bool,
        /// Skip client hooks (pre-push).
        #[arg(long)]
        no_verify: bool,
        /// Skip TLS certificate verification (dangerous; local dev only).
        #[arg(long)]
        insecure: bool,
    },
    Pull(PullArgs),
    Stash {
        #[command(subcommand)]
        action: StashAction,
    },
    Clone {
        url: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Bearer token for authenticated HTTP remotes (stored in origin remote config).
        #[arg(long)]
        token: Option<String>,
        /// Limit fetched ancestor history to N timeline entries from the branch tip.
        #[arg(long)]
        depth: Option<usize>,
        /// Skip TLS certificate verification (dangerous; local dev only).
        #[arg(long)]
        insecure: bool,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 9421)]
        port: u16,
        /// Bearer token required for mutating requests (and reads unless --public-read).
        #[arg(long)]
        token: Option<String>,
        /// Allow anonymous GET/HEAD on /v1/* when a token is configured.
        #[arg(long)]
        public_read: bool,
        /// PEM certificate for HTTPS (requires --tls-key).
        #[arg(long)]
        tls_cert: Option<PathBuf>,
        /// PEM private key for HTTPS (requires --tls-cert).
        #[arg(long)]
        tls_key: Option<PathBuf>,
    },
    /// Internal: serve repository over newline-delimited JSON on stdin/stdout (used by SSH remotes).
    RemoteServe {
        #[arg(long)]
        repo: PathBuf,
        /// Bearer token required for mutating requests (and reads unless --public-read).
        #[arg(long)]
        token: Option<String>,
        /// Allow anonymous GET/HEAD on /v1/* when a token is configured.
        #[arg(long)]
        public_read: bool,
    },
    Gc {
        /// Delete unreachable blobs (default is dry-run).
        #[arg(long)]
        prune: bool,
        /// Delete unreachable timeline entries and state manifests.
        #[arg(long)]
        prune_history: bool,
    },
    Fsck {
        /// Rewrite index.json from HEAD and remove unambiguous stray temp files.
        #[arg(long)]
        repair: bool,
        /// Delete local and remote-tracking refs pointing at missing timeline entries.
        #[arg(long)]
        prune_refs: bool,
    },
    Repack,
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },
    /// Import git HEAD tree snapshot into this repository (one commit).
    ImportGit {
        git_path: PathBuf,
        #[arg(short, long)]
        message: Option<String>,
    },
}

#[derive(Subcommand)]
enum IdentityAction {
    /// Show configured author identity (repository or global).
    Get {
        /// Read global identity from ~/.astvcs/config.json instead of the repository.
        #[arg(long)]
        global: bool,
    },
    /// Set author name and email for future commits, merges, and reverts.
    Set {
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: String,
        /// Write to global identity config instead of the repository.
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum RemoteAction {
    Add {
        name: String,
        url: String,
        #[arg(long)]
        token: Option<String>,
    },
    List,
    Remove {
        name: String,
    },
}

#[derive(Args)]
struct DiffArgs {
    /// Diff staged changes vs HEAD (--cached).
    #[arg(long = "staged", alias = "cached", conflicts_with_all = ["state", "base", "left", "right"])]
    staged: bool,

    /// Diff current HEAD against this state id.
    #[arg(long, conflicts_with_all = ["base", "left", "right"])]
    state: Option<String>,

    /// Three-way diff: common ancestor state id (requires --left and --right).
    #[arg(long, requires = "left", requires = "right")]
    base: Option<String>,

    /// Three-way diff: left branch tip or state id.
    #[arg(long, requires = "base", requires = "right")]
    left: Option<String>,

    /// Three-way diff: right branch tip or state id.
    #[arg(long, requires = "base", requires = "left")]
    right: Option<String>,

    /// Open an alignment-first HTML diff viewer in the default browser.
    #[arg(long)]
    view: bool,

    path: Option<String>,
}

#[derive(Args)]
struct PullArgs {
    remote: String,
    #[arg(long)]
    branch: Option<String>,
    /// Limit fetched ancestor history to N timeline entries from each tip.
    #[arg(long)]
    depth: Option<usize>,
    #[arg(short, long)]
    message: Option<String>,
    #[arg(long)]
    force: bool,
    /// Skip TLS certificate verification (dangerous; local dev only).
    #[arg(long)]
    insecure: bool,
    /// Skip client hooks during the merge step.
    #[arg(long)]
    no_verify: bool,
    #[arg(long = "resolve", value_name = "PATH:OURS|THEIRS")]
    resolve: Vec<String>,
}

#[derive(Args)]
struct MergeArgs {
    branch: String,

    #[arg(short, long, required_unless_present = "dry_run")]
    message: Option<String>,

    /// Simulate merge and print conflicts without changing the repository.
    #[arg(long)]
    dry_run: bool,

    /// Allow merge when the working tree has uncommitted changes.
    #[arg(long)]
    force: bool,

    /// Skip client hooks (pre-merge).
    #[arg(long)]
    no_verify: bool,

    /// Pick ours (HEAD) or theirs (merged branch) for a conflicted path.
    #[arg(long = "resolve", value_name = "PATH:OURS|THEIRS")]
    resolve: Vec<String>,
}

#[derive(Args)]
struct RebaseArgs {
    upstream: Option<String>,

    #[arg(long)]
    abort: bool,

    #[arg(long)]
    r#continue: bool,

    /// Allow rebase when the working tree has uncommitted changes.
    #[arg(long)]
    force: bool,

    /// Pick ours (current replay base) or theirs (replayed commit) for a conflicted path.
    #[arg(long = "resolve", value_name = "PATH:OURS|THEIRS")]
    resolve: Vec<String>,
}

#[derive(Subcommand)]
enum BisectAction {
    /// Start bisect; default bad is HEAD, good revision is required.
    Start {
        bad: Option<String>,
        good: Option<String>,
    },
    /// Mark a revision as bad (default: current HEAD).
    Bad { reference: Option<String> },
    /// Mark a revision as good (default: current HEAD).
    Good { reference: Option<String> },
    /// Run a script to classify revisions (exit 0=good, 1=bad, 125=skip).
    Run {
        script: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// End bisect and restore the original checkout.
    Reset,
}

#[derive(Subcommand)]
enum StashAction {
    Push {
        #[arg(short, long)]
        message: Option<String>,
        #[arg(short = 'u', long)]
        include_untracked: bool,
    },
    List,
    Pop {
        #[arg(default_value = "0")]
        index: usize,
    },
    Apply {
        #[arg(default_value = "0")]
        index: usize,
    },
}

#[derive(Subcommand)]
enum TagAction {
    Create { name: String, reference: String },
    List,
    Remove { name: String },
}

#[derive(Subcommand)]
enum BranchAction {
    List,
    Create {
        name: String,
        #[arg(long)]
        from: Option<String>,
    },
    Remove {
        name: String,
    },
}

fn repo_root(cli: &Cli) -> PathBuf {
    cli.repo.clone().unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(e) = run(cli) {
        print_cli_error(json, &e);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn print_cli_error(json: bool, err: &RepoError) {
    if json {
        eprintln!(
            "{}",
            serde_json::to_string(err).expect("serialize structured error")
        );
    } else {
        eprintln!("error: {err}");
    }
}

fn format_status_column(col: ChangeColumn) -> &'static str {
    match col {
        ChangeColumn::Clean => " ",
        ChangeColumn::Modified => "M",
        ChangeColumn::Added => "A",
        ChangeColumn::Deleted => "D",
    }
}

fn print_file_status(path: &str, status: &FileStatus, text_fallback: bool) {
    if status.untracked {
        let suffix = if text_fallback {
            " (text fallback)"
        } else {
            ""
        };
        println!("?? {path}{suffix}");
        return;
    }
    if let Some(from) = &status.staged_rename_from {
        let suffix = if text_fallback {
            " (text fallback)"
        } else {
            ""
        };
        println!("R  {from} -> {path}{suffix}");
        return;
    }
    if let Some(from) = &status.unstaged_rename_from {
        let suffix = if text_fallback {
            " (text fallback)"
        } else {
            ""
        };
        let staged = format_status_column(status.staged);
        println!("{staged}R {from} -> {path}{suffix}");
        return;
    }
    let label = format!(
        "{}{}",
        format_status_column(status.staged),
        format_status_column(status.unstaged)
    );
    let suffix = if text_fallback {
        " (text fallback)"
    } else {
        ""
    };
    println!("{label} {path}{suffix}");
}

fn run(cli: Cli) -> RepoResult<()> {
    trace::set_verbose(cli.verbose);
    let root = repo_root(&cli);
    match cli.command {
        Commands::Init { path } => {
            Repo::init(&path)?;
            println!("Initialized astvcs repository in {}", path.display());
        }
        Commands::Status { full_scan } => {
            let repo = Repo::open(&root)?;
            let status = repo.status_with_options(ScanOptions { full_scan })?;
            let branch = repo.head_branch()?;
            let head = repo.head_state()?;
            match branch {
                Some(name) => println!("On branch {name}"),
                None => println!("HEAD detached at {head}"),
            }
            println!("State: {head}");
            let mut paths: Vec<_> = status.entries.keys().cloned().collect();
            paths.sort();
            let mut any = false;
            for path in paths {
                let file_status = status.entries.get(&path).unwrap();
                if file_status.is_clean() {
                    continue;
                }
                any = true;
                print_file_status(
                    &path,
                    file_status,
                    status.text_fallback_paths.contains(&path),
                );
            }
            if !any {
                println!("nothing to commit, working tree clean");
            }
        }
        Commands::Add { update, all, paths } => {
            if update && all {
                return Err(RepoError::invalid_input(
                    "cannot use -u/--update and -A/--all together",
                ));
            }
            let repo = Repo::open(&root)?;
            repo.add(&paths, update, all)?;
        }
        Commands::Diff(args) => {
            let repo = Repo::open(&root)?;
            if args.view {
                let doc = if let (Some(base), Some(left), Some(right)) =
                    (&args.base, &args.left, &args.right)
                {
                    let base_id = repo.resolve_state_ref(base)?;
                    let left_id = repo.resolve_state_ref(left)?;
                    let right_id = repo.resolve_state_ref(right)?;
                    repo.diff_view_three_way(&base_id, &left_id, &right_id, args.path.as_deref())?
                } else if let Some(to) = args.state {
                    let from = repo.head_state()?;
                    let to_id = repo.resolve_state_ref(&to)?;
                    repo.diff_view_states(&from, &to_id, args.path.as_deref())?
                } else if args.staged {
                    repo.diff_view_staged(args.path.as_deref())?
                } else {
                    repo.diff_view_working(args.path.as_deref())?
                };
                let path = write_diff_view_html(&doc).map_err(RepoError::from_message)?;
                open_in_browser(&path).map_err(RepoError::from_message)?;
                println!("{}", path.display());
            } else {
                let output = if let (Some(base), Some(left), Some(right)) =
                    (&args.base, &args.left, &args.right)
                {
                    let base_id = repo.resolve_state_ref(base)?;
                    let left_id = repo.resolve_state_ref(left)?;
                    let right_id = repo.resolve_state_ref(right)?;
                    repo.diff_three_way(&base_id, &left_id, &right_id, args.path.as_deref())?
                } else if let Some(to) = args.state {
                    let from = repo.head_state()?;
                    let to_id = repo.resolve_state_ref(&to)?;
                    match args.path {
                        Some(p) => repo.diff_state_path(&from, &to_id, &p)?,
                        None => repo.diff_states(&from, &to_id)?,
                    }
                } else if args.staged {
                    match args.path.as_deref() {
                        Some(p) => repo.diff_staged(p)?,
                        None => repo.diff_staged_tree()?,
                    }
                } else if let Some(p) = args.path.as_deref() {
                    repo.diff_working(p)?
                } else {
                    repo.diff_working_tree()?
                };
                print!("{output}");
            }
        }
        Commands::Commit {
            message,
            full_scan,
            no_verify,
        } => {
            let repo = Repo::open(&root)?;
            let outcome = repo.commit_with_options(
                &message,
                CommitOptions {
                    scan: ScanOptions { full_scan },
                    no_verify,
                },
            )?;
            if outcome.created {
                println!("Committed state {}", outcome.state_id);
            } else {
                println!("No changes (state {} unchanged)", outcome.state_id);
            }
        }
        Commands::Branch { action } => match action {
            BranchAction::List => {
                let repo = Repo::open(&root)?;
                let current = repo.head_branch()?;
                for b in repo.list_branches()? {
                    let marker = if current.as_ref() == Some(&b.name) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {} ({})", b.name, b.state_id);
                }
            }
            BranchAction::Create { name, from } => {
                let repo = Repo::open(&root)?;
                repo.create_branch(&name, from.as_deref())?;
                println!("Created branch {name}");
            }
            BranchAction::Remove { name } => {
                let repo = Repo::open(&root)?;
                repo.remove_branch(&name)?;
                println!("Removed branch {name}");
            }
        },
        Commands::Tag { action } => match action {
            TagAction::Create { name, reference } => {
                let repo = Repo::open(&root)?;
                repo.create_tag(&name, &reference)?;
                println!("Created tag {name}");
            }
            TagAction::List => {
                let repo = Repo::open(&root)?;
                for tag in repo.list_tags()? {
                    println!("{} ({})", tag.name, tag.state_id);
                }
            }
            TagAction::Remove { name } => {
                let repo = Repo::open(&root)?;
                repo.remove_tag(&name)?;
                println!("Removed tag {name}");
            }
        },
        Commands::Merge(args) => {
            let repo = Repo::open(&root)?;
            let resolutions =
                parse_merge_resolutions(&args.resolve).map_err(RepoError::from_message)?;
            if args.dry_run {
                let plan = repo.prepare_merge(&args.branch, &resolutions)?;
                if plan.is_clean() {
                    print!("{}", plan.format_dry_run());
                } else {
                    print!("{}", plan.format_conflicts());
                    trace::warn("merge dry-run: would conflict");
                    return Err(RepoError::merge_conflict("merge would conflict"));
                }
            } else {
                let message = args.message.expect("message required");
                let id = repo.merge_branch_with_resolutions_force(
                    &args.branch,
                    &message,
                    &resolutions,
                    args.force,
                    args.no_verify,
                )?;
                println!(
                    "Merged branch {} into current branch (state {id})",
                    args.branch
                );
            }
        }
        Commands::MergeBase { left, right } => {
            let repo = Repo::open(&root)?;
            let base = repo.merge_base_refs(&left, &right)?;
            println!("{base}");
        }
        Commands::Checkout {
            branch,
            state,
            force,
        } => {
            let repo = Repo::open(&root)?;
            if let Some(name) = branch {
                repo.checkout_branch_with_force(&name, force)?;
                println!("Switched to branch {name}");
            } else if let Some(reference) = state {
                let id = repo.resolve_state_ref(&reference)?;
                repo.checkout_state_with_force(&id, force)?;
                println!("Checked out state {id} (detached HEAD)");
            } else {
                return Err(RepoError::invalid_input("specify --branch or --state"));
            }
        }
        Commands::Reset {
            reference,
            soft,
            mixed,
            force,
        } => {
            if soft && mixed {
                return Err(RepoError::invalid_input(
                    "cannot use --soft and --mixed together",
                ));
            }
            let repo = Repo::open(&root)?;
            let target = repo.reset(&reference, soft, mixed, force)?;
            if soft {
                match repo.head_branch()? {
                    Some(name) => {
                        println!("Reset branch {name} to state {target} (soft)");
                    }
                    None => println!("Reset HEAD to state {target} (soft)"),
                }
            } else if mixed {
                match repo.head_branch()? {
                    Some(name) => {
                        println!("Reset branch {name} to state {target} (mixed)");
                    }
                    None => println!("Reset HEAD to state {target} (mixed)"),
                }
            } else {
                println!("Reset to state {target}");
            }
        }
        Commands::Revert {
            reference,
            message,
            dry_run,
            force,
        } => {
            let repo = Repo::open(&root)?;
            if dry_run {
                let plan = repo.revert_state_dry_run(&reference)?;
                if plan.is_clean() {
                    print!("{}", plan.format_dry_run());
                } else {
                    print!("{}", plan.format_conflicts());
                    trace::warn("revert dry-run: would conflict");
                    return Err(RepoError::revert_conflict("revert would conflict"));
                }
            } else {
                let outcome = repo.revert_state_with_force(&reference, &message, force)?;
                if outcome.created {
                    println!(
                        "Reverted state {} (new state {})",
                        reference, outcome.state_id
                    );
                } else {
                    println!("No changes (state {} unchanged)", outcome.state_id);
                }
            }
        }
        Commands::Rebase(args) => {
            let repo = Repo::open(&root)?;
            if args.abort {
                if args.r#continue || args.upstream.is_some() || !args.resolve.is_empty() {
                    return Err(RepoError::invalid_input(
                        "rebase --abort cannot be combined with other rebase options",
                    ));
                }
                repo.rebase_abort()?;
                println!("Rebase aborted");
            } else if args.r#continue {
                if args.upstream.is_some() {
                    return Err(RepoError::invalid_input(
                        "rebase --continue cannot take an upstream branch",
                    ));
                }
                let resolutions =
                    parse_merge_resolutions(&args.resolve).map_err(RepoError::from_message)?;
                repo.rebase_continue(&resolutions, args.force)?;
                println!("Rebase continued");
            } else {
                let upstream = args.upstream.ok_or_else(|| {
                    RepoError::invalid_input("upstream branch required (or use --abort/--continue)")
                })?;
                if !args.resolve.is_empty() {
                    return Err(RepoError::invalid_input(
                        "--resolve is only valid with rebase --continue",
                    ));
                }
                repo.rebase_start(&upstream, args.force)?;
                println!("Rebased onto {upstream}");
            }
        }
        Commands::CherryPick {
            reference,
            message,
            force,
        } => {
            let repo = Repo::open(&root)?;
            let id = repo.cherry_pick(&reference, &message, force)?;
            println!("Cherry-picked {reference} (new state {id})");
        }
        Commands::Log { limit } => {
            let repo = Repo::open(&root)?;
            for entry in repo.history(limit)? {
                println!("state {}", entry.id);
                println!("  message: {}", entry.message);
                println!("  timestamp: {}", entry.timestamp);
                if !entry.author_name.is_empty() || !entry.author_email.is_empty() {
                    println!("  author: {} <{}>", entry.author_name, entry.author_email);
                }
                if let Some(parent) = &entry.parent {
                    println!("  parent: {parent}");
                }
                for p in &entry.parents {
                    if entry.parent.as_ref() != Some(p) {
                        println!("  parent: {p}");
                    }
                }
                println!();
            }
        }
        Commands::Blame { path } => {
            let repo = Repo::open(&root)?;
            for line in repo.blame(&path)? {
                println!(
                    "{} ({} {} {}) {}",
                    line.short_state_id(),
                    line.author_name,
                    line.author_email,
                    line.timestamp,
                    line.message
                );
                println!("{}", line.content);
            }
        }
        Commands::Bisect { action } => {
            let repo = Repo::open(&root)?;
            match action {
                BisectAction::Start { bad, good } => {
                    let good_ref = match (&bad, &good) {
                        (None, None) => {
                            return Err(RepoError::invalid_input(
                                "good revision required; usage: bisect start [bad] good",
                            ));
                        }
                        (Some(b), None) => b.as_str(),
                        (_, Some(g)) => g.as_str(),
                    };
                    let bad_ref = good.as_ref().and(bad.as_deref());
                    repo.bisect_start(bad_ref, good_ref)?;
                }
                BisectAction::Bad { reference } => {
                    repo.bisect_mark_bad(reference.as_deref())?;
                }
                BisectAction::Good { reference } => {
                    repo.bisect_mark_good(reference.as_deref())?;
                }
                BisectAction::Run { script, args } => {
                    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    repo.bisect_run(&script, &arg_refs)?;
                }
                BisectAction::Reset => {
                    repo.bisect_reset()?;
                    println!("Bisect reset");
                }
            }
        }
        Commands::Identity { action } => {
            let repo = Repo::open(&root)?;
            match action {
                IdentityAction::Get { global } => match configured_identity(&repo, global)? {
                    Some(id) => println!("{} <{}>", id.name, id.email),
                    None => {
                        return Err(RepoError::missing_identity());
                    }
                },
                IdentityAction::Set {
                    name,
                    email,
                    global,
                } => {
                    set_identity(&repo, &name, &email, global)?;
                    if global {
                        println!("Set global author identity to {name} <{email}>");
                    } else {
                        println!("Set repository author identity to {name} <{email}>");
                    }
                }
            }
        }
        Commands::Remote { action } => {
            let repo = Repo::open(&root)?;
            match action {
                RemoteAction::Add { name, url, token } => {
                    add_remote(&repo, &name, &url, token.as_deref())
                        .map_err(RepoError::from_message)?;
                    println!("Added remote {name} ({url})");
                }
                RemoteAction::List => {
                    for (name, url) in list_remotes(&repo).map_err(RepoError::from_message)? {
                        println!("{name}\t{url}");
                    }
                }
                RemoteAction::Remove { name } => {
                    remove_remote(&repo, &name).map_err(RepoError::from_message)?;
                    println!("Removed remote {name}");
                }
            }
        }
        Commands::Fetch {
            remote,
            branch,
            depth,
            insecure,
        } => {
            let repo = Repo::open(&root)?;
            let outcome = fetch(&repo, &remote, branch.as_deref(), depth, insecure)
                .map_err(RepoError::from_message)?;
            for (name, tip) in outcome.branches {
                println!("Fetched {remote}/{name} -> {tip}");
            }
        }
        Commands::Push {
            remote,
            branch,
            force,
            no_verify,
            insecure,
        } => {
            let repo = Repo::open(&root)?;
            let outcome = push(
                &repo,
                &remote,
                branch.as_deref(),
                force,
                no_verify,
                insecure,
            )
            .map_err(RepoError::from_message)?;
            println!(
                "Pushed {} to {}/{} ({})",
                outcome.branch, remote, outcome.branch, outcome.state_id
            );
        }
        Commands::Pull(args) => {
            let repo = Repo::open(&root)?;
            let branch_name = match args.branch {
                Some(name) => name,
                None => repo.head_branch()?.ok_or_else(|| {
                    RepoError::invalid_input("detached HEAD; specify --branch to pull")
                })?,
            };
            let remote_ref = format!("{}/{}", args.remote, branch_name);
            let outcome = fetch(
                &repo,
                &args.remote,
                Some(&branch_name),
                args.depth,
                args.insecure,
            )
            .map_err(RepoError::from_message)?;
            for (name, tip) in outcome.branches {
                println!("Fetched {}/{} -> {tip}", args.remote, name);
            }
            let resolutions =
                parse_merge_resolutions(&args.resolve).map_err(RepoError::from_message)?;
            let message = args
                .message
                .unwrap_or_else(|| format!("Merge {remote_ref}"));
            match repo.merge_branch_with_resolutions_force(
                &remote_ref,
                &message,
                &resolutions,
                args.force,
                args.no_verify,
            ) {
                Ok(id) => {
                    println!("Merged {remote_ref} into current branch (state {id})");
                }
                Err(e) if e.message == "already up to date" => {
                    println!("Already up to date with {remote_ref}");
                }
                Err(e) => {
                    trace::warn("pull: merge failed after successful fetch");
                    return Err(e);
                }
            }
        }
        Commands::Stash { action } => {
            let repo = Repo::open(&root)?;
            match action {
                StashAction::Push {
                    message,
                    include_untracked,
                } => {
                    let id = repo.stash_push(message, include_untracked)?;
                    println!("Saved stash@{{0}} (id {id})");
                }
                StashAction::List => {
                    for info in repo.stash_list()? {
                        println!(
                            "stash@{{{}}}: {} (base {})",
                            info.index, info.message, info.base_state_id
                        );
                    }
                }
                StashAction::Pop { index } => {
                    repo.stash_pop(index)?;
                    println!("Dropped stash@{{{index}}}");
                }
                StashAction::Apply { index } => {
                    repo.stash_apply(index)?;
                    println!("Applied stash@{{{index}}}");
                }
            }
        }
        Commands::Clone {
            url,
            path,
            token,
            depth,
            insecure,
        } => {
            let (_, branch) = clone_repo(&url, &path, token.as_deref(), depth, insecure)
                .map_err(RepoError::from_message)?;
            println!(
                "Cloned into {} (checked out branch {branch})",
                path.display()
            );
        }
        Commands::Serve {
            bind,
            port,
            token,
            public_read,
            tls_cert,
            tls_key,
        } => {
            let repo = Repo::open(&root)?;
            let token = token.or_else(|| std::env::var("ASTVCS_SERVE_TOKEN").ok());
            let options = astvcs::network::ServeOptions {
                token,
                public_read,
                tls_cert,
                tls_key,
            };
            serve_repo(&repo, &bind, port, options).map_err(RepoError::from_message)?;
        }
        Commands::RemoteServe {
            repo,
            token,
            public_read,
        } => {
            let token = token.or_else(|| std::env::var("ASTVCS_SERVE_TOKEN").ok());
            let options = astvcs::network::ServeAuthOptions { token, public_read };
            let repo = Repo::open(&repo)?;
            astvcs::network::remote_serve_repo(repo.root_path(), options)
                .map_err(RepoError::from_message)?;
        }
        Commands::Gc {
            prune,
            prune_history,
        } => {
            let repo = Repo::open(&root)?;
            let report = repo.gc(prune, prune_history)?;
            print!("{}", report.format_output());
        }
        Commands::Fsck { repair, prune_refs } => {
            let repo = Repo::open(&root)?;
            let report = repo.fsck(FsckOptions { repair, prune_refs })?;
            print!("{}", report.format_output());
            if !report.is_clean() {
                return Err(RepoError::integrity_check(
                    "repository integrity check failed",
                ));
            }
        }
        Commands::Repack => {
            let repo = Repo::open(&root)?;
            let report = repo.repack()?;
            print!("{}", report.format_output());
        }
        Commands::ImportGit { git_path, message } => {
            if !root.join(".astvcs").is_dir() {
                Repo::init(&root)?;
                trace::notice(format!(
                    "import-git: initialized astvcs repository in {}",
                    root.display()
                ));
            }
            let repo = Repo::open(&root)?;
            let message = message.unwrap_or_else(|| "Import from git HEAD".into());
            let outcome = import_git_snapshot(&repo, &git_path, &message)?;
            if outcome.created {
                println!("import-git: committed {} ({})", outcome.state_id, message);
            } else {
                println!(
                    "import-git: no changes (state {} unchanged)",
                    outcome.state_id
                );
            }
        }
    }
    Ok(())
}
