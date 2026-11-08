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

/// Where releases are published; `KILLI_RELEASES_URL` points elsewhere, e.g. a local mirror.
/// The raw host directly: `github.com/.../raw/...` only redirects here, and that hop is the
/// one that answers 504 when GitHub's front door is having a bad minute.
const RELEASES_URL: &str = "https://raw.githubusercontent.com/timwuhaotian/killi/main/releases";
const BINARY: &str = "killi-arm64";

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
    let base = std::env::var("KILLI_RELEASES_URL").unwrap_or_else(|_| RELEASES_URL.to_string());
    let http = http_client()?;
    let (version, sha256) = status
        .during("checking", "for updates", latest(&http, &base))
        .await?;
    let current = env!("CARGO_PKG_VERSION");
    if parse(&version) <= parse(current) {
        status.finish(&format!("killi {current} is up to date."));
        return Ok(());
    }

    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let staging = exe.with_file_name(format!(".killi-update-{}", std::process::id()));
    let bytes = download(status, &http, &format!("{base}/{BINARY}"), &version).await?;
    fs::write(&staging, &bytes)
        .with_context(|| format!("could not write {}", staging.display()))?;
    let result = verify_and_install(&staging, &sha256, &version, &exe);
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result?;
    status.finish(&format!(
        "Updated killi {current} → {version} ({}).",
        exe.display()
    ));
    Ok(())
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
    let (Some(version), Some(sha256)) = (field("version"), field("sha256")) else {
        bail!("latest.toml needs `version` and `sha256`");
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
    let output = Command::new("/usr/bin/shasum")
        .arg("-a")
        .arg("256")
        .arg(staging)
        .output()?;
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
/// working island — the snowflake and the action word — its detail beside it, and for a
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
mod tests {
    use super::*;

    #[test]
    fn only_the_published_version_is_installed() {
        let tmp = crate::test_temp::TempDir::new("killi-update-version");
        let right = tmp.join("killi-right");
        let wrong = tmp.join("killi-wrong");
        std::fs::write(&right, "#!/bin/sh\necho 'killi 0.2.31'\n").unwrap();
        std::fs::write(&wrong, "#!/bin/sh\necho 'something else'\n").unwrap();
        for path in [&right, &wrong] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(super::check_version(&right, "0.2.31").is_ok());
        let longer = tmp.join("killi-longer");
        std::fs::write(&longer, "#!/bin/sh\necho 'killi 0.2.310'\n").unwrap();
        std::fs::set_permissions(&longer, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            super::check_version(&longer, "0.2.31").is_err(),
            "a version that only starts with the expected one is not it"
        );
        let err = super::check_version(&wrong, "0.2.31")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("something else") && err.contains("0.2.31"),
            "{err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_download_past_the_limit_is_refused() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut discard = [0u8; 1024];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\n\r\n")
                .await;
            let chunk = vec![b'a'; 1024 * 1024];
            for _ in 0..70 {
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
            }
        });
        let http = super::http_client().unwrap();
        let mut status = super::Status::new();
        let err = format!(
            "{:#}",
            super::download(&mut status, &http, &url, "9.9.9")
                .await
                .unwrap_err()
        );
        assert!(err.contains("64 MB"), "{err}");
    }

    /// Serves each response once, in order, and returns the base URL.
    async fn serve(responses: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut discard = [0u8; 1024];
                let _ = socket.read(&mut discard).await;
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        url
    }

    fn respond(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_504_checking_for_updates_is_retried() {
        let base = serve(vec![
            respond("504 Gateway Timeout", ""),
            respond("504 Gateway Timeout", ""),
            respond("200 OK", "version = \"9.9.9\"\nsha256 = \"abc\"\n"),
        ])
        .await;
        let http = super::http_client().unwrap();
        assert_eq!(
            super::latest(&http, &base).await.unwrap(),
            ("9.9.9".to_string(), "abc".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_504_download_is_retried_from_the_start() {
        let base = serve(vec![
            respond("504 Gateway Timeout", ""),
            respond("200 OK", "bin"),
        ])
        .await;
        let http = super::http_client().unwrap();
        let mut status = super::Status::new();
        let bytes = super::download(&mut status, &http, &format!("{base}/killi-arm64"), "9.9.9")
            .await
            .unwrap();
        assert_eq!(bytes, b"bin");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_404_is_not_retried() {
        let base = serve(vec![respond("404 Not Found", "")]).await;
        let http = super::http_client().unwrap();
        let err = super::latest(&http, &base).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("could not check for updates"),
            "{err:#}"
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(super::parse("0.10.0") > super::parse("0.9.3"));
        assert!(super::parse("0.1.0") == super::parse("0.1.0"));
    }

    fn transfer(done: u64, total: Option<u64>) -> Option<Transfer> {
        Some(Transfer {
            done,
            total,
            per_second: 2_300_000,
        })
    }

    #[test]
    fn the_status_line_always_fits_the_terminal() {
        for columns in 1..=140 {
            for transfer in [
                None,
                transfer(0, Some(6_700_000)),
                transfer(4_100_000, Some(6_700_000)),
                transfer(4_100_000, None),
            ] {
                let line = live_line(7, "downloading", "0.2.31", transfer, columns);
                assert!(width(&line) < columns.max(2), "{columns} columns: {line:?}");
            }
        }
    }

    #[test]
    fn a_download_shows_bar_sizes_and_speed_until_the_terminal_is_too_narrow() {
        let at = |columns| {
            live_line(
                30,
                "downloading",
                "0.2.31",
                transfer(4_100_000, Some(6_700_000)),
                columns,
            )
        };
        let wide = at(100);
        assert!(
            wide.contains("4.1 MB / 6.7 MB")
                && wide.contains("2.3 MB/s")
                && wide.contains('━')
                && wide.contains('─')
        );
        let narrow = at(55);
        assert!(
            narrow.contains("4.1 MB / 6.7 MB") && !narrow.contains("MB/s") && narrow.contains('━'),
            "{narrow:?}"
        );
        let tiny = at(30);
        assert!(!tiny.contains('━') && !tiny.contains("MB"), "{tiny:?}");
    }

    #[test]
    fn an_unknown_size_has_no_bar() {
        let line = live_line(30, "downloading", "0.2.31", transfer(4_100_000, None), 100);
        assert!(
            line.contains("4.1 MB")
                && line.contains("MB/s")
                && !line.contains('━')
                && !line.contains('─')
        );
    }

    #[test]
    fn before_the_download_starts_there_is_only_the_island_and_its_detail() {
        let line = live_line(40, "checking", "for updates", None, 100);
        assert!(
            line.starts_with(&crate::tui::text::SELECT.to_string()),
            "the session's island ground: {line:?}"
        );
        assert!(
            line.ends_with("for updates") && !line.contains("MB"),
            "{line:?}"
        );
        assert!(
            ['❄', '❅', '❆'].iter().any(|flake| line.contains(*flake)),
            "the snowflake spinner: {line:?}"
        );
    }
}
