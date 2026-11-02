//! `concurv` command-line interface. Human output by default; `--json` prints one
//! versioned JSON document per command (schema in `docs/cli.md`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use concurv_core::{ConcurvError, ErrorCode, Result};
use concurv_workspace::{
    Change, Repo, RepoOptions, RestoreOptions, StatusOptions, UpdateKind, UpdateOptions,
    WorkspaceInfo, abort_merge, branches, diff_with, log, merge_branch, op_log, publish, restore,
    save, status_with, undo, update_with, verify, workspaces,
};
use serde_json::{Value, json};

/// JSON output schema version.
const JSON_VERSION: u32 = 1;

#[derive(Parser)]
#[command(
    name = "concurv",
    version,
    about = "concurv — a local-first, agent-native version control system"
)]
struct Cli {
    /// Emit a single versioned JSON document instead of human output.
    #[arg(long, global = true)]
    json: bool,
    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new repository in the current directory.
    Init,
    /// Show working-copy changes relative to the workspace head (read-only).
    Status {
        /// Hash every file instead of trusting the stat cache.
        #[arg(long)]
        no_cache: bool,
    },
    /// Show content differences relative to the workspace head (read-only).
    Diff {
        /// Hash every file instead of trusting the stat cache.
        #[arg(long)]
        no_cache: bool,
    },
    /// Durably record the working copy as a snapshot in this workspace (does not publish).
    Save {
        /// Snapshot message.
        #[arg(short, long)]
        message: String,
    },
    /// Show snapshot history of the workspace head (first parent).
    Log {
        /// Maximum number of snapshots.
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Make the working copy equal to a snapshot. History is not rewritten.
    Restore {
        /// Snapshot id or unique prefix (≥ 8 hex characters).
        #[arg(long)]
        snapshot: String,
        /// Overwrite unsaved changes after saving them as a backup snapshot.
        #[arg(long)]
        force: bool,
    },
    /// Explain whether paths are ignored (.gitignore, .concurv/exclude) or tracked (read-only).
    CheckIgnore {
        /// Paths to check (relative to the current directory; `dir/` marks a directory).
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Check integrity of the repository (read-only).
    Verify {
        /// Also check every object file, including unreachable ones.
        #[arg(long)]
        full: bool,
    },
    /// Inspect the operation log (every repository operation).
    Op {
        #[command(subcommand)]
        cmd: OpCmd,
    },
    /// Manage private workspaces that share this repository.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Move this workspace's branch forward to the workspace head (CAS).
    Publish,
    /// Create, list and switch branches (ADR 0007).
    Branch {
        #[command(subcommand)]
        cmd: BranchCmd,
    },
    /// Exchange history with a Git repository (requires `git` on PATH).
    Git {
        #[command(subcommand)]
        cmd: GitCmd,
    },
    /// Reverse the most recent operation of this workspace (never shared state).
    Undo,
    /// Bring the branch this workspace tracks into it (fast-forward or three-way merge).
    Update {
        /// Internal: set by `merge`.
        #[arg(skip)]
        from: Option<String>,
        /// Write conflicts into the working copy and record a pending merge
        /// instead of refusing (resolve, then `concurv save`).
        #[arg(long)]
        allow_conflicts: bool,
        /// Give up a pending merge: restore the working copy to the head
        /// (edits are kept in a backup snapshot).
        #[arg(long)]
        abort: bool,
    },
    /// Merge another branch into this workspace (same rules as `update`); `publish`
    /// then moves the tracked branch.
    Merge {
        /// Branch to merge.
        #[arg(required_unless_present = "abort")]
        branch: Option<String>,
        /// Write conflicts into the working copy and record a pending merge
        /// instead of refusing (resolve, then `concurv save`).
        #[arg(long)]
        allow_conflicts: bool,
        /// Give up a pending merge (same as `update --abort`).
        #[arg(long)]
        abort: bool,
    },
}

#[derive(Subcommand)]
enum GitCmd {
    /// Write a branch's history into a Git repository and move a Git branch to it (CAS).
    Export {
        /// Path of the Git repository (bare or not).
        repo: PathBuf,
        /// concurv branch to export.
        #[arg(long, default_value = "main")]
        from: String,
        /// Git branch to update (default: the same name as `--from`).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Import a Git branch into this fresh repository and check it out.
    Import {
        /// Path of the Git repository.
        repo: PathBuf,
        /// Git branch to import.
        #[arg(long, default_value = "main")]
        branch: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Create a new workspace in an empty or new directory and check out its branch there.
    Add {
        /// Directory for the new workspace.
        path: PathBuf,
        /// Workspace name (default: the directory name).
        #[arg(long)]
        name: Option<String>,
        /// Branch the new workspace tracks (default: the current workspace's).
        #[arg(long)]
        branch: Option<String>,
    },
    /// List all workspaces of this repository.
    List,
}

#[derive(Subcommand)]
enum BranchCmd {
    /// Create a branch at this workspace's head (or `--at` a snapshot or branch).
    Create {
        /// Branch name (ASCII letters, digits and `._-/`).
        name: String,
        /// Snapshot id prefix or branch name to start from.
        #[arg(long)]
        at: Option<String>,
        /// Also make this workspace track the new branch (created at its head; no
        /// files change). Keeps unpublished saves on a branch.
        #[arg(long, conflicts_with = "at")]
        switch: bool,
    },
    /// List branches and the workspaces tracking them.
    List,
    /// Make this workspace track another branch and check out its head.
    Switch {
        /// Branch name.
        name: String,
    },
}

#[derive(Subcommand)]
enum OpCmd {
    /// List operations, newest first, including backups made by `restore --force`.
    Log {
        /// Maximum number of operations.
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
}

impl Cmd {
    fn name(&self) -> &'static str {
        match self {
            Cmd::Init => "init",
            Cmd::Status { .. } => "status",
            Cmd::Diff { .. } => "diff",
            Cmd::Save { .. } => "save",
            Cmd::Log { .. } => "log",
            Cmd::Restore { .. } => "restore",
            Cmd::Verify { .. } => "verify",
            Cmd::CheckIgnore { .. } => "check-ignore",
            Cmd::Op {
                cmd: OpCmd::Log { .. },
            } => "op log",
            Cmd::Workspace {
                cmd: WorkspaceCmd::Add { .. },
            } => "workspace add",
            Cmd::Workspace {
                cmd: WorkspaceCmd::List,
            } => "workspace list",
            Cmd::Publish => "publish",
            Cmd::Branch {
                cmd: BranchCmd::Create { .. },
            } => "branch create",
            Cmd::Branch {
                cmd: BranchCmd::List,
            } => "branch list",
            Cmd::Branch {
                cmd: BranchCmd::Switch { .. },
            } => "branch switch",
            Cmd::Update { .. } => "update",
            Cmd::Merge { .. } => "merge",
            Cmd::Undo => "undo",
            Cmd::Git {
                cmd: GitCmd::Export { .. },
            } => "git export",
            Cmd::Git {
                cmd: GitCmd::Import { .. },
            } => "git import",
        }
    }
}

/// Outcome of a command: JSON data, human text, and an optional failure
/// that still carries data (e.g. `verify` with problems).
struct Output {
    data: Value,
    human: String,
    failure: Option<ConcurvError>,
}

fn ok(data: Value, human: String) -> Result<Output> {
    Ok(Output {
        data,
        human,
        failure: None,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = cli.cmd.name();
    let json = cli.json;
    let result = run(cli);
    let (out, err) = match result {
        Ok(o) => {
            let f = o.failure;
            (Some((o.data, o.human)), f)
        }
        Err(e) => (None, Some(e)),
    };
    if json {
        let mut doc =
            json!({ "concurv_json_version": JSON_VERSION, "command": name, "ok": err.is_none() });
        if let Some((data, _)) = &out {
            doc["data"] = data.clone();
        }
        if let Some(e) = &err {
            doc["error"] = json!({
                "code": e.code.as_str(),
                "message": e.message,
                "exit_code": e.code.exit_code(),
            });
        }
        println!("{doc}");
    } else {
        if let Some((_, human)) = &out
            && !human.is_empty()
        {
            print!("{human}");
            if !human.ends_with('\n') {
                println!();
            }
        }
        if let Some(e) = &err {
            eprintln!("error[{}]: {}", e.code, e.message);
        }
    }
    match err {
        None => ExitCode::SUCCESS,
        Some(e) => ExitCode::from(u8::try_from(e.code.exit_code()).unwrap_or(1)),
    }
}

fn opts() -> RepoOptions {
    RepoOptions::default()
}

fn run(cli: Cli) -> Result<Output> {
    // `merge <branch>` runs the integration path of `update` with another source.
    let cmd = match cli.cmd {
        Cmd::Merge {
            branch,
            allow_conflicts,
            abort,
        } => Cmd::Update {
            from: branch,
            allow_conflicts,
            abort,
        },
        other => other,
    };
    let cwd = match cli.dir {
        Some(d) => d,
        None => std::env::current_dir()
            .map_err(|e| ConcurvError::io(e, "read current directory", "."))?,
    };
    match cmd {
        Cmd::Init => {
            let repo = Repo::init(&cwd, opts())?;
            ok(
                json!({ "root": repo.root().display().to_string(), "branch": concurv_workspace::DEFAULT_BRANCH }),
                format!(
                    "Initialized empty concurv repository in {}",
                    repo.store_dir().display()
                ),
            )
        }
        Cmd::Status { no_cache } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_status(&repo, status_opts(no_cache))
        }
        Cmd::Diff { no_cache } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_diff(&repo, status_opts(no_cache))
        }
        Cmd::Save { message } => {
            let repo = Repo::discover(&cwd, opts())?;
            let o = save(&repo, &message)?;
            let changes: Vec<Value> = o.changes.iter().map(change_json).collect();
            let human = match &o.snapshot {
                Some(s) => format!("Saved snapshot {s} ({} change(s))", o.changes.len()),
                None => "Nothing to save: working copy matches the head".into(),
            };
            ok(
                json!({
                    "created": o.snapshot.is_some(),
                    "snapshot": o.snapshot.map(|s| s.to_hex()),
                    "operation": o.operation.map(|s| s.to_hex()),
                    "changes": changes,
                }),
                human,
            )
        }
        Cmd::Log { limit } => {
            let repo = Repo::discover(&cwd, opts())?;
            let entries = log(&repo, limit)?;
            let mut human = String::new();
            let mut arr = Vec::new();
            for (id, s) in &entries {
                human.push_str(&format!(
                    "snapshot {id}\nAuthor: {}\nDate:   {}\n\n",
                    s.author,
                    format_time(s.timestamp_ms)
                ));
                for line in s.message.lines() {
                    human.push_str(&format!("    {line}\n"));
                }
                human.push('\n');
                arr.push(json!({
                    "snapshot": id.to_hex(),
                    "parents": s.parents.iter().map(|p| p.to_hex()).collect::<Vec<_>>(),
                    "tree": s.tree.to_hex(),
                    "change_id": s.change_id.to_hex(),
                    "author": s.author,
                    "timestamp_ms": s.timestamp_ms,
                    "message": s.message,
                }));
            }
            if entries.is_empty() {
                human = "No snapshots yet (use `concurv save -m ...`)".into();
            }
            ok(json!({ "snapshots": arr }), human)
        }
        Cmd::Restore { snapshot, force } => {
            let repo = Repo::discover(&cwd, opts())?;
            let target = repo.resolve_snapshot(&snapshot)?;
            let o = restore(&repo, target, RestoreOptions { force })?;
            let mut human = format!(
                "Restored working copy to {} ({} written, {} deleted). History unchanged; `concurv save` to record it.",
                o.target, o.written, o.deleted
            );
            if let Some(b) = o.backup {
                human.push_str(&format!(
                    "\nPrevious working copy saved as backup snapshot {b} (recover with `concurv restore --snapshot {b} --force`)."
                ));
            }
            ok(
                json!({
                    "target": o.target.to_hex(),
                    "backup": o.backup.map(|b| b.to_hex()),
                    "written": o.written,
                    "deleted": o.deleted,
                    "operation": o.operation.to_hex(),
                }),
                human,
            )
        }
        Cmd::CheckIgnore { paths } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_check_ignore(&repo, &cwd, &paths)
        }
        Cmd::Verify { full } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_verify(&repo, full)
        }
        Cmd::Op {
            cmd: OpCmd::Log { limit },
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_op_log(&repo, limit)
        }
        Cmd::Workspace {
            cmd: WorkspaceCmd::Add { path, name, branch },
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let o = workspaces::add_on(&repo, &path, name.as_deref(), branch.as_deref())?;
            let root = o.info.root.as_ref().map(|r| r.display().to_string());
            let human = format!(
                "Created workspace `{}` at {}{}",
                o.info.name,
                root.as_deref().unwrap_or("?"),
                match o.info.head {
                    Some(h) => format!(
                        " (checked out {} {}, {} file(s))",
                        o.info.branch.as_deref().unwrap_or("?"),
                        &h.to_hex()[..12],
                        o.written
                    ),
                    None => " (empty: nothing published yet)".into(),
                }
            );
            ok(
                json!({ "workspace": workspace_json(&o.info), "written": o.written }),
                human,
            )
        }
        Cmd::Workspace {
            cmd: WorkspaceCmd::List,
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            let all = workspaces::list(&repo)?;
            let mut human = String::new();
            for w in &all {
                human.push_str(&format!(
                    "{} {:<16} on {:<16} head {:<12}  {}\n",
                    if w.current { "*" } else { " " },
                    w.name,
                    w.branch.as_deref().unwrap_or("(none)"),
                    w.head
                        .map_or("(none)".to_owned(), |h| h.to_hex()[..12].to_owned()),
                    w.root
                        .as_ref()
                        .map_or("(path unknown)".to_owned(), |r| r.display().to_string()),
                ));
            }
            ok(
                json!({ "workspaces": all.iter().map(workspace_json).collect::<Vec<_>>() }),
                human,
            )
        }
        Cmd::Merge { .. } => Err(ConcurvError::internal("merge is dispatched as update")),
        Cmd::Branch { cmd } => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_branch(&repo, cmd)
        }
        Cmd::Publish => {
            let repo = Repo::discover(&cwd, opts())?;
            let o = publish(&repo)?;
            let human = match o.operation {
                Some(_) => format!(
                    "Published {} to `{}` (was {}).",
                    o.to,
                    o.branch,
                    o.from
                        .map_or("empty".to_owned(), |f| f.to_hex()[..12].to_owned())
                ),
                None => format!("`{}` is already at {}; nothing to publish.", o.branch, o.to),
            };
            ok(
                json!({
                    "branch": o.branch,
                    "from": o.from.map(|s| s.to_hex()),
                    "to": o.to.to_hex(),
                    "published": o.operation.is_some(),
                    "operation": o.operation.map(|s| s.to_hex()),
                }),
                human,
            )
        }
        Cmd::Git {
            cmd:
                GitCmd::Export {
                    repo: target,
                    from,
                    branch,
                },
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            let target = if target.is_absolute() {
                target
            } else {
                cwd.join(target)
            };
            let git = concurv_git::GitRepo::open(&target)?;
            let branch = branch.unwrap_or_else(|| from.clone());
            let o = concurv_git::export_from(&repo, &git, &from, &branch)?;
            let human = if o.updated {
                format!(
                    "Exported to Git: `{}` is now {} ({} new commit(s), {} already present).",
                    o.branch, o.tip, o.exported, o.reused
                )
            } else {
                format!(
                    "Git branch `{}` is already at {}; nothing to export.",
                    o.branch, o.tip
                )
            };
            ok(
                json!({
                    "branch": o.branch,
                    "tip": o.tip,
                    "previous": o.previous,
                    "exported": o.exported,
                    "reused": o.reused,
                    "updated": o.updated,
                }),
                human,
            )
        }
        Cmd::Git {
            cmd:
                GitCmd::Import {
                    repo: source,
                    branch,
                },
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            let source = if source.is_absolute() {
                source
            } else {
                cwd.join(source)
            };
            let git = concurv_git::GitRepo::open(&source)?;
            let o = concurv_git::import(&repo, &git, &branch)?;
            ok(
                json!({
                    "branch": o.branch,
                    "git_tip": o.git_tip,
                    "tip": o.tip.to_hex(),
                    "commits": o.commits,
                    "written": o.written,
                }),
                format!(
                    "Imported {} commit(s) from Git `{}` ({}); `main` is now {} and checked out ({} file(s)).",
                    o.commits, o.branch, o.git_tip, o.tip, o.written
                ),
            )
        }
        Cmd::Undo => {
            let repo = Repo::discover(&cwd, opts())?;
            let o = undo(&repo)?;
            let mut human = format!(
                "Undid `{}` {} ({}).\nWorkspace head is now {}.",
                o.undone_kind,
                &o.undone.to_hex()[..12],
                o.undone_description,
                o.head.map_or("(none)".to_owned(), |h| h.to_hex())
            );
            match o.files_restored_to {
                Some(s) => human.push_str(&format!(
                    "\nWorking files restored to {} ({} written, {} deleted).",
                    &s.to_hex()[..12],
                    o.written,
                    o.deleted
                )),
                None => human.push_str(
                    "\nWorking files unchanged: the undone save's content is now an unsaved change.",
                ),
            }
            ok(
                json!({
                    "undone": o.undone.to_hex(),
                    "undone_kind": o.undone_kind,
                    "undone_description": o.undone_description,
                    "head": o.head.map(|h| h.to_hex()),
                    "files_restored_to": o.files_restored_to.map(|s| s.to_hex()),
                    "written": o.written,
                    "deleted": o.deleted,
                    "operation": o.operation.to_hex(),
                }),
                human,
            )
        }
        Cmd::Update {
            allow_conflicts,
            abort: true,
            from,
        } => {
            if allow_conflicts || from.is_some() {
                return Err(ConcurvError::new(
                    ErrorCode::InvalidArgument,
                    "--abort cannot be combined with --allow-conflicts or a branch to merge",
                ));
            }
            let repo = Repo::discover(&cwd, opts())?;
            let o = abort_merge(&repo)?;
            let mut human = format!(
                "Merge aborted: working copy restored to {} ({} written, {} deleted).",
                o.target, o.written, o.deleted
            );
            if let Some(b) = o.backup {
                human.push_str(&format!(
                    "\nEdits made during the merge were saved as backup snapshot {b}."
                ));
            }
            ok(
                json!({
                    "result": "aborted",
                    "head": o.target.to_hex(),
                    "backup": o.backup.map(|b| b.to_hex()),
                    "written": o.written,
                    "deleted": o.deleted,
                    "operation": o.operation.to_hex(),
                }),
                human,
            )
        }
        Cmd::Update {
            allow_conflicts,
            abort: false,
            from,
        } => {
            let repo = Repo::discover(&cwd, opts())?;
            let how = UpdateOptions { allow_conflicts };
            let o = match from.as_deref() {
                Some(b) => merge_branch(&repo, b, how)?,
                None => update_with(&repo, how)?,
            };
            let mut human = match o.kind {
                UpdateKind::UpToDate => format!("Already up to date with `{}`.", o.branch),
                UpdateKind::FastForward => format!(
                    "Fast-forwarded to `{}` {} ({} written, {} deleted).",
                    o.branch,
                    o.head.map_or(String::new(), |h| h.to_hex()),
                    o.written,
                    o.deleted
                ),
                UpdateKind::Merged => format!(
                    "Merged `{}` into this workspace as {} ({} written, {} deleted).\n\
                     A clean merge is not a tested one: build and test before `concurv publish`.",
                    o.branch,
                    o.head.map_or(String::new(), |h| h.to_hex()),
                    o.written,
                    o.deleted
                ),
                UpdateKind::Conflicted => {
                    let mut s = format!(
                        "Merge of `{}` has conflicts; they are written into the working copy:\n",
                        o.branch
                    );
                    for e in o.conflict.iter().flat_map(|(_, es)| es) {
                        s.push_str(&format!(
                            "  {:<14} {}{}\n",
                            e.kind.as_str(),
                            e.path,
                            if e.markers {
                                "  (conflict markers)"
                            } else {
                                ""
                            }
                        ));
                    }
                    s.push_str(
                        "Resolve each one (see its `.concurv-conflict` file), delete the side files, \
                         then `concurv save -m ...`. Or give up with `concurv update --abort`.",
                    );
                    s
                }
            };
            if !o.content_merged.is_empty() {
                human.push_str(
                    "
Merged line by line (both sides edited different parts):",
                );
                for p in &o.content_merged {
                    human.push_str(&format!(
                        "
  {p}"
                    ));
                }
            }
            let conflicts: Vec<Value> = o
                .conflict
                .iter()
                .flat_map(|(_, es)| es)
                .map(|e| {
                    json!({
                        "path": e.path.to_string(),
                        "kind": e.kind.as_str(),
                        "markers": e.markers,
                        "side_files": e.side_files.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            ok(
                json!({
                    "branch": o.branch,
                    "result": o.kind.as_str(),
                    "head": o.head.map(|h| h.to_hex()),
                    "written": o.written,
                    "deleted": o.deleted,
                    "operation": o.operation.map(|s| s.to_hex()),
                    "conflict": o.conflict.as_ref().map(|(id, _)| id.to_hex()),
                    "conflicts": conflicts,
                    "content_merged": o.content_merged.iter().map(ToString::to_string).collect::<Vec<_>>(),
                }),
                human,
            )
        }
    }
}

fn workspace_json(w: &WorkspaceInfo) -> Value {
    json!({
        "id": w.id.to_hex(),
        "name": w.name,
        "branch": w.branch,
        "head": w.head.map(|h| h.to_hex()),
        "root": w.root.as_ref().map(|r| r.display().to_string()),
        "primary": w.primary,
        "current": w.current,
    })
}

fn change_json(c: &Change) -> Value {
    json!({
        "path": c.path.to_string(),
        "kind": c.kind.as_str(),
        "old_blob": c.old.map(|s| s.blob.to_hex()),
        "new_blob": c.new.map(|s| s.blob.to_hex()),
    })
}

fn status_opts(no_cache: bool) -> StatusOptions {
    StatusOptions {
        use_stat_cache: !no_cache,
    }
}

fn cmd_status(repo: &Repo, how: StatusOptions) -> Result<Output> {
    let st = status_with(repo, how)?;
    let mut human = format!(
        "On branch {}, head {}\n",
        st.branch.as_deref().unwrap_or("(none)"),
        st.head
            .map_or("(no snapshots yet)".to_owned(), |h| h.to_hex()[..12]
                .to_owned())
    );
    if let Some(m) = &st.interrupted_restore {
        match (&m.branch, m.origin) {
            (Some(b), Some(o)) => human.push_str(&format!(
                "WARNING: a switch to branch `{b}` was interrupted; every file holds its old or new content.\n\
                 \x20        Finish it with `concurv branch switch {b}`, or go back with \
                 `concurv restore --snapshot {o} --force`.\n"
            )),
            _ => human.push_str(&format!(
                "WARNING: a restore to {} was interrupted; re-run `concurv restore --snapshot {} --force` to finish it.\n",
                m.target, m.target
            )),
        }
        if let Some(b) = m.backup {
            human.push_str(&format!(
                "         Pre-restore working copy is preserved in snapshot {b}.\n"
            ));
        }
    }
    if let Some(m) = &st.merge {
        let open = m.conflicts.iter().filter(|c| !c.resolved()).count();
        human.push_str(&format!(
            "MERGE IN PROGRESS with {} ({} of {} conflict(s) unresolved):\n",
            &m.theirs.to_hex()[..12],
            open,
            m.conflicts.len()
        ));
        for c in &m.conflicts {
            let state = if c.resolved() {
                "resolved".to_owned()
            } else {
                let mut why = Vec::new();
                if c.markers_remaining {
                    why.push("markers".to_owned());
                }
                why.extend(c.remaining_side_files.iter().map(|s| s.to_string()));
                format!("unresolved: {}", why.join(", "))
            };
            human.push_str(&format!(
                "  {:<14} {}  [{state}]\n",
                c.kind.as_str(),
                c.path
            ));
        }
        human.push_str(if open == 0 {
            "All conflicts resolved: `concurv save -m ...` completes the merge.\n"
        } else {
            "Resolve them, then `concurv save -m ...`; or `concurv update --abort`.\n"
        });
    }
    if st.changes.is_empty() {
        human.push_str("Working copy clean.\n");
    } else {
        human.push_str(&format!("Changes ({}):\n", st.changes.len()));
        for c in &st.changes {
            human.push_str(&format!("  {:<13}{}\n", c.kind.as_str(), c.path));
        }
    }
    let merge = st.merge.as_ref().map(|m| {
        json!({
            "conflict": m.id.to_hex(),
            "theirs": m.theirs.to_hex(),
            "conflicts": m.conflicts.iter().map(|c| json!({
                "path": c.path.to_string(),
                "kind": c.kind.as_str(),
                "resolved": c.resolved(),
                "markers_remaining": c.markers_remaining,
                "remaining_side_files": c.remaining_side_files.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    });
    ok(
        json!({
            "branch": st.branch,
            "head": st.head.map(|h| h.to_hex()),
            "clean": st.changes.is_empty(),
            "changes": st.changes.iter().map(change_json).collect::<Vec<_>>(),
            "interrupted_restore": st.interrupted_restore.map(|m| json!({
                "target": m.target.to_hex(),
                "backup": m.backup.map(|b| b.to_hex()),
                "origin": m.origin.map(|o| o.to_hex()),
                "branch": m.branch,
            })),
            "merge": merge,
        }),
        human,
    )
}

fn cmd_diff(repo: &Repo, how: StatusOptions) -> Result<Output> {
    let diffs = diff_with(repo, how)?;
    let mut human = String::new();
    let mut arr = Vec::new();
    for d in &diffs {
        let path = d.change.path.to_string();
        match (&d.unified, d.note) {
            (Some(text), _) => human.push_str(text),
            (None, Some(note)) => {
                human.push_str(&format!("{} {path}: {note}\n", d.change.kind.as_str()))
            }
            (None, None) => {}
        }
        arr.push(json!({
            "path": path,
            "kind": d.change.kind.as_str(),
            "unified": d.unified,
            "note": d.note,
        }));
    }
    ok(json!({ "files": arr }), human)
}

fn cmd_op_log(repo: &Repo, limit: usize) -> Result<Output> {
    let ops = op_log(repo, limit)?;
    let ws = repo.workspace_id();
    let mut human = String::new();
    let mut arr = Vec::new();
    for (id, op) in &ops {
        let head = op.view.workspaces.get(&ws).and_then(|w| w.head);
        human.push_str(&format!(
            "operation {id} ({})\nDate:   {}\n    {}\n",
            op.kind,
            format_time(op.timestamp_ms),
            op.description
        ));
        if let Some(h) = head {
            human.push_str(&format!("    head after: {h}\n"));
        }
        if let Some(b) = op.backup {
            human.push_str(&format!(
                "    backup of discarded working copy: {b}\n    (recover with `concurv restore --snapshot {b} --force`)\n"
            ));
        }
        human.push('\n');
        arr.push(json!({
            "operation": id.to_hex(),
            "parents": op.parents.iter().map(|p| p.to_hex()).collect::<Vec<_>>(),
            "kind": op.kind,
            "description": op.description,
            "timestamp_ms": op.timestamp_ms,
            "device": op.device_id.to_hex(),
            "workspace": op.workspace_id.map(|w| w.to_hex()),
            "backup": op.backup.map(|b| b.to_hex()),
            "undoes": op.undoes.map(|u| u.to_hex()),
            "checked_out": op.checked_out.map(|s| s.to_hex()),
            "head": head.map(|h| h.to_hex()),
            "branches": op.view.branches.iter()
                .map(|(name, s)| (name.clone(), Value::String(s.to_hex())))
                .collect::<serde_json::Map<_, _>>(),
        }));
    }
    ok(json!({ "operations": arr }), human)
}

fn cmd_verify(repo: &Repo, full: bool) -> Result<Output> {
    let r = verify(repo, full);
    let mut human = format!(
        "Checked {} operation(s), {} snapshot(s), {} tree(s), {} blob(s){}.\n",
        r.operations,
        r.snapshots,
        r.trees,
        r.blobs,
        if full {
            format!(", {} unreachable object(s)", r.unreachable)
        } else {
            String::new()
        }
    );
    for p in &r.problems {
        human.push_str(&format!(
            "PROBLEM [{}] {}: {}\n",
            p.code, p.subject, p.message
        ));
    }
    for n in &r.notes {
        human.push_str(&format!("note: {n}\n"));
    }
    if r.ok() {
        human.push_str("Repository OK.\n");
    }
    let failure = (!r.ok()).then(|| {
        let code = if r
            .problems
            .iter()
            .any(|p| p.code == ErrorCode::ObjectMissing)
        {
            ErrorCode::ObjectMissing
        } else {
            r.problems
                .first()
                .map_or(ErrorCode::ObjectCorrupt, |p| p.code)
        };
        ConcurvError::new(
            code,
            format!("verification found {} problem(s)", r.problems.len()),
        )
    });
    Ok(Output {
        data: json!({
            "full": full,
            "operations": r.operations,
            "conflicts": r.conflicts,
            "snapshots": r.snapshots,
            "trees": r.trees,
            "blobs": r.blobs,
            "unreachable": r.unreachable,
            "problems": r.problems.iter().map(|p| json!({
                "code": p.code.as_str(), "subject": p.subject, "message": p.message,
            })).collect::<Vec<_>>(),
            "notes": r.notes,
        }),
        human,
        failure,
    })
}

/// Format ms since epoch as `YYYY-MM-DD HH:MM:SS UTC` (proleptic Gregorian).
fn format_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
}

fn cmd_check_ignore(repo: &Repo, cwd: &std::path::Path, inputs: &[String]) -> Result<Output> {
    let mut paths = Vec::with_capacity(inputs.len());
    for i in inputs {
        paths.push(repo.repo_relative(cwd, i)?);
    }
    let (_, _, ws) = repo.current()?;
    let head = repo.head_files(ws.head.as_ref())?;
    let (reports, unusable) = concurv_workspace::ignore::check_ignore(repo.root(), &head, &paths)?;
    let mut human = String::new();
    let mut arr = Vec::new();
    for (input, r) in inputs.iter().zip(&reports) {
        use concurv_workspace::ignore::Verdict;
        let (rule, negated) = match &r.verdict {
            Verdict::Unmatched => (None, false),
            Verdict::Ignored { source, pattern } => {
                (Some((source.to_string(), pattern.clone())), false)
            }
            Verdict::Included { source, pattern } => {
                (Some((source.to_string(), pattern.clone())), true)
            }
        };
        let why = match (&rule, &r.excluded_parent) {
            (Some((s, p)), Some(parent)) => format!("parent `{parent}/` excluded by `{p}` in {s}"),
            (Some((s, p)), None) => format!(
                "{} by `{p}` in {s}",
                if negated { "re-included" } else { "excluded" }
            ),
            (None, _) => "no rule matches".to_owned(),
        };
        let state = if r.tracked && (r.excluded_parent.is_some() || r.verdict.is_ignored()) {
            "tracked (still versioned: ignore rules only apply to untracked paths)"
        } else if r.tracked {
            "tracked"
        } else if r.ignored {
            "ignored"
        } else {
            "not ignored"
        };
        human.push_str(&format!("{input}: {state}; {why}\n"));
        arr.push(json!({
            "input": input,
            "path": r.path.to_string(),
            "is_dir": r.is_dir,
            "tracked": r.tracked,
            "ignored": r.ignored,
            "rule": rule.as_ref().map(|(s, p)| json!({ "source": s, "pattern": p, "negated": negated })),
            "excluded_parent": r.excluded_parent.as_ref().map(|p| p.to_string()),
        }));
    }
    for u in &unusable {
        human.push_str(&format!(
            "warning: {} line {}: pattern `{}` is unusable and never matches ({})\n",
            u.source, u.line, u.pattern, u.reason
        ));
    }
    let warnings: Vec<Value> = unusable
        .iter()
        .map(|u| json!({ "source": u.source.to_string(), "line": u.line, "pattern": u.pattern, "reason": u.reason }))
        .collect();
    ok(
        json!({ "paths": arr, "unusable_patterns": warnings }),
        human,
    )
}

fn cmd_branch(repo: &Repo, cmd: BranchCmd) -> Result<Output> {
    match cmd {
        BranchCmd::List => {
            let all = branches::list(repo)?;
            let mut human = String::new();
            for b in &all {
                human.push_str(&format!(
                    "{} {:<24} {}  {}\n",
                    if b.current { "*" } else { " " },
                    b.name,
                    &b.head.to_hex()[..12],
                    if b.workspaces.is_empty() {
                        "(no workspace)".to_owned()
                    } else {
                        format!("workspaces: {}", b.workspaces.join(", "))
                    }
                ));
            }
            if all.is_empty() {
                human.push_str("No branches yet: `concurv publish` creates the first one.\n");
            }
            let list: Vec<Value> = all
                .iter()
                .map(|b| {
                    json!({
                        "name": b.name,
                        "head": b.head.to_hex(),
                        "workspaces": b.workspaces,
                        "current": b.current,
                    })
                })
                .collect();
            ok(json!({ "branches": list }), human)
        }
        BranchCmd::Create { name, at, switch } => {
            let at = match at {
                None => None,
                Some(a) => {
                    let named = branches::list(repo)?
                        .into_iter()
                        .find(|b| b.name == a)
                        .map(|b| b.head);
                    Some(match named {
                        Some(h) => h,
                        None => repo.resolve_snapshot(&a)?,
                    })
                }
            };
            let o = branches::create(repo, &name, branches::CreateOptions { at, switch })?;
            ok(
                json!({
                    "branch": o.name,
                    "head": o.head.to_hex(),
                    "switched": o.switched,
                    "operation": o.operation.to_hex(),
                }),
                format!(
                    "Created branch `{}` at {}. Work on it with `concurv branch switch {}` or \
                     `concurv workspace add <dir> --branch {}`.",
                    o.name,
                    &o.head.to_hex()[..12],
                    o.name,
                    o.name
                ),
            )
        }
        BranchCmd::Switch { name } => {
            let o = branches::switch(repo, &name)?;
            let human = match o.operation {
                None => format!("Already on `{}`.", o.to),
                Some(_) => format!(
                    "{} from `{}` to `{}` at {} ({} written, {} deleted).",
                    if o.resumed {
                        "Finished the interrupted switch"
                    } else {
                        "Switched"
                    },
                    o.from.as_deref().unwrap_or("(none)"),
                    o.to,
                    &o.head.to_hex()[..12],
                    o.written,
                    o.deleted
                ),
            };
            ok(
                json!({
                    "from": o.from,
                    "branch": o.to,
                    "head": o.head.to_hex(),
                    "written": o.written,
                    "deleted": o.deleted,
                    "resumed": o.resumed,
                    "operation": o.operation.map(|s| s.to_hex()),
                }),
                human,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_time(1_793_491_200_000), "2026-11-01 00:00:00 UTC");
        assert_eq!(format_time(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_time(-1000), "1969-12-31 23:59:59 UTC");
    }
}
