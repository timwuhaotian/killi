//! Undo: before each turn, the working tree is recorded in a separate git directory under
//! `~/.killi/snapshots/`, through the user's `git`. Their own repository is never touched.
//! Files ignored by `.gitignore` are skipped, so build output doesn't slow it down, and so
//! are files over 50 MB; undo never deletes either kind.
//!
//! Sessions in one work tree share its snapshot directory, each staging through its own index
//! file in it, so their git steps never interleave; `/undo` is told when another session worked
//! in the tree since, because putting the tree back undoes its work too.

mod deadline;
mod gc;
mod keep;

pub use gc::prune_store;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::killi_home;

const MAX_FILES: usize = 50_000;

/// Files larger than this stay out of snapshots: hashing one would hold up the turn and
/// copying it would fill the disk. Undo never deletes one.
const BIG_FILE: u64 = 50 * 1024 * 1024;

/// How often a git command that found a lock on this session's index file is tried again,
/// 100 ms apart.
const LOCK_TRIES: u32 = 30;

/// How long one git command of a snapshot or an undo may run before it is stopped.
const GIT_DEADLINE: Duration = Duration::from_secs(60);

pub struct Snapshots {
    git_dir: PathBuf,
    work_tree: PathBuf,
    /// Why snapshots are off, if they are.
    pub disabled: Option<String>,
    /// Why the last turn has no snapshot, if it doesn't; the turns before it can still be
    /// undone.
    pub skipped: Option<String>,
    /// One entry per turn, oldest first: the snapshot of the state just before that turn and
    /// when it was taken in ms since the epoch, or `None` when that turn's snapshot failed.
    trees: Vec<Option<(Taken, u64)>>,
    /// How long one git command may run; `GIT_DEADLINE`, shorter in tests.
    deadline: Duration,
}

/// A finished snapshot: the tree, the files left out of it for their size, and the
/// `.gitignore` files left out because they ignore themselves, as paths with their blob ids.
/// Undo reads the rules of both kinds of `.gitignore`.
#[derive(Clone)]
pub struct Taken {
    tree: String,
    big: Vec<Vec<u8>>,
    ignores: Vec<(Vec<u8>, String)>,
}

/// The blocking half of a snapshot: everything the git commands need, owned so the work
/// can move to a blocking thread while the turn's first request is already running.
/// `run` retries once like `take`; the caller records the tree with `commit_take`.
pub struct TakeWork {
    git_dir: PathBuf,
    work_tree: PathBuf,
    index_file: PathBuf,
    lock_name: String,
    deadline: Duration,
    cancel: Cancel,
}

impl TakeWork {
    /// The git commands of a snapshot: init once, stage everything, hash the tree. A git
    /// command that ran out of time or was given up on isn't tried again.
    pub fn run(&self) -> Result<Taken> {
        write_tree(self).or_else(|e| {
            if deadline::stopped(&e) {
                Err(e)
            } else {
                write_tree(self)
            }
        })
    }

    /// A handle that gives up on this work from another thread.
    pub fn canceler(&self) -> Cancel {
        self.cancel.clone()
    }
}

/// Gives up on a `TakeWork` running elsewhere: the git command it is on is stopped with what
/// it started, its lock on the session's index goes, and `run` ends with an error at once.
/// Nothing is left running to hold the index, or to hold up the exit, after the caller
/// stopped waiting.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Gives up on the work.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether the work was given up on.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// git with the snapshot repo's settings, for `work_tree` and run from it.
fn base(git_dir: &Path, work_tree: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(git_dir)
        .arg("--work-tree")
        .arg(work_tree)
        .args(["-c", "gc.auto=0", "-c", "core.autocrlf=false"])
        .current_dir(work_tree);
    command
}

/// Runs git through this session's index, writing `input` to its stdin, and returns what it
/// printed. A lock on that index is waited out; each try has the work's deadline. A command
/// stopped at its deadline, or because the work was given up on, can leave its lock on the
/// index behind; nothing else of this session's writes it, so it is removed, or every later
/// snapshot would wait on it and fail.
fn git_bytes<S: AsRef<OsStr>>(
    work: &TakeWork,
    args: &[S],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let shown = || {
        args.iter()
            .map(|arg| arg.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut tries = 0;
    loop {
        if work.cancel.is_set() {
            return Err(anyhow::Error::new(deadline::Canceled))
                .with_context(|| format!("git {}", shown()));
        }
        let mut command = base(&work.git_dir, &work.work_tree);
        command.env("GIT_INDEX_FILE", &work.index_file).args(args);
        let output = deadline::output(
            &mut command,
            input.map(<[u8]>::to_vec),
            work.deadline,
            &work.cancel.0,
        );
        if output.as_ref().is_err_and(deadline::stopped) {
            let _ = fs::remove_file(work.git_dir.join(&work.lock_name));
        }
        let output = output.with_context(|| format!("git {}", shown()))?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        let error = String::from_utf8_lossy(&output.stderr);
        tries += 1;
        if !error.contains(&work.lock_name) || tries >= LOCK_TRIES {
            bail!("git {} failed: {}", shown(), error.trim());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn git(work: &TakeWork, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_bytes(work, args, None)?).into_owned())
}

/// NUL-separated paths as git prints them with `-z`, without the empty end.
fn paths(listed: &[u8]) -> impl Iterator<Item = &[u8]> {
    listed.split(|b| *b == 0).filter(|path| !path.is_empty())
}

/// Paths as git reads them with `-z --stdin`.
fn nul_joined(paths: &[Vec<u8>]) -> Vec<u8> {
    paths
        .iter()
        .flat_map(|path| path.iter().chain([&0]))
        .copied()
        .collect()
}

/// Takes these paths out of the session's index, leaving the files alone.
fn unstage(work: &TakeWork, paths: &[Vec<u8>]) -> Result<()> {
    git_bytes(
        work,
        &["update-index", "-z", "--force-remove", "--stdin"],
        Some(&nul_joined(paths)),
    )?;
    Ok(())
}

fn write_tree(work: &TakeWork) -> Result<Taken> {
    if !work.git_dir.exists() {
        create_private_dir(&work.git_dir)?;
        let output = deadline::output(
            Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&work.git_dir),
            None,
            work.deadline,
            &work.cancel.0,
        )
        .context("git init")?;
        if !output.status.success() {
            bail!("git init failed for {}", work.git_dir.display());
        }
    }
    tighten_private_dir(&work.git_dir);
    gc::mark(&work.git_dir, &work.work_tree);
    let big = big_files(work)?;
    let mut add: Vec<OsString> = ["add", "-A", "--", "."].map(OsString::from).into();
    for path in &big {
        let mut exclude = OsString::from(":(exclude,literal)");
        exclude.push(OsStr::from_bytes(path));
        add.push(exclude);
    }
    git_bytes(work, &add, None)?;
    if !big.is_empty() {
        unstage(work, &big)?;
    }
    let ignores = keep::self_ignored(work)?;
    Ok(Taken {
        tree: git(work, &["write-tree"])?.trim().to_string(),
        big,
        ignores,
    })
}

/// New and changed files over `BIG_FILE`, as git prints their paths.
fn big_files(work: &TakeWork) -> Result<Vec<Vec<u8>>> {
    let listed = git_bytes(
        work,
        &[
            "ls-files",
            "-z",
            "--others",
            "--modified",
            "--exclude-standard",
        ],
        None,
    )?;
    Ok(paths(&listed)
        .filter(|path| is_big(&work.work_tree, path))
        .map(<[u8]>::to_vec)
        .collect())
}

fn is_big(work_tree: &Path, path: &[u8]) -> bool {
    fs::symlink_metadata(work_tree.join(OsStr::from_bytes(path)))
        .is_ok_and(|meta| meta.is_file() && meta.len() > BIG_FILE)
}

impl Snapshots {
    pub fn new(cwd: &Path) -> Self {
        let work_tree = crate::live::work_tree(cwd);
        let name = work_tree.to_string_lossy().replace('/', "-");
        let git_dir = killi_home().join("snapshots").join(format!("{name}.git"));
        let disabled = if work_tree == crate::config::home() || work_tree == Path::new("/") {
            Some("undo is off in your home folder and at the root".to_string())
        } else if too_many_files(&work_tree) {
            Some(format!(
                "undo is off: this folder has more than {MAX_FILES} files"
            ))
        } else if Command::new("git").arg("--version").output().is_err() {
            Some("undo needs git".to_string())
        } else {
            None
        };
        gc::sweep_indexes(&git_dir);
        Self {
            git_dir,
            work_tree,
            disabled,
            skipped: None,
            trees: Vec::new(),
            deadline: GIT_DEADLINE,
        }
    }

    /// Snapshots of `root` kept in `git_dir` rather than under the user's home.
    #[cfg(test)]
    pub(crate) fn in_dir(root: &Path, git_dir: &Path) -> Self {
        let mut snapshots = Self::new(root);
        snapshots.git_dir = git_dir.to_path_buf();
        snapshots
    }

    /// This session's own index inside the shared snapshot directory, so sessions running in
    /// the same work tree at once never stage through, or lock, the same index.
    fn index_file(&self) -> PathBuf {
        self.git_dir.join(format!("index-{}", std::process::id()))
    }

    /// The lock file git holds while it uses that index.
    fn lock_name(&self) -> String {
        format!("index-{}.lock", std::process::id())
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        git(&self.work(), args)
    }

    /// Everything the blocking half needs, cloned so it can move to another thread.
    fn work(&self) -> TakeWork {
        TakeWork {
            git_dir: self.git_dir.clone(),
            work_tree: self.work_tree.clone(),
            index_file: self.index_file(),
            lock_name: self.lock_name(),
            deadline: self.deadline,
            cancel: Cancel::default(),
        }
    }

    /// The blocking half of `take`, without the bookkeeping: `None` when snapshots are
    /// off for this tree. The caller runs the work elsewhere and finishes it with
    /// `commit_take`, so the snapshot can overlap the turn's first request; `take_async`
    /// does both halves for callers with nothing to overlap.
    pub fn start_take(&mut self) -> Option<TakeWork> {
        self.skipped = None;
        if self.disabled.is_some() {
            return None;
        }
        Some(self.work())
    }

    /// Records a finished `TakeWork`: the tree, or why this turn has no snapshot. The tree
    /// still predates the turn's changes when the caller waited for the work before running
    /// the first tool call. A turn without one still gets its place, so `/undo` can't skip
    /// past it to the turn before.
    pub fn commit_take(&mut self, tree: Result<Taken>) {
        match tree {
            Ok(tree) => self.trees.push(Some((tree, crate::live::now()))),
            Err(e) => {
                self.trees.push(None);
                self.skipped = Some(format!("undo snapshot skipped for this turn: {e:#}"));
            }
        }
    }

    /// `take` without freezing the caller: the git commands run on a blocking thread.
    #[allow(dead_code)]
    pub async fn take_async(&mut self) {
        let Some(work) = self.start_take() else {
            return;
        };
        let tree = tokio::task::spawn_blocking(move || work.run())
            .await
            .map_err(|e| anyhow::anyhow!("the snapshot task ended: {e}"));
        let tree = tree.and_then(|tree| tree);
        self.commit_take(tree);
    }

    /// Records the working tree before a turn. A snapshot that fails twice says why in
    /// `skipped` and leaves the turns before it to undo; snapshots stay on for the session.
    /// The live path overlaps this with the turn via `start_take`; only the tests call this.
    #[allow(dead_code)]
    pub fn take(&mut self) {
        if let Some(work) = self.start_take() {
            self.commit_take(work.run());
        }
    }

    /// The work tree the snapshots are of.
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    /// How many turns have a place in the undo list, snapshot or not, for `forget_after`.
    pub fn taken(&self) -> usize {
        self.trees.len()
    }

    /// Forgets every snapshot after the first `count`, for a turn taken back before it ran.
    pub fn forget_after(&mut self, count: usize) {
        self.trees.truncate(count);
    }

    /// When the newest snapshot there is was taken, in ms since the epoch.
    pub fn last_taken(&self) -> Option<u64> {
        self.trees.iter().rev().flatten().map(|(_, at)| *at).next()
    }

    /// Puts the working tree back as it was before the last turn and returns what changed,
    /// as `git diff --name-status` lines. The snapshot is dropped only by a successful
    /// restore, so a failed undo can just be tried again. When the last turns have no
    /// snapshot this refuses once, forgetting them, so the next undo goes back further
    /// only when asked again.
    pub fn undo(&mut self) -> Result<Vec<String>> {
        if let Some(why) = &self.disabled {
            bail!("{why}");
        }
        let snapshot = match self.trees.last() {
            None => bail!("there is nothing to undo yet"),
            Some(Some((taken, _))) => taken.clone(),
            Some(None) => {
                let missing = self.trees.iter().rev().take_while(|t| t.is_none()).count();
                self.trees.truncate(self.trees.len() - missing);
                if self.trees.is_empty() {
                    bail!("the last turn has no undo snapshot, and no turn before it has one");
                }
                if missing == 1 {
                    bail!(
                        "the last turn has no undo snapshot; /undo again goes back to the turn before it, which undoes both"
                    );
                }
                bail!(
                    "the last {missing} turns have no undo snapshot; /undo again goes back to the turn before them, which undoes those too"
                );
            }
        };
        let work = self.work();
        let now = write_tree(&work)?.tree;
        let kept = keep::kept(&work, &snapshot, &now)?;
        let now = if kept.is_empty() {
            now
        } else {
            unstage(&work, &kept)?;
            self.git(&["write-tree"])?.trim().to_string()
        };
        let changes: Vec<String> = self
            .git(&[
                "diff",
                "--name-status",
                "--no-renames",
                &snapshot.tree,
                &now,
            ])?
            .lines()
            .map(String::from)
            .collect();
        self.git(&["read-tree", "--reset", "-u", &snapshot.tree])?;
        let kept: HashSet<&[u8]> = kept.iter().map(Vec::as_slice).collect();
        let untracked = git_bytes(
            &work,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            None,
        )?;
        for path in paths(&untracked) {
            if !kept.contains(path) && !is_big(&self.work_tree, path) {
                let _ = fs::remove_file(self.work_tree.join(OsStr::from_bytes(path)));
            }
        }
        self.trees.pop();
        Ok(changes)
    }
}

impl Drop for Snapshots {
    /// The session is done with the snapshot repo: its index goes, and the repo's gc may
    /// start in the background.
    fn drop(&mut self) {
        gc::on_drop(&self.git_dir, &self.index_file());
    }
}

/// A folder for snapshot repos, private to this user: repos hold tool output and full tree copies.
fn create_private_dir(root: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .with_context(|| format!("could not create {}", root.display()))
}

fn tighten_private_dir(dir: &Path) {
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
}

fn too_many_files(root: &Path) -> bool {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .take(MAX_FILES + 1)
        .count()
        > MAX_FILES
}

#[cfg(test)]
mod tests;
