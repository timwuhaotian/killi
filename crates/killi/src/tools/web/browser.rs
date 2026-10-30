use super::*;

const BROWSER_TIMEOUT: Duration = Duration::from_secs(45);

/// The browser killi can drive for pages that need JavaScript and for screenshots.
fn chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KILLI_CHROME")
        .map(PathBuf::from)
        .filter(|p| p.exists())
    {
        return Some(path);
    }
    [
        "Google Chrome",
        "Chromium",
        "Microsoft Edge",
        "Brave Browser",
    ]
    .iter()
    .map(|name| PathBuf::from(format!("/Applications/{name}.app/Contents/MacOS/{name}")))
    .find(|path| path.exists())
}

/// Headless Chrome on `url` with a throwaway profile, so a running Chrome is left alone, and
/// in its own session so the helpers it starts can be killed with it.
fn browser(url: &str, extra: &[String]) -> Result<(Command, PathBuf), String> {
    let Some(chrome) = chrome() else {
        return Err(
            "No Chrome, Chromium, Edge or Brave found in /Applications (or KILLI_CHROME).".into(),
        );
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let profile = std::env::temp_dir().join(format!("killi-chrome-{}-{nanos}", std::process::id()));
    let mut command = Command::new(chrome);
    command
        .args([
            "--headless=new",
            "--use-angle=swiftshader",
            "--disable-software-rasterizer",
            "--disable-gpu",
        ])
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-scrollbars",
            "--virtual-time-budget=8000",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(extra)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .own_session()
        .kill_on_drop(true);
    Ok((command, profile))
}

/// A running browser and its profile folder. Dropping it, also when the call is interrupted
/// and its future dropped, kills the browser and the helpers it started and removes the profile.
/// The group is tracked while it runs, so quitting on a signal ends it too.
pub(super) struct Running {
    pub(super) child: tokio::process::Child,
    profile: PathBuf,
    _tracked: Option<Tracked>,
}

impl Running {
    pub(super) fn start(command: &mut Command, profile: PathBuf) -> Result<Self, String> {
        let child = command
            .spawn()
            .map_err(|e| format!("Could not run the browser: {e}"))?;
        let tracked = child.id().map(track);
        Ok(Self {
            child,
            profile,
            _tracked: tracked,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: the child leads its own process group, so this reaches only the browser.
            unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// The page's DOM after its scripts have run. Chrome prints the DOM but often doesn't exit
/// afterwards, so this stops reading at the closing `</html>`.
pub(super) async fn render(url: &str) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let (mut command, profile) = browser(url, &["--dump-dom".to_string()])?;
    let mut browser = Running::start(command.stdout(Stdio::piped()), profile)?;
    let mut stdout = browser.child.stdout.take().expect("piped stdout");
    let mut dom = Vec::new();
    let read = async {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => dom.extend_from_slice(&chunk[..n]),
            }
            if dom.trim_ascii_end().ends_with(b"</html>") {
                break;
            }
        }
    };
    let timed_out = tokio::time::timeout(BROWSER_TIMEOUT, read).await.is_err();
    drop(browser);
    let dom = String::from_utf8_lossy(&dom).into_owned();
    match (dom.trim().is_empty(), timed_out) {
        (true, true) => Err(format!(
            "The browser took more than {} s on {url}.",
            BROWSER_TIMEOUT.as_secs()
        )),
        (true, false) => Err(format!("The browser returned nothing for {url}.")),
        _ => Ok(dom),
    }
}

/// Takes a screenshot into `file`. Chrome writes the file but then often doesn't exit, so
/// this waits for the file to stop growing and kills the browser.
async fn capture(url: &str, file: &std::path::Path) -> Result<(), String> {
    let extra = [
        format!("--screenshot={}", file.display()),
        "--window-size=1280,900".to_string(),
    ];
    let (mut command, profile) = browser(url, &extra)?;
    let mut browser = Running::start(&mut command, profile)?;
    let started = std::time::Instant::now();
    let mut last = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let size = std::fs::metadata(file).map_or(0, |m| m.len());
        if size > 0 && size == last {
            break Ok(());
        }
        last = size;
        if browser
            .child
            .try_wait()
            .is_ok_and(|status| status.is_some())
        {
            break if file.exists() {
                Ok(())
            } else {
                Err(format!("The browser made no screenshot of {url}."))
            };
        }
        if started.elapsed() > BROWSER_TIMEOUT {
            break Err(format!(
                "The browser took more than {} s on {url}.",
                BROWSER_TIMEOUT.as_secs()
            ));
        }
    }
}

/// A file removed when it goes out of scope, also if the call that made it is interrupted.
pub(super) struct Temp(pub(super) PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) async fn screenshot(ctx: &Context, url: &str) -> Output {
    if !ctx.images.get() {
        return Output::error("This model can't see images, so a screenshot is no use.");
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let file =
        Temp(std::env::temp_dir().join(format!("killi-shot-{}-{nanos}.png", std::process::id())));
    match capture(url, &file.0).await {
        Ok(()) => {
            let mut output = files::read_image(ctx, url, &file.0, "image/png");
            if !output.is_error {
                output.text = format!(
                    "Screenshot of {url} (1280x900, top of the page); it is attached below."
                );
            }
            output
        }
        Err(e) => Output::error(e),
    }
}
