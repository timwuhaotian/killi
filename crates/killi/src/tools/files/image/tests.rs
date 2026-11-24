use super::*;

#[test]
fn image_bytes_must_match_the_claimed_media_type() {
    let dir = crate::test_temp::TempDir::new("killi-magic");
    let png = dir.join("a.png");
    fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]).unwrap();
    let (url, _) = image_url("a.png", &png, "image/png").unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "{url}");

    let jpeg = dir.join("b.png");
    fs::write(&jpeg, [0xFF, 0xD8, 0xFF, 0xE0, 0x00]).unwrap();
    let err = image_url("b.png", &jpeg, "image/png").unwrap_err();
    assert!(err.contains("JPEG") && err.contains("image/png"), "{err}");

    let lfs = dir.join("c.png");
    fs::write(
        &lfs,
        "version https://git-lfs/spec/v1\noid sha256:abc\nsize 12\n",
    )
    .unwrap();
    let err = image_url("c.png", &lfs, "image/png").unwrap_err();
    assert!(err.contains("Git LFS"), "{err}");

    let text = dir.join("d.png");
    fs::write(&text, "just some words").unwrap();
    let err = image_url("d.png", &text, "image/png").unwrap_err();
    assert!(err.contains("text"), "{err}");
}

#[test]
fn shrink_args_stay_stable() {
    assert_eq!(
        imagemagick_args(),
        ["-resize", "1568x1568>", "-quality", "80"]
    );
    assert_eq!(
        ffmpeg_filter(),
        "scale='min(1568,iw)':'min(1568,ih)':force_original_aspect_ratio=decrease"
    );
}

#[test]
fn shrinkers_take_only_the_first_frame() {
    let (full, out) = (Path::new("/in/a.gif"), Path::new("/out/image.jpg"));
    let args = |command: &Command| -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(args(&imagemagick("magick", full, out))[0], "/in/a.gif[0]");
    let ffmpeg = args(&ffmpeg(full, out));
    assert!(
        ffmpeg.windows(2).any(|pair| pair == ["-frames:v", "1"]),
        "{ffmpeg:?}"
    );
}

#[test]
fn small_images_pass_through_unshrunk() {
    let dir = crate::test_temp::TempDir::new("killi-smallimg");
    let path = dir.join("s.png");
    let bytes = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0x02];
    fs::write(&path, bytes).unwrap();
    let (url, len) = image_url("s.png", &path, "image/png").unwrap();
    assert_eq!(len, bytes.len());
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
}

#[test]
fn each_shrink_gets_its_own_private_folder() {
    use std::os::unix::fs::PermissionsExt;
    let first = ShrinkDir::new().unwrap();
    let second = ShrinkDir::new().unwrap();
    assert_ne!(first.0, second.0);
    let mode = fs::metadata(&first.0).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let path = first.0.clone();
    fs::write(first.out(), "x").unwrap();
    fs::write(path.join("image-1.jpg"), "x").unwrap();
    drop(first);
    assert!(!path.exists());
}

/// A shrinker that never finishes, with a child of its own in its group; it writes both pids
/// to `pids` first.
fn hung(pids: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "sleep 30 & echo $$ $! > '{}'; wait",
        pids.display()
    ));
    command
}

/// A command that exits at once with `code`.
fn exits(code: i32) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!("exit {code}"));
    command
}

/// Whether the hung shrinker has written both its pids yet.
fn started(pids: &Path) -> bool {
    fs::read_to_string(pids).is_ok_and(|s| s.split_whitespace().count() == 2)
}

/// Waits up to five seconds for the hung shrinker to write its pids.
fn wait_started(pids: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started(pids) {
        assert!(Instant::now() < deadline, "the shrinker never started");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `wait_started` while the runtime runs.
async fn wait_started_async(pids: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started(pids) {
        assert!(Instant::now() < deadline, "the shrinker never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether any pid written to `pids` still runs or waits to be reaped.
fn any_left(pids: &Path) -> bool {
    let pids: Vec<libc::pid_t> = fs::read_to_string(pids)
        .unwrap()
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    // SAFETY: signal 0 only checks whether the process exists.
    pids.iter().any(|&pid| unsafe { libc::kill(pid, 0) } == 0)
}

/// Whether the shrinker and its child are gone within two seconds, the time init takes to
/// reap the orphaned child.
fn all_gone(pids: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while any_left(pids) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// `all_gone` while the runtime runs, so it can reap the shrinker it killed.
async fn all_gone_async(pids: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while any_left(pids) {
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

#[test]
fn a_hung_shrinker_is_killed_with_its_group_at_the_deadline() {
    let dir = crate::test_temp::TempDir::new("killi-hung-shrink");
    let pids = dir.join("pids");
    let begun = Instant::now();
    let run = std::thread::spawn({
        let pids = pids.clone();
        move || run_blocking(hung(&pids), Duration::from_secs(1))
    });
    wait_started(&pids);
    assert_eq!(run.join().unwrap(), Ran::TimedOut);
    assert!(begun.elapsed() < Duration::from_secs(5));
    assert!(all_gone(&pids));
    assert_eq!(run_blocking(exits(0), Duration::from_secs(5)), Ran::Done);
    assert_eq!(run_blocking(exits(1), Duration::from_secs(5)), Ran::Failed);
}

#[tokio::test(flavor = "current_thread")]
async fn a_hung_shrinker_does_not_block_the_runtime() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = crate::test_temp::TempDir::new("killi-hung-shrink-async");
    let pids = dir.join("pids");
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker = tokio::spawn({
        let ticks = ticks.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ticks.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    let (ran, ()) = tokio::join!(
        run_async(hung(&pids), Duration::from_secs(1)),
        wait_started_async(&pids)
    );
    ticker.abort();
    assert_eq!(ran, Ran::TimedOut);
    let ticks = ticks.load(Ordering::Relaxed);
    assert!(ticks >= 5, "the runtime stalled: {ticks} ticks");
    assert!(all_gone_async(&pids).await);
    assert_eq!(run_async(exits(0), Duration::from_secs(5)).await, Ran::Done);
}

#[tokio::test(flavor = "current_thread")]
async fn an_interrupted_shrink_kills_its_group() {
    let dir = crate::test_temp::TempDir::new("killi-dropped-shrink");
    let pids = dir.join("pids");
    let run = run_async(hung(&pids), Duration::from_secs(30));
    tokio::select! {
        _ = run => panic!("the shrinker finished"),
        () = wait_started_async(&pids) => {}
    }
    assert!(all_gone_async(&pids).await);
}

/// A PNG over `SHRINK_ABOVE`, made by ImageMagick when it is installed: noise, so it can't
/// compress below it.
fn large_png(path: &Path) -> bool {
    Command::new("magick")
        .args(["-size", "1200x1200", "xc:", "+noise", "Random"])
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
        && fs::metadata(path).is_ok_and(|meta| meta.len() > SHRINK_ABOVE)
}

#[tokio::test(flavor = "current_thread")]
async fn a_large_image_is_shrunk_to_a_jpeg_both_ways() {
    let dir = crate::test_temp::TempDir::new("killi-shrink");
    let path = dir.join("big.png");
    if !large_png(&path) {
        return;
    }
    let size = fs::metadata(&path).unwrap().len();
    let (url, len) = image_url("big.png", &path, "image/png").unwrap();
    assert!(url.starts_with("data:image/jpeg;base64,"), "{}", &url[..40]);
    assert!((len as u64) < size);
    let (url, len) = image_url_async("big.png", &path, "image/png")
        .await
        .unwrap();
    assert!(url.starts_with("data:image/jpeg;base64,"), "{}", &url[..40]);
    assert!((len as u64) < size);
}

/// The width and height a baseline or progressive JPEG's frame header gives.
fn jpeg_size(bytes: &[u8]) -> Option<(u16, u16)> {
    let be16 = |at: usize| Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
    let mut at = 2;
    while *bytes.get(at)? == 0xFF {
        let marker = *bytes.get(at + 1)?;
        if (0xC0..=0xC2).contains(&marker) {
            return Some((be16(at + 7)?, be16(at + 5)?));
        }
        at += 2 + usize::from(be16(at + 2)?);
    }
    None
}

#[test]
fn ffmpeg_shrinks_the_long_side_and_keeps_the_aspect() {
    let dir = crate::test_temp::TempDir::new("killi-ffmpeg-shrink");
    for ((width, height), expected) in [((2001, 1201), (1568, 941)), ((1201, 3001), (628, 1568))] {
        let full = dir.join(format!("{width}x{height}.png"));
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c=red:s={width}x{height}"))
            .args(["-frames:v", "1"])
            .arg(&full)
            .stderr(Stdio::null())
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let out = dir.join(format!("{width}x{height}.jpg"));
        assert_eq!(
            run_blocking(ffmpeg(&full, &out), Duration::from_secs(20)),
            Ran::Done
        );
        assert_eq!(jpeg_size(&fs::read(&out).unwrap()), Some(expected));
    }
}
