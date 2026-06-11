//! All actual libgit2 work: pure functions from a `Repository` to the plain
//! data types in `mod.rs`. Called only from the worker thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use git2::build::CheckoutBuilder;
use git2::{
    BranchType, DiffOptions, ErrorCode, IndexAddOption, Oid, Repository, Sort, Status,
    StatusOptions,
};

use super::{
    BranchInfo, ChangeKind, CommitInfo, DiffData, DiffKey, DiffLine, FileChange, FileDiff,
    HistoryPage, LineKind, RefreshData, RepoMeta,
};

/// Stop collecting diff lines past this point so a pathological diff
/// (generated file, vendored lockfile) cannot freeze the UI thread's render.
const DIFF_LINE_CAP: usize = 20_000;

/// `git2::Error` -> plain `String`, so results cross the thread boundary as
/// owned data. Used everywhere as `.map_err(g)?`.
fn g(e: git2::Error) -> String {
    e.message().to_string()
}

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

    Ok(RefreshData {
        head,
        head_detached,
        branches: locals,
        staged,
        unstaged,
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
    if let Some(parent) = &parent {
        if parent.tree_id() == tree_id {
            return Err("nothing staged to commit".to_string());
        }
    }
    // `parents` must be a slice of references; Option -> 0-or-1-element Vec.
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(g)?;
    Ok(format!("committed {}", short_id(oid)))
}

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

pub fn fetch(workdir: &Path) -> Result<String, String> {
    let stderr = run_git(workdir, &["fetch", "--all", "--prune"], "git fetch failed")?;
    let last_line = stderr.lines().rev().find(|l| !l.trim().is_empty());
    // git prints ref updates ("abc..def  main -> origin/main") to stderr.
    match last_line {
        Some(line) if stderr.contains("->") => Ok(format!("fetch complete — {}", line.trim())),
        _ => Ok("fetch complete".to_string()),
    }
}

pub fn push(workdir: &Path) -> Result<String, String> {
    // No refspec: git pushes the current branch per the user's `push.default`
    // (the default `simple` pushes HEAD to its upstream of the same name).
    // `--set-upstream` makes the very first push of a new branch also record
    // its tracking ref, so later pushes need no arguments either.
    let stderr = run_git(
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

/// Shells out to the git CLI in `workdir` so the user's credential helpers
/// (and ssh agent) handle network auth. Returns stderr on success; on failure
/// returns the last non-empty stderr line, or `fail_msg` if there is none.
fn run_git(workdir: &Path, args: &[&str], fail_msg: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(["-C"]).arg(workdir).args(args);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: stop a console window from flashing up.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|e| {
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
    Ok(stderr)
}
