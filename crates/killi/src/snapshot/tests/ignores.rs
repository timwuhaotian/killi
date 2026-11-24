use super::*;

#[test]
fn undo_keeps_a_file_the_turn_unignored() {
    let (root, mut snaps) = scratch_tree("unignored");
    fs::write(root.join(".gitignore"), ".env\ndata/\n*.log\n").unwrap();
    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::create_dir_all(root.join("data/raw")).unwrap();
    fs::write(root.join("data/raw/rows.csv"), "1,2").unwrap();
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(root.join("a.txt"), "two").unwrap();
    fs::write(root.join("new.txt"), "made by the turn").unwrap();
    let changes = snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join(".env")).unwrap(), "SECRET=1");
    assert_eq!(
        fs::read_to_string(root.join("data/raw/rows.csv")).unwrap(),
        "1,2"
    );
    assert_eq!(
        fs::read_to_string(root.join(".gitignore")).unwrap(),
        ".env\ndata/\n*.log\n"
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    assert!(!root.join("new.txt").exists(), "a file the turn made goes");
    assert_eq!(
        changes,
        ["M\t.gitignore", "M\ta.txt", "A\tnew.txt"],
        "kept files aren't listed as changed back"
    );
    clean_up(root, &snaps);
}

#[test]
fn undo_keeps_files_unignored_by_a_deleted_gitignore_in_a_subfolder() {
    let (root, mut snaps) = scratch_tree("unignored-sub");
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app/.gitignore"), "local.toml\n").unwrap();
    fs::write(root.join("app/local.toml"), "key = 1").unwrap();
    snaps.take();
    fs::remove_file(root.join("app/.gitignore")).unwrap();
    fs::write(root.join("app/made.toml"), "x").unwrap();
    snaps.undo().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("app/local.toml")).unwrap(),
        "key = 1"
    );
    assert!(root.join("app/.gitignore").exists());
    assert!(!root.join("app/made.toml").exists());
    clean_up(root, &snaps);
}

#[test]
fn undo_keeps_files_a_deleted_self_ignoring_gitignore_ignored() {
    let (root, mut snaps) = scratch_tree("self-ignored-sub");
    fs::create_dir_all(root.join("secrets")).unwrap();
    fs::write(root.join("secrets/.gitignore"), "*\n").unwrap();
    fs::write(root.join("secrets/key.pem"), "PRECIOUS").unwrap();
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    fs::remove_file(root.join("secrets/.gitignore")).unwrap();
    fs::write(root.join("a.txt"), "two").unwrap();
    fs::write(root.join("secrets/made.pem"), "x").unwrap();
    snaps.undo().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("secrets/key.pem")).unwrap(),
        "PRECIOUS"
    );
    assert_eq!(
        fs::read_to_string(root.join("secrets/made.pem")).unwrap(),
        "x",
        "the deleted rules ignored it too, so undo can't tell it is new"
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    clean_up(root, &snaps);
}

#[test]
fn undo_keeps_files_a_root_gitignore_that_ignores_itself_stopped_ignoring() {
    let (root, mut snaps) = scratch_tree("self-ignored-root");
    fs::write(root.join(".gitignore"), ".gitignore\n.env\n").unwrap();
    fs::write(root.join(".env"), "SECRET").unwrap();
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    let taken = &snaps.trees[0].as_ref().unwrap().0;
    assert_eq!(
        taken
            .ignores
            .iter()
            .map(|(path, _)| path.as_slice())
            .collect::<Vec<_>>(),
        [b".gitignore".as_slice()]
    );
    fs::write(root.join(".gitignore"), ".gitignore\n").unwrap();
    fs::write(root.join("new.txt"), "made by the turn").unwrap();
    snaps.undo().unwrap();
    assert_eq!(fs::read_to_string(root.join(".env")).unwrap(), "SECRET");
    assert!(!root.join("new.txt").exists(), "a file the turn made goes");
    clean_up(root, &snaps);
}

#[test]
fn undo_follows_negated_ignore_rules() {
    let (root, mut snaps) = scratch_tree("negated");
    fs::write(root.join(".gitignore"), "*.log\n!keep.log\n").unwrap();
    fs::write(root.join("debug.log"), "before").unwrap();
    fs::write(root.join("keep.log"), "tracked").unwrap();
    snaps.take();
    fs::write(root.join(".gitignore"), "").unwrap();
    fs::write(root.join("new.log"), "turn made").unwrap();
    fs::write(root.join("new.txt"), "turn made").unwrap();
    snaps.undo().unwrap();
    assert!(root.join("debug.log").exists() && root.join("keep.log").exists());
    assert!(
        root.join("new.log").exists(),
        "the snapshot's rules ignore it"
    );
    assert!(!root.join("new.txt").exists());
    clean_up(root, &snaps);
}
