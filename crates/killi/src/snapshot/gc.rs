//! Keeping `~/.killi/snapshots` in bounds. Snapshot trees have no refs: the only record of
//! one is the `trees` list of the session that took it. So a repo nobody stages through
//! holds objects no undo can reach, and `git gc` would count every object unreachable.
//! Objects are therefore only ever pruned after a week (`--prune=7.days.ago`, never `now`),
//! and only in a repo no running session uses, so no live session's undo loses its trees.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::config::killi_home;

/// Names, inside a snapshot repo, the work tree it is a snapshot of.
const MARKER: &str = "killi-worktree";
/// Touched each time the repo is collected; its age says when the next one is due.
const STAMP: &str = "killi-gc";
/// How often a repo is collected.
const GC_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How long a repo from before the marker must go unused before its tree, gone, drops it.
const UNMARKED_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);
/// How many folders the search for an unmarked repo's work tree may look at.
const SEARCH_LIMIT: usize = 10_000;

/// Records which work tree `git_dir` is for, so `prune_store` can tell once that folder is
/// gone, and starts its gc clock. Cheap when both are already there.
pub(super) fn mark(git_dir: &Path, work_tree: &Path) {
    let marker = git_dir.join(MARKER);
    if !marker.exists() {
        let _ = fs::write(marker, work_tree.as_os_str().as_bytes());
    }
    let stamp = git_dir.join(STAMP);
    if !stamp.exists() {
        let _ = fs::write(stamp, "");
    }
}

/// The pid an `index-<pid>` file, or its `.lock`, belongs to.
fn index_pid(name: &OsStr) -> Option<libc::pid_t> {
    let name = name.to_str()?.strip_prefix("index-")?;
    let pid = name.strip_suffix(".lock").unwrap_or(name);
    pid.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| pid.parse().ok())
        .flatten()
        .filter(|pid| *pid > 0)
}

/// Whether a process with this pid exists. One that can't be signalled still exists.
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The per-session index files and locks in `git_dir`, with the pid each belongs to.
fn indexes(git_dir: &Path) -> Vec<(PathBuf, libc::pid_t)> {
    fs::read_dir(git_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| Some((entry.path(), index_pid(&entry.file_name())?)))
        .collect()
}

/// Removes the index files, and the locks, of sessions that have ended without removing
/// their own.
pub(super) fn sweep_indexes(git_dir: &Path) {
    for (path, pid) in indexes(git_dir) {
        if !alive(pid) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Whether a running session stages through this repo: each one keeps its index there.
fn in_use(git_dir: &Path) -> bool {
    indexes(git_dir).into_iter().any(|(_, pid)| alive(pid))
}

/// Whether a repo's gc is due: its stamp is a day old, or it has none.
fn due(git_dir: &Path) -> bool {
    fs::metadata(git_dir.join(STAMP))
        .and_then(|m| m.modified())
        .map_or(true, |at| older_than(at, GC_EVERY))
}

fn older_than(at: SystemTime, age: Duration) -> bool {
    SystemTime::now()
        .duration_since(at)
        .is_ok_and(|elapsed| elapsed > age)
}

/// A session is done with `git_dir`: its index goes, and when the repo's gc is due and no
/// other session uses it, `git gc` starts before this returns, so it runs even when killi
/// exits right after; only the wait for it is left to a thread. Returns the gc's pid.
pub(super) fn on_drop(git_dir: &Path, index_file: &Path) -> Option<u32> {
    let _ = fs::remove_file(index_file);
    if !(git_dir.join("objects").is_dir() && due(git_dir) && !in_use(git_dir)) {
        return None;
    }
    let mut child = start(git_dir).ok()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Some(pid)
}

/// What `sweep_store` did: repos removed with their size, and repos whose gc is due.
#[derive(Debug, Default)]
pub(super) struct Swept {
    pub dropped: Vec<PathBuf>,
    pub bytes: u64,
    pub collect: Vec<PathBuf>,
}

/// Cleans every snapshot repo no running session uses: removes leaked index files, drops a
/// repo whose work tree is gone, and starts the gc of those due, in the background. The
/// bytes freed are only what dropping repos freed; gc runs on after this returns.
pub fn prune_store() -> u64 {
    let swept = sweep_store(&killi_home().join("snapshots"));
    collect_in_background(swept.collect);
    swept.bytes
}

pub(super) fn sweep_store(root: &Path) -> Swept {
    let mut swept = Swept::default();
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let repo = entry.path();
        let is_repo = repo.extension() == Some(OsStr::new("git"))
            && entry.file_type().is_ok_and(|t| t.is_dir())
            && repo.join("objects").is_dir();
        if !is_repo {
            continue;
        }
        sweep_indexes(&repo);
        if in_use(&repo) {
            continue;
        }
        if tree_gone(&repo) {
            let size = dir_size(&repo);
            if fs::remove_dir_all(&repo).is_ok() {
                swept.bytes += size;
                swept.dropped.push(repo);
            }
        } else if due(&repo) {
            swept.collect.push(repo);
        }
    }
    swept
}

/// Whether the work tree a repo is a snapshot of no longer exists. A repo from before the
/// marker is matched against its name, which is the path with `/` turned into `-`, and is
/// only called gone once no folder fits that name and it has gone a month unused. Anything
/// unclear keeps the repo.
fn tree_gone(repo: &Path) -> bool {
    if let Ok(bytes) = fs::read(repo.join(MARKER))
        && !bytes.is_empty()
    {
        let tree = Path::new(OsStr::from_bytes(&bytes));
        return tree.is_absolute()
            && fs::metadata(tree).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    }
    let Some(name) = repo.file_stem().and_then(OsStr::to_str) else {
        return false;
    };
    let idle = fs::metadata(repo)
        .and_then(|m| m.modified())
        .is_ok_and(|at| older_than(at, UNMARKED_IDLE));
    idle && name.starts_with('-') && !named_tree_exists(name)
}

/// Whether some folder's path, with `/` turned into `-`, is `name`. A `-` may stand for a
/// `/` or be part of a folder's name, so this tries both, depth first; past its search
/// limit it answers yes, which keeps the repo.
pub(super) fn named_tree_exists(name: &str) -> bool {
    let parts: Vec<&str> = name[1..].split('-').collect();
    let mut budget = SEARCH_LIMIT;
    exists_from(Path::new("/"), &parts, &mut budget)
}

fn exists_from(dir: &Path, parts: &[&str], budget: &mut usize) -> bool {
    for end in 1..=parts.len() {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let segment = parts[..end].join("-");
        if segment.is_empty() {
            continue;
        }
        let next = dir.join(&segment);
        if next.is_dir() && (end == parts.len() || exists_from(&next, &parts[end..], budget)) {
            return true;
        }
    }
    false
}

/// Runs `git gc` on each repo in turn, on a thread of its own. Each runs in a process group
/// of its own, so one in flight when killi exits finishes on its own; a repo whose gc hadn't
/// started stays due.
fn collect_in_background(repos: Vec<PathBuf>) {
    if repos.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for repo in repos {
            let _ = collect(&repo);
        }
    });
}

/// Packs a repo and prunes objects unused for a week. Never `--prune=now`: snapshot trees
/// have no refs, so that would take the newest ones too. Its output goes nowhere, so it can
/// outlive killi without writing to a closed pipe.
pub(super) fn collect(repo: &Path) -> anyhow::Result<()> {
    let status = start(repo)?.wait()?;
    if !status.success() {
        anyhow::bail!("git gc failed for {}: {status}", repo.display());
    }
    Ok(())
}

/// Starts `collect`'s `git gc` in a process group of its own, one `kill_all` doesn't know,
/// and restarts the repo's gc clock once it is running.
fn start(repo: &Path) -> std::io::Result<Child> {
    let child = Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(["-c", "gc.auto=0", "gc", "--quiet", "--prune=7.days.ago"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let _ = fs::write(repo.join(STAMP), "");
    Ok(child)
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            _ => entry.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}
