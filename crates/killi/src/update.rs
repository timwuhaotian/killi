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
const RELEASES_URL: &str = "https://github.com/timwuhaotian/killi/raw/main/releases";
const BINARY: &str = "killi-arm64";

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
    let http = reqwest::Client::new();
    let (version, sha256) = status.during("checking for updates", latest(&http, &base)).await?;
    let current = env!("CARGO_PKG_VERSION");
    if parse(&version) <= parse(current) {
        status.finish(&format!("killi {current} is up to date."));
        return Ok(());
    }

    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let staging = exe.with_file_name(format!(".killi-update-{}", std::process::id()));
    let bytes = download(status, &http, &format!("{base}/{BINARY}"), &format!("downloading {version}")).await?;
    fs::write(&staging, &bytes).with_context(|| format!("could not write {}", staging.display()))?;
    let result = verify_and_install(&staging, &sha256, &exe);
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result?;
    status.finish(&format!("Updated killi {current} → {version} ({}).", exe.display()));
    Ok(())
}

/// The published version and the SHA-256 of its binary, from `latest.toml`.
async fn latest(http: &reqwest::Client, base: &str) -> Result<(String, String)> {
    let text = http.get(format!("{base}/latest.toml")).send().await?.error_for_status()?.text().await?;
    let table: toml::Table = toml::from_str(&text).context("invalid latest.toml")?;
    let field = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    let (Some(version), Some(sha256)) = (field("version"), field("sha256")) else {
        bail!("latest.toml needs `version` and `sha256`");
    };
    Ok((version, sha256))
}

/// The binary at `url`, drawing its progress as it arrives.
async fn download(status: &mut Status, http: &reqwest::Client, url: &str, label: &str) -> Result<Vec<u8>> {
    let response = status.during(label, http.get(url).send()).await?.error_for_status()?;
    let total = response.content_length();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let began = Instant::now();
    let mut tick = tokio::time::interval(FRAME);
    loop {
        tokio::select! {
            chunk = stream.next() => match chunk {
                Some(chunk) => bytes.extend_from_slice(&chunk?),
                None => return Ok(bytes),
            },
            _ = tick.tick() => {
                let done = bytes.len() as u64;
                let per_second = (u128::from(done) * 1000 / began.elapsed().max(SPEED_WINDOW).as_millis()) as u64;
                status.draw(label, Some(Transfer { done, total, per_second }));
            }
        }
    }
}

fn verify_and_install(staging: &std::path::Path, expected: &str, exe: &std::path::Path) -> Result<()> {
    let output = Command::new("/usr/bin/shasum").arg("-a").arg("256").arg(staging).output()?;
    let actual = String::from_utf8_lossy(&output.stdout);
    let actual = actual.split_whitespace().next().unwrap_or_default();
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch: expected {expected}, downloaded {actual}");
    }
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;
    fs::rename(staging, exe).with_context(|| format!("could not replace {}", exe.display()))
}

fn parse(version: &str) -> Vec<u64> {
    version.split('.').map(|part| part.parse().unwrap_or(0)).collect()
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
        let live = std::io::stderr().is_terminal() && std::env::var("TERM").is_ok_and(|term| term != "dumb");
        Self { live, started: Instant::now(), drawn: false }
    }

    /// Redraws the line in one write, inside a synchronized update with the cursor hidden.
    fn draw(&mut self, label: &str, transfer: Option<Transfer>) {
        if !self.live {
            return;
        }
        let frame = (self.started.elapsed().as_millis() / FRAME.as_millis()) as usize;
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        let line = plain(&live_line(frame, label, transfer, columns)).into_owned();
        let _ = std::io::stderr().write_all(format!("\x1b[?2026h\x1b[?25l\r\x1b[2K{line}\x1b[?2026l").as_bytes());
        self.drawn = true;
    }

    /// Erases the line and shows the cursor again.
    fn clear(&mut self) {
        if std::mem::take(&mut self.drawn) {
            let _ = std::io::stderr().write_all(b"\r\x1b[2K\x1b[?25h");
        }
    }

    /// Animates `label` until `work` is done.
    async fn during<T>(&mut self, label: &str, work: impl Future<Output = T>) -> T {
        let mut tick = tokio::time::interval(FRAME);
        tokio::pin!(work);
        loop {
            tokio::select! {
                output = &mut work => return output,
                _ = tick.tick() => self.draw(label, None),
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

/// The status line `frame` tenths of a second in, cut to fit `columns`: the wave and wordmark,
/// what is happening, and for a download its progress.
fn live_line(frame: usize, label: &str, transfer: Option<Transfer>, columns: usize) -> String {
    let mut line = format!("{}  {label}", banner::working(frame));
    if let Some(transfer) = transfer {
        line.push_str(&progress(transfer, columns.saturating_sub(1 + width(&line))));
    }
    wrap(&line, columns.saturating_sub(1)).into_iter().next().unwrap_or_default()
}

/// A download's bar, sizes and speed, each led by two spaces, in `room` columns. The speed, then
/// the sizes, go first when the bar would get too short.
fn progress(Transfer { done, total, per_second }: Transfer, room: usize) -> String {
    let sizes = match total {
        Some(total) => format!("{} / {}", megabytes(done), megabytes(total)),
        None => megabytes(done),
    };
    let mut figures = vec![sizes, format!("{DIM}{}/s{RESET}", megabytes(per_second))];
    let cells = |figures: &[String]| room.saturating_sub(2 + figures.iter().map(|figure| 2 + width(figure)).sum::<usize>()).min(BAR);
    while total.is_some() && cells(&figures) < MIN_BAR && figures.pop().is_some() {}
    let mut out = String::new();
    if let Some(total) = total.filter(|_| cells(&figures) >= MIN_BAR) {
        out.push_str(&format!("  {}", banner::bar(done as f32 / total.max(1) as f32, cells(&figures))));
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
    fn versions_compare_numerically() {
        assert!(super::parse("0.10.0") > super::parse("0.9.3"));
        assert!(super::parse("0.1.0") == super::parse("0.1.0"));
    }

    fn transfer(done: u64, total: Option<u64>) -> Option<Transfer> {
        Some(Transfer { done, total, per_second: 2_300_000 })
    }

    #[test]
    fn the_status_line_always_fits_the_terminal() {
        for columns in 1..=140 {
            for transfer in [None, transfer(0, Some(6_700_000)), transfer(4_100_000, Some(6_700_000)), transfer(4_100_000, None)] {
                let line = live_line(7, "downloading 0.2.31", transfer, columns);
                assert!(width(&line) < columns.max(2), "{columns} columns: {line:?}");
            }
        }
    }

    #[test]
    fn a_download_shows_bar_sizes_and_speed_until_the_terminal_is_too_narrow() {
        let at = |columns| live_line(30, "downloading 0.2.31", transfer(4_100_000, Some(6_700_000)), columns);
        let wide = at(100);
        assert!(wide.contains("4.1 MB / 6.7 MB") && wide.contains("2.3 MB/s") && wide.contains('━') && wide.contains('─'));
        let narrow = at(62);
        assert!(narrow.contains("4.1 MB / 6.7 MB") && !narrow.contains("MB/s") && narrow.contains('━'), "{narrow:?}");
        let tiny = at(40);
        assert!(!tiny.contains('━') && !tiny.contains("MB"), "{tiny:?}");
    }

    #[test]
    fn an_unknown_size_has_no_bar() {
        let line = live_line(30, "downloading 0.2.31", transfer(4_100_000, None), 100);
        assert!(line.contains("4.1 MB") && line.contains("MB/s") && !line.contains('━') && !line.contains('─'));
    }

    #[test]
    fn before_the_download_starts_there_is_only_the_label() {
        let line = live_line(3, "checking for updates", None, 100);
        assert!(line.ends_with("checking for updates") && !line.contains("MB"));
    }
}
