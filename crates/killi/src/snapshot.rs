//! Undo: before each turn, the working tree is recorded in a separate git directory under
//! `~/.killi/snapshots/`, through the user's `git`. Their own repository is never touched.
//! Files ignored by `.gitignore` are skipped, so build output doesn't slow it down.
//!
//! Sessions in one work tree share its snapshot directory: a turn's snapshot waits out
//! another session's (`index.lock`) instead of turning undo off, and `/undo` is told when
//! another session worked in the tree since, because putting the tree back undoes its work too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::killi_home;

const MAX_FILES: usize = 50_000;

/// How often a git command that found another session's `index.lock` is tried again, 100 ms apart.
const LOCK_TRIES: u32 = 30;

pub struct Snapshots {
    git_dir: PathBuf,
    work_tree: PathBuf,
    /// Why snapshots are off, if they are.
    pub disabled: Option<String>,
    /// One tree per turn, oldest first: the state just before that turn, and when it was
    /// taken in ms since the epoch.
    trees: Vec<(String, u64)>,
}

impl Snapshots {
    pub fn new(cwd: &Path) -> Self {
        let work_tree = crate::live::work_tree(cwd);
        let name = work_tree.to_string_lossy().replace('/', "-");
        let git_dir = killi_home().join("snapshots").join(format!("{name}.git"));
        let disabled = if work_tree == crate::config::home() || work_tree == Path::new("/") {
            Some("undo is off in your home folder and at the root".to_string())
        } else if too_many_files(&work_tree) {
            Some(format!("undo is off: this folder has more than {MAX_FILES} files"))
        } else if Command::new("git").arg("--version").output().is_err() {
            Some("undo needs git".to_string())
        } else {
            None
        };
        Self { git_dir, work_tree, disabled, trees: Vec::new() }
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let mut tries = 0;
        loop {
            let output = Command::new("git")
                .arg("--git-dir")
                .arg(&self.git_dir)
                .arg("--work-tree")
                .arg(&self.work_tree)
                .args(["-c", "gc.auto=0", "-c", "core.autocrlf=false"])
                .args(args)
                .current_dir(&self.work_tree)
                .output()
                .context("could not run git")?;
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            let error = String::from_utf8_lossy(&output.stderr);
            tries += 1;
            if !error.contains("index.lock") || tries >= LOCK_TRIES {
                bail!("git {} failed: {}", args.join(" "), error.trim());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn write_tree(&self) -> Result<String> {
        if !self.git_dir.exists() {
            let status = Command::new("git").args(["init", "--bare", "-q"]).arg(&self.git_dir).status().context("could not run git")?;
            if !status.success() {
                bail!("git init failed for {}", self.git_dir.display());
            }
        }
        self.git(&["add", "-A", "."])?;
        Ok(self.git(&["write-tree"])?.trim().to_string())
    }

    /// Records the working tree before a turn.
    pub fn take(&mut self) {
        if self.disabled.is_some() {
            return;
        }
        match self.write_tree() {
            Ok(tree) => self.trees.push((tree, crate::live::now())),
            Err(e) => self.disabled = Some(format!("undo is off: {e:#}")),
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
    /// as `git diff --name-status` lines.
    pub fn undo(&mut self) -> Result<Vec<String>> {
        if let Some(why) = &self.disabled {
            bail!("{why}");
        }
        let Some((tree, _)) = self.trees.pop() else { bail!("there is nothing to undo yet") };
        let now = self.write_tree()?;
        let changes: Vec<String> = self.git(&["diff", "--name-status", "--no-renames", &tree, &now])?.lines().map(String::from).collect();
        self.git(&["read-tree", &tree])?;
        for path in self.git(&["ls-files", "--others", "--exclude-standard", "-z"])?.split('\0').filter(|p| !p.is_empty()) {
            let _ = std::fs::remove_file(self.work_tree.join(path));
        }
        self.git(&["checkout-index", "-a", "-f"])?;
        Ok(changes)
    }
}

fn too_many_files(root: &Path) -> bool {
    ignore::WalkBuilder::new(root).hidden(false).filter_entry(|e| e.file_name() != ".git").build().take(MAX_FILES + 1).count()
        > MAX_FILES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        assert!(root.join("target/out").exists(), "ignored files are left alone");
        assert_eq!(changes.len(), 3, "{changes:?}");
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_snapshot_waits_for_another_sessions_index_lock_instead_of_turning_undo_off() {
        let root = std::env::temp_dir().join(format!("killi-snap-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let mut snaps = Snapshots::new(&root);
        snaps.git_dir = root.with_extension("shadow.git");
        let _ = fs::remove_dir_all(&snaps.git_dir);
        snaps.take();
        assert!(snaps.last_taken().is_some());
        let lock = snaps.git_dir.join("index.lock");
        fs::write(&lock, "").unwrap();
        let held = lock.clone();
        let other = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            fs::remove_file(held).unwrap();
        });
        snaps.take();
        other.join().unwrap();
        assert_eq!(snaps.disabled, None);
        assert_eq!(snaps.trees.len(), 2);
        fs::remove_dir_all(&snaps.git_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
