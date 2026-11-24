use super::*;

#[test]
fn a_session_removes_its_index_and_sweeps_those_of_ended_ones() {
    let (root, mut snaps) = scratch_tree("index");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    let git_dir = snaps.git_dir.clone();
    let own = snaps.index_file();
    assert!(own.exists());
    let dead = dead_pid();
    let leaked = git_dir.join(format!("index-{dead}"));
    let stale_lock = git_dir.join(format!("index-{dead}.lock"));
    fs::write(&leaked, "").unwrap();
    fs::write(&stale_lock, "").unwrap();
    gc::sweep_indexes(&git_dir);
    assert!(!leaked.exists() && !stale_lock.exists());
    assert!(own.exists(), "a running session keeps its index");
    drop(snaps);
    assert!(!own.exists(), "a session's index goes with it");
    assert!(git_dir.join("objects").is_dir(), "the repo stays");
    let _ = fs::remove_dir_all(&git_dir);
    let _ = fs::remove_dir_all(root);
}

/// A snapshot repo in `store` for `tree`, as `write_tree` leaves it.
fn store_repo(store: &Path, name: &str, tree: &Path) -> PathBuf {
    let repo = store.join(format!("{name}.git"));
    fs::create_dir_all(repo.join("objects")).unwrap();
    gc::mark(&repo, tree);
    repo
}

fn age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
    fs::File::open(path).unwrap().set_modified(when).unwrap();
}

#[test]
fn pruning_the_store_drops_repos_of_gone_trees_and_keeps_used_ones() {
    let base = std::env::temp_dir().join(format!("killi-snap-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let store = base.join("snapshots");
    let here = base.join("here");
    fs::create_dir_all(&here).unwrap();
    let gone = store_repo(&store, "gone", &base.join("deleted"));
    let kept = store_repo(&store, "kept", &here);
    let used = store_repo(&store, "used", &base.join("deleted-too"));
    fs::write(used.join(format!("index-{}", std::process::id())), "").unwrap();
    let ended = store_repo(&store, "ended", &base.join("deleted-also"));
    fs::write(ended.join(format!("index-{}", dead_pid())), "").unwrap();
    let due = store_repo(&store, "due", &here);
    age(&due.join("killi-gc"), 2);
    let unmarked = store.join("-nowhere-killi-test-unmarked.git");
    fs::create_dir_all(unmarked.join("objects")).unwrap();
    let not_a_repo = store.join("notes.git");
    fs::create_dir_all(&not_a_repo).unwrap();

    let swept = gc::sweep_store(&store);
    assert!(!gone.exists() && !ended.exists());
    let mut dropped = swept.dropped.clone();
    dropped.sort();
    assert_eq!(dropped, [ended, gone]);
    assert!(kept.exists() && due.exists());
    assert!(used.exists(), "a repo a running session uses stays");
    assert!(unmarked.exists(), "an unmarked repo used lately stays");
    assert!(not_a_repo.exists());
    let mut collect = swept.collect.clone();
    collect.sort();
    assert_eq!(
        collect,
        [unmarked.clone(), due],
        "repos whose stamp is old or missing are collected"
    );

    age(&unmarked, 40);
    assert_eq!(gc::sweep_store(&store).dropped, [unmarked]);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn an_unmarked_repo_finds_its_tree_through_dashes_in_names() {
    let base = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("killi-snap-name-{}", std::process::id()));
    let tree = base.join("my-proj").join("sub");
    fs::create_dir_all(&tree).unwrap();
    let name = tree.to_string_lossy().replace('/', "-");
    assert!(gc::named_tree_exists(&name));
    assert!(!gc::named_tree_exists(&format!("{name}-missing")));
    let _ = fs::remove_dir_all(base);
}

#[test]
fn gc_keeps_a_running_sessions_snapshots() {
    let (root, mut snaps) = scratch_tree("gc");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    fs::write(root.join("a.txt"), "two").unwrap();
    gc::collect(&snaps.git_dir).unwrap();
    snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    clean_up(root, &snaps);
}

#[test]
fn a_session_that_ends_starts_its_repos_due_gc_before_it_returns() {
    let (root, mut snaps) = scratch_tree("gc-drop");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    let git_dir = snaps.git_dir.clone();
    let stale = git_bytes(
        &snaps.work(),
        &["hash-object", "-w", "--stdin"],
        Some(b"unreachable for ten days"),
    )
    .unwrap();
    let stale = String::from_utf8(stale).unwrap();
    let stale = git_dir
        .join("objects")
        .join(&stale[..2])
        .join(stale[2..].trim());
    age(&stale, 10);
    age(&git_dir.join("killi-gc"), 2);
    let pid = gc::on_drop(&git_dir, &snaps.index_file()).expect("gc started") as libc::pid_t;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    // SAFETY: signal 0 only checks whether the process exists.
    while (stale.exists() || unsafe { libc::kill(pid, 0) } == 0)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!stale.exists(), "git gc pruned the week-old object");
    assert!(
        gc::on_drop(&git_dir, &snaps.index_file()).is_none(),
        "the gc clock restarted"
    );
    clean_up(root, &snaps);
}
