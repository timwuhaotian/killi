//! Undo: before each turn, the working tree is recorded in a separate git directory under
//! `~/.killi/snapshots/`, through the user's `git`. Their own repository is never touched.
//! Files ignored by `.gitignore` are skipped, so build output doesn't slow it down.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::killi_home;

const MAX_FILES: usize = 50_000;

pub struct Snapshots {
    git_dir: PathBuf,
    work_tree: PathBuf,
    /// Why snapshots are off, if they are.
    pub disabled: Option<String>,
    /// One tree per turn, oldest first: the state just before that turn.
    trees: Vec<String>,
}

impl Snapshots {
    pub fn new(cwd: &Path) -> Self {
        let work_tree = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(cwd).to_path_buf();
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
        if !output.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
            Ok(tree) => self.trees.push(tree),
            Err(e) => self.disabled = Some(format!("undo is off: {e:#}")),
        }
    }

    /// Puts the working tree back as it was before the last turn and returns what changed,
    /// as `git diff --name-status` lines.
    pub fn undo(&mut self) -> Result<Vec<String>> {
        if let Some(why) = &self.disabled {
            bail!("{why}");
        }
        let Some(tree) = self.trees.pop() else { bail!("there is nothing to undo yet") };
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
}
