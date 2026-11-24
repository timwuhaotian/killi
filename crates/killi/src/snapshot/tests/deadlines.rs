use super::*;

#[test]
fn a_command_past_its_deadline_is_stopped_with_what_it_started() {
    let dir = std::env::temp_dir().join(format!("killi-snap-deadline-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("pid");
    let started = std::time::Instant::now();
    let error = deadline::output(
        Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$0\"; wait")
            .arg(&pid_file),
        None,
        Duration::from_millis(300),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(
        error.downcast_ref::<deadline::TimedOut>().is_some(),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let child: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks whether the process exists.
    while unsafe { libc::kill(child, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: as above.
    assert_ne!(
        unsafe { libc::kill(child, 0) },
        0,
        "the background sleep was killed too"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_command_gets_its_input_and_returns_more_than_a_pipe_holds() {
    let echoed = deadline::output(
        &mut Command::new("cat"),
        Some(b"a\0b\0".to_vec()),
        Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(echoed.stdout, b"a\0b\0");
    let long = deadline::output(
        Command::new("head").args(["-c", "1000000", "/dev/zero"]),
        None,
        Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(long.status.success());
    assert_eq!(long.stdout.len(), 1_000_000);
}

#[test]
fn a_snapshot_after_one_stopped_at_its_deadline_succeeds() {
    let (root, mut snaps) = scratch_tree("timed-out");
    snaps.deadline = Duration::from_secs(1);
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    assert_eq!(snaps.skipped, None);
    let config = snaps.git_dir.join("config");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str("[filter \"slow\"]\n\tclean = sleep 30\n");
    fs::write(&config, text).unwrap();
    fs::write(root.join(".gitattributes"), "*.new filter=slow\n").unwrap();
    fs::write(root.join("b.new"), "new").unwrap();
    snaps.take();
    assert!(
        snaps
            .skipped
            .as_deref()
            .is_some_and(|why| why.contains("was stopped")),
        "{:?}",
        snaps.skipped
    );
    assert!(
        !snaps.git_dir.join(snaps.lock_name()).exists(),
        "the stopped git's lock on the index is gone"
    );
    fs::remove_file(root.join(".gitattributes")).unwrap();
    snaps.take();
    assert_eq!(snaps.skipped, None);
    assert_eq!(snaps.trees.len(), 3);
    clean_up(root, &snaps);
}

#[test]
fn a_snapshot_given_up_on_stops_its_git_and_leaves_no_lock() {
    let (root, mut snaps) = scratch_tree("canceled");
    fs::write(root.join("a.txt"), "one").unwrap();
    snaps.take();
    assert_eq!(snaps.skipped, None);
    let config = snaps.git_dir.join("config");
    let mut text = fs::read_to_string(&config).unwrap();
    let pid_file = root.with_extension("filter-pid");
    let _ = fs::remove_file(&pid_file);
    text.push_str(&format!(
        "[filter \"slow\"]\n\tclean = \"echo $$ > '{}'; sleep 30\"\n",
        pid_file.display()
    ));
    fs::write(&config, text).unwrap();
    fs::write(root.join(".gitattributes"), "*.new filter=slow\n").unwrap();
    fs::write(root.join("b.new"), "new").unwrap();
    let work = snaps.start_take().expect("enabled");
    let cancel = work.canceler();
    let started = std::time::Instant::now();
    let running = std::thread::spawn(move || work.run());
    let waited = std::time::Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && std::time::Instant::now() < waited {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pid_file.exists(), "the clean filter started");
    cancel.cancel();
    let error = running.join().unwrap().err().expect("it was given up on");
    assert!(
        error.downcast_ref::<deadline::Canceled>().is_some(),
        "{error:#}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "not tried again"
    );
    assert!(
        !snaps.git_dir.join(snaps.lock_name()).exists(),
        "the stopped git's lock on the index is gone"
    );
    let filter: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let waited = std::time::Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks whether the process exists.
    while unsafe { libc::kill(filter, 0) } == 0 && std::time::Instant::now() < waited {
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: as above.
    assert_ne!(
        unsafe { libc::kill(filter, 0) },
        0,
        "the clean filter git started was stopped too"
    );
    snaps.commit_take(Err(error));
    fs::remove_file(root.join(".gitattributes")).unwrap();
    snaps.take();
    assert_eq!(snaps.skipped, None);
    assert_eq!(snaps.trees.len(), 3);
    let _ = fs::remove_file(pid_file);
    clean_up(root, &snaps);
}

#[test]
fn work_given_up_on_before_it_starts_runs_nothing() {
    let (root, mut snaps) = scratch_tree("canceled-early");
    fs::write(root.join("a.txt"), "one").unwrap();
    let work = snaps.start_take().expect("enabled");
    work.canceler().cancel();
    let error = work.run().err().expect("it was given up on");
    assert!(deadline::stopped(&error), "{error:#}");
    assert!(
        !snaps.git_dir.join("HEAD").exists(),
        "not even git init ran"
    );
    clean_up(root, &snaps);
}
