//! `killi update`: replaces this binary with the published one after checking its SHA-256,
//! with a live status line while it works when stderr is a terminal.

use std::fs;
use std::future::Future;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;

use crate::tui::banner;
use crate::tui::megabytes;
use crate::tui::text::{DIM, LAGOON, RESET, plain, width, wrap};

pub(crate) mod notes;

/// Where releases are published; `KILLI_RELEASES_URL` points elsewhere, e.g. a local mirror.
/// The raw host directly: `github.com/.../raw/...` only redirects here, and that hop is the
/// one that answers 504 when GitHub's front door is having a bad minute.
const RELEASES_URL: &str = "https://raw.githubusercontent.com/timwuhaotian/killi/main/releases";
/// The release binary and its checksum key for this host.
#[cfg(target_os = "macos")]
const BINARY: &str = "killi-arm64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BINARY: &str = "killi-linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BINARY: &str = "killi-linux-arm64";
#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
const BINARY: &str = "killi-linux-x64";
#[cfg(target_os = "macos")]
const SHA_KEY: &str = "sha256-arm64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SHA_KEY: &str = "sha256-linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SHA_KEY: &str = "sha256-linux-arm64";
#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
const SHA_KEY: &str = "sha256-linux-x64";

/// How long a connection may take to start, and how long the body may stall between
/// reads: without them a stuck server hangs the update for a minute or more.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Tries for one fetch before giving up: a 504 from GitHub is usually gone a second later.
const ATTEMPTS: u32 = 3;
/// The largest download accepted: the binary is a few megabytes, so anything past this
/// is not killi.
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
/// How often the status line is redrawn; the wave advances a frame each time.
const FRAME: Duration = Duration::from_millis(100);
/// The shortest time a download's speed is averaged over, so the first chunk doesn't read as a spike.
const SPEED_WINDOW: Duration = Duration::from_millis(500);
/// The progress bar's widest and narrowest sizes in cells; below the narrowest it is left out.
const BAR: usize = 24;
const MIN_BAR: usize = 8;

pub async fn update() -> Result<()> {
    let mut status = Status::new();
    // Ctrl-C ends the future between awaits, so no staged file is left; dropping `status` then
    // clears the line and shows the cursor again.
    tokio::select! {
        result = run(&mut status) => result,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("cancelled")),
    }
}

async fn run(status: &mut Status) -> Result<()> {
    let base = releases_url();
    let http = http_client()?;
    let (version, sha256) = status
        .during("checking", "for updates", latest(&http, &base))
        .await?;
    let current = env!("CARGO_PKG_VERSION");
    if parse(&version) <= parse(current) {
        status.finish(&format!("killi {current} is up to date."));
        return Ok(());
    }
    let exe = install_with(status, &http, &base, &version, &sha256).await?;
    status.finish(&format!(
        "Updated killi {current} → {version} ({}).",
        exe.display()
    ));
    Ok(())
}

fn releases_url() -> String {
    std::env::var("KILLI_RELEASES_URL").unwrap_or_else(|_| RELEASES_URL.to_string())
}

/// Downloads `version`, checks it against `sha256` and its own `--version`, and puts it in
/// place of the running binary, whose path it returns.
async fn install_with(
    status: &mut Status,
    http: &reqwest::Client,
    base: &str,
    version: &str,
    sha256: &str,
) -> Result<std::path::PathBuf> {
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let staging = exe.with_file_name(format!(".killi-update-{}", std::process::id()));
    let bytes = download(status, http, &format!("{base}/{BINARY}"), version).await?;
    fs::write(&staging, &bytes)
        .with_context(|| format!("could not write {}", staging.display()))?;
    let (sha256, version) = (sha256.to_string(), version.to_string());
    let (staged, target) = (staging.clone(), exe.clone());
    // Checking runs `shasum` and the staged binary: off the runtime's thread, so a session's
    // screen keeps answering while it does.
    let result = tokio::task::spawn_blocking(move || {
        verify_and_install(&staged, &sha256, &version, &target)
    })
    .await
    .map_err(|e| anyhow!("the install check stopped: {e}"))
    .and_then(|result| result);
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result.map(|()| exe)
}

/// A published version newer than this one, offered by a session: what installing it needs,
/// and the changelog's sections from here to it.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    sha256: String,
    /// Each release's heading and its notes, newest first.
    pub notes: Vec<notes::Section>,
}

#[cfg(test)]
impl Release {
    pub fn sample(version: &str) -> Self {
        Self {
            version: version.into(),
            sha256: String::new(),
            notes: Vec::new(),
        }
    }
}

/// Asks the release site for a version newer than this one, with its changelog. The
/// changelog is a courtesy: when it can't be fetched the release comes without notes.
pub async fn check() -> Result<Option<Release>> {
    check_at(&http_client()?, &releases_url(), env!("CARGO_PKG_VERSION")).await
}

async fn check_at(http: &reqwest::Client, base: &str, current: &str) -> Result<Option<Release>> {
    let (version, sha256) = latest(http, base).await?;
    if parse(&version) <= parse(current) {
        return Ok(None);
    }
    let notes = match changelog(http, base).await {
        Ok(text) => notes::between(&text, current, &version),
        Err(_) => Vec::new(),
    };
    Ok(Some(Release {
        version,
        sha256,
        notes,
    }))
}

/// Installs `release` in place of the running binary without drawing anything, for a session
/// that shows its own progress.
pub async fn install(release: Release) -> Result<()> {
    let base = releases_url();
    let http = http_client()?;
    let mut status = Status::silent();
    install_with(
        &mut status,
        &http,
        &base,
        &release.version,
        &release.sha256,
    )
    .await
    .map(|_| ())
}

/// `CHANGELOG.md`, which sits beside the `releases/` folder: one level above `base`.
async fn changelog(http: &reqwest::Client, base: &str) -> Result<String> {
    let url = reqwest::Url::parse(&format!("{}/", base.trim_end_matches('/')))?
        .join("../CHANGELOG.md")?;
    Ok(http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

/// Whether a failed fetch is worth trying again: a 5xx, a 429, or no answer at all.
fn retryable(error: &anyhow::Error) -> bool {
    let Some(error) = error.downcast_ref::<reqwest::Error>() else {
        return false;
    };
    error.is_timeout()
        || error.is_connect()
        || error.is_request()
        || error.is_body()
        || error.status().is_some_and(|status| {
            status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        })
}

/// The pause before try `attempt` (1-based) is repeated: 1s, then 2s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << (attempt - 1))
}

/// The published version and the SHA-256 of its binary, from `latest.toml`.
async fn latest(http: &reqwest::Client, base: &str) -> Result<(String, String)> {
    let url = format!("{base}/latest.toml");
    let mut attempt = 0;
    let text = loop {
        attempt += 1;
        let fetched = async {
            http.get(&url)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
                .map_err(anyhow::Error::from)
        }
        .await;
        match fetched {
            Ok(text) => break text,
            Err(error) if attempt < ATTEMPTS && retryable(&error) => {
                tokio::time::sleep(backoff(attempt)).await;
            }
            Err(error) => {
                return Err(error.context(format!("could not check for updates at {url}")));
            }
        }
    };
    let table: toml::Table = toml::from_str(&text).context("invalid latest.toml")?;
    let field = |key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let (Some(version), Some(sha256)) = (
        field("version"),
        field(SHA_KEY).or_else(|| field("sha256")),
    ) else {
        bail!("latest.toml needs `version` and `{SHA_KEY}`");
    };
    Ok((version, sha256))
}

/// The HTTP client for the version check and the download, bounded on connects and
/// on stalled reads.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .context("could not create an HTTP client")
}

/// The binary at `url`, drawing its progress as it arrives, with `version` as the detail
/// beside the spinner. A transient failure starts the download over; the binary is a few
/// megabytes, so there is nothing to resume.
async fn download(
    status: &mut Status,
    http: &reqwest::Client,
    url: &str,
    version: &str,
) -> Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_once(status, http, url, version).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) if attempt < ATTEMPTS && retryable(&error) => {
                status
                    .during(
                        "downloading",
                        version,
                        tokio::time::sleep(backoff(attempt)),
                    )
                    .await;
            }
            Err(error) => return Err(error.context(format!("could not download {url}"))),
        }
    }
}

/// One try at the binary at `url`.
async fn download_once(
    status: &mut Status,
    http: &reqwest::Client,
    url: &str,
    version: &str,
) -> Result<Vec<u8>> {
    let response = status
        .during("downloading", version, http.get(url).send())
        .await?
        .error_for_status()?;
    let total = response.content_length();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let began = Instant::now();
    let mut tick = tokio::time::interval(FRAME);
    loop {
        tokio::select! {
            chunk = stream.next() => match chunk {
                Some(chunk) => {
                bytes.extend_from_slice(&chunk?);
                if bytes.len() as u64 > MAX_DOWNLOAD {
                    bail!("the download passed the {} MB limit; refusing it", MAX_DOWNLOAD / 1024 / 1024);
                }
            }
                None => return Ok(bytes),
            },
            _ = tick.tick() => {
                let done = bytes.len() as u64;
                let per_second = (u128::from(done) * 1000 / began.elapsed().max(SPEED_WINDOW).as_millis()) as u64;
                status.draw("downloading", version, Some(Transfer { done, total, per_second }));
            }
        }
    }
}

fn verify_and_install(
    staging: &std::path::Path,
    sha256: &str,
    version: &str,
    exe: &std::path::Path,
) -> Result<()> {
    let output = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(staging)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(staging).output())?;
    let actual = String::from_utf8_lossy(&output.stdout);
    let actual = actual.split_whitespace().next().unwrap_or_default();
    if !actual.eq_ignore_ascii_case(sha256) {
        bail!("checksum mismatch: expected {sha256}, downloaded {actual}");
    }
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;
    check_version(staging, version)?;
    fs::rename(staging, exe).with_context(|| format!("could not replace {}", exe.display()))
}

/// Runs the staged binary and requires it to say the published version: a download that
/// matches the checksum but isn't killi is refused before it replaces the binary.
fn check_version(staging: &std::path::Path, expected: &str) -> Result<()> {
    let output = Command::new(staging).arg("--version").output()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() && text.trim() == format!("killi {expected}") {
        return Ok(());
    }
    bail!("the download reports {text:?} instead of version {expected}; refusing to install it")
}

fn parse(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// How far a download has got.
#[derive(Clone, Copy)]
struct Transfer {
    done: u64,
    total: Option<u64>,
    per_second: u64,
}

/// The live line on stderr. It does nothing when stderr isn't a terminal that takes escape
/// codes, so piped output stays plain.
struct Status {
    live: bool,
    started: Instant,
    drawn: bool,
}

impl Status {
    fn new() -> Self {
        let live = std::io::stderr().is_terminal()
            && std::env::var("TERM").is_ok_and(|term| term != "dumb");
        Self {
            live,
            started: Instant::now(),
            drawn: false,
        }
    }

    /// A status that never draws, for an update a session runs under its own screen.
    fn silent() -> Self {
        Self {
            live: false,
            started: Instant::now(),
            drawn: false,
        }
    }

    /// Redraws the line in one write, inside a synchronized update with the cursor hidden.
    fn draw(&mut self, word: &str, detail: &str, transfer: Option<Transfer>) {
        if !self.live {
            return;
        }
        let frame = (self.started.elapsed().as_millis() / FRAME.as_millis()) as usize;
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        let line = plain(&live_line(frame, word, detail, transfer, columns)).into_owned();
        let _ = std::io::stderr()
            .write_all(format!("\x1b[?2026h\x1b[?25l\r\x1b[2K{line}\x1b[?2026l").as_bytes());
        self.drawn = true;
    }

    /// Erases the line and shows the cursor again.
    fn clear(&mut self) {
        if std::mem::take(&mut self.drawn) {
            let _ = std::io::stderr().write_all(b"\r\x1b[2K\x1b[?25h");
        }
    }

    /// Animates the action `word` and its `detail` until `work` is done.
    async fn during<T>(&mut self, word: &str, detail: &str, work: impl Future<Output = T>) -> T {
        let mut tick = tokio::time::interval(FRAME);
        tokio::pin!(work);
        loop {
            tokio::select! {
                output = &mut work => return output,
                _ = tick.tick() => self.draw(word, detail, None),
            }
        }
    }

    /// Replaces the live line with the outcome, marked as done on a terminal.
    fn finish(&mut self, message: &str) {
        self.clear();
        if self.live {
            println!("{}", plain(&format!("{LAGOON}✓{RESET} {message}")));
        } else {
            println!("{message}");
        }
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The status line `frame` tenths of a second in, cut to fit `columns`: the session's
/// working island — the ice cube and the action word — its detail beside it, and for a
/// download its progress.
fn live_line(
    frame: usize,
    word: &str,
    detail: &str,
    transfer: Option<Transfer>,
    columns: usize,
) -> String {
    let mut line = banner::working_island(frame, word);
    if !detail.is_empty() {
        line.push_str(&format!(" {detail}"));
    }
    if let Some(transfer) = transfer {
        line.push_str(&progress(
            transfer,
            columns.saturating_sub(1 + width(&line)),
        ));
    }
    wrap(&line, columns.saturating_sub(1))
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// A download's bar, sizes and speed, each led by two spaces, in `room` columns. The speed, then
/// the sizes, go first when the bar would get too short.
fn progress(
    Transfer {
        done,
        total,
        per_second,
    }: Transfer,
    room: usize,
) -> String {
    let sizes = match total {
        Some(total) => format!("{} / {}", megabytes(done), megabytes(total)),
        None => megabytes(done),
    };
    let mut figures = vec![sizes, format!("{DIM}{}/s{RESET}", megabytes(per_second))];
    let cells = |figures: &[String]| {
        room.saturating_sub(
            2 + figures
                .iter()
                .map(|figure| 2 + width(figure))
                .sum::<usize>(),
        )
        .min(BAR)
    };
    while total.is_some() && cells(&figures) < MIN_BAR && figures.pop().is_some() {}
    let mut out = String::new();
    if let Some(total) = total.filter(|_| cells(&figures) >= MIN_BAR) {
        out.push_str(&format!(
            "  {}",
            banner::bar(done as f32 / total.max(1) as f32, cells(&figures))
        ));
    }
    for figure in figures {
        out.push_str(&format!("  {figure}"));
    }
    out
}

#[cfg(test)]
mod tests;
