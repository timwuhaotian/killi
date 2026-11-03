use super::*;

const BROWSER_TIMEOUT: Duration = Duration::from_secs(45);

/// The headless renderer: `KILLI_MOLI` first, then `~/.local/bin/moli` (the installer's
/// default spot), then `moli` on `PATH`. killi bundles no browser: moli does the headless
/// work when it is around, and the Chrome-family shell-out below stays as the fallback,
/// so a machine without moli keeps working.
fn moli() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KILLI_MOLI")
        .map(PathBuf::from)
        .filter(|p| p.exists())
    {
        return Some(path);
    }
    let local = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/bin/moli"))
        .filter(|path| path.exists());
    if local.is_some() {
        return local;
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("moli"))
            .find(|path| path.exists())
    })
}

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

/// A running browser, and its throwaway profile folder when the command needs one.
/// Dropping it, also when the call is interrupted and its future dropped, kills the browser
/// and the helpers it started and removes the profile. The group is tracked while it runs,
/// so quitting on a signal ends it too.
pub(super) struct Running {
    pub(super) child: tokio::process::Child,
    profile: Option<PathBuf>,
    _tracked: Option<Tracked>,
}

impl Running {
    pub(super) fn start(command: &mut Command, profile: Option<PathBuf>) -> Result<Self, String> {
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
        if let Some(profile) = &self.profile {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

/// The page's DOM after its scripts have run. Moli renders it when it is around, headless
/// Chrome otherwise.
pub(super) async fn render(url: &str) -> Result<String, String> {
    if moli().is_some() {
        return moli_render(url).await;
    }
    chrome_render(url).await
}

/// One-shot `moli fetch` with `extra`, on `url`: moli exits on its own, so unlike the
/// Chrome shell-out below there is nothing to stop early. Returns stdout with stderr beside
/// it, which only names a failure. The whole run shares one deadline with moli's own
/// `--timeout`, which sits inside it.
async fn moli_fetch(extra: &[&str], url: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    use tokio::io::AsyncReadExt;
    let moli = moli().expect("the caller checked moli is around");
    let mut command = Command::new(moli);
    command
        .arg("fetch")
        .args(extra)
        .args(["--wait", "done", "--timeout", "40000"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .own_session()
        .kill_on_drop(true);
    let mut browser = Running::start(&mut command, None)?;
    let mut stdout = browser.child.stdout.take().expect("piped stdout");
    let mut stderr = browser.child.stderr.take().expect("piped stderr");
    let read = async {
        let mut out = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
            }
        }
        let mut said = Vec::new();
        let _ = stderr.read_to_end(&mut said).await;
        (out, said)
    };
    let output = tokio::time::timeout(BROWSER_TIMEOUT, read)
        .await
        .map_err(|_| {
            format!(
                "Moli took more than {} s on {url}.",
                BROWSER_TIMEOUT.as_secs()
            )
        })?;
    drop(browser);
    Ok(output)
}

/// Moli's stderr, trimmed and capped, as a sentence hung on a failure — or nothing.
fn moli_said(said: &[u8]) -> String {
    let said = String::from_utf8_lossy(said);
    let said = truncate(&said, 300).trim();
    if said.is_empty() {
        String::new()
    } else {
        format!(" It said: {said}")
    }
}

/// `moli fetch` runs the page's JavaScript and prints the live DOM as HTML.
async fn moli_render(url: &str) -> Result<String, String> {
    let (out, said) = moli_fetch(&["--dump", "html"], url).await?;
    let dom = String::from_utf8_lossy(&out).into_owned();
    if dom.trim().is_empty() {
        return Err(format!("Moli returned nothing for {url}.{}", moli_said(&said)));
    }
    Ok(dom)
}

/// Chrome prints the DOM but often doesn't exit afterwards, so this stops reading at the
/// closing `</html>`.
async fn chrome_render(url: &str) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let (mut command, profile) = browser(url, &["--dump-dom".to_string()])?;
    let mut browser = Running::start(command.stdout(Stdio::piped()), Some(profile))?;
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

/// Takes a screenshot into `file`. Moli prints a viewport PNG when it is around, headless
/// Chrome otherwise.
async fn capture(url: &str, file: &std::path::Path) -> Result<(), String> {
    if moli().is_some() {
        return moli_capture(url, file).await;
    }
    chrome_capture(url, file).await
}

/// `moli fetch --layout --dump screenshot` prints a viewport PNG.
async fn moli_capture(url: &str, file: &std::path::Path) -> Result<(), String> {
    let (png, said) = moli_fetch(&["--layout", "--dump", "screenshot"], url).await?;
    if png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return std::fs::write(file, &png)
            .map_err(|e| format!("Could not save the screenshot of {url}: {e}"));
    }
    Err(format!(
        "Moli made no screenshot of {url}.{}",
        moli_said(&said)
    ))
}

/// Chrome writes the file but then often doesn't exit, so this waits for the file to stop
/// growing and kills the browser.
async fn chrome_capture(url: &str, file: &std::path::Path) -> Result<(), String> {
    let extra = [
        format!("--screenshot={}", file.display()),
        "--window-size=1280,900".to_string(),
    ];
    let (mut command, profile) = browser(url, &extra)?;
    let mut browser = Running::start(&mut command, Some(profile))?;
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
                output.text =
                    format!("Screenshot of {url} (top of the page); it is attached below.");
            }
            output
        }
        Err(e) => Output::error(e),
    }
}
