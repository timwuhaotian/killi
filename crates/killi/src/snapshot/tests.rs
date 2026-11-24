use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
fn a_failed_snapshot_makes_undo_refuse_once_without_turning_undo_off() {
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
    assert_eq!(snaps.trees.len(), 2, "the turn keeps its place");
    assert!(
        snaps.last_taken().is_some(),
        "the turn before still has one"
    );
    let refused = snaps.undo().unwrap_err().to_string();
    assert!(
        refused.contains("the last turn has no undo snapshot"),
        "{refused}"
    );
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).unwrap(),
        "two",
        "nothing is restored past the turn without a snapshot"
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

/// A work tree under the temp folder with a `.git` folder in it, and its snapshots kept in a
/// repo beside it.
fn scratch_tree(name: &str) -> (PathBuf, Snapshots) {
    let root = std::env::temp_dir().join(format!("killi-snap-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let mut snaps = Snapshots::new(&root);
    snaps.git_dir = root.with_extension("shadow.git");
    let _ = fs::remove_dir_all(&snaps.git_dir);
    (root, snaps)
}

fn clean_up(root: PathBuf, snaps: &Snapshots) {
    let _ = fs::remove_dir_all(&snaps.git_dir);
    let _ = fs::remove_dir_all(root);
}

/// A pid no process has: one that just ended.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn turns_without_a_snapshot_are_refused_together() {
    let (root, mut snaps) = scratch_tree("refuse-two");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.commit_take(Err(anyhow::anyhow!("index locked")));
    let refused = snaps.undo().unwrap_err().to_string();
    assert!(refused.contains("no turn before it has one"), "{refused}");
    assert_eq!(snaps.taken(), 0);
    assert!(snaps.last_taken().is_none());
    snaps.take();
    fs::write(root.join("a.txt"), "two").unwrap();
    snaps.commit_take(Err(anyhow::anyhow!("index locked")));
    snaps.commit_take(Err(anyhow::anyhow!("index locked")));
    assert_eq!(snaps.taken(), 3);
    let refused = snaps.undo().unwrap_err().to_string();
    assert!(
        refused.contains("the last 2 turns have no undo snapshot"),
        "{refused}"
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
    snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    clean_up(root, &snaps);
}

#[test]
fn a_taken_back_turn_forgets_its_missing_snapshot_too() {
    let (root, mut snaps) = scratch_tree("forget-marker");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    let taken = snaps.taken();
    snaps.commit_take(Err(anyhow::anyhow!("index locked")));
    snaps.forget_after(taken);
    fs::write(root.join("a.txt"), "two").unwrap();
    snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    clean_up(root, &snaps);
}

/// A sparse file over `BIG_FILE`: as big as git would see it, without writing it out.
fn big_file(path: &Path) {
    fs::File::create(path)
        .unwrap()
        .set_len(BIG_FILE + 1)
        .unwrap();
}

#[test]
fn a_big_file_is_left_out_of_snapshots_and_survives_undo() {
    let (root, mut snaps) = scratch_tree("big");
    fs::write(root.join("a.txt"), "one").unwrap();
    big_file(&root.join("old.bin"));
    big_file(&root.join("shrinks.bin"));
    fs::write(root.join("grows.bin"), "small").unwrap();
    snaps.take();
    let tree = snaps.trees[0].as_ref().unwrap().0.tree.clone();
    let listed = snaps.git(&["ls-tree", "-r", "--name-only", &tree]).unwrap();
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        ["a.txt", "grows.bin"],
        "big files aren't copied"
    );
    fs::write(root.join("a.txt"), "two").unwrap();
    big_file(&root.join("new.bin"));
    fs::write(root.join("shrinks.bin"), "now small").unwrap();
    big_file(&root.join("grows.bin"));
    snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    assert_eq!(
        fs::metadata(root.join("old.bin")).unwrap().len(),
        BIG_FILE + 1
    );
    assert_eq!(
        fs::metadata(root.join("new.bin")).unwrap().len(),
        BIG_FILE + 1,
        "undo never deletes a big file"
    );
    assert_eq!(
        fs::read_to_string(root.join("shrinks.bin")).unwrap(),
        "now small",
        "a file the snapshot left out for its size isn't deleted"
    );
    assert_eq!(
        fs::read_to_string(root.join("grows.bin")).unwrap(),
        "small",
        "a file the snapshot has goes back"
    );
    clean_up(root, &snaps);
}

mod deadlines;
mod ignores;
mod store;
