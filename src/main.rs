use astvcs::network::{
    add_remote, clone_repo, fetch, list_remotes, push, remove_remote, serve_repo,
};
use astvcs::store::{FileStatus, Repo, parse_merge_resolutions};
use astvcs::trace;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "astvcs",
    about = "Local version control with AST structural diff"
)]
struct Cli {
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// Print operational detail (notice:) to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Status,
    Diff(DiffArgs),
    Commit {
        #[arg(short, long)]
        message: String,
    },
    Branch {
        #[command(subcommand)]
        action: BranchAction,
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
        /// Move the ref only; leave the working tree and index unchanged.
        #[arg(long)]
        soft: bool,
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
    Log {
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    Fetch {
        remote: String,
        #[arg(long)]
        branch: Option<String>,
    },
    Push {
        remote: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        force: bool,
    },
    Clone {
        url: String,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 9421)]
        port: u16,
    },
    Gc {
        /// Delete unreachable blobs (default is dry-run).
        #[arg(long)]
        prune: bool,
    },
    Fsck,
}

#[derive(Subcommand)]
enum RemoteAction {
    Add { name: String, url: String },
    List,
    Remove { name: String },
}

#[derive(Args)]
struct DiffArgs {
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

    path: Option<String>,
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

    /// Pick ours (HEAD) or theirs (merged branch) for a conflicted path.
    #[arg(long = "resolve", value_name = "PATH:OURS|THEIRS")]
    resolve: Vec<String>,
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
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run(cli: Cli) -> Result<(), String> {
    trace::set_verbose(cli.verbose);
    let root = repo_root(&cli);
    match cli.command {
        Commands::Init { path } => {
            Repo::init(&path)?;
            println!("Initialized astvcs repository in {}", path.display());
        }
        Commands::Status => {
            let repo = Repo::open(&root)?;
            let status = repo.status()?;
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
                let label = match status.entries.get(&path).unwrap() {
                    FileStatus::Unchanged => continue,
                    FileStatus::Modified => " M",
                    FileStatus::Added => " A",
                    FileStatus::Removed => " D",
                    FileStatus::Untracked => "??",
                };
                any = true;
                println!("{label} {path}");
            }
            if !any {
                println!("nothing to commit, working tree clean");
            }
        }
        Commands::Diff(args) => {
            let repo = Repo::open(&root)?;
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
            } else if let Some(p) = args.path {
                repo.diff_working(&p)?
            } else {
                let head = repo.head_state()?;
                let status = repo.status()?;
                let mut out = String::new();
                for (path, st) in &status.entries {
                    if matches!(st, FileStatus::Modified | FileStatus::Added) {
                        out.push_str(&repo.diff_working(path)?);
                    }
                }
                if out.is_empty() {
                    repo.diff_states(&head, &head)?
                } else {
                    out
                }
            };
            print!("{output}");
        }
        Commands::Commit { message } => {
            let repo = Repo::open(&root)?;
            let outcome = repo.commit(&message)?;
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
        Commands::Merge(args) => {
            let repo = Repo::open(&root)?;
            let resolutions = parse_merge_resolutions(&args.resolve)?;
            if args.dry_run {
                let plan = repo.prepare_merge(&args.branch, &resolutions)?;
                if plan.is_clean() {
                    print!("{}", plan.format_dry_run());
                } else {
                    print!("{}", plan.format_conflicts());
                    trace::warn("merge dry-run: would conflict");
                    return Err("merge would conflict".into());
                }
            } else {
                let message = args.message.expect("message required");
                let id = repo.merge_branch_with_resolutions_force(
                    &args.branch,
                    &message,
                    &resolutions,
                    args.force,
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
                return Err("specify --branch or --state".into());
            }
        }
        Commands::Reset {
            reference,
            soft,
            force,
        } => {
            let repo = Repo::open(&root)?;
            let target = repo.reset(&reference, soft, force)?;
            if soft {
                match repo.head_branch()? {
                    Some(name) => {
                        println!("Reset branch {name} to state {target} (soft)");
                    }
                    None => println!("Reset HEAD to state {target} (soft)"),
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
                    return Err("revert would conflict".into());
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
        Commands::Log { limit } => {
            let repo = Repo::open(&root)?;
            for entry in repo.history(limit)? {
                println!("state {}", entry.id);
                println!("  message: {}", entry.message);
                println!("  timestamp: {}", entry.timestamp);
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
        Commands::Remote { action } => {
            let repo = Repo::open(&root)?;
            match action {
                RemoteAction::Add { name, url } => {
                    add_remote(&repo, &name, &url)?;
                    println!("Added remote {name} ({url})");
                }
                RemoteAction::List => {
                    for (name, url) in list_remotes(&repo)? {
                        println!("{name}\t{url}");
                    }
                }
                RemoteAction::Remove { name } => {
                    remove_remote(&repo, &name)?;
                    println!("Removed remote {name}");
                }
            }
        }
        Commands::Fetch { remote, branch } => {
            let repo = Repo::open(&root)?;
            let outcome = fetch(&repo, &remote, branch.as_deref())?;
            for (name, tip) in outcome.branches {
                println!("Fetched {remote}/{name} -> {tip}");
            }
        }
        Commands::Push {
            remote,
            branch,
            force,
        } => {
            let repo = Repo::open(&root)?;
            let outcome = push(&repo, &remote, branch.as_deref(), force)?;
            println!(
                "Pushed {} to {}/{} ({})",
                outcome.branch, remote, outcome.branch, outcome.state_id
            );
        }
        Commands::Clone { url, path } => {
            let (_, branch) = clone_repo(&url, &path)?;
            println!(
                "Cloned into {} (checked out branch {branch})",
                path.display()
            );
        }
        Commands::Serve { bind, port } => {
            let repo = Repo::open(&root)?;
            serve_repo(&repo, &bind, port)?;
        }
        Commands::Gc { prune } => {
            let repo = Repo::open(&root)?;
            let report = repo.gc(prune)?;
            print!("{}", report.format_output());
        }
        Commands::Fsck => {
            let repo = Repo::open(&root)?;
            let report = repo.fsck()?;
            print!("{}", report.format_output());
            if !report.is_clean() {
                return Err("repository integrity check failed".into());
            }
        }
    }
    Ok(())
}
