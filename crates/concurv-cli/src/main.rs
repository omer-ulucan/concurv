//! `concurv` command-line interface. Human output by default; `--json` prints one
//! versioned JSON document per command (schema in `docs/cli.md`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use concurv_core::{ConcurvError, ErrorCode, Result};
use concurv_workspace::{
    Change, Repo, RepoOptions, RestoreOptions, diff, log, op_log, restore, save, status, verify,
};
use serde_json::{Value, json};

/// JSON output schema version.
const JSON_VERSION: u32 = 1;

#[derive(Parser)]
#[command(
    name = "concurv",
    version,
    about = "concurv — a local-first version control system (R1 kernel)"
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
    Status,
    /// Show content differences relative to the workspace head (read-only).
    Diff,
    /// Durably record the working copy as a new snapshot.
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
    /// Check integrity of the repository (read-only).
    Verify {
        /// Also check every object file, including unreachable ones.
        #[arg(long)]
        full: bool,
    },
    /// Inspect the operation log (every init, save and restore).
    Op {
        #[command(subcommand)]
        cmd: OpCmd,
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
            Cmd::Status => "status",
            Cmd::Diff => "diff",
            Cmd::Save { .. } => "save",
            Cmd::Log { .. } => "log",
            Cmd::Restore { .. } => "restore",
            Cmd::Verify { .. } => "verify",
            Cmd::Op {
                cmd: OpCmd::Log { .. },
            } => "op log",
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
    let cwd = match cli.dir {
        Some(d) => d,
        None => std::env::current_dir()
            .map_err(|e| ConcurvError::io(e, "read current directory", "."))?,
    };
    match cli.cmd {
        Cmd::Init => {
            let repo = Repo::init(&cwd, opts())?;
            ok(
                json!({ "root": repo.root().display().to_string(), "branch": concurv_workspace::DEFAULT_BRANCH }),
                format!(
                    "Initialized empty concurv repository in {}",
                    repo.meta_dir().display()
                ),
            )
        }
        Cmd::Status => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_status(&repo)
        }
        Cmd::Diff => {
            let repo = Repo::discover(&cwd, opts())?;
            cmd_diff(&repo)
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
    }
}

fn change_json(c: &Change) -> Value {
    json!({
        "path": c.path.to_string(),
        "kind": c.kind.as_str(),
        "old_blob": c.old.map(|s| s.blob.to_hex()),
        "new_blob": c.new.map(|s| s.blob.to_hex()),
    })
}

fn cmd_status(repo: &Repo) -> Result<Output> {
    let st = status(repo)?;
    let mut human = format!(
        "On branch {}, head {}\n",
        st.branch.as_deref().unwrap_or("(none)"),
        st.head
            .map_or("(no snapshots yet)".to_owned(), |h| h.to_hex()[..12]
                .to_owned())
    );
    if let Some(m) = &st.interrupted_restore {
        human.push_str(&format!(
            "WARNING: a restore to {} was interrupted; re-run `concurv restore --snapshot {} --force` to finish it.\n",
            m.target, m.target
        ));
        if let Some(b) = m.backup {
            human.push_str(&format!(
                "         Pre-restore working copy is preserved in snapshot {b}.\n"
            ));
        }
    }
    if st.changes.is_empty() {
        human.push_str("Working copy clean.\n");
    } else {
        human.push_str(&format!("Changes ({}):\n", st.changes.len()));
        for c in &st.changes {
            human.push_str(&format!("  {:<13}{}\n", c.kind.as_str(), c.path));
        }
    }
    ok(
        json!({
            "branch": st.branch,
            "head": st.head.map(|h| h.to_hex()),
            "clean": st.changes.is_empty(),
            "changes": st.changes.iter().map(change_json).collect::<Vec<_>>(),
            "interrupted_restore": st.interrupted_restore.map(|m| json!({
                "target": m.target.to_hex(),
                "backup": m.backup.map(|b| b.to_hex()),
            })),
        }),
        human,
    )
}

fn cmd_diff(repo: &Repo) -> Result<Output> {
    let diffs = diff(repo)?;
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
