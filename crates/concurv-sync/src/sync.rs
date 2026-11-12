//! `fetch`, `push` and `clone`: the repository side of sync (RFC 0003 §2,
//! §5). Sync never changes working files or workspace heads; a workspace
//! integrates fetched work only through `update` or `merge`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use concurv_core::{
    AttestationId, ConcurvError, ErrorCode, ObjectId, OperationId, RandomId, Result, SnapshotId,
};
use concurv_objects::encoding::validate_branch_name;
use concurv_objects::fs::TempSession;
use concurv_objects::{ObjectStore, Operation, RemoteHead, View};
use concurv_workspace::reconcile::{self, Relation};
use concurv_workspace::{Repo, RepoOptions, refname};

use crate::client::{ConnectOptions, Connection, RemoteHeadInfo};
use crate::graph::{objects_to_send, staged_closure};
use crate::proto::{MAX_HAVES, MAX_WANTS};
use crate::remote::{self, RemoteConfig};

/// Options for sync commands.
#[derive(Clone, Debug)]
pub struct SyncOptions {
    /// Connection options.
    pub connect: ConnectOptions,
    /// How often `push` retries after a lost connection.
    pub retries: u32,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            connect: ConnectOptions::default(),
            retries: 2,
        }
    }
}

/// What `fetch` did to one branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchAction {
    /// The local branch already equals the relay's head.
    UpToDate,
    /// A local branch was created at the relay's head.
    Created,
    /// The local branch moved forward to the relay's head.
    FastForwarded,
    /// The local branch has work the relay lacks: `push` it.
    LocalAhead,
    /// Both sides have new work: integrate (`update`/`merge`), then push.
    Diverged,
    /// The relay's head does not descend from the head last seen there
    /// (moved backwards or sideways): nothing follows it.
    Rewound,
    /// The branch is gone from the relay; its remote-tracking reference
    /// is kept.
    Vanished,
    /// The relay's branch name is not acceptable here; ignored.
    InvalidName,
    /// A local branch would collide with an existing name; not created.
    NameCollision,
    /// The relay has the branch, but this repository deleted or renamed its
    /// local branch: only the remote-tracking reference moves.
    NotLocal,
}

impl BranchAction {
    /// Stable lowercase name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Created => "created",
            Self::FastForwarded => "fast_forwarded",
            Self::LocalAhead => "local_ahead",
            Self::Diverged => "diverged",
            Self::Rewound => "rewound",
            Self::Vanished => "vanished",
            Self::InvalidName => "invalid_name",
            Self::NameCollision => "name_collision",
            Self::NotLocal => "not_local",
        }
    }
}

/// Per-branch result of `fetch`.
#[derive(Clone, Debug)]
pub struct BranchReport {
    /// Branch name as the relay sent it.
    pub branch: String,
    /// The relay's head (`None` if the branch vanished).
    pub remote: Option<RemoteHeadInfo>,
    /// Remote-tracking reference before the fetch.
    pub tracked_before: Option<SnapshotId>,
    /// Local branch before.
    pub local_before: Option<SnapshotId>,
    /// Local branch after.
    pub local_after: Option<SnapshotId>,
    /// What happened.
    pub action: BranchAction,
    /// Explanation for `invalid_name`, `name_collision` and `rewound`.
    pub note: Option<String>,
}

/// Result of `fetch`.
#[derive(Clone, Debug)]
pub struct FetchOutcome {
    /// Remote name.
    pub remote: String,
    /// The relay's id.
    pub relay: String,
    /// Objects received.
    pub objects: u32,
    /// Per-branch results, sorted by name.
    pub branches: Vec<BranchReport>,
    /// Per-tag results, sorted by name (empty if the relay has no tags or
    /// does not support them).
    pub tags: Vec<TagReport>,
    /// Operation recorded, if any reference changed.
    pub operation: Option<OperationId>,
}

/// What `fetch` did with one of the relay's tags (ADR 0008 §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagAction {
    /// A local tag was created.
    Created,
    /// The local tag already names the same snapshot.
    Unchanged,
    /// A local tag of that name names another snapshot; it was kept (tags
    /// never move).
    Conflict,
    /// The snapshot is not here (not in the fetched history); not created.
    MissingTarget,
    /// The name or id is not acceptable here; ignored.
    Invalid,
}

impl TagAction {
    /// Stable lowercase name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
            Self::MissingTarget => "missing_target",
            Self::Invalid => "invalid",
        }
    }
}

/// One relay tag after `fetch`.
#[derive(Clone, Debug)]
pub struct TagReport {
    /// Tag name, as the relay sent it.
    pub name: String,
    /// Snapshot it names at the relay (if the id was valid).
    pub target: Option<SnapshotId>,
    /// What happened.
    pub action: TagAction,
    /// Explanation for conflicts and refusals.
    pub note: Option<String>,
}

/// Decide what to do with the relay's tags: never trust them. A tag is
/// created only under a valid, non-colliding new name, for a snapshot this
/// repository has; an existing local tag is never moved.
pub fn plan_fetched_tags(
    store: &ObjectStore,
    local: &BTreeMap<String, SnapshotId>,
    relay: &BTreeMap<String, String>,
) -> (Vec<(String, SnapshotId)>, Vec<TagReport>) {
    let mut create: Vec<(String, SnapshotId)> = Vec::new();
    let mut reports = Vec::new();
    for (name, hex) in relay {
        let mut rep = TagReport {
            name: name.clone(),
            target: None,
            action: TagAction::Invalid,
            note: None,
        };
        let Ok(id) = ObjectId::from_hex(hex) else {
            rep.note = Some("invalid snapshot id".into());
            reports.push(rep);
            continue;
        };
        let target = SnapshotId::from_object_id(id);
        rep.target = Some(target);
        match local.get(name) {
            Some(t) if *t == target => rep.action = TagAction::Unchanged,
            Some(t) => {
                rep.action = TagAction::Conflict;
                rep.note = Some(format!("the local tag names {t}; kept (tags never move)"));
            }
            None => {
                let names: Vec<&String> =
                    local.keys().chain(create.iter().map(|(n, _)| n)).collect();
                if let Err(e) = concurv_workspace::tags::check_new_name(name, names) {
                    rep.note = Some(e.message);
                } else if store.read_snapshot(&target).is_err() {
                    rep.action = TagAction::MissingTarget;
                    rep.note = Some("the snapshot is not in the fetched history".into());
                } else {
                    rep.action = TagAction::Created;
                    create.push((name.clone(), target));
                }
            }
        }
        reports.push(rep);
    }
    (create, reports)
}

/// What `push` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushResult {
    /// The relay's branch moved to the local head.
    Pushed,
    /// The relay already has the local head.
    UpToDate,
    /// The relay is ahead of the local branch: nothing to push (fetch).
    Behind,
}

impl PushResult {
    /// Stable lowercase name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pushed => "pushed",
            Self::UpToDate => "up_to_date",
            Self::Behind => "behind",
        }
    }
}

/// Result of `push`.
#[derive(Clone, Debug)]
pub struct PushOutcome {
    /// Remote name.
    pub remote: String,
    /// Branch.
    pub branch: String,
    /// The relay's head before.
    pub from: Option<SnapshotId>,
    /// The local branch head (now the relay's head if pushed).
    pub head: SnapshotId,
    /// The relay's sequence number for the branch afterwards, if known.
    pub seq: Option<u64>,
    /// What happened.
    pub result: PushResult,
    /// Objects uploaded.
    pub objects: usize,
    /// The relay confirmed an earlier attempt of this push (lost reply).
    pub replayed: bool,
    /// Operation recorded, if the remote-tracking reference changed.
    pub operation: Option<OperationId>,
}

/// Result of `clone`.
#[derive(Debug)]
pub struct CloneOutcome {
    /// Root of the new repository.
    pub root: PathBuf,
    /// The fetch.
    pub fetch: FetchOutcome,
    /// Snapshot checked out into the working copy, if the default branch
    /// exists at the relay.
    pub checked_out: Option<SnapshotId>,
    /// Files written.
    pub written: usize,
}

/// A temp session in the store's `tmp/` holding a staging object store
/// for received objects (RFC 0003 §3.6 quarantine).
struct Staging {
    _session: TempSession,
    store: ObjectStore,
}

impl Staging {
    fn new(repo: &Repo) -> Result<Self> {
        let faults = repo.options().faults.clone();
        let session = {
            let _l = repo.lock()?;
            TempSession::create(repo.store().tmp_dir(), &faults)?
        };
        concurv_objects::store::create_layout(session.dir(), &faults)?;
        let store = ObjectStore::new(session.dir(), faults);
        Ok(Self {
            _session: session,
            store,
        })
    }
}

/// A relay branch name must also be a valid new local name (Git ref rules,
/// portability policy), so it can never alias a local reference.
fn acceptable_name(name: &str) -> Result<()> {
    validate_branch_name(name)?;
    refname::check_new(name, &[])
}

fn haves(view: &View, store: &ObjectStore) -> Vec<SnapshotId> {
    let mut seen = HashSet::new();
    view.branches
        .values()
        .copied()
        .chain(
            view.remotes
                .values()
                .flat_map(|m| m.values().map(|r| r.head)),
        )
        .filter(|s| store.contains(&s.object_id()) && seen.insert(*s))
        .take(MAX_HAVES)
        .collect()
}

fn connect(repo: &Repo, cfg: &RemoteConfig, opts: &SyncOptions) -> Result<Connection> {
    let token = remote::token(repo, &cfg.name)?;
    Connection::open(&cfg.url, &cfg.fingerprint, &token, &opts.connect)
}

/// Exchange advisory intents with the relay (RFC 0006 §2), last in a
/// session. A relay that cannot exchange them (for example an older one) is
/// ignored: intents are advisory. Writing the local cache of the received
/// intents is a local I/O step like any other, and its failure is returned.
fn sync_intents(repo: &Repo, conn: &mut Connection, remote: &str) -> Result<()> {
    let me = repo.device_id().to_hex();
    let submit = conn.welcome().scopes.iter().any(|s| s == "write").then(|| {
        concurv_workspace::coord::local_intents(repo)
            .into_iter()
            .filter(|i| i.device == me)
            .collect()
    });
    match conn.intents(&me, submit) {
        Ok(all) => concurv_workspace::coord::store_remote_intents(repo, remote, &all),
        Err(_) => Ok(()),
    }
}

/// [`sync_intents`] in a push: a push never fails because of the intents
/// cache (after a publish, the publish stands either way; the next
/// fetch or push rewrites the cache).
fn sync_intents_in_push(repo: &Repo, conn: &mut Connection, remote: &str) {
    let _ = sync_intents(repo, conn, remote);
}

/// Download the relay's published heads and the objects they need, then,
/// in one operation, record them as remote-tracking references and
/// fast-forward local branches that are strictly behind (RFC 0003 §5).
/// Divergent local branches stay where they are; nothing is published and
/// no working file or workspace head changes.
///
/// Objects are received into a staging area, verified one by one, checked
/// for graph closure, and only then adopted into the store; if anything is
/// wrong nothing is adopted and no reference changes.
pub fn fetch(repo: &Repo, remote_name: &str, opts: &SyncOptions) -> Result<FetchOutcome> {
    let cfg = remote::get(repo, remote_name)?;
    let mut conn = connect(repo, &cfg, opts)?;
    let relay = conn.welcome().relay.clone();
    let heads = conn.heads()?;
    let store = repo.store();
    let mut reports = Vec::new();
    let mut valid: BTreeMap<String, RemoteHeadInfo> = BTreeMap::new();
    for (b, h) in heads {
        match acceptable_name(&b) {
            Ok(()) => {
                valid.insert(b, h);
            }
            Err(e) => reports.push(BranchReport {
                branch: b,
                remote: Some(h),
                tracked_before: None,
                local_before: None,
                local_after: None,
                action: BranchAction::InvalidName,
                note: Some(e.message),
            }),
        }
    }
    let mut wants: Vec<SnapshotId> = valid
        .values()
        .map(|h| h.head)
        .filter(|h| !store.contains(&h.object_id()))
        .collect();
    wants.sort();
    wants.dedup();
    let mut objects = 0;
    if !wants.is_empty() {
        let (_, op) = repo.oplog().head()?;
        let haves = haves(&op.view, store);
        let staging = Staging::new(repo)?;
        for chunk in wants.chunks(MAX_WANTS) {
            objects += conn.fetch_into(chunk, &haves, &staging.store)?;
        }
        let closure = staged_closure(&staging.store, store, &wants)?;
        if !closure.missing.is_empty() {
            return Err(ConcurvError::new(
                ErrorCode::ObjectMissing,
                format!(
                    "the relay sent an incomplete history ({} objects missing, e.g. {}); nothing was changed",
                    closure.missing.len(),
                    closure.missing[0]
                ),
            ));
        }
        for id in &closure.order {
            store.adopt(&staging.store, id)?;
        }
    }
    // Tags (ADR 0008 §7): an older relay that does not know the request
    // ends the session; then there are no tags, as before.
    let relay_tags = conn.tags(None).unwrap_or_default();
    sync_intents(repo, &mut conn, remote_name)?;
    conn.close();

    let lock = repo.lock()?;
    if !repo.supports_remotes()? {
        return Err(ConcurvError::internal(
            "a remote exists but the repository is not format 2",
        ));
    }
    let (op_id, op) = repo.oplog().head()?;
    let mut view = op.view.clone();
    let tracked_map = view.remotes.get(remote_name).cloned().unwrap_or_default();
    for (b, h) in &valid {
        let local = view.branches.get(b).copied();
        let tracked = tracked_map.get(b).map(|t| t.head);
        let plan = reconcile::plan(store, local, tracked, Some(h.head))?;
        let mut rep = BranchReport {
            branch: b.clone(),
            remote: Some(*h),
            tracked_before: tracked,
            local_before: local,
            local_after: local,
            action: BranchAction::UpToDate,
            note: None,
        };
        if plan.remote_rewound {
            rep.action = BranchAction::Rewound;
            rep.note = Some(format!(
                "the relay's head {} does not descend from {} seen there before; nothing follows it",
                h.head,
                tracked.map(|t| t.to_hex()).unwrap_or_default()
            ));
            reports.push(rep);
            continue;
        }
        view.remotes
            .entry(remote_name.to_owned())
            .or_default()
            .insert(
                b.clone(),
                RemoteHead {
                    head: h.head,
                    seq: h.seq,
                },
            );
        rep.action = match plan.relation {
            Relation::UpToDate => BranchAction::UpToDate,
            Relation::LocalAhead | Relation::LocalOnly => BranchAction::LocalAhead,
            Relation::Diverged { .. } => BranchAction::Diverged,
            Relation::RemoteAhead | Relation::RemoteOnly => BranchAction::FastForwarded,
        };
        if let Some(ff) = plan.fast_forward_local {
            if local.is_none() && tracked.is_some() {
                // Seen at the relay before and missing here: deleted or
                // renamed on this repository (or refused earlier). Only the
                // remote-tracking reference follows the relay.
                rep.action = BranchAction::NotLocal;
                rep.note = Some(format!(
                    "no local branch (deleted or renamed here); recreate it with \
                     `concurv branch create {b} --at remote/{remote_name}/{b}`"
                ));
            } else if local.is_none() {
                let existing: Vec<&String> = view.branches.keys().collect();
                match refname::check_new(b, &existing) {
                    Ok(()) => {
                        view.branches.insert(b.clone(), ff);
                        rep.action = BranchAction::Created;
                        rep.local_after = Some(ff);
                    }
                    Err(e) => {
                        rep.action = BranchAction::NameCollision;
                        rep.note = Some(e.message);
                    }
                }
            } else {
                view.branches.insert(b.clone(), ff);
                rep.local_after = Some(ff);
            }
        }
        reports.push(rep);
    }
    for (b, t) in &tracked_map {
        if !valid.contains_key(b) {
            reports.push(BranchReport {
                branch: b.clone(),
                remote: None,
                tracked_before: Some(t.head),
                local_before: view.branches.get(b).copied(),
                local_after: view.branches.get(b).copied(),
                action: BranchAction::Vanished,
                note: Some("the branch is no longer published at the relay".into()),
            });
        }
    }
    reports.sort_by(|a, b| a.branch.cmp(&b.branch));
    let (new_tags, tag_reports) = plan_fetched_tags(store, &view.tags, &relay_tags);
    if !new_tags.is_empty() {
        repo.upgrade_format(&lock, concurv_workspace::tags::TAGS_FORMAT)?;
        view.tags.extend(new_tags.iter().cloned());
    }
    let operation = if view != op.view {
        let changed: Vec<String> = reports
            .iter()
            .filter(|r| {
                matches!(
                    r.action,
                    BranchAction::Created | BranchAction::FastForwarded
                )
            })
            .map(|r| r.branch.clone())
            .chain(new_tags.iter().map(|(n, _)| format!("tag {n}")))
            .collect();
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: None,
            kind: "fetch".into(),
            description: if changed.is_empty() {
                format!("fetch from {remote_name}")
            } else {
                format!("fetch from {remote_name}: {}", changed.join(", "))
            },
            backup: None,
            undoes: None,
            checked_out: None,
            view,
        };
        Some(repo.oplog().commit(&lock, &op_id, &new_op)?)
    } else {
        None
    };
    Ok(FetchOutcome {
        remote: remote_name.to_owned(),
        relay,
        objects,
        branches: reports,
        tags: tag_reports,
        operation,
    })
}

struct Attempt {
    result: PushResult,
    from: Option<SnapshotId>,
    seq: Option<u64>,
    objects: usize,
    replayed: bool,
}

#[allow(clippy::too_many_arguments)]
fn push_once(
    repo: &Repo,
    cfg: &RemoteConfig,
    branch: &str,
    local: SnapshotId,
    tracked: Option<SnapshotId>,
    request_id: &str,
    sent: &mut Option<(Option<SnapshotId>, Option<AttestationId>)>,
    opts: &SyncOptions,
) -> Result<Attempt> {
    let store = repo.store();
    let mut conn = connect(repo, cfg, opts)?;
    // A previous attempt may have published and lost the reply: ask again
    // with the same request id; the relay answers with the original result.
    if let Some((expected, attestation)) = *sent {
        match conn.publish(request_id, branch, expected, local, attestation) {
            Ok(r) => {
                sync_intents_in_push(repo, &mut conn, &cfg.name);
                conn.close();
                return Ok(Attempt {
                    result: PushResult::Pushed,
                    from: expected,
                    seq: Some(r.seq),
                    objects: 0,
                    replayed: r.replayed,
                });
            }
            Err(e) if e.code == ErrorCode::ObjectMissing => {}
            Err(e) => return Err(e),
        }
    }
    let heads = conn.heads()?;
    let remote = heads.get(branch).copied();
    if let Some(r) = remote
        && !store.contains(&r.head.object_id())
    {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "the relay's {branch} has snapshots this repository has not fetched; nothing was pushed. \
                 Run `concurv fetch`, integrate with `concurv update`, `concurv publish`, then push again"
            ),
        ));
    }
    let plan = reconcile::plan(store, Some(local), tracked, remote.map(|r| r.head))?;
    if plan.remote_rewound {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "the relay's {branch} does not descend from the head last seen there; it was rewound or replaced. \
                 Nothing was pushed; inspect it with `concurv fetch`"
            ),
        ));
    }
    let mut attempt = Attempt {
        result: PushResult::UpToDate,
        from: remote.map(|r| r.head),
        seq: remote.map(|r| r.seq),
        objects: 0,
        replayed: false,
    };
    match plan.relation {
        Relation::UpToDate => {
            sync_intents_in_push(repo, &mut conn, &cfg.name);
            conn.close();
            return Ok(attempt);
        }
        Relation::RemoteAhead => {
            attempt.result = PushResult::Behind;
            sync_intents_in_push(repo, &mut conn, &cfg.name);
            conn.close();
            return Ok(attempt);
        }
        Relation::Diverged { .. } => {
            return Err(ConcurvError::new(
                ErrorCode::Diverged,
                format!(
                    "{branch} has new snapshots both here and at the relay; nothing was pushed. \
                     Run `concurv fetch`, then `concurv update` in a workspace tracking {branch}, \
                     `concurv publish`, and push again"
                ),
            ));
        }
        Relation::LocalAhead | Relation::LocalOnly | Relation::RemoteOnly => {}
    }
    // R4 (RFC 0005 §7): if the relay's policy protects the branch, bring a
    // passing attestation for exactly this head and vouch for it.
    let attestation = match concurv_workspace::policy::protection(
        store,
        branch,
        remote.map(|r| r.head),
    )? {
        concurv_workspace::policy::Protection::None => None,
        concurv_workspace::policy::Protection::Required { policy_id, policy } => {
            let (found, reasons) = concurv_workspace::check::find_authorizing(
                repo, local, branch, policy_id, &policy,
            )?;
            match found {
                Some(a) => Some(a),
                None => {
                    return Err(ConcurvError::new(
                        ErrorCode::PolicyFail,
                        format!(
                            "the relay's {branch} is protected by its policy: pushing {local} needs a \
                             passing attestation ({}). Run `concurv check --branch {branch}`; nothing was pushed",
                            if reasons.is_empty() {
                                "none is recorded".to_owned()
                            } else {
                                reasons.join("; ")
                            }
                        ),
                    ));
                }
            }
        }
    };
    let relay_heads: Vec<SnapshotId> = heads.values().map(|h| h.head).collect();
    let mut send = objects_to_send(store, &[local], &relay_heads)?;
    send.extend(attestation.map(|a| a.object_id()));
    let lacking: HashSet<_> = conn.missing(&send)?.into_iter().collect();
    let upload: Vec<_> = send.into_iter().filter(|i| lacking.contains(i)).collect();
    conn.upload(store, &upload)?;
    if let Some(a) = attestation {
        conn.attest(a)?;
    }
    *sent = Some((attempt.from, attestation));
    let reply = conn.publish(request_id, branch, attempt.from, local, attestation)?;
    sync_intents_in_push(repo, &mut conn, &cfg.name);
    conn.close();
    attempt.result = PushResult::Pushed;
    attempt.seq = Some(reply.seq);
    attempt.objects = upload.len();
    attempt.replayed = reply.replayed;
    Ok(attempt)
}

/// Publish local branch `branch` (default: the one this workspace tracks)
/// at the relay by compare-and-swap, expecting the relay's current head.
/// Only a fast-forward is offered (RFC 0003 §5 rule 3): if the relay has
/// work this repository lacks, nothing is pushed. A lost connection is
/// retried with the same request id, so a publish whose reply was lost is
/// never applied twice.
pub fn push(
    repo: &Repo,
    remote_name: &str,
    branch: Option<&str>,
    opts: &SyncOptions,
) -> Result<PushOutcome> {
    let cfg = remote::get(repo, remote_name)?;
    let (_, op, ws) = repo.current()?;
    let branch = match branch {
        Some(b) => b.to_owned(),
        None => ws.branch.clone().ok_or_else(|| {
            ConcurvError::new(
                ErrorCode::InvalidArgument,
                "this workspace tracks no branch; pass --branch",
            )
        })?,
    };
    let local = op.view.branches.get(&branch).copied().ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::NotFound,
            format!(
                "branch {branch} has no snapshot to push; `concurv save` and `concurv publish` first"
            ),
        )
    })?;
    let tracked = op
        .view
        .remotes
        .get(remote_name)
        .and_then(|m| m.get(&branch))
        .map(|t| t.head);
    let request_id = RandomId::generate()?.to_hex();
    let mut sent = None;
    let mut tries = 0;
    let attempt = loop {
        match push_once(
            repo,
            &cfg,
            &branch,
            local,
            tracked,
            &request_id,
            &mut sent,
            opts,
        ) {
            Err(e) if e.code == ErrorCode::Network && tries < opts.retries => {
                tries += 1;
                sleep(Duration::from_millis(200 * u64::from(tries)));
            }
            other => break other?,
        }
    };
    let mut operation = None;
    if let Some(seq) = attempt.seq
        && attempt.result != PushResult::Behind
    {
        let lock = repo.lock()?;
        let (op_id, op) = repo.oplog().head()?;
        let mut view = op.view.clone();
        let entry = view.remotes.entry(remote_name.to_owned()).or_default();
        if entry.get(&branch).is_none_or(|t| t.seq < seq) {
            entry.insert(branch.clone(), RemoteHead { head: local, seq });
        }
        if view != op.view {
            let new_op = Operation {
                parents: vec![op_id],
                timestamp_ms: repo.now_ms(),
                device_id: repo.device_id(),
                workspace_id: None,
                kind: "push".into(),
                description: format!("push {branch} to {remote_name} at {local} (seq {seq})"),
                backup: None,
                undoes: None,
                checked_out: None,
                view,
            };
            operation = Some(repo.oplog().commit(&lock, &op_id, &new_op)?);
        }
    }
    Ok(PushOutcome {
        remote: remote_name.to_owned(),
        branch,
        from: attempt.from,
        head: local,
        seq: attempt.seq,
        result: attempt.result,
        objects: attempt.objects,
        replayed: attempt.replayed,
        operation,
    })
}

/// What `push --tags` did.
#[derive(Clone, Debug, Default)]
pub struct PushTagsOutcome {
    /// Tags created at the relay: name and snapshot.
    pub created: Vec<(String, SnapshotId)>,
    /// Tags the relay already had at the same snapshot.
    pub existing: usize,
    /// Tags not sent, with the reason (their snapshot is not published at
    /// the relay).
    pub skipped: Vec<(String, String)>,
}

/// Create this repository's tags at the relay (ADR 0008 §7): those whose
/// snapshot the relay has (published history), in one all-or-nothing
/// request. A relay tag of the same name at another snapshot refuses the
/// whole push of tags (`CONCURV_DIVERGED`); tags never move.
pub fn push_tags(repo: &Repo, remote_name: &str, opts: &SyncOptions) -> Result<PushTagsOutcome> {
    let cfg = remote::get(repo, remote_name)?;
    let local = repo.current()?.1.view.tags;
    let mut conn = connect(repo, &cfg, opts)?;
    let relay_tags = conn.tags(None).map_err(|e| {
        ConcurvError::new(
            e.code,
            format!(
                "the relay did not answer the tags request ({}); relays up to \
                 v0.1.0-alpha.3 do not keep tags",
                e.message
            ),
        )
    })?;
    let mut out = PushTagsOutcome::default();
    let mut conflicts = Vec::new();
    let mut candidates: BTreeMap<String, SnapshotId> = BTreeMap::new();
    for (name, target) in &local {
        match relay_tags.get(name) {
            Some(t) if *t == target.to_hex() => out.existing += 1,
            Some(t) => conflicts.push(format!("{name} (relay: {t}, here: {target})")),
            None => {
                candidates.insert(name.clone(), *target);
            }
        }
    }
    if !conflicts.is_empty() {
        conn.close();
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "the relay has tag(s) with these names at other snapshots: {}; tags never move, so \
                 no tag was pushed. Rename or delete the local tag",
                conflicts.join(", ")
            ),
        ));
    }
    let ids: Vec<ObjectId> = candidates.values().map(|t| t.object_id()).collect();
    let lacking: HashSet<ObjectId> = conn.missing(&ids)?.into_iter().collect();
    candidates.retain(|name, target| {
        let ok = !lacking.contains(&target.object_id());
        if !ok {
            out.skipped.push((
                name.clone(),
                "its snapshot is not published at the relay (push its branch first)".into(),
            ));
        }
        ok
    });
    if !candidates.is_empty() {
        conn.tags(Some(candidates.clone()))?;
        out.created = candidates.into_iter().collect();
    }
    conn.close();
    Ok(out)
}

/// Create a repository in `dir` (missing or empty), add remote
/// `remote_name`, fetch, and check out the default branch if the relay
/// has it. If a later step fails, the repository stays (no data to lose)
/// and the error says how to continue.
pub fn clone_repo(
    dir: &Path,
    url: &str,
    fingerprint: &str,
    token: &str,
    remote_name: &str,
    repo_opts: RepoOptions,
    opts: &SyncOptions,
) -> Result<CloneOutcome> {
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| ConcurvError::io(e, "create", dir))?;
    }
    let repo = Repo::init(dir, repo_opts)?;
    let cont = |mut e: ConcurvError| {
        e.message = format!(
            "{} (the repository was created in `{}`; fix the cause and run `concurv fetch` there)",
            e.message,
            dir.display()
        );
        e
    };
    remote::add(&repo, remote_name, url, fingerprint, token).map_err(cont)?;
    let fetch = fetch(&repo, remote_name, opts).map_err(cont)?;
    let (_, op, ws) = repo.current()?;
    let mut checked_out = None;
    let mut written = 0;
    if let Some(b) = ws.branch
        && op.view.branches.contains_key(&b)
    {
        let u = concurv_workspace::update(&repo).map_err(cont)?;
        checked_out = u.head;
        written = u.written;
    }
    Ok(CloneOutcome {
        root: repo.root().to_path_buf(),
        fetch,
        checked_out,
        written,
    })
}
