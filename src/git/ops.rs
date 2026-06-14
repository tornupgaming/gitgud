//! All actual libgit2 work: pure functions from a `Repository` to the plain
//! data types in `mod.rs`. Called only from the worker thread.
//!
//! Conventions: every error becomes a `String` (libgit2 errors via [`g`]) so
//! results cross the channel as owned data. Mutating ops return their
//! status-bar message as `Ok(String)`. Ops taking a `&Path` workdir instead
//! of a `&Repository` shell out to the git CLI -- see [`run_git`] for why.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use git2::build::CheckoutBuilder;
use git2::{
    BranchType, DiffOptions, ErrorCode, IndexAddOption, Oid, Repository, RepositoryState, Sort,
    Status, StatusOptions,
};

use super::{
    BranchInfo, ChangeKind, CommitInfo, ConflictFile, ConflictSegment, DiffData, DiffKey, DiffLine,
    FileChange, FileDiff, HistoryPage, LineKind, MergeInfo, RefreshData, RepoMeta, Resolution,
};

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
    Ok(format!("switched to {local}"))
}

/// `git merge <branch>` via the CLI so the user's merge config, strategy,
/// and hooks all apply. A conflicted merge returns `Ok` -- the repo is now
/// legitimately in the merge state; only a merge that never started is `Err`.
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

pub fn merge(workdir: &Path, branch: &str) -> Result<String, String> {
    // Branch names reaching here come from libgit2's ref enumeration, which
    // forbids a leading '-'. Re-check so this value can never be parsed as a
    // git flag if a future caller is less careful.
    if branch.starts_with('-') {
        return Err(format!("invalid branch name: {branch}"));
    }
    let output = git_output(workdir, &["merge", branch])?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status.success() {
        // First stdout line is git's summary: "Merge made by the 'ort'
        // strategy." / "Already up to date." / "Updating abc..def" (ff).
        return Ok(stdout
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| "merge complete".to_string()));
    }
    // A conflicted merge exits nonzero but is a *successful* job: the repo is
    // now in the merge state and the follow-up Refresh surfaces the conflicts.
    if let Some(n) = conflict_count(&stdout, &stderr) {
        return Ok(match n {
            0 => "merge conflicts — resolve and commit".to_string(),
            n => format!("merge conflicts in {n} file(s) — resolve and commit"),
        });
    }
    // Genuine failure: unknown branch, dirty worktree, already merging, ...
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .or_else(|| stdout.lines().rev().find(|l| !l.trim().is_empty()));
    Err(last
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "git merge failed".to_string()))
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
    use std::path::Component;
    let confined = Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !confined {
        return Err(format!("refusing path outside the worktree: {path}"));
    }
    Ok(repo
        .workdir()
        .ok_or("bare repositories are not supported")?
        .join(path))
}

/// `git fetch --all --prune` via the CLI. Network op -- the worker runs it
/// on a throwaway thread so it never blocks local jobs.
pub fn fetch(workdir: &Path) -> Result<String, String> {
    let (_, stderr) = run_git(workdir, &["fetch", "--all", "--prune"], "git fetch failed")?;
    let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
    // git prints ref updates ("abc..def  main -> origin/main") to stderr.
    match last_line {
        Some(line) if stderr.contains("->") => Ok(format!("fetch complete — {}", line.trim())),
        _ => Ok("fetch complete".to_string()),
    }
}

/// Pushes the current branch to origin via the CLI. Network op, same
/// throwaway-thread treatment as [`fetch`].
///
/// Pulls first so commits another user has landed on origin are merged into
/// the local branch before we push: git rejects a non-fast-forward push
/// outright, so integrating those commits here is what lets the push land at
/// all. If that merge stops on conflicts the repo is left mid-merge and the
/// push is skipped — the returned status tells the user to resolve first.
pub fn push(workdir: &Path) -> Result<String, String> {
    // Only pull when the branch already tracks an upstream. A brand-new local
    // branch has none, so there is nothing on origin to merge yet and a pull
    // would just error; the push below records the upstream with -u instead.
    if has_upstream(workdir) {
        if let Some(status) = pull_before_push(workdir)? {
            return Ok(status);
        }
    }
    // No refspec: git pushes the current branch per the user's `push.default`
    // (the default `simple` pushes HEAD to its upstream of the same name).
    // `--set-upstream` makes the very first push of a new branch also record
    // its tracking ref, so later pushes need no arguments either.
    let (_, stderr) = run_git(
        workdir,
        &["push", "--set-upstream", "origin", "HEAD"],
        "git push failed",
    )?;
    let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
    match last_line {
        // "Everything up-to-date" already reads as a full status message.
        Some(line) if line.trim() == "Everything up-to-date" => Ok(line.trim().to_string()),
        Some(line) => Ok(format!("push complete — {}", line.trim())),
        None => Ok("push complete".to_string()),
    }
}

/// True when the current branch tracks an upstream ref — i.e. it has been
/// pushed before, or was checked out from a remote. Decides whether a
/// pull-before-push has anything to merge.
fn has_upstream(workdir: &Path) -> bool {
    git_output(
        workdir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// Merges origin's commits into the current branch ahead of a push. Returns
/// `Ok(None)` when the branch is reconciled and ready to push, or
/// `Ok(Some(status))` when the merge stopped on conflicts and the caller must
/// not push — the repo is left mid-merge for the user to resolve and commit.
fn pull_before_push(workdir: &Path) -> Result<Option<String>, String> {
    // --no-rebase: reconcile a diverged branch with a merge so the other
    // user's commits are integrated (an ff-only pull would just fail, and a
    // rebase would rewrite local commits). Repo-local pull.rebase is ignored.
    let output = git_output(workdir, &["pull", "--no-rebase"])?;
    if output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    // A conflicted merge exits nonzero but leaves the repo in a valid merging
    // state — surface it as a status, the same way the Merge job does.
    if let Some(n) = conflict_count(&stdout, &stderr) {
        return Ok(Some(match n {
            0 => "pull conflicts — resolve and commit before pushing".to_string(),
            n => format!("pull conflicts in {n} file(s) — resolve and commit before pushing"),
        }));
    }
    // Genuine failure: network error, dirty worktree, detached HEAD, ...
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .or_else(|| stdout.lines().rev().find(|l| !l.trim().is_empty()));
    Err(last
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "git pull before push failed".to_string()))
}

/// Fast-forward-only pull via the CLI; a diverged branch fails with git's
/// own error. Network op, same throwaway-thread treatment as [`fetch`].
pub fn pull(workdir: &Path) -> Result<String, String> {
    // --ff-only: a GUI must not silently create merge commits. A diverged
    // branch surfaces git's own "Not possible to fast-forward" error instead.
    let (stdout, _) = run_git(workdir, &["pull", "--ff-only"], "git pull failed")?;
    // git reports the ref move ("Updating abc..def") on stdout's first line.
    let first_line = stdout.lines().find(|l| !l.trim().is_empty());
    match first_line {
        Some(line) if line.trim() == "Already up to date." => Ok(line.trim().to_string()),
        Some(line) => Ok(format!("pull complete — {}", line.trim())),
        None => Ok("pull complete".to_string()),
    }
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
        assert!(workdir_path(&repo, "C:/evil.txt").is_err());
        assert!(workdir_path(&repo, "/evil.txt").is_err());

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

    // ---- conflict_count: drives both merge and pull-before-push status ----

    #[test]
    fn conflict_count_none_when_clean() {
        // A fast-forward / up-to-date pull says neither "CONFLICT" nor
        // "Automatic merge failed", so the push is free to proceed.
        assert_eq!(conflict_count("Updating abc..def\nFast-forward\n", ""), None);
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
