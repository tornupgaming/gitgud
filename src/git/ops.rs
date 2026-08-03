//! All actual libgit2 work: pure functions from a `Repository` to the plain
//! data types in `mod.rs`. Called only from the worker thread.
//!
//! Conventions: every error becomes a `String` (libgit2 errors via [`g`]) so
//! results cross the channel as owned data. Mutating ops return their
//! status-bar message as `Ok(String)`. Ops taking a `&Path` workdir instead
//! of a `&Repository` shell out to the git CLI -- see [`run_git`] for why.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};

use git2::build::CheckoutBuilder;
use git2::{
    BranchType, DiffOptions, ErrorCode, IndexAddOption, Oid, Repository, RepositoryState, Sort,
    Status, StatusOptions, SubmoduleIgnore, SubmoduleStatus,
};

use super::{
    BranchInfo, ChangeKind, CommitInfo, ConflictFile, ConflictSegment, DiffData, DiffKey, DiffLine,
    FileChange, FileDiff, HistoryPage, LineKind, MergeDirection, MergeInfo, ProgressEvent,
    RefreshData, RepoMeta, Resolution, StepState, SubmoduleInfo, SyncStrategy,
};

/// A sink for live progress from a long-running op. The worker hands each
/// network op one of these; it wraps every event into an `Update::Progress`
/// and wakes the UI. Defined as a trait-object closure so `ops` stays free of
/// the channel and egui types -- it just calls `report(event)`.
pub type Reporter<'a> = &'a mut dyn FnMut(ProgressEvent);

/// Longest console line we keep before flushing it as-is. A subprocess that
/// never emits a newline (or `\r`) can't grow the line buffer without bound --
/// the same defensive cap the diff and highlight passes apply to repo content.
const CONSOLE_LINE_CAP: usize = 8192;

/// Stop collecting diff lines past this point so a pathological diff
/// (generated file, vendored lockfile) cannot freeze the UI thread's render.
const DIFF_LINE_CAP: usize = 20_000;

/// `git2::Error` -> plain `String`, so results cross the thread boundary as
/// owned data. Used everywhere as `.map_err(g)?`.
fn g(e: git2::Error) -> String {
    e.message().to_string()
}

/// Opens the repo enclosing `path`. Bare repos are rejected -- everything
/// downstream assumes a working tree.
pub fn open(path: &Path) -> Result<(Repository, RepoMeta), String> {
    // `discover` walks up from `path` to find the enclosing repo, so the
    // user can pick any subfolder of a worktree.
    let repo = Repository::discover(path).map_err(g)?;
    let workdir = repo
        .workdir()
        .ok_or("bare repositories are not supported")?
        .to_path_buf();
    let name = workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workdir.display().to_string());
    Ok((repo, RepoMeta { name, workdir }))
}

/// Builds the full sidebar snapshot in one pass: HEAD, branches, staged and
/// unstaged lists, merge state. Cheap enough that the app re-runs it after
/// every mutating action rather than tracking state incrementally.
pub fn refresh(repo: &Repository) -> Result<RefreshData, String> {
    let (head, head_detached) = head_label(repo)?;

    let mut locals: Vec<BranchInfo> = Vec::new();
    let mut remotes: Vec<BranchInfo> = Vec::new();
    for entry in repo.branches(None).map_err(g)? {
        let (branch, kind) = entry.map_err(g)?;
        let Some(name) = branch.name().map_err(g)?.map(str::to_string) else {
            continue; // non-utf8 branch name; nothing sane to display
        };
        if kind == BranchType::Remote {
            // Every remote has a symbolic "<remote>/HEAD" entry; it is not a
            // real branch, so hide it.
            if name.ends_with("/HEAD") {
                continue;
            }
            remotes.push(BranchInfo {
                name,
                is_head: false,
                is_remote: true,
                ahead: 0,
                behind: 0,
            });
            continue;
        }
        let (ahead, behind) = match (branch.get().target(), branch.upstream()) {
            (Some(local_oid), Ok(upstream)) => match upstream.get().target() {
                Some(up_oid) => repo.graph_ahead_behind(local_oid, up_oid).map_err(g)?,
                None => (0, 0),
            },
            _ => (0, 0), // no upstream configured
        };
        locals.push(BranchInfo {
            name,
            is_head: branch.is_head(),
            is_remote: false,
            ahead,
            behind,
        });
    }
    locals.sort_by(|a, b| a.name.cmp(&b.name));
    remotes.sort_by(|a, b| a.name.cmp(&b.name));
    locals.extend(remotes);

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(g)?;

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for entry in statuses.iter() {
        let path = String::from_utf8_lossy(entry.path_bytes()).into_owned();
        let s = entry.status();
        // One git status is a bitflag set: a file can be staged-modified AND
        // workdir-modified at once, so it lands in both lists.
        let index_kind = [
            (Status::INDEX_NEW, ChangeKind::Added),
            (Status::INDEX_MODIFIED, ChangeKind::Modified),
            (Status::INDEX_DELETED, ChangeKind::Deleted),
            (Status::INDEX_RENAMED, ChangeKind::Renamed),
            (Status::INDEX_TYPECHANGE, ChangeKind::Typechange),
        ]
        .into_iter()
        .find(|(flag, _)| s.contains(*flag));
        if let Some((_, kind)) = index_kind {
            staged.push(FileChange {
                path: path.clone(),
                kind,
            });
        }
        let wt_kind = [
            (Status::CONFLICTED, ChangeKind::Conflicted),
            (Status::WT_NEW, ChangeKind::Untracked),
            (Status::WT_MODIFIED, ChangeKind::Modified),
            (Status::WT_DELETED, ChangeKind::Deleted),
            (Status::WT_RENAMED, ChangeKind::Renamed),
            (Status::WT_TYPECHANGE, ChangeKind::Typechange),
        ]
        .into_iter()
        .find(|(flag, _)| s.contains(*flag));
        if let Some((_, kind)) = wt_kind {
            unstaged.push(FileChange { path, kind });
        }
    }

    // Submodules are excluded from the status walk above, so their gitlink
    // changes are gathered here instead -- a staged pointer (parent index gitlink
    // differs from HEAD) joins `staged`, an unstaged one (the submodule's own HEAD
    // moved past the recorded commit) joins `unstaged`, both as Submodule rows.
    let submodules = submodules(repo)?;
    for sm in &submodules {
        if sm.staged {
            staged.push(FileChange {
                path: sm.path.clone(),
                kind: ChangeKind::Submodule,
            });
        }
        if sm.new_commits {
            unstaged.push(FileChange {
                path: sm.path.clone(),
                kind: ChangeKind::Submodule,
            });
        }
    }

    staged.sort_by(|a, b| a.path.cmp(&b.path));
    unstaged.sort_by(|a, b| a.path.cmp(&b.path));

    let merging = (repo.state() == RepositoryState::Merge).then(|| {
        let head_short = repo
            .find_reference("MERGE_HEAD")
            .ok()
            .and_then(|r| r.target())
            .map(short_id)
            .or_else(|| {
                // Fallback when the ref lookup fails: MERGE_HEAD is a plain
                // file holding one full hex id per merged head.
                std::fs::read_to_string(repo.path().join("MERGE_HEAD"))
                    .ok()
                    .map(|s| s.trim().chars().take(7).collect())
            })
            .unwrap_or_default();
        let msg = std::fs::read_to_string(repo.path().join("MERGE_MSG"))
            .map(|s| {
                s.lines()
                    .filter(|l| !l.starts_with('#'))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        MergeInfo { head_short, msg }
    });

    Ok(RefreshData {
        head,
        head_detached,
        branches: locals,
        staged,
        unstaged,
        merging,
        submodules,
    })
}

/// Snapshots every submodule's state from its `SubmoduleStatus` flags. Paths
/// that escape the worktree (a crafted `.gitmodules`) are dropped: they must
/// not reach the open-as-tab join or the `git submodule update` path arg.
fn submodules(repo: &Repository) -> Result<Vec<SubmoduleInfo>, String> {
    let mut out = Vec::new();
    for sm in repo.submodules().map_err(g)? {
        let path = sm.path().to_string_lossy().into_owned();
        if !is_confined(&path) {
            continue;
        }
        let Ok(name) = sm.name() else {
            continue; // non-utf8 name; submodule_status needs the name
        };
        // `Ignore::None` reports dirty work tree and untracked files inside the
        // submodule; the config default would hide them.
        let st = repo
            .submodule_status(name, SubmoduleIgnore::None)
            .map_err(g)?;
        out.push(SubmoduleInfo {
            path,
            head_short: sm.workdir_id().map(short_id).unwrap_or_default(),
            initialized: !st.contains(SubmoduleStatus::WD_UNINITIALIZED),
            dirty: st.intersects(
                SubmoduleStatus::WD_INDEX_MODIFIED
                    | SubmoduleStatus::WD_WD_MODIFIED
                    | SubmoduleStatus::WD_UNTRACKED,
            ),
            new_commits: st.contains(SubmoduleStatus::WD_MODIFIED),
            staged: st.contains(SubmoduleStatus::INDEX_MODIFIED),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// `git submodule update [--init] --recursive` (`path: None` = all). Network op
/// run off the worker thread. The path comes from our own confined snapshot,
/// but is re-validated here so a future caller can't slip a flag or an escape
/// through to the CLI.
pub fn update_submodules(
    workdir: &Path,
    path: Option<&str>,
    init: bool,
    report: Reporter<'_>,
) -> Result<String, String> {
    if let Some(p) = path
        && (p.starts_with('-') || !is_confined(p))
    {
        return Err(format!("invalid submodule path: {p}"));
    }
    let mut args = vec!["submodule", "update"];
    if init {
        args.push("--init");
    }
    args.push("--recursive");
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    run_step(
        workdir,
        &args,
        "git submodule update failed",
        "submodules",
        "SUBMODULE UPDATE — init, recursive",
        report,
    )?;
    Ok(match path {
        Some(p) => format!("submodule updated — {p}"),
        None => "submodules updated".to_string(),
    })
}

/// Current branch name, short id when detached, or the future branch name
/// when the repo has no commits yet ("unborn" HEAD).
fn head_label(repo: &Repository) -> Result<(String, bool), String> {
    match repo.head() {
        Ok(head) => {
            if repo.head_detached().map_err(g)? {
                let commit = head.peel_to_commit().map_err(g)?;
                Ok((short_id(commit.id()), true))
            } else {
                Ok((head.shorthand().unwrap_or("HEAD").to_string(), false))
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            // New repo: HEAD points at a branch that has no commit yet.
            let head_ref = repo.find_reference("HEAD").map_err(g)?;
            let target = head_ref
                .symbolic_target()
                .map_err(g)?
                .unwrap_or("refs/heads/main");
            Ok((target.trim_start_matches("refs/heads/").to_string(), false))
        }
        Err(e) => Err(g(e)),
    }
}

fn short_id(oid: Oid) -> String {
    oid.to_string()[..7].to_string()
}

/// One page of up to `limit` commits from HEAD, newest first, skipping the
/// first `skip`. An unborn HEAD (no commits yet) is an empty ended page,
/// not an error.
pub fn history(repo: &Repository, skip: usize, limit: usize) -> Result<HistoryPage, String> {
    match repo.head() {
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            return Ok(HistoryPage {
                skip,
                commits: Vec::new(),
                reached_end: true,
            });
        }
        Err(e) => return Err(g(e)),
        Ok(_) => {}
    }

    // Branch tips, computed once per page rather than per commit.
    let mut decorations: HashMap<Oid, Vec<String>> = HashMap::new();
    for entry in repo.branches(None).map_err(g)? {
        let (branch, _) = entry.map_err(g)?;
        let (Ok(Some(name)), Some(oid)) = (branch.name(), branch.get().target()) else {
            continue;
        };
        if name.ends_with("/HEAD") {
            continue;
        }
        decorations.entry(oid).or_default().push(name.to_string());
    }

    let mut walk = repo.revwalk().map_err(g)?;
    // TOPOLOGICAL breaks ties between commits sharing the same second --
    // plain TIME shows those in arbitrary order.
    walk.set_sorting(Sort::TIME | Sort::TOPOLOGICAL)
        .map_err(g)?;
    walk.push_head().map_err(g)?;

    let mut commits = Vec::new();
    // Walk one past `limit`: if that extra commit exists there are more pages.
    let mut reached_end = true;
    for oid in walk.skip(skip) {
        let oid = oid.map_err(g)?;
        if commits.len() == limit {
            reached_end = false;
            break;
        }
        let commit = repo.find_commit(oid).map_err(g)?;
        let author = commit.author();
        commits.push(CommitInfo {
            id: oid.to_string(),
            short: short_id(oid),
            summary: String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default())
                .into_owned(),
            author: String::from_utf8_lossy(author.name_bytes()).into_owned(),
            email: String::from_utf8_lossy(author.email_bytes()).into_owned(),
            time: commit.time().seconds(),
            is_merge: commit.parent_count() > 1,
            refs: decorations.get(&oid).cloned().unwrap_or_default(),
            parents: commit.parent_ids().map(|p| p.to_string()).collect(),
        });
    }
    Ok(HistoryPage {
        skip,
        commits,
        reached_end,
    })
}

/// Mutable accumulator shared by the four `foreach` callbacks. Each closure
/// needs `&mut` access, but Rust forbids two closures holding `&mut` to the
/// same value -- so they share a `&RefCell` and borrow mutably one at a time.
#[derive(Default)]
struct DiffState {
    files: Vec<FileDiff>,
    adds: usize,
    dels: usize,
    line_count: usize,
    truncated: bool,
}

/// Builds the diff for any `DiffKey` except `Conflict`. Output stops at
/// [`DIFF_LINE_CAP`]; a truncated diff is still `Ok`, with a marker line
/// appended so the UI can say so.
pub fn diff(repo: &Repository, key: &DiffKey) -> Result<DiffData, String> {
    let diff = match key {
        DiffKey::Workdir {
            path,
            staged: false,
        } => {
            let mut opts = DiffOptions::new();
            opts.pathspec(path)
                .include_untracked(true)
                .show_untracked_content(true)
                .recurse_untracked_dirs(true)
                .context_lines(3);
            repo.diff_index_to_workdir(None, Some(&mut opts))
                .map_err(g)?
        }
        DiffKey::Workdir { path, staged: true } => {
            let mut opts = DiffOptions::new();
            opts.pathspec(path).context_lines(3);
            // Unborn HEAD has no tree; diffing index against "nothing" shows
            // the very first staged files as pure additions.
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))
                .map_err(g)?
        }
        DiffKey::Commit { id } => {
            let oid = Oid::from_str(id).map_err(g)?;
            let commit = repo.find_commit(oid).map_err(g)?;
            let new_tree = commit.tree().map_err(g)?;
            // Root commit has no parent: diff against the empty tree.
            let old_tree = match commit.parent(0) {
                Ok(parent) => Some(parent.tree().map_err(g)?),
                Err(_) => None,
            };
            repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), None)
                .map_err(g)?
        }
        // Conflicted files go through `load_conflict`, never the diff path.
        DiffKey::Conflict { .. } => return Err("conflicted file has no diff".to_string()),
    };
    collect_diff(&diff)
}

/// Flattens a libgit2 `Diff` into the UI's `DiffData`, capping output at
/// [`DIFF_LINE_CAP`]. Submodule (gitlink) deltas come through as the two
/// `Subproject commit <id>` lines libgit2 emits for them.
fn collect_diff(diff: &git2::Diff<'_>) -> Result<DiffData, String> {
    let state = RefCell::new(DiffState::default());

    let mut file_cb = |delta: git2::DiffDelta<'_>, _progress: f32| -> bool {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        state.borrow_mut().files.push(FileDiff {
            path,
            adds: 0,
            dels: 0,
            binary: false,
            lines: Vec::new(),
        });
        true
    };
    let mut binary_cb = |_delta: git2::DiffDelta<'_>, _bin: git2::DiffBinary<'_>| -> bool {
        if let Some(file) = state.borrow_mut().files.last_mut() {
            file.binary = true;
        }
        true
    };
    let mut hunk_cb = |_delta: git2::DiffDelta<'_>, hunk: git2::DiffHunk<'_>| -> bool {
        let mut s = state.borrow_mut();
        if s.line_count >= DIFF_LINE_CAP {
            return truncate(&mut s);
        }
        let text = String::from_utf8_lossy(hunk.header())
            .trim_end_matches(['\n', '\r'])
            .to_string();
        push_line(
            &mut s,
            DiffLine {
                kind: LineKind::Hunk,
                old_no: 0,
                new_no: 0,
                text,
            },
        );
        true
    };
    let mut line_cb = |_delta: git2::DiffDelta<'_>,
                       _hunk: Option<git2::DiffHunk<'_>>,
                       line: git2::DiffLine<'_>|
     -> bool {
        let mut s = state.borrow_mut();
        if s.line_count >= DIFF_LINE_CAP {
            return truncate(&mut s);
        }
        let kind = match line.origin() {
            '+' => LineKind::Add,
            '-' => LineKind::Del,
            ' ' => LineKind::Ctx,
            _ => return true, // file/hunk header origins; already handled
        };
        match kind {
            LineKind::Add => s.adds += 1,
            LineKind::Del => s.dels += 1,
            _ => {}
        }
        if let Some(file) = s.files.last_mut() {
            match kind {
                LineKind::Add => file.adds += 1,
                LineKind::Del => file.dels += 1,
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(line.content())
            .trim_end_matches(['\n', '\r'])
            .to_string();
        push_line(
            &mut s,
            DiffLine {
                kind,
                old_no: line.old_lineno().unwrap_or(0),
                new_no: line.new_lineno().unwrap_or(0),
                text,
            },
        );
        true
    };

    let result = diff.foreach(
        &mut file_cb,
        Some(&mut binary_cb),
        Some(&mut hunk_cb),
        Some(&mut line_cb),
    );
    let state = state.into_inner();
    match result {
        // Returning `false` from a callback makes foreach report an error;
        // when *we* aborted (cap reached) the partial diff is still valid.
        Err(e) if !state.truncated => Err(g(e)),
        _ => Ok(DiffData {
            files: state.files,
            adds: state.adds,
            dels: state.dels,
        }),
    }
}

fn push_line(s: &mut DiffState, line: DiffLine) {
    s.line_count += 1;
    if let Some(file) = s.files.last_mut() {
        file.lines.push(line);
    }
}

/// Marks the diff truncated (once) and tells `foreach` to stop.
fn truncate(s: &mut DiffState) -> bool {
    if !s.truncated {
        s.truncated = true;
        if let Some(file) = s.files.last_mut() {
            file.lines.push(DiffLine {
                kind: LineKind::Hunk,
                old_no: 0,
                new_no: 0,
                text: format!("… diff truncated at {DIFF_LINE_CAP} lines …"),
            });
        }
    }
    false
}

/// Stages one path, or everything when `path` is `None`. Handles deletions
/// too: a path missing from disk is recorded as removed.
pub fn stage(repo: &Repository, path: Option<&str>) -> Result<String, String> {
    let mut index = repo.index().map_err(g)?;
    let label = match path {
        Some(path) => {
            let rel = Path::new(path);
            let on_disk = repo
                .workdir()
                .map(|w| w.join(rel).exists())
                .unwrap_or(false);
            if on_disk {
                index.add_path(rel).map_err(g)?;
            } else {
                // Staging a deletion: the file is gone from disk, so record
                // its removal in the index.
                index.remove_path(rel).map_err(g)?;
            }
            path.to_string()
        }
        None => {
            // add_all stages new + modified files; update_all additionally
            // records deletions of already-tracked files.
            index
                .add_all(["*"], IndexAddOption::DEFAULT, None)
                .map_err(g)?;
            index.update_all(["*"], None).map_err(g)?;
            "all changes".to_string()
        }
    };
    index.write().map_err(g)?;
    Ok(format!("staged {label}"))
}

/// `git restore --staged` equivalent for one path or (`None`) everything.
/// Works on an unborn HEAD, where there is no commit to reset to.
pub fn unstage(repo: &Repository, path: Option<&str>) -> Result<String, String> {
    let label = path.unwrap_or("all changes").to_string();
    let pathspec = path.unwrap_or("*");
    match repo.head() {
        Ok(head) => {
            // Same as `git restore --staged`: reset index entries to HEAD.
            let head_obj = head.peel_to_commit().map_err(g)?.into_object();
            repo.reset_default(Some(&head_obj), [pathspec]).map_err(g)?;
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            // No commit to reset to: unstaging just removes index entries.
            let mut index = repo.index().map_err(g)?;
            index.remove_all([pathspec], None).map_err(g)?;
            index.write().map_err(g)?;
        }
        Err(e) => return Err(g(e)),
    }
    Ok(format!("unstaged {label}"))
}

/// `git restore <path>` + `git clean` rolled together: reset the working tree
/// to the index, which discards unstaged edits, restores deleted tracked
/// files, and (because the unstaged list shows them) deletes untracked files.
/// Staged changes are untouched.
pub fn discard(repo: &Repository, path: Option<&str>) -> Result<String, String> {
    let mut co = CheckoutBuilder::new();
    co.force().remove_untracked(true);
    if let Some(path) = path {
        co.path(path);
    }
    // `None` index = the repo's own index. Ignored files are kept:
    // `remove_ignored` stays off.
    repo.checkout_index(None, Some(&mut co)).map_err(g)?;
    Ok(format!(
        "discarded {}",
        path.unwrap_or("all unstaged changes")
    ))
}

/// Commits the index. Inside a merge (MERGE_HEAD exists) the result is a
/// merge commit with MERGE_HEAD as second parent and the merge state is
/// cleared; outside one, an index identical to HEAD is refused.
pub fn commit(repo: &Repository, message: &str) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("commit message is empty".to_string());
    }
    // Reads user.name / user.email from git config; errors if unset.
    let sig = repo.signature().map_err(g)?;
    let mut index = repo.index().map_err(g)?;
    let tree_id = index.write_tree().map_err(g)?;
    let tree = repo.find_tree(tree_id).map_err(g)?;

    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().map_err(g)?),
        Err(e) if e.code() == ErrorCode::UnbornBranch => None,
        Err(e) => return Err(g(e)),
    };
    let merge_head = match repo.find_reference("MERGE_HEAD") {
        Ok(r) => {
            let oid = r.target().ok_or("MERGE_HEAD is not a direct reference")?;
            Some(repo.find_commit(oid).map_err(g)?)
        }
        Err(_) => None,
    };
    // A merge resolved entirely to HEAD's content is still a legal merge
    // commit (it records the second parent), so the empty-commit guard only
    // applies outside a merge.
    if merge_head.is_none()
        && let Some(parent) = &parent
        && parent.tree_id() == tree_id
    {
        return Err("nothing staged to commit".to_string());
    }
    let parents: Vec<&git2::Commit<'_>> = parent.iter().chain(merge_head.iter()).collect();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(g)?;
    if merge_head.is_some() {
        // Drop MERGE_HEAD/MERGE_MSG so the repo leaves the merge state.
        repo.cleanup_state().map_err(g)?;
    }
    Ok(format!("committed {}", short_id(oid)))
}

/// Switches HEAD to `branch`, first creating a local tracking branch when
/// given a remote name. Refuses (with libgit2's message) rather than
/// overwrite local modifications.
pub fn checkout(repo: &Repository, branch: &str) -> Result<String, String> {
    // "origin/feature" -> create/find local "feature" tracking it.
    let local = match repo.find_branch(branch, BranchType::Remote) {
        Ok(remote) => {
            let local_name = branch
                .split_once('/')
                .map(|(_, rest)| rest)
                .unwrap_or(branch);
            if repo.find_branch(local_name, BranchType::Local).is_err() {
                let tip = remote.get().peel_to_commit().map_err(g)?;
                let mut created = repo.branch(local_name, &tip, false).map_err(g)?;
                created.set_upstream(Some(branch)).map_err(g)?;
            }
            local_name.to_string()
        }
        Err(_) => branch.to_string(),
    };

    let obj = repo.revparse_single(&local).map_err(g)?;
    // `safe()` refuses to overwrite local modifications; libgit2's error
    // message ("1 conflict prevents checkout") is descriptive, pass it on.
    repo.checkout_tree(&obj, Some(CheckoutBuilder::new().safe()))
        .map_err(g)?;
    repo.set_head(&format!("refs/heads/{local}")).map_err(g)?;
    // The switch may have moved submodule pointers; resync them locally so a
    // stale pointer doesn't leave the worktree dirty and block the next flow.
    if let Some(workdir) = repo.workdir() {
        sync_submodules(workdir)?;
    }
    Ok(format!("switched to {local}"))
}

/// Hard-resets the worktree for "Discard & continue": drops tracked staged +
/// unstaged changes and resets dirty submodules to their recorded commits,
/// leaving untracked files alone. Runs as the first step of a checkout/merge
/// it unblocks so the reset and the switch land atomically. `--no-fetch` keeps
/// the submodule reset local. Called only from the worker.
pub fn reset_hard(workdir: &Path) -> Result<(), String> {
    run_git(workdir, &["reset", "--hard", "HEAD"], "git reset failed")?;
    run_git(
        workdir,
        &[
            "submodule",
            "update",
            "--recursive",
            "--force",
            "--no-fetch",
        ],
        "git submodule update failed",
    )?;
    Ok(())
}

/// `git submodule update --recursive --no-fetch` after a checkout/merge moved
/// submodule pointers, so the worktree isn't left dirty. No-op when the repo
/// has no submodules. `--no-fetch` keeps it local: a recorded commit missing
/// locally just leaves the submodule at its old commit for the user to update
/// from the SUBMODULES panel (which fetches).
fn sync_submodules(workdir: &Path) -> Result<(), String> {
    if !workdir.join(".gitmodules").exists() {
        return Ok(());
    }
    run_git(
        workdir,
        &["submodule", "update", "--recursive", "--no-fetch"],
        "git submodule update failed",
    )?;
    Ok(())
}

/// Creates a local branch `name` at `start_point` (a commit id or an existing
/// branch name) and, with `checkout`, switches HEAD to it. Refuses to clobber
/// an existing branch (`force = false`); libgit2 rejects malformed names.
pub fn create_branch(
    repo: &Repository,
    name: &str,
    start_point: &str,
    checkout: bool,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("branch name is empty".to_string());
    }
    // `start_point` is supplied by the UI from the repo's own history/refs
    // (a commit id or branch name), never free-form user text.
    let commit = repo
        .revparse_single(start_point)
        .map_err(g)?
        .peel_to_commit()
        .map_err(g)?;
    // `force = false`: refuse rather than move a branch that already exists.
    // Invalid ref names are rejected here with libgit2's message.
    repo.branch(name, &commit, false).map_err(g)?;
    if checkout {
        // The new branch points at `commit`, so check out that tree. `safe()`
        // refuses to overwrite local modifications, same as `checkout`.
        repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))
            .map_err(g)?;
        repo.set_head(&format!("refs/heads/{name}")).map_err(g)?;
        return Ok(format!("created {name} and switched to it"));
    }
    Ok(format!("created branch {name}"))
}

/// Inspects a merge/pull's combined output. Returns `Some(n)` — the count of
/// conflicted files — when the run stopped on merge conflicts (a recoverable
/// state, the repo is now mid-merge), or `None` when it didn't conflict.
fn conflict_count(stdout: &str, stderr: &str) -> Option<usize> {
    let conflicted = stdout.contains("CONFLICT")
        || stderr.contains("CONFLICT")
        || stdout.contains("Automatic merge failed")
        || stderr.contains("Automatic merge failed");
    if !conflicted {
        return None;
    }
    Some(
        stdout
            .lines()
            .chain(stderr.lines())
            .filter(|l| l.starts_with("CONFLICT"))
            .count(),
    )
}

/// Merges `branch` via the git CLI so the user's merge config, strategy, and
/// hooks all apply. `IntoCurrent` merges `branch` into HEAD; `IntoSelected`
/// checks out `branch` first and merges the previously-current branch into it,
/// finishing on `branch`. A conflicted merge returns `Ok` — the repo is now
/// legitimately mid-merge; only a merge that never started is `Err`. On a
/// clean merge submodule pointers are resynced so a moved pointer doesn't
/// leave the worktree dirty; on conflict the resync waits until the user
/// commits the resolution.
pub fn merge(workdir: &Path, branch: &str, direction: MergeDirection) -> Result<String, String> {
    // Branch names reaching here come from libgit2's ref enumeration, which
    // forbids a leading '-'. Re-check so this value can never be parsed as a
    // git flag if a future caller is less careful.
    if branch.starts_with('-') {
        return Err(format!("invalid branch name: {branch}"));
    }
    let step = match direction {
        MergeDirection::IntoCurrent => run_merge(workdir, branch)?,
        MergeDirection::IntoSelected => {
            // Remember where we are so we merge the original branch in, then
            // switch onto the selected branch (the flow's preconditions
            // guarantee a clean worktree, so this won't clobber anything).
            let original = current_branch(workdir)?;
            run_git(workdir, &["checkout", branch], "git checkout failed")?;
            run_merge(workdir, &original)?
        }
    };
    match step {
        IntegrateStep::Done(status) => {
            sync_submodules(workdir)?;
            Ok(status)
        }
        // Conflicts: the repo is mid-merge on the selected branch and the
        // follow-up Refresh surfaces them. Submodules resync on commit.
        IntegrateStep::Stopped(status) => Ok(status),
    }
}

/// Runs one `git merge <branch>` and classifies the result: a clean merge is
/// `Done`, a conflicted one is `Stopped` (repo now mid-merge), anything else
/// is a genuine `Err`.
fn run_merge(workdir: &Path, branch: &str) -> Result<IntegrateStep, String> {
    let output = git_output(workdir, &["merge", branch])?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status.success() {
        // First stdout line is git's summary: "Merge made by the 'ort'
        // strategy." / "Already up to date." / "Updating abc..def" (ff).
        return Ok(IntegrateStep::Done(
            stdout
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| "merge complete".to_string()),
        ));
    }
    if let Some(n) = conflict_count(&stdout, &stderr) {
        return Ok(IntegrateStep::Stopped(match n {
            0 => "merge conflicts — resolve and commit".to_string(),
            n => format!("merge conflicts in {n} file(s) — resolve and commit"),
        }));
    }
    // Genuine failure: unknown branch, dirty worktree, already merging, ...
    Err(last_nonempty(&stderr, &stdout, "git merge failed"))
}

/// Current branch name via the CLI. Errors on a detached HEAD — the merge
/// flow gates that out UI-side, but `IntoSelected` needs a branch to merge in.
fn current_branch(workdir: &Path) -> Result<String, String> {
    let (stdout, _) = run_git(
        workdir,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "git rev-parse failed",
    )?;
    let name = stdout.trim();
    if name.is_empty() || name == "HEAD" {
        return Err("cannot merge: HEAD is detached".to_string());
    }
    Ok(name.to_string())
}

pub fn abort_merge(workdir: &Path) -> Result<String, String> {
    run_git(workdir, &["merge", "--abort"], "git merge --abort failed")?;
    Ok("merge aborted".to_string())
}

/// Classifies one conflicted file for the resolution UI: deleted on either
/// side, binary (git's early-NUL heuristic), or text parsed into marker
/// segments.
pub fn load_conflict(repo: &Repository, path: &str) -> Result<ConflictFile, String> {
    let conflict = find_conflict(&repo.index().map_err(g)?, path)?;
    if conflict.our.is_none() {
        return Ok(ConflictFile::DeletedByUs);
    }
    if conflict.their.is_none() {
        return Ok(ConflictFile::DeletedByThem);
    }
    let blob_binary = [&conflict.our, &conflict.their].into_iter().any(|e| {
        e.as_ref()
            .and_then(|e| repo.find_blob(e.id).ok())
            .is_some_and(|b| b.is_binary())
    });
    let bytes = std::fs::read(workdir_path(repo, path)?).map_err(|e| e.to_string())?;
    // Same heuristic as git: a NUL early in the file means binary.
    if blob_binary || bytes.iter().take(8000).any(|&b| b == 0) {
        return Ok(ConflictFile::Binary);
    }
    let text = String::from_utf8_lossy(&bytes);
    let (segments, ours_label, theirs_label) = parse_conflict(&text);
    Ok(ConflictFile::Text {
        ours_label,
        theirs_label,
        segments,
        trailing_newline: text.ends_with('\n'),
    })
}

/// Splits a file holding `<<<<<<<` markers into context runs and conflict
/// blocks. Returns the segments plus the ours/theirs labels from the first
/// block (they are identical across blocks); defaults "OURS"/"THEIRS".
fn parse_conflict(text: &str) -> (Vec<ConflictSegment>, String, String) {
    let lines: Vec<&str> = text.lines().collect();
    let mut segments = Vec::new();
    let mut context: Vec<String> = Vec::new();
    let mut ours_label = String::new();
    let mut theirs_label = String::new();
    let mut i = 0;
    while i < lines.len() {
        if marker_label(lines[i], "<<<<<<<").is_some()
            && let Some((segment, ol, tl, next)) = scan_block(&lines, i)
        {
            if !context.is_empty() {
                segments.push(ConflictSegment::Context(std::mem::take(&mut context)));
            }
            if ours_label.is_empty() {
                ours_label = ol;
            }
            if theirs_label.is_empty() {
                theirs_label = tl;
            }
            segments.push(segment);
            i = next;
        } else {
            // Includes dangling markers from blocks that never close:
            // malformed input degrades to context instead of erroring.
            context.push(lines[i].to_string());
            i += 1;
        }
    }
    if !context.is_empty() {
        segments.push(ConflictSegment::Context(context));
    }
    if ours_label.is_empty() {
        ours_label = "OURS".to_string();
    }
    if theirs_label.is_empty() {
        theirs_label = "THEIRS".to_string();
    }
    (segments, ours_label, theirs_label)
}

/// One conflict block opening at `lines[start]`. Returns the segment, both
/// labels, and the index just past the closing `>>>>>>>` -- or None when the
/// block never closes (the caller then degrades the lines to context).
fn scan_block(lines: &[&str], start: usize) -> Option<(ConflictSegment, String, String, usize)> {
    let ours_label = marker_label(lines[start], "<<<<<<<")?.to_string();
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    // 0 = ours, 1 = base (diff3/zdiff3 `|||||||` section, discarded),
    // 2 = theirs.
    let mut section = 0;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        match section {
            0 | 1 => {
                if *line == "=======" {
                    section = 2;
                } else if section == 0 && marker_label(line, "|||||||").is_some() {
                    section = 1;
                } else if section == 0 {
                    ours.push((*line).to_string());
                }
                // section 1: base lines are consumed but never kept.
            }
            _ => {
                if let Some(theirs_label) = marker_label(line, ">>>>>>>") {
                    return Some((
                        ConflictSegment::Conflict { ours, theirs },
                        ours_label,
                        theirs_label.to_string(),
                        i + 1,
                    ));
                }
                theirs.push((*line).to_string());
            }
        }
    }
    None
}

/// `">>>>>>> feature"` -> `Some("feature")`; None when the line is not this
/// marker. Git always writes a space after the 7 marker chars (only the
/// `=======` separator stands alone, matched exactly elsewhere).
fn marker_label<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.strip_prefix(marker)?.strip_prefix(' ').map(str::trim)
}

/// Applies a `Resolution` to the working tree and stages the result, which
/// clears the path's conflict stages from the index. Choosing a deleted
/// side deletes the file.
pub fn resolve(repo: &Repository, path: &str, resolution: &Resolution) -> Result<String, String> {
    let mut index = repo.index().map_err(g)?;
    let rel = Path::new(path);
    let file = workdir_path(repo, path)?;
    let msg = match resolution {
        Resolution::Content(text) => {
            std::fs::write(&file, text).map_err(|e| e.to_string())?;
            // add_path also clears the conflict stages for this path.
            index.add_path(rel).map_err(g)?;
            format!("resolved {path}")
        }
        Resolution::Ours | Resolution::Theirs => {
            let conflict = find_conflict(&index, path)?;
            let (side, label) = match resolution {
                Resolution::Ours => (&conflict.our, "ours"),
                _ => (&conflict.their, "theirs"),
            };
            match side {
                Some(entry) => {
                    let blob = repo.find_blob(entry.id).map_err(g)?;
                    std::fs::write(&file, blob.content()).map_err(|e| e.to_string())?;
                    index.add_path(rel).map_err(g)?;
                    format!("resolved {path} ({label})")
                }
                None => {
                    // The chosen side deleted the file: make the deletion win.
                    if file.exists() {
                        std::fs::remove_file(&file).map_err(|e| e.to_string())?;
                    }
                    index.remove_path(rel).map_err(g)?;
                    format!("resolved {path} — deleted")
                }
            }
        }
    };
    index.write().map_err(g)?;
    Ok(msg)
}

fn entry_is(entry: &Option<git2::IndexEntry>, path: &[u8]) -> bool {
    entry.as_ref().is_some_and(|e| e.path == path)
}

fn find_conflict(index: &git2::Index, path: &str) -> Result<git2::IndexConflict, String> {
    let p = path.as_bytes();
    index
        .conflicts()
        .map_err(g)?
        .filter_map(Result::ok)
        .find(|c| entry_is(&c.our, p) || entry_is(&c.their, p) || entry_is(&c.ancestor, p))
        .ok_or_else(|| "not conflicted".to_string())
}

/// Repo-relative `/`-separated path -> absolute workdir path (`join` accepts
/// forward slashes on Windows). Paths come from the repo's own index, which a
/// crafted repo controls: reject `..`/absolute components, because `join`
/// would otherwise escape the worktree (an absolute right operand *replaces*
/// the base) and `resolve()` writes/deletes at the result.
fn workdir_path(repo: &Repository, path: &str) -> Result<std::path::PathBuf, String> {
    if !is_confined(path) {
        return Err(format!("refusing path outside the worktree: {path}"));
    }
    Ok(repo
        .workdir()
        .ok_or("bare repositories are not supported")?
        .join(path))
}

/// True when every component of `path` is a plain name -- no `..` and no
/// absolute root. Repo-controlled paths (index entries, `.gitmodules`) must
/// pass this before they reach the filesystem or the git CLI, because `join`
/// lets an absolute or `..` operand escape the worktree.
fn is_confined(path: &str) -> bool {
    use std::path::Component;
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// Repo-relative paths of every *initialized* submodule, read in-process via
/// libgit2. Uninitialized submodules (no local clone to fetch into) and paths
/// that would escape the worktree (a crafted `.gitmodules`) are dropped here,
/// before any reach a `git -C` invocation.
///
/// This is the same enumeration `refresh` already does, and it is deliberately
/// *not* `git submodule status`: that CLI walks every submodule serially and
/// runs `git describe` in each, which on a repo with many submodules stalls for
/// seconds (and spawns a process storm on Windows). The worker computes this
/// before it spawns the fetch thread, so the modal can show every row at once.
pub fn initialized_submodule_paths(repo: &Repository) -> Vec<String> {
    let Ok(subs) = repo.submodules() else {
        return Vec::new();
    };
    let workdir = repo.workdir();
    subs.into_iter()
        .filter_map(|sm| {
            let path = sm.path().to_string_lossy().into_owned();
            if !is_confined(&path) {
                return None;
            }
            let name = sm.name().ok()?;
            let st = repo.submodule_status(name, SubmoduleIgnore::None).ok()?;
            if st.contains(SubmoduleStatus::WD_UNINITIALIZED) {
                return None;
            }
            // The submodule must actually be checked out on disk: `git -C` into
            // a missing directory aborts with "cannot change to ...". This guards
            // against a registered-but-absent submodule reaching a fetch.
            workdir?.join(&path).is_dir().then_some(path)
        })
        .collect()
}

/// Fetches the superproject and every initialized submodule *together* across
/// one bounded thread pool. Network op -- the worker runs it on a throwaway
/// thread so it never blocks local jobs. Streams git's per-ref output to
/// `report` so the progress modal shows the remotes answering.
///
/// There is no ordering between the tasks: each submodule fetches its own
/// remote independently of the superproject, so fetching the top level first
/// would only add latency. The superproject fetch runs with
/// `--recurse-submodules=no` because we drive the submodule fetches ourselves;
/// git's built-in `on-demand` recursion would do the same work serially.
///
/// The wrinkle: `report` is a single-thread `&mut dyn FnMut`, so the worker
/// threads can't touch it. They emit [`ProgressEvent`]s into an mpsc channel
/// instead; this thread (the one that already owns `report`) drains the channel
/// and forwards each event. So the reporter stays single-threaded while the
/// fetches run in parallel -- no locks on the reporter.
pub fn fetch(workdir: &Path, subs: &[String], report: Reporter<'_>) -> Result<String, String> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Announce every row up front -- superproject and each submodule -- so the
    // modal shows the full scope of the fetch immediately, not as rows trickle
    // in. The UI keeps a step's first label and only updates its state on
    // re-send, so each Done/Failed below reuses that exact label.
    report(ProgressEvent::step(
        "fetch",
        "FETCH — all remotes, prune",
        StepState::Running,
    ));
    for path in subs {
        report(ProgressEvent::step(
            &sub_step_key(path),
            &format!("SUBMODULE — {path}"),
            StepState::Running,
        ));
    }

    let (tx, rx) = channel::<ProgressEvent>();
    // Task 0 is the superproject; 1.. are the submodules. The superproject's
    // outcome is the fetch's outcome (a submodule failure is best-effort), so
    // it is captured out of band while submodule failures only colour their row.
    let top_result: Mutex<Result<String, String>> = Mutex::new(Ok("fetch complete".to_string()));
    let next = AtomicUsize::new(0);
    let n_tasks = subs.len() + 1;
    let workers = SUBMODULE_FETCH_CONCURRENCY.min(n_tasks);

    // Scoped threads let the workers borrow `workdir`/`subs`/`top_result`
    // directly -- no `Arc`/clone of the path list, and the scope joins them all
    // before it returns.
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let next = &next;
            let top_result = &top_result;
            scope.spawn(move || {
                // Each worker claims tasks off a shared counter until they run
                // out -- a tiny work-stealing queue with no extra deps.
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n_tasks {
                        break;
                    }
                    if i == 0 {
                        let r = fetch_super(workdir, &tx);
                        *top_result.lock().unwrap() = r;
                    } else {
                        fetch_one_submodule(workdir, &subs[i - 1], &tx);
                    }
                }
            });
        }
        // Drop this thread's own handle so `rx` closes once every worker exits,
        // then forward each event here -- the only thread that calls `report`.
        drop(tx);
        for event in rx {
            report(event);
        }
    });

    top_result.into_inner().unwrap()
}

/// How many fetches run at once (superproject + submodules share the pool).
/// Fetch is network-bound, so we go a little past the CPU count, but a hard cap
/// keeps a repo with hundreds of submodules from spawning hundreds of git
/// processes (and credential lookups) at once. The pool is `min(this, tasks)`.
const SUBMODULE_FETCH_CONCURRENCY: usize = 8;

/// Fetches the superproject's remotes, streaming output to the modal and
/// closing the `fetch` row. Returns the status-bar message; this is the fetch's
/// overall result. `--recurse-submodules=no` because [`fetch`] drives the
/// submodules itself.
fn fetch_super(workdir: &Path, tx: &Sender<ProgressEvent>) -> Result<String, String> {
    let mut sink = |event: ProgressEvent| {
        let _ = tx.send(event);
    };
    let (_, stderr) = run_step(
        workdir,
        &["fetch", "--all", "--prune", "--recurse-submodules=no"],
        "git fetch failed",
        "fetch",
        "FETCH — all remotes, prune",
        &mut sink,
    )?;
    let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
    // git prints ref updates ("abc..def  main -> origin/main") to stderr.
    match last_line {
        Some(line) if stderr.contains("->") => Ok(format!("fetch complete — {}", line.trim())),
        _ => Ok("fetch complete".to_string()),
    }
}

/// Fetches one submodule's default remote, streaming its output as console
/// lines and closing its checklist row Done/Failed. Errors are reported but
/// never propagated: the superproject fetch stands on its own and a stale
/// submodule is recoverable. `git_cmd`'s hardening and the confined `path`
/// still apply.
///
/// `--recurse-submodules=no`: we drive every submodule fetch ourselves, so the
/// per-submodule fetch must not also recurse into *nested* submodules. Letting
/// it would resurrect the serial on-demand walk we replaced -- and a nested
/// submodule that isn't checked out makes git abort with "cannot change to ...".
fn fetch_one_submodule(parent: &Path, path: &str, tx: &Sender<ProgressEvent>) {
    let sub_dir = parent.join(path);
    let key = sub_step_key(path);
    let label = format!("SUBMODULE — {path}");
    let mut sink = |event: ProgressEvent| {
        let _ = tx.send(event);
    };
    let state = match stream_git(
        &sub_dir,
        &["fetch", "--prune", "--recurse-submodules=no"],
        &mut sink,
    ) {
        Ok((_, _, true)) => StepState::Done,
        // The console tail already carries git's stderr, so the failure reason
        // is visible; the row just flips red (the label can't change on re-send).
        Ok(_) | Err(_) => StepState::Failed,
    };
    let _ = tx.send(ProgressEvent::step(&key, &label, state));
}

/// Stable checklist-row key for a submodule's fetch step, namespaced so it
/// never collides with the superproject's own `fetch` row.
fn sub_step_key(path: &str) -> String {
    format!("submodule:{path}")
}

/// What a push/pull probe decided. `Completed` means the op ran (or failed)
/// without a choice to make; `Diverged` means both sides hold commits the
/// other lacks and only the user can pick how to reconcile.
pub enum SyncOutcome {
    Completed(Result<String, String>),
    Diverged { behind: usize, ahead: usize },
}

/// Pushes the current branch to origin via the CLI. Network op, same
/// throwaway-thread treatment as [`fetch`].
///
/// Fetches first and inspects divergence so the push never silently fails (or
/// silently merges): a branch that has diverged from its upstream returns
/// [`SyncOutcome::Diverged`] for the UI to resolve, while a fast-forwardable
/// branch just pushes.
pub fn push(workdir: &Path, report: Reporter<'_>) -> SyncOutcome {
    // A brand-new branch has no upstream to diverge from -- push and let -u
    // record the tracking ref.
    if !has_upstream(workdir) {
        return SyncOutcome::Completed(do_push(workdir, report));
    }
    // Refresh our view of the upstream so the counts below reflect reality,
    // not the last fetch -- origin may have moved since.
    if let Err(e) = run_step(
        workdir,
        &["fetch"],
        "git fetch failed",
        "probe",
        "CHECK REMOTE — fetch upstream",
        report,
    ) {
        return SyncOutcome::Completed(Err(e));
    }
    match divergence(workdir) {
        Err(e) => SyncOutcome::Completed(Err(e)),
        Ok((behind, ahead)) if behind > 0 && ahead > 0 => {
            // Non-fast-forward push: the user decides merge vs rebase.
            SyncOutcome::Diverged { behind, ahead }
        }
        Ok((behind, _)) if behind > 0 => {
            // Strictly behind: nothing of ours to push, just catch up so the
            // branch (and its ahead/behind readout) is current again.
            SyncOutcome::Completed(fast_forward(workdir, report))
        }
        // Even with or ahead of the upstream: an ordinary fast-forward push.
        Ok(_) => SyncOutcome::Completed(do_push(workdir, report)),
    }
}

/// Pulls the current branch from its upstream via the CLI. Network op, same
/// throwaway-thread treatment as [`fetch`].
///
/// Fast-forwards when it cleanly can; on divergence returns
/// [`SyncOutcome::Diverged`] so the user chooses merge vs rebase rather than
/// the app silently creating a merge commit.
pub fn pull(workdir: &Path, report: Reporter<'_>) -> SyncOutcome {
    if !has_upstream(workdir) {
        return SyncOutcome::Completed(Err("no upstream branch to pull from".to_string()));
    }
    if let Err(e) = run_step(
        workdir,
        &["fetch"],
        "git fetch failed",
        "probe",
        "CHECK REMOTE — fetch upstream",
        report,
    ) {
        return SyncOutcome::Completed(Err(e));
    }
    match divergence(workdir) {
        Err(e) => SyncOutcome::Completed(Err(e)),
        Ok((behind, ahead)) if behind > 0 && ahead > 0 => SyncOutcome::Diverged { behind, ahead },
        Ok((behind, _)) if behind > 0 => SyncOutcome::Completed(fast_forward(workdir, report)),
        // Already up to date with (or ahead of) the upstream: nothing to pull.
        Ok(_) => SyncOutcome::Completed(Ok("Already up to date.".to_string())),
    }
}

/// Reconciles a diverged branch with the user's chosen strategy, then pushes
/// when the flow began at the PUSH button. A conflict (or aborted rebase)
/// stops short of pushing and reports the status so the user resolves first.
pub fn integrate(
    workdir: &Path,
    strategy: SyncStrategy,
    push_after: bool,
    report: Reporter<'_>,
) -> Result<String, String> {
    let step = match strategy {
        SyncStrategy::Merge => merge_upstream(workdir, report)?,
        SyncStrategy::Rebase => rebase_upstream(workdir, report)?,
    };
    match step {
        // The branch is reconciled. Push it on if this began as a push.
        IntegrateStep::Done(status) => {
            if push_after {
                do_push(workdir, report)
            } else {
                Ok(status)
            }
        }
        // Conflicts (merge) or an aborted rebase: never push a tree that isn't
        // cleanly integrated -- report and let the user act.
        IntegrateStep::Stopped(status) => Ok(status),
    }
}

/// Result of one reconcile step, before any follow-up push.
enum IntegrateStep {
    /// Cleanly integrated; safe to push.
    Done(String),
    /// Stopped on conflicts (or was aborted); must not push.
    Stopped(String),
}

/// `git pull --no-rebase`: fetches and merges the upstream, creating a merge
/// commit on divergence. A conflict leaves the repo mid-merge -- the conflict
/// UI drives the resolve-and-commit from there.
fn merge_upstream(workdir: &Path, report: Reporter<'_>) -> Result<IntegrateStep, String> {
    report(ProgressEvent::step(
        "integrate",
        "PULL — merge origin",
        StepState::Running,
    ));
    let (stdout, stderr, ok) = stream_git(workdir, &["pull", "--no-rebase"], report)?;
    if ok {
        report(ProgressEvent::step(
            "integrate",
            "PULL — merge origin",
            StepState::Done,
        ));
        return Ok(IntegrateStep::Done(
            "pull complete — merged origin".to_string(),
        ));
    }
    if let Some(n) = conflict_count(&stdout, &stderr) {
        // Conflicts are an expected, recoverable outcome -- the step ran, the
        // repo is now mid-merge and the conflict UI takes over.
        report(ProgressEvent::step(
            "integrate",
            "PULL — merge origin",
            StepState::Done,
        ));
        return Ok(IntegrateStep::Stopped(match n {
            0 => "merge conflicts — resolve and commit".to_string(),
            n => format!("merge conflicts in {n} file(s) — resolve and commit"),
        }));
    }
    report(ProgressEvent::step(
        "integrate",
        "PULL — merge origin",
        StepState::Failed,
    ));
    Err(last_nonempty(&stderr, &stdout, "git pull failed"))
}

/// `git pull --rebase`: replays local commits on top of the upstream for a
/// linear history. The app has no rebase-continue UI, so a conflict is aborted
/// and reported -- the branch is left as it was and the user can pick merge.
fn rebase_upstream(workdir: &Path, report: Reporter<'_>) -> Result<IntegrateStep, String> {
    report(ProgressEvent::step(
        "integrate",
        "PULL — rebase onto origin",
        StepState::Running,
    ));
    let (stdout, stderr, ok) = stream_git(workdir, &["pull", "--rebase"], report)?;
    if ok {
        report(ProgressEvent::step(
            "integrate",
            "PULL — rebase onto origin",
            StepState::Done,
        ));
        return Ok(IntegrateStep::Done(
            "pull complete — rebased onto origin".to_string(),
        ));
    }
    if conflict_count(&stdout, &stderr).is_some() {
        // Leave no half-finished rebase behind -- there is no UI to continue it.
        let _ = git_output(workdir, &["rebase", "--abort"]);
        report(ProgressEvent::step(
            "integrate",
            "PULL — rebase onto origin",
            StepState::Done,
        ));
        return Ok(IntegrateStep::Stopped(
            "rebase hit conflicts and was aborted — try merge instead".to_string(),
        ));
    }
    report(ProgressEvent::step(
        "integrate",
        "PULL — rebase onto origin",
        StepState::Failed,
    ));
    Err(last_nonempty(&stderr, &stdout, "git pull --rebase failed"))
}

/// Pushes HEAD to origin, recording the upstream on a branch's first push.
///
/// Streams the push so its checklist row shows live transfer progress, and
/// surfaces the **pre-push hook** as its own step: git runs that hook before it
/// emits any of its own transfer output, so any line arriving ahead of git's
/// first recognisable push line (see [`is_push_output`]) is hook output -- the
/// lint/format/test runners the user wanted to watch. The hook step opens on
/// the first such line and closes when git's own output begins (or with the
/// push itself if the hook was the whole story).
fn do_push(workdir: &Path, report: Reporter<'_>) -> Result<String, String> {
    report(ProgressEvent::step(
        "push",
        "PUSH — origin HEAD",
        StepState::Running,
    ));

    // State for the pre-push-hook heuristic, tracked across the streamed lines.
    let mut hook_open = false;
    let mut saw_git_output = false;
    // No refspec: git pushes the current branch per the user's `push.default`
    // (the default `simple` pushes HEAD to its upstream of the same name).
    // `--set-upstream` makes the very first push of a new branch also record
    // its tracking ref, so later pushes need no arguments either.
    let result = {
        let mut tap = |event: ProgressEvent| {
            if let ProgressEvent::Console(line) = &event
                && !saw_git_output
            {
                if is_push_output(line) {
                    // git's own output has started; close the hook step if open.
                    saw_git_output = true;
                    if hook_open {
                        report(ProgressEvent::step(
                            "hook",
                            "PRE-PUSH HOOK",
                            StepState::Done,
                        ));
                    }
                } else if !hook_open {
                    // First line and it isn't git's: the pre-push hook is talking.
                    hook_open = true;
                    report(ProgressEvent::step(
                        "hook",
                        "PRE-PUSH HOOK",
                        StepState::Running,
                    ));
                }
            }
            report(event);
        };
        stream_git(
            workdir,
            &["push", "--set-upstream", "origin", "HEAD"],
            &mut tap,
        )
    };

    match result {
        Ok((_, stderr, true)) => {
            // A push that was nothing but hook output (no transfer, e.g. already
            // up to date) still leaves the hook step open -- close it.
            if hook_open && !saw_git_output {
                report(ProgressEvent::step(
                    "hook",
                    "PRE-PUSH HOOK",
                    StepState::Done,
                ));
            }
            report(ProgressEvent::step(
                "push",
                "PUSH — origin HEAD",
                StepState::Done,
            ));
            let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
            match last_line {
                // "Everything up-to-date" already reads as a full status message.
                Some(line) if line.trim() == "Everything up-to-date" => Ok(line.trim().to_string()),
                Some(line) => Ok(format!("push complete — {}", line.trim())),
                None => Ok("push complete".to_string()),
            }
        }
        Ok((stdout, stderr, false)) => {
            // A failed push commonly means the pre-push hook rejected it.
            if hook_open && !saw_git_output {
                report(ProgressEvent::step(
                    "hook",
                    "PRE-PUSH HOOK",
                    StepState::Failed,
                ));
            }
            report(ProgressEvent::step(
                "push",
                "PUSH — origin HEAD",
                StepState::Failed,
            ));
            Err(last_nonempty(&stderr, &stdout, "git push failed"))
        }
        Err(e) => {
            report(ProgressEvent::step(
                "push",
                "PUSH — origin HEAD",
                StepState::Failed,
            ));
            Err(e)
        }
    }
}

/// True when `line` looks like git's own `push` output rather than a pre-push
/// hook's. Used to split hook chatter (which always precedes git's transfer)
/// from the push proper. Heuristic by nature -- a worst-case misclassification
/// only mislabels a checklist row, never affects the push.
fn is_push_output(line: &str) -> bool {
    let l = line.trim_start();
    l.contains("->") // ref update: "abc..def  main -> main"
        || [
            "Enumerating objects",
            "Counting objects",
            "Compressing objects",
            "Delta compression",
            "Resolving deltas",
            "Writing objects",
            "Total ",
            "remote:",
            "To ",
            "* [new",
            "Everything up-to-date",
            "Pushing to",
            "error:",
            "fatal:",
            "hint:",
        ]
        .iter()
        .any(|p| l.starts_with(p))
}

/// Fast-forwards HEAD onto its already-fetched upstream. Used when the local
/// branch is strictly behind, so no merge commit is possible or needed.
fn fast_forward(workdir: &Path, report: Reporter<'_>) -> Result<String, String> {
    let (stdout, _) = run_step(
        workdir,
        &["merge", "--ff-only", "@{u}"],
        "git merge failed",
        "ff",
        "FAST-FORWARD — catch up to upstream",
        report,
    )?;
    let first_line = stdout.lines().find(|l| !l.trim().is_empty());
    match first_line {
        Some(line) if line.trim() == "Already up to date." => Ok(line.trim().to_string()),
        Some(line) => Ok(format!("pull complete — {}", line.trim())),
        None => Ok("pull complete".to_string()),
    }
}

/// `(behind, ahead)`: commits the upstream has that HEAD lacks, and commits
/// HEAD has that the upstream lacks. Only meaningful right after a fetch.
fn divergence(workdir: &Path) -> Result<(usize, usize), String> {
    // --left-right --count over `@{u}...HEAD` prints "<behind>\t<ahead>":
    // left side is the upstream's exclusive commits, right side is ours.
    let (stdout, _) = run_git(
        workdir,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
        "git rev-list failed",
    )?;
    let mut nums = stdout.split_whitespace();
    let behind = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok((behind, ahead))
}

/// True when the current branch tracks an upstream ref — i.e. it has been
/// pushed before, or was checked out from a remote.
fn has_upstream(workdir: &Path) -> bool {
    git_output(
        workdir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// Last non-empty line of stderr, falling back to stdout, then `fallback`.
/// Trims it for a one-line status-bar message.
fn last_nonempty(stderr: &str, stdout: &str, fallback: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .or_else(|| stdout.lines().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// Base git CLI invocation. The `-c` overrides neutralize repo-local config
/// that would otherwise let a crafted repository run arbitrary commands the
/// moment a network or merge op executes inside it: an `ext::` remote URL
/// spawns the named program on fetch, and a command-valued `core.fsmonitor`
/// runs on any index refresh. Hooks still run -- same trust model as using
/// the git CLI in that repo yourself.
fn git_cmd(workdir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(workdir)
        .args(["-c", "protocol.ext.allow=never", "-c", "core.fsmonitor="]);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: stop a console window from flashing up.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Like `run_git` but hands back the raw `Output` even on nonzero exit:
/// `merge` must inspect the stdout/stderr of *failed* runs to tell a
/// conflicted merge (job succeeded, repo is merging) from a genuine error.
fn git_output(workdir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    git_cmd(workdir).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git CLI not found on PATH".to_string()
        } else {
            e.to_string()
        }
    })
}

/// Shells out to the git CLI in `workdir` so the user's credential helpers
/// (and ssh agent) handle network auth. Returns (stdout, stderr) on success;
/// on failure returns the last non-empty stderr line, or `fail_msg`.
fn run_git(workdir: &Path, args: &[&str], fail_msg: &str) -> Result<(String, String), String> {
    let output = git_cmd(workdir).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git CLI not found on PATH".to_string()
        } else {
            e.to_string()
        }
    })?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
        return Err(last_line.unwrap_or(fail_msg).to_string());
    }
    Ok((String::from_utf8_lossy(&output.stdout).into_owned(), stderr))
}

/// Runs a git subprocess while streaming its output to `report` line by line,
/// so a multi-second op (a push to a slow remote, a hook running tests) reads
/// as work underway instead of a frozen spinner. Returns the full
/// `(stdout, stderr, success)` once the process exits, same shape callers parse
/// from a buffered run.
///
/// Output is split on **both** `\n` and `\r`: git writes transfer progress
/// ("Writing objects:  40%") as carriage-return redraws, so splitting on `\r`
/// too surfaces each redraw as its own console line as it happens. Bytes become
/// `String` only via `from_utf8_lossy`, per the security invariants, and each
/// line is capped at [`CONSOLE_LINE_CAP`].
fn stream_git(
    workdir: &Path,
    args: &[&str],
    report: Reporter<'_>,
) -> Result<(String, String, bool), String> {
    let mut child = git_cmd(workdir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git CLI not found on PATH".to_string()
            } else {
                e.to_string()
            }
        })?;
    let stdout = child.stdout.take().ok_or("failed to capture git stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture git stderr")?;

    // git interleaves progress on stderr with results on stdout; read both on
    // their own threads and merge the lines over a channel. `true` tags a
    // stdout line. The loop ends when both readers drop their sender.
    let (tx, rx) = channel::<(bool, String)>();
    let tx_err = tx.clone();
    let h_out = std::thread::spawn(move || pump(stdout, true, &tx));
    let h_err = std::thread::spawn(move || pump(stderr, false, &tx_err));

    let mut out = String::new();
    let mut err = String::new();
    for (is_stdout, line) in rx {
        if is_stdout {
            out.push_str(&line);
            out.push('\n');
        } else {
            err.push_str(&line);
            err.push('\n');
        }
        if !line.trim().is_empty() {
            report(ProgressEvent::Console(line));
        }
    }
    let _ = h_out.join();
    let _ = h_err.join();

    let status = child.wait().map_err(|e| e.to_string())?;
    Ok((out, err, status.success()))
}

/// Reader-thread body: split one pipe into lines on `\n`/`\r` and send each
/// over `tx`, tagged `is_stdout`. A line that overruns [`CONSOLE_LINE_CAP`] is
/// flushed early so a newline-less stream can't grow the buffer without bound.
fn pump<R: Read>(reader: R, is_stdout: bool, tx: &Sender<(bool, String)>) {
    let mut buf: Vec<u8> = Vec::new();
    for byte in BufReader::new(reader).bytes() {
        let Ok(b) = byte else { break };
        if b == b'\n' || b == b'\r' || buf.len() >= CONSOLE_LINE_CAP {
            flush_line(&mut buf, is_stdout, tx);
            if b != b'\n' && b != b'\r' {
                buf.push(b);
            }
        } else {
            buf.push(b);
        }
    }
    flush_line(&mut buf, is_stdout, tx);
}

/// Sends `buf` as one line (lossy UTF-8) and clears it; no-op when empty.
fn flush_line(buf: &mut Vec<u8>, is_stdout: bool, tx: &Sender<(bool, String)>) {
    if !buf.is_empty() {
        let _ = tx.send((is_stdout, String::from_utf8_lossy(buf).into_owned()));
        buf.clear();
    }
}

/// Runs one streamed git invocation as a single checklist step: emits the step
/// `Running`, then `Done`/`Failed` from the exit status. Returns the captured
/// `(stdout, stderr)` on success, or the last error line on failure -- the same
/// contract as [`run_git`], but with live output and a checklist row.
fn run_step(
    workdir: &Path,
    args: &[&str],
    fail_msg: &str,
    key: &str,
    label: &str,
    report: Reporter<'_>,
) -> Result<(String, String), String> {
    report(ProgressEvent::step(key, label, StepState::Running));
    match stream_git(workdir, args, report) {
        Ok((out, err, true)) => {
            report(ProgressEvent::step(key, label, StepState::Done));
            Ok((out, err))
        }
        Ok((out, err, false)) => {
            report(ProgressEvent::step(key, label, StepState::Failed));
            Err(last_nonempty(&err, &out, fail_msg))
        }
        Err(e) => {
            report(ProgressEvent::step(key, label, StepState::Failed));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discard_restores_tracked_deletes_untracked_keeps_staged() {
        let dir = std::env::temp_dir().join(format!("gitgud_discard_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "test").unwrap();
        cfg.set_str("user.email", "test@test").unwrap();

        fs::write(dir.join("a.txt"), "original").unwrap();
        fs::write(dir.join("c.txt"), "original").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "init").unwrap();

        fs::write(dir.join("a.txt"), "unstaged edit").unwrap();
        fs::write(dir.join("b.txt"), "untracked").unwrap();
        fs::write(dir.join("c.txt"), "staged edit").unwrap();
        stage(&repo, Some("c.txt")).unwrap();

        discard(&repo, None).unwrap();

        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "original");
        assert!(!dir.join("b.txt").exists(), "untracked file not deleted");
        // The staged edit lives in the index; discard resets workdir TO the
        // index, so the file on disk keeps the staged content.
        assert_eq!(
            fs::read_to_string(dir.join("c.txt")).unwrap(),
            "staged edit"
        );

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_single_file_leaves_others_alone() {
        let dir =
            std::env::temp_dir().join(format!("gitgud_discard_one_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "test").unwrap();
        cfg.set_str("user.email", "test@test").unwrap();

        fs::write(dir.join("a.txt"), "original").unwrap();
        fs::write(dir.join("b.txt"), "original").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "init").unwrap();

        fs::write(dir.join("a.txt"), "edit a").unwrap();
        fs::write(dir.join("b.txt"), "edit b").unwrap();

        discard(&repo, Some("a.txt")).unwrap();

        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "original");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "edit b");

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workdir_path_rejects_escapes() {
        let dir = std::env::temp_dir().join(format!("gitgud_wdpath_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();

        assert!(workdir_path(&repo, "sub/file.txt").is_ok());
        assert!(workdir_path(&repo, "../evil.txt").is_err());
        assert!(workdir_path(&repo, "a/../../evil.txt").is_err());
        // A drive-letter prefix is only an absolute root on Windows; on Unix
        // "C:" is an ordinary file name, so the path stays confined there.
        #[cfg(windows)]
        assert!(workdir_path(&repo, "C:/evil.txt").is_err());
        #[cfg(not(windows))]
        assert!(workdir_path(&repo, "C:/evil.txt").is_ok());
        assert!(workdir_path(&repo, "/evil.txt").is_err());

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_branch_makes_ref_switches_and_guards() {
        let dir = std::env::temp_dir().join(format!("gitgud_newbranch_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "test").unwrap();
        cfg.set_str("user.email", "test@test").unwrap();

        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "base").unwrap();
        let base_id = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        // Default branch name varies with git config (master/main); compare
        // against whatever HEAD is rather than hardcoding it.
        let initial = head_label(&repo).unwrap().0;

        // Create without switching: the ref exists, HEAD stays put.
        create_branch(&repo, "feature", &base_id, false).unwrap();
        assert!(repo.find_branch("feature", BranchType::Local).is_ok());
        assert_eq!(head_label(&repo).unwrap().0, initial);

        // Creating the same name again is refused (force = false).
        assert!(create_branch(&repo, "feature", &base_id, false).is_err());

        // Empty / whitespace names are refused before touching the repo.
        assert!(create_branch(&repo, "   ", &base_id, false).is_err());

        // Create + switch moves HEAD onto the new branch.
        create_branch(&repo, "other", &base_id, true).unwrap();
        assert_eq!(head_label(&repo).unwrap().0, "other");

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- conflict marker parser ----

    fn block(seg: &ConflictSegment) -> (&[String], &[String]) {
        match seg {
            ConflictSegment::Conflict { ours, theirs } => (ours, theirs),
            other => panic!("expected Conflict segment, got {other:?}"),
        }
    }

    fn context(seg: &ConflictSegment) -> &[String] {
        match seg {
            ConflictSegment::Context(lines) => lines,
            other => panic!("expected Context segment, got {other:?}"),
        }
    }

    #[test]
    fn parse_single_block_with_labels() {
        let text = "ctx1\n<<<<<<< HEAD\nour line\n=======\ntheir line\n>>>>>>> feature\nctx2\n";
        let (segs, ours_label, theirs_label) = parse_conflict(text);
        assert_eq!(ours_label, "HEAD");
        assert_eq!(theirs_label, "feature");
        assert_eq!(segs.len(), 3);
        assert_eq!(context(&segs[0]), ["ctx1"]);
        let (ours, theirs) = block(&segs[1]);
        assert_eq!(ours, ["our line"]);
        assert_eq!(theirs, ["their line"]);
        assert_eq!(context(&segs[2]), ["ctx2"]);
    }

    #[test]
    fn parse_multiple_blocks_with_context_between() {
        let text = "<<<<<<< HEAD\na1\n=======\nb1\n>>>>>>> dev\nmid1\nmid2\n\
                    <<<<<<< HEAD\na2\n=======\nb2\n>>>>>>> dev\n";
        let (segs, ours_label, theirs_label) = parse_conflict(text);
        assert_eq!(segs.len(), 3);
        assert_eq!(block(&segs[0]).0, ["a1"]);
        assert_eq!(context(&segs[1]), ["mid1", "mid2"]);
        assert_eq!(block(&segs[2]).1, ["b2"]);
        assert_eq!(ours_label, "HEAD");
        assert_eq!(theirs_label, "dev");
    }

    #[test]
    fn parse_diff3_base_section() {
        let text = "<<<<<<< HEAD\nour\n||||||| merged common ancestors\nold\n=======\ntheir\n>>>>>>> dev\n";
        let (segs, _, _) = parse_conflict(text);
        assert_eq!(segs.len(), 1);
        let (ours, theirs) = block(&segs[0]);
        assert_eq!(ours, ["our"]);
        assert_eq!(theirs, ["their"], "base section must be discarded");
    }

    #[test]
    fn parse_no_trailing_newline() {
        let text = "<<<<<<< HEAD\no\n=======\nt\n>>>>>>> br";
        let (segs, _, theirs_label) = parse_conflict(text);
        assert_eq!(segs.len(), 1);
        assert_eq!(block(&segs[0]).1, ["t"]);
        assert_eq!(theirs_label, "br");
    }

    #[test]
    fn parse_dangling_marker_degrades_to_context() {
        let text = "<<<<<<< HEAD\nour\n=======\ntheir\n";
        let (segs, ours_label, theirs_label) = parse_conflict(text);
        assert_eq!(segs.len(), 1);
        assert_eq!(
            context(&segs[0]),
            ["<<<<<<< HEAD", "our", "=======", "their"]
        );
        assert_eq!(ours_label, "OURS");
        assert_eq!(theirs_label, "THEIRS");
    }

    // ---- merge state integration (libgit2 only; no PATH git) ----

    fn temp_repo(name: &str) -> (std::path::PathBuf, Repository) {
        let dir = std::env::temp_dir().join(format!("gitgud_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "test").unwrap();
        cfg.set_str("user.email", "test@test").unwrap();
        (dir, repo)
    }

    /// Commits a change to f.txt (None = delete) on `refname` straight from a
    /// tree, so the divergent branch exists without touching the workdir.
    fn commit_on_ref(
        repo: &Repository,
        refname: &str,
        parent: &git2::Commit<'_>,
        content: Option<&str>,
    ) -> Oid {
        let mut tb = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
        match content {
            Some(c) => {
                let blob = repo.blob(c.as_bytes()).unwrap();
                tb.insert("f.txt", blob, 0o100644).unwrap();
            }
            None => {
                tb.remove("f.txt").unwrap();
            }
        }
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some(refname), &sig, &sig, "theirs", &tree, &[parent])
            .unwrap()
    }

    /// Base commit, divergent edits on HEAD (ours) and refs/heads/other
    /// (theirs), then a libgit2 merge that leaves the repo conflicted.
    /// Returns the other branch's tip (the future MERGE_HEAD).
    fn make_conflict(
        name: &str,
        ours: Option<&str>,
        theirs: Option<&str>,
    ) -> (std::path::PathBuf, Repository, Oid) {
        let (dir, repo) = temp_repo(name);
        fs::write(dir.join("f.txt"), "a\nshared\nb\n").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "base").unwrap();
        // Scoped: Commit/AnnotatedCommit borrow `repo`, which is moved out.
        let their_oid = {
            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            commit_on_ref(&repo, "refs/heads/other", &base_commit, theirs)
        };

        match ours {
            Some(c) => fs::write(dir.join("f.txt"), c).unwrap(),
            None => fs::remove_file(dir.join("f.txt")).unwrap(),
        }
        stage(&repo, Some("f.txt")).unwrap();
        commit(&repo, "ours").unwrap();

        {
            let annotated = repo.find_annotated_commit(their_oid).unwrap();
            repo.merge(&[&annotated], None, None).unwrap();
        }
        (dir, repo, their_oid)
    }

    #[test]
    fn merge_conflict_resolve_commit_flow() {
        let (dir, repo, their_oid) = make_conflict(
            "conflict_flow",
            Some("a\nours\nb\n"),
            Some("a\ntheirs\nb\n"),
        );

        let data = refresh(&repo).unwrap();
        let merging = data.merging.expect("refresh detects the merge state");
        assert_eq!(merging.head_short, their_oid.to_string()[..7]);
        assert!(
            data.unstaged
                .iter()
                .any(|f| f.path == "f.txt" && f.kind == ChangeKind::Conflicted)
        );

        match load_conflict(&repo, "f.txt").unwrap() {
            ConflictFile::Text {
                segments,
                trailing_newline,
                ..
            } => {
                assert!(trailing_newline);
                let conflict = segments
                    .iter()
                    .find(|s| matches!(s, ConflictSegment::Conflict { .. }))
                    .expect("one conflict block");
                let (ours, theirs) = block(conflict);
                assert_eq!(ours, ["ours"]);
                assert_eq!(theirs, ["theirs"]);
            }
            other => panic!("expected Text, got {other:?}"),
        }

        resolve(
            &repo,
            "f.txt",
            &Resolution::Content("a\nresolved\nb\n".to_string()),
        )
        .unwrap();
        let data = refresh(&repo).unwrap();
        assert!(
            !data
                .unstaged
                .iter()
                .any(|f| f.kind == ChangeKind::Conflicted)
        );
        assert!(
            data.staged
                .iter()
                .any(|f| f.path == "f.txt" && f.kind == ChangeKind::Modified)
        );

        commit(&repo, "merge other").unwrap();
        {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            assert_eq!(head.parent_count(), 2);
            assert_eq!(head.parent_id(1).unwrap(), their_oid);
        }
        assert_eq!(repo.state(), RepositoryState::Clean);
        assert!(refresh(&repo).unwrap().merging.is_none());

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_by_us_resolve_theirs_restores_file() {
        let (dir, repo, _) = make_conflict("del_by_us", None, Some("a\ntheirs\nb\n"));

        assert!(matches!(
            load_conflict(&repo, "f.txt").unwrap(),
            ConflictFile::DeletedByUs
        ));

        resolve(&repo, "f.txt", &Resolution::Theirs).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("f.txt")).unwrap(),
            "a\ntheirs\nb\n"
        );
        assert!(!repo.index().unwrap().has_conflicts());

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_by_them_resolve_theirs_removes_file() {
        let (dir, repo, _) = make_conflict("del_by_them", Some("a\nours\nb\n"), None);

        assert!(matches!(
            load_conflict(&repo, "f.txt").unwrap(),
            ConflictFile::DeletedByThem
        ));

        resolve(&repo, "f.txt", &Resolution::Theirs).unwrap();
        assert!(!dir.join("f.txt").exists());
        let index = repo.index().unwrap();
        assert!(!index.has_conflicts());
        assert!(index.get_path(Path::new("f.txt"), 0).is_none());

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- submodule gitlink diff synthesis ----

    #[test]
    fn submodule_pointer_diff_shows_both_commits() {
        let (dir, repo) = temp_repo("sub_ptr_diff");
        // Two real commits to stand in as the old/new submodule pointers.
        fs::write(dir.join("f.txt"), "x").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "c1").unwrap();
        let a = repo.head().unwrap().peel_to_commit().unwrap().id();
        fs::write(dir.join("f.txt"), "y").unwrap();
        stage(&repo, None).unwrap();
        commit(&repo, "c2").unwrap();
        let b = repo.head().unwrap().peel_to_commit().unwrap().id();

        // Two trees differing only by a gitlink "sub" pointing at A vs B; the
        // 0o160000 filemode is what makes libgit2 report it as a submodule.
        // Scoped so the tree/diff borrows of `repo` end before `drop(repo)`.
        let data = {
            let base = repo.head().unwrap().peel_to_tree().unwrap();
            let tree = |oid| {
                let mut tb = repo.treebuilder(Some(&base)).unwrap();
                tb.insert("sub", oid, 0o160000).unwrap();
                repo.find_tree(tb.write().unwrap()).unwrap()
            };
            let (t1, t2) = (tree(a), tree(b));
            let d = repo.diff_tree_to_tree(Some(&t1), Some(&t2), None).unwrap();
            collect_diff(&d).unwrap()
        };
        let sub = data
            .files
            .iter()
            .find(|f| f.path == "sub")
            .expect("gitlink delta");
        assert_eq!((sub.adds, sub.dels), (1, 1));
        assert!(
            sub.lines
                .iter()
                .any(|l| l.kind == LineKind::Del && l.text == format!("Subproject commit {a}"))
        );
        assert!(
            sub.lines
                .iter()
                .any(|l| l.kind == LineKind::Add && l.text == format!("Subproject commit {b}"))
        );

        drop(repo);
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- conflict_count: drives both merge and pull-before-push status ----

    #[test]
    fn conflict_count_none_when_clean() {
        // A fast-forward / up-to-date pull says neither "CONFLICT" nor
        // "Automatic merge failed", so the push is free to proceed.
        assert_eq!(
            conflict_count("Updating abc..def\nFast-forward\n", ""),
            None
        );
        assert_eq!(conflict_count("Already up to date.\n", ""), None);
    }

    #[test]
    fn conflict_count_tallies_conflicted_files() {
        let stdout = "Auto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\n\
                      CONFLICT (content): Merge conflict in b.txt\n";
        let stderr = "Automatic merge failed; fix conflicts and then commit the result.\n";
        assert_eq!(conflict_count(stdout, stderr), Some(2));
    }

    #[test]
    fn conflict_count_some_zero_without_named_files() {
        // "Automatic merge failed" with no per-file CONFLICT lines still marks
        // a recoverable merge state — count just falls back to zero.
        assert_eq!(
            conflict_count("", "Automatic merge failed; fix conflicts.\n"),
            Some(0)
        );
    }
}
