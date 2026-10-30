//! Undo: before each turn, the working tree is recorded in a separate git directory under
//! `~/.killi/snapshots/`, through the user's `git`. Their own repository is never touched.
//! Files ignored by `.gitignore` are skipped, so build output doesn't slow it down.
//!
//! Sessions in one work tree share its snapshot directory, each staging through its own index
//! file in it, so their git steps never interleave; `/undo` is told when another session worked
//! in the tree since, because putting the tree back undoes its work too.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::killi_home;

const MAX_FILES: usize = 50_000;

/// How often a git command that found a lock on this session's index file is tried again,
/// 100 ms apart.
const LOCK_TRIES: u32 = 30;

pub struct Snapshots {
    git_dir: PathBuf,
    work_tree: PathBuf,
    /// Why snapshots are off, if they are.
    pub disabled: Option<String>,
    /// Why the last turn has no snapshot, if it doesn't; the turns before it can still be
    /// undone.
    pub skipped: Option<String>,
    /// One tree per turn, oldest first: the state just before that turn, and when it was
    /// taken in ms since the epoch.
    trees: Vec<(String, u64)>,
}

/// The blocking half of a snapshot: everything the git commands need, owned so the work
/// can move to a blocking thread while the turn's first request is already running.
/// `run` retries once like `take`; the caller records the tree with `commit_take`.
pub struct TakeWork {
    git_dir: PathBuf,
    work_tree: PathBuf,
    index_file: PathBuf,
    lock_name: String,
}

impl TakeWork {
    /// The git commands of a snapshot: init once, stage everything, hash the tree.
    pub fn run(&self) -> Result<String> {
        write_tree(self).or_else(|_| write_tree(self))
    }
}

fn git(work: &TakeWork, args: &[&str]) -> Result<String> {
    let mut tries = 0;
    loop {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&work.git_dir)
            .arg("--work-tree")
            .arg(&work.work_tree)
            .env("GIT_INDEX_FILE", &work.index_file)
            .args(["-c", "gc.auto=0", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(&work.work_tree)
            .output()
            .context("could not run git")?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let error = String::from_utf8_lossy(&output.stderr);
        tries += 1;
        if !error.contains(&work.lock_name) || tries >= LOCK_TRIES {
            bail!("git {} failed: {}", args.join(" "), error.trim());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_tree(work: &TakeWork) -> Result<String> {
    if !work.git_dir.exists() {
        create_private_dir(&work.git_dir)?;
        let status = Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&work.git_dir)
            .status()
            .context("could not run git")?;
        if !status.success() {
            bail!("git init failed for {}", work.git_dir.display());
        }
    }
    tighten_private_dir(&work.git_dir);
    git(work, &["add", "-A", "."])?;
    Ok(git(work, &["write-tree"])?.trim().to_string())
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
        Self {
            git_dir,
            work_tree,
            disabled,
            skipped: None,
            trees: Vec::new(),
        }
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
    /// the first tool call.
    pub fn commit_take(&mut self, tree: Result<String>) {
        match tree {
            Ok(tree) => self.trees.push((tree, crate::live::now())),
            Err(e) => {
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

    /// When the snapshot the next undo goes back to was taken, in ms since the epoch.
    pub fn last_taken(&self) -> Option<u64> {
        self.trees.last().map(|(_, at)| *at)
    }

    /// Puts the working tree back as it was before the last turn and returns what changed,
    /// as `git diff --name-status` lines. The snapshot is dropped only by a successful
    /// restore, so a failed undo can just be tried again.
    pub fn undo(&mut self) -> Result<Vec<String>> {
        if let Some(why) = &self.disabled {
            bail!("{why}");
        }
        let Some(tree) = self.trees.last().map(|(tree, _)| tree.clone()) else {
            bail!("there is nothing to undo yet")
        };
        let now = write_tree(&self.work())?;
        let changes: Vec<String> = self
            .git(&["diff", "--name-status", "--no-renames", &tree, &now])?
            .lines()
            .map(String::from)
            .collect();
        self.git(&["read-tree", "--reset", "-u", &tree])?;
        for path in self
            .git(&["ls-files", "--others", "--exclude-standard", "-z"])?
            .split('\0')
            .filter(|p| !p.is_empty())
        {
            let _ = std::fs::remove_file(self.work_tree.join(path));
        }
        self.trees.pop();
        Ok(changes)
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
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    #[test]
    fn undo_restores_changed_and_deleted_files_and_removes_new_ones() {
        let root = std::env::temp_dir().join(format!("killi-snap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        fs::write(root.join("b.txt"), "keep").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        assert_eq!(snaps.disabled, None);
        fs::write(root.join("a.txt"), "two").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("c.txt"), "new").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/out"), "build").unwrap();
        let changes = snaps.undo().unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "keep");
        assert!(!root.join("c.txt").exists());
        assert!(
            root.join("target/out").exists(),
            "ignored files are left alone"
        );
        assert_eq!(changes.len(), 3, "{changes:?}");
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_snapshot_waits_out_a_lock_on_its_index_instead_of_turning_undo_off() {
        let root = std::env::temp_dir().join(format!("killi-snap-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        assert!(snaps.last_taken().is_some());
        let lock = snaps.git_dir.join(snaps.lock_name());
        fs::write(&lock, "").unwrap();
        let held = lock.clone();
        let other = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            fs::remove_file(held).unwrap();
        });
        snaps.take();
        other.join().unwrap();
        assert_eq!(snaps.disabled, None);
        assert_eq!(snaps.skipped, None);
        assert_eq!(snaps.trees.len(), 2);
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_touches_only_changed_files() {
        let root = std::env::temp_dir().join(format!("killi-snap-mtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        fs::write(root.join("b.txt"), "keep").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        let old = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(root.join("a.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("b.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::write(root.join("a.txt"), "two").unwrap();
        snaps.undo().unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert_eq!(
            fs::metadata(root.join("b.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            old,
            "an untouched file keeps its mtime"
        );
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_undo_keeps_its_snapshot() {
        let root = std::env::temp_dir().join(format!("killi-snap-keep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        fs::write(root.join("a.txt"), "two").unwrap();
        snaps.take();
        fs::write(root.join("a.txt"), "three").unwrap();
        let tree = snaps.work_tree.clone();
        snaps.work_tree = root.join("gone");
        assert!(snaps.undo().is_err());
        snaps.work_tree = tree;
        snaps.undo().unwrap();
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "two",
            "the retry undoes one turn, not two"
        );
        assert_eq!(snaps.trees.len(), 1);
        snaps.undo().unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_snapshot_skips_the_turn_without_turning_undo_off() {
        let root = std::env::temp_dir().join(format!("killi-snap-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        fs::write(root.join("a.txt"), "two").unwrap();
        let tree = snaps.work_tree.clone();
        snaps.work_tree = root.join("gone");
        snaps.take();
        snaps.work_tree = tree;
        assert_eq!(snaps.disabled, None);
        assert!(
            snaps
                .skipped
                .as_deref()
                .is_some_and(|why| why.starts_with("undo snapshot skipped for this turn:"))
        );
        assert_eq!(snaps.trees.len(), 1);
        snaps.undo().unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_async_take_and_a_split_take_match_a_sync_one() {
        let root = std::env::temp_dir().join(format!("killi-snap-async-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take_async().await;
        assert!(snaps.last_taken().is_some());
        assert_eq!(snaps.skipped, None);
        assert_eq!(snaps.trees.len(), 1);
        fs::write(root.join("a.txt"), "two").unwrap();
        let work = snaps.start_take().expect("enabled");
        assert_eq!(snaps.skipped, None, "starting clears the last turn's skip");
        snaps.commit_take(work.run());
        assert_eq!(snaps.trees.len(), 2);
        snaps.undo().unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
        snaps.disabled = Some("off".to_string());
        assert!(snaps.start_take().is_none());
        snaps.take_async().await;
        assert_eq!(snaps.trees.len(), 1, "a disabled tree takes nothing");
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_repos_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("killi-snap-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        assert_eq!(
            snaps.git_dir.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::set_permissions(&snaps.git_dir, fs::Permissions::from_mode(0o755)).unwrap();
        snaps.take();
        assert_eq!(
            snaps.git_dir.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
