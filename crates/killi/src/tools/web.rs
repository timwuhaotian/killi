use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;
use serde_json::Value;
use tokio::process::Command;

use super::{Context, MAX_BYTES, Output, SearchSettings, files};
use super::bash::{OwnSession, Tracked, track};
use crate::llm::truncate;

const MAX_DOWNLOAD: usize = 10 * 1024 * 1024;
/// Readable text shorter than this suggests a page built by JavaScript.
const MIN_READABLE: usize = 200;
const BROWSER_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_RESULTS: usize = 8;
const MAX_RESULTS: usize = 20;
/// Some sites turn away clients that don't look like a browser.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";

pub async fn fetch_url(ctx: &Context, args: &Value) -> Output {
    let Some(url) = args.get("url").and_then(Value::as_str) else {
        return Output::error("fetch_url needs `url`.");
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Output::error("fetch_url only fetches http:// and https:// URLs.");
    }
    let url = raw_github(url);
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let raw = flag("raw");
    if flag("screenshot") {
        return screenshot(ctx, &url).await;
    }
    if flag("render") {
        return match render(&url).await {
            Ok(dom) => page(ctx, dom, &url, raw).await,
            Err(e) => Output::error(e),
        };
    }
    let client = match reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build() {
        Ok(client) => client,
        Err(e) => return Output::error(format!("Could not create an HTTP client: {e}")),
    };
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => return Output::error(format!("Could not fetch {url}: {}", fetch_error(&e))),
    };
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let final_url = response.url().to_string();
    let is_pdf = kind.contains("application/pdf") || url.split(['?', '#']).next().is_some_and(|u| u.to_ascii_lowercase().ends_with(".pdf"));
    // A body that can't be shown is not downloaded at all.
    if status.is_success() && !kind.is_empty() && !is_text(&kind) && !is_pdf {
        return Output::error(format!("{url} is `{kind}`, not text. PDFs aside, fetch_url only shows text, including HTML."));
    }
    let bytes = match capped_body(response).await {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {url}: {}", fetch_error(&e))),
    };
    if !status.is_success() {
        let page = String::from_utf8_lossy(&bytes);
        let title = html_title(&page);
        let summary = match title {
            Some(title) => format!("{title} — {}", truncate(&plain(&page), 300)),
            None => truncate(&plain(&page), 400).to_string(),
        };
        let hint = if status.as_u16() == 404 { " The address is likely wrong; do not guess variants — find the real URL with web_search." } else { "" };
        return Output::error(format!("{url} answered HTTP {status}: {summary}{hint}"));
    }
    if is_pdf {
        return pdf_text(ctx, &bytes, &url).await;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if kind.contains("html") || (kind.is_empty() && looks_like_html(&bytes)) {
        if raw {
            return shown(ctx, &text);
        }
        if let Some(article) = extract(text, final_url.clone()).await.filter(|a| a.len() >= MIN_READABLE) {
            return shown(ctx, &article);
        }
        // Little or nothing in the static HTML: the page probably builds itself with
        // JavaScript, so let a real browser run it, if there is one.
        return match render(&final_url).await {
            Ok(dom) => page(ctx, dom, &final_url, false).await,
            Err(_) => Output::error(format!("Could not find readable text in {url}. It may need JavaScript; try render.")),
        };
    }
    if kind.is_empty() || is_text(&kind) {
        return shown(ctx, &text);
    }
    Output::error(format!("{url} is `{kind}`, not text."))
}

/// Why a request failed, with the whole cause chain and what to do about it: a name that
/// does not resolve means the address itself is invented, so guessing spellings is wasted.
fn fetch_error(error: &reqwest::Error) -> String {
    let detail = crate::llm::error_chain(error);
    let hint = if error.is_timeout() {
        " The site took too long; retry once or try another source."
    } else if detail.contains("dns error") || detail.contains("failed to lookup address") || detail.contains("Name or service not known") || detail.contains("nodename nor servname") {
        " The domain does not resolve — the host name is likely wrong or invented. Do not retry spelling variants; find the real domain with web_search."
    } else if error.is_connect() {
        " The site could not be reached (connection refused or network down); do not retry the same address repeatedly."
    } else if detail.contains("certificate") || detail.contains("tls") || detail.contains("SSL") {
        " The site's TLS certificate was rejected; report it instead of retrying."
    } else {
        ""
    };
    format!("{detail}.{hint}")
}

/// The `<title>` of an HTML page, tags and extra space stripped.
fn html_title(html: &str) -> Option<String> {
    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
    title.captures(html).map(|c| plain(&c[1])).filter(|t| !t.is_empty())
}

/// How long `pdftotext` may take over a PDF before the read gives up and kills it.
const PDF_TIMEOUT: Duration = Duration::from_secs(60);

/// Extracts a downloaded PDF's text with `pdftotext` (poppler) when it is installed.
/// The run has its own session and a timeout, and is tracked so quitting ends it too.
async fn pdf_text(ctx: &Context, bytes: &[u8], url: &str) -> Output {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let file = Temp(std::env::temp_dir().join(format!("killi-pdf-{}-{nanos}.pdf", std::process::id())));
    if let Err(e) = tokio::fs::write(&file.0, bytes).await {
        return Output::error(format!("Could not save the PDF from {url}: {e}"));
    }
    let child = match Command::new("pdftotext")
        .arg("-layout")
        .arg(&file.0)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .own_session()
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Output::error(format!(
                "{url} is a PDF, and no `pdftotext` (poppler) is installed to read it; install it (e.g. `brew install poppler`) or use the HTML page instead."
            ));
        }
        Err(e) => return Output::error(format!("Could not run pdftotext on {url}: {e}")),
    };
    let _tracked = child.id().map(track);
    match tokio::time::timeout(PDF_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            if text.trim().is_empty() {
                Output::error(format!("{url} is a PDF with no extractable text (scanned images or encrypted); fetch_url cannot read it."))
            } else {
                shown(ctx, text.trim())
            }
        }
        Ok(Ok(out)) => Output::error(format!("{url} is a PDF pdftotext could not read: {}", truncate(&String::from_utf8_lossy(&out.stderr), 300))),
        Ok(Err(e)) => Output::error(format!("Could not run pdftotext on {url}: {e}")),
        Err(_) => Output::error(format!("{url} is a PDF pdftotext took more than {} s to read.", PDF_TIMEOUT.as_secs())),
    }
}

/// Whether a content type is one `fetch_url` can show.
fn is_text(kind: &str) -> bool {
    kind.contains("html") || kind.starts_with("text/") || kind.contains("json") || kind.contains("xml")
}

/// The response body up to `MAX_DOWNLOAD` bytes; the rest is never read.
async fn capped_body(mut response: reqwest::Response) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = MAX_DOWNLOAD - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= MAX_DOWNLOAD {
            break;
        }
    }
    Ok(body)
}

/// A GitHub `blob` page URL becomes its raw file URL.
fn raw_github(url: &str) -> String {
    let Some(rest) = url.strip_prefix("https://github.com/") else { return url.to_string() };
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    match parts.as_slice() {
        [owner, repo, "blob", reference, path] => {
            format!("https://raw.githubusercontent.com/{owner}/{repo}/{reference}/{path}")
        }
        _ => url.to_string(),
    }
}

/// A page's DOM as the model should see it: the raw HTML, or its main text.
async fn page(ctx: &Context, dom: String, url: &str, raw: bool) -> Output {
    if raw {
        return shown(ctx, &dom);
    }
    match extract(dom, url.to_string()).await {
        Some(article) => shown(ctx, &article),
        None => Output::error(format!("Could not find readable text in {url}.")),
    }
}

async fn extract(html: String, url: String) -> Option<String> {
    tokio::task::spawn_blocking(move || readable(&html, &url)).await.ok().flatten()
}

/// The text within the display budget; longer text is cut and spilled to a file, like other
/// tools' truncated output, so the model pages through it with `read`.
fn shown(ctx: &Context, text: &str) -> Output {
    let shown = truncate(text, MAX_BYTES);
    let more = text.len() - shown.len();
    if more == 0 {
        return Output::ok(text.to_string()).brief(format!("{} KB", text.len() / 1024));
    }
    let saved = match super::spill(ctx, "fetch", text) {
        Some(path) => format!(" The full text is saved at {}; read it with offset and limit.", path.display()),
        None => String::new(),
    };
    Output::ok(format!("{shown}\n\n[Showing the first {} of {} bytes.{saved}]", shown.len(), text.len()))
        .brief(format!("{} KB", text.len() / 1024))
}

/// The browser killi can drive for pages that need JavaScript and for screenshots.
fn chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KILLI_CHROME").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(path);
    }
    ["Google Chrome", "Chromium", "Microsoft Edge", "Brave Browser"]
        .iter()
        .map(|name| PathBuf::from(format!("/Applications/{name}.app/Contents/MacOS/{name}")))
        .find(|path| path.exists())
}

/// Headless Chrome on `url` with a throwaway profile, so a running Chrome is left alone, and
/// in its own session so the helpers it starts can be killed with it.
fn browser(url: &str, extra: &[String]) -> Result<(Command, PathBuf), String> {
    let Some(chrome) = chrome() else {
        return Err("No Chrome, Chromium, Edge or Brave found in /Applications (or KILLI_CHROME).".into());
    };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let profile = std::env::temp_dir().join(format!("killi-chrome-{}-{nanos}", std::process::id()));
    let mut command = Command::new(chrome);
    command
        .args(["--headless=new", "--use-angle=swiftshader", "--disable-software-rasterizer", "--disable-gpu"])
        .args(["--no-first-run", "--no-default-browser-check", "--hide-scrollbars", "--virtual-time-budget=8000"])
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
struct Running {
    child: tokio::process::Child,
    profile: PathBuf,
    _tracked: Option<Tracked>,
}

impl Running {
    fn start(command: &mut Command, profile: PathBuf) -> Result<Self, String> {
        let child = command.spawn().map_err(|e| format!("Could not run the browser: {e}"))?;
        let tracked = child.id().map(track);
        Ok(Self { child, profile, _tracked: tracked })
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
async fn render(url: &str) -> Result<String, String> {
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
        (true, true) => Err(format!("The browser took more than {} s on {url}.", BROWSER_TIMEOUT.as_secs())),
        (true, false) => Err(format!("The browser returned nothing for {url}.")),
        _ => Ok(dom),
    }
}

/// Takes a screenshot into `file`. Chrome writes the file but then often doesn't exit, so
/// this waits for the file to stop growing and kills the browser.
async fn capture(url: &str, file: &std::path::Path) -> Result<(), String> {
    let extra = [format!("--screenshot={}", file.display()), "--window-size=1280,900".to_string()];
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
        if browser.child.try_wait().is_ok_and(|status| status.is_some()) {
            break if file.exists() { Ok(()) } else { Err(format!("The browser made no screenshot of {url}.")) };
        }
        if started.elapsed() > BROWSER_TIMEOUT {
            break Err(format!("The browser took more than {} s on {url}.", BROWSER_TIMEOUT.as_secs()));
        }
    }
}

/// A file removed when it goes out of scope, also if the call that made it is interrupted.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn screenshot(ctx: &Context, url: &str) -> Output {
    if !ctx.images.get() {
        return Output::error("This model can't see images, so a screenshot is no use.");
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let file = Temp(std::env::temp_dir().join(format!("killi-shot-{}-{nanos}.png", std::process::id())));
    match capture(url, &file.0).await {
        Ok(()) => {
            let mut output = files::read_image(ctx, url, &file.0, "image/png");
            if !output.is_error {
                output.text = format!("Screenshot of {url} (1280x900, top of the page); it is attached below.");
            }
            output
        }
        Err(e) => Output::error(e),
    }
}

pub async fn web_search(ctx: &Context, args: &Value) -> Output {
    let Some(query) = args.get("query").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()) else {
        return Output::error("web_search needs `query`.");
    };
    let limit = super::uint(args, "limit").map_or(DEFAULT_RESULTS, |l| (l as usize).clamp(1, MAX_RESULTS));
    let settings = ctx.search.borrow().clone();
    let found = match search(&settings, query, limit).await {
        Ok(found) => found,
        Err(e) => return Output::error(e),
    };
    if found.is_empty() {
        return Output::ok(format!("No results for `{query}`."));
    }
    let count = found.len();
    let text = found
        .iter()
        .enumerate()
        .map(|(n, hit)| format!("{}. {}\n   {}\n   {}", n + 1, hit.title, hit.url, hit.snippet))
        .collect::<Vec<_>>()
        .join("\n");
    Output::ok(text).brief(format!("{count} result{}", if count == 1 { "" } else { "s" }))
}

#[derive(Debug)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// One way of searching, tried in `search`'s order until one returns results.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    Tavily(String),
    BraveApi(String),
    BravePage,
    Searxng { base: String, key: Option<String> },
}

impl Source {
    fn name(&self) -> &'static str {
        match self {
            Source::Tavily(_) => "Tavily",
            Source::BraveApi(_) | Source::BravePage => "Brave",
            Source::Searxng { .. } => "SearXNG",
        }
    }
}

/// The sources to try, in order: the configured provider first, then any other source the
/// config already makes usable — a key for another provider or a SearXNG instance — and
/// Brave's public page last, which needs nothing. One `api_key` serves one provider, so a
/// fallback with another service's key only exists when that service needs no key (SearXNG
/// without auth) or is the public page.
fn sources(settings: &SearchSettings) -> Result<Vec<Source>, String> {
    let provider = if settings.provider.is_empty() { "brave" } else { settings.provider.as_str() };
    let key = settings.api_key.as_deref().filter(|k| !k.is_empty());
    let instance = settings.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let primary = match (provider, key, instance) {
        ("tavily", Some(key), _) => Source::Tavily(key.to_string()),
        ("tavily", None, _) => {
            return Err("web_search with Tavily needs an API key: set `api_key` or `api_key_env` under [search].".into())
        }
        ("brave", Some(key), _) => Source::BraveApi(key.to_string()),
        ("brave", None, _) => Source::BravePage,
        ("searxng", _, Some(base)) => Source::Searxng { base: base.to_string(), key: key.map(str::to_string) },
        ("searxng", _, None) => {
            return Err("web_search with SearXNG needs the instance: set `url` under [search].".into())
        }
        (other, _, _) => {
            return Err(format!("Unknown search provider `{other}`; use \"brave\", \"tavily\" or \"searxng\"."))
        }
    };
    let mut chain = vec![primary];
    // A SearXNG instance in `url` is usable beside a keyed provider when it asks for no key
    // of its own — killi cannot know its key, so try it without one.
    if !matches!(chain[0], Source::Searxng { .. }) && let Some(base) = instance {
        chain.push(Source::Searxng { base: base.to_string(), key: None });
    }
    if !matches!(chain[0], Source::BravePage) {
        chain.push(Source::BravePage);
    }
    Ok(chain)
}

async fn search(settings: &SearchSettings, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    search_chain(&sources(settings)?, query, limit).await
}

/// Tries each source in order, retrying a transient failure once, until one returns
/// results; when none does, every failure is named so the model can say what it tried.
async fn search_chain(chain: &[Source], query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Could not create an HTTP client: {e}"))?;
    let mut failures: Vec<String> = Vec::new();
    for source in chain {
        match search_once(&client, source, query, limit).await {
            Ok(found) if !found.is_empty() => return Ok(found),
            Ok(_) => failures.push(format!("{}: no results", source.name())),
            Err(e) if retryable(&e) => match search_once(&client, source, query, limit).await {
                Ok(found) if !found.is_empty() => return Ok(found),
                Ok(_) => failures.push(format!("{}: no results", source.name())),
                Err(e) => failures.push(format!("{}: {e}", source.name())),
            },
            Err(e) => failures.push(format!("{}: {e}", source.name())),
        }
    }
    Err(format!(
        "Search failed on every source ({}). Set an API key under [search] in the config to search reliably.",
        failures.join("; ")
    ))
}

/// A failure another attempt or another source may get past: rate limits, server errors
/// and the network itself. A refusal (4xx other than 429) is final for that source.
fn retryable(error: &str) -> bool {
    ["429", "500", "502", "503", "504", "failed:", "timed out"]
        .iter()
        .any(|mark| error.contains(mark))
}

async fn search_once(client: &reqwest::Client, source: &Source, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    match source {
        Source::Tavily(key) => {
            let body = serde_json::json!({"query": query, "max_results": limit});
            let reply = client.post("https://api.tavily.com/search").bearer_auth(key).header("content-type", "application/json").body(body.to_string()).send().await;
            let json = json_reply(reply, "Tavily").await?;
            Ok(hits(&json["results"], "title", "url", "content", limit))
        }
        Source::BraveApi(key) => {
            let reply = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("X-Subscription-Token", key)
                .query(&[("q", query), ("count", &limit.to_string())])
                .send()
                .await;
            let json = json_reply(reply, "Brave").await?;
            Ok(hits(&json["web"]["results"], "title", "url", "description", limit))
        }
        Source::BravePage => {
            let reply = client
                .get("https://search.brave.com/search")
                .query(&[("q", query), ("source", "web")])
                .send()
                .await
                .map_err(|e| format!("Search failed: {e}"))?;
            let status = reply.status();
            let html = reply.text().await.map_err(|e| format!("Search failed: {e}"))?;
            let found = brave_page(&html, limit);
            if found.is_empty() && (!status.is_success() || html.to_ascii_lowercase().contains("captcha")) {
                return Err(format!("turned the search away (HTTP {status})"));
            }
            Ok(found)
        }
        Source::Searxng { base, key } => {
            let mut request = client.get(format!("{}/search", base.trim_end_matches('/'))).query(&[("q", query), ("format", "json")]);
            if let Some(key) = key {
                request = request.bearer_auth(key);
            }
            let json = match json_reply(request.send().await, "SearXNG").await {
                Ok(json) => json,
                // An instance without the json format in its settings answers 403.
                Err(e) if e.contains("403") => {
                    return Err(format!("{e}. The instance must allow the json format (search.formats in its settings.yml)."))
                }
                Err(e) => return Err(e),
            };
            Ok(hits(&json["results"], "title", "url", "content", limit))
        }
    }
}

async fn json_reply(reply: reqwest::Result<reqwest::Response>, who: &str) -> Result<Value, String> {
    let reply = reply.map_err(|e| format!("{who} search failed: {e}"))?;
    let status = reply.status();
    let body = reply.text().await.map_err(|e| format!("{who} search failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("{who} search answered HTTP {status}: {}", truncate(&body, 300)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{who} search sent something that isn't JSON: {e}"))
}

fn hits(results: &Value, title: &str, url: &str, snippet: &str, limit: usize) -> Vec<Hit> {
    let text = |item: &Value, key: &str| item[key].as_str().unwrap_or_default().trim().to_string();
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item[url].is_string())
        .take(limit)
        .map(|item| Hit { title: text(item, title), url: text(item, url), snippet: truncate(&text(item, snippet), 400).to_string() })
        .collect()
}

/// Results from Brave's public search page, for use without an API key.
fn brave_page(html: &str, limit: usize) -> Vec<Hit> {
    let (link, title, snippet) = (
        Regex::new(r#"<a href="(https?://[^"]+)""#).unwrap(),
        Regex::new(r#"(?s)<div class="title[^"]*"[^>]*>(.*?)</div>"#).unwrap(),
        Regex::new(r#"(?s)<div class="content[^"]*"[^>]*>(.*?)</div>"#).unwrap(),
    );
    html.split(r#"data-type="web""#)
        .skip(1)
        .filter_map(|block| {
            let url = link.captures(block)?[1].to_string();
            let title = plain(&title.captures(block)?[1]);
            let snippet = snippet.captures(block).map(|c| plain(&c[1])).unwrap_or_default();
            Some(Hit { title, url: decode(&url), snippet: truncate(&snippet, 400).to_string() })
        })
        .take(limit)
        .collect()
}

/// Text of an HTML fragment: comments and tags removed, entities decoded, spaces collapsed.
fn plain(fragment: &str) -> String {
    let comments = Regex::new(r"(?s)<!--.*?-->").unwrap().replace_all(fragment, "");
    let tags = Regex::new(r"(?s)<[^>]*>").unwrap().replace_all(&comments, "");
    decode(&tags).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    start.contains("<!doctype html") || start.contains("<html")
}

/// The page's main content as Markdown, with its title on top. The Markdown keeps links;
/// on a short page (a home or index page) the page's own links are appended, so the next
/// address comes from the page and is never guessed.
fn readable(html: &str, url: &str) -> Option<String> {
    let config = Config { text_mode: TextMode::Markdown, ..Default::default() };
    let article = Readability::new(html, Some(url), Some(config)).ok()?.parse().ok()?;
    // With `TextMode::Markdown`, `text_content` is the Markdown (links kept as `[text](url)`).
    let body = article.text_content.trim().to_string();
    if body.is_empty() {
        return None;
    }
    let mut text = if article.title.is_empty() { body } else { format!("# {}\n\n{body}", article.title.trim()) };
    if text.len() < MIN_READABLE * 10 {
        let links = page_links(html, url);
        if !links.is_empty() {
            text.push_str("\n\nLinks on this page:\n");
            text.push_str(&links.join("\n"));
        }
    }
    Some(text)
}

/// Up to 60 distinct links of a page as `- [text](absolute-url)`, fragments and scripts out.
fn page_links(html: &str, base: &str) -> Vec<String> {
    let anchor = Regex::new(r#"(?is)<a\s[^>]*href\s*=\s*"([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let base = reqwest::Url::parse(base).ok();
    let mut seen = std::collections::HashSet::new();
    let mut links = Vec::new();
    for capture in anchor.captures_iter(html) {
        let href = capture[1].trim();
        if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") || href.starts_with("tel:") {
            continue;
        }
        let Some(base) = &base else { break };
        let Ok(absolute) = base.join(href) else { continue };
        let absolute = absolute.to_string();
        if !seen.insert(absolute.clone()) {
            continue;
        }
        let label = plain(&capture[2]);
        let label = if label.is_empty() { absolute.clone() } else { truncate(&label, 80).to_string() };
        links.push(format!("- [{label}]({absolute})"));
        if links.len() >= 60 {
            break;
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves one response: `head` (status line and headers), then `body_len` bytes of `a`, then
    /// holds the connection open for `hold`.
    async fn serve_once(head: &'static str, body_len: usize, hold: std::time::Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket.read(&mut [0u8; 1024]).await;
            let _ = socket.write_all(head.as_bytes()).await;
            let chunk = vec![b'a'; 64 * 1024];
            let mut sent = 0;
            while sent < body_len {
                let n = chunk.len().min(body_len - sent);
                if socket.write_all(&chunk[..n]).await.is_err() {
                    return;
                }
                sent += n;
            }
            tokio::time::sleep(hold).await;
        });
        url
    }

    /// Serves one response with a fixed `body`.
    async fn serve_body(head: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket.read(&mut [0u8; 1024]).await;
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
        });
        url
    }

    fn ctx() -> super::Context {
        let tmp = crate::test_temp::TempDir::new("killi-fetch");
        let mut ctx = super::Context::new(tmp.path().to_path_buf(), tmp.join("spill"));
        ctx.temp = Some(tmp);
        ctx
    }

    #[test]
    fn long_text_is_spilled_for_paging_with_read() {
        let tmp = crate::test_temp::TempDir::new("killi-fetch-shown");
        let ctx = super::Context::new(tmp.path().to_path_buf(), tmp.join("spill"));
        let text = "x\n".repeat(30_000);
        let out = super::shown(&ctx, &text);
        assert!(!out.is_error);
        assert!(out.text.contains("The full text is saved at ") && out.text.contains("read it with offset and limit"), "{}", &out.text[out.text.len() - 200..]);
        let mut spilled = std::fs::read_dir(&ctx.spill_dir).unwrap();
        let path = spilled.next().expect("one spill file").unwrap().path();
        assert!(spilled.next().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(super::shown(&ctx, "hi").text, "hi");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_body_that_is_not_text_is_not_downloaded() {
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: 1000000000\r\n\r\n";
        let url = serve_once(head, 0, std::time::Duration::from_secs(20)).await;
        let args = serde_json::json!({"url": url});
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), super::fetch_url(&ctx(), &args)).await.expect("waited for the body");
        assert!(out.is_error && out.text.contains("`application/octet-stream`, not text"), "{}", out.text);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_text_body_is_read_only_up_to_the_cap() {
        let size = super::MAX_DOWNLOAD + 5 * 1024 * 1024;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\n\r\n";
        let url = serve_once(head, size, std::time::Duration::from_secs(20)).await;
        let args = serde_json::json!({"url": url});
        let out = tokio::time::timeout(std::time::Duration::from_secs(10), super::fetch_url(&ctx(), &args)).await.expect("waited for a body with no end");
        assert!(out.text.contains("The full text is saved at "), "{}", &out.text[out.text.len() - 200..]);
        assert!(out.text.ends_with("read it with offset and limit.]"), "{}", &out.text[out.text.len() - 60..]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_a_browser_kills_its_group_and_removes_its_profile() {
        let tmp = crate::test_temp::TempDir::new("killi-chrome-test");
        let profile = tmp.join("profile");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & sleep 30"]).process_group(0).kill_on_drop(true);
        let browser = super::Running::start(&mut command, profile.clone()).unwrap();
        let pgid = browser.child.id().unwrap() as libc::pid_t;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // SAFETY: signal 0 only checks that the group exists.
        assert_eq!(unsafe { libc::killpg(pgid, 0) }, 0);
        drop(browser);
        assert!(!profile.exists());
        for _ in 0..50 {
            // SAFETY: as above.
            if unsafe { libc::killpg(pgid, 0) } != 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the browser's process group is still running");
    }

    #[test]
    fn a_short_page_lists_its_links_and_the_article_keeps_them() {
        let html = r##"<html><head><title>Home</title></head><body><nav>
            <a href="/timetable/">Timetable</a> <a href="https://other.test/x">Other</a>
            <a href="#top">Top</a> <a href="/timetable/">Again</a></nav>
            <p>Welcome.</p></body></html>"##;
        let text = super::readable(html, "https://example.test/").unwrap();
        assert!(text.contains("[Timetable](https://example.test/timetable/)"), "{text}");
        assert!(text.contains("[Other](https://other.test/x)"), "{text}");
        assert!(!text.contains("- [Top]"), "fragment links are not listed: {text}");
        assert_eq!(text.matches("- [Timetable]").count(), 1, "duplicates are listed once: {text}");
    }

    #[test]
    fn an_error_page_is_named_by_its_title() {
        let html = "<html><head><title>404 Not Found</title></head><body><h1>Not Found</h1></body></html>";
        assert_eq!(super::html_title(html).as_deref(), Some("404 Not Found"));
        assert_eq!(super::html_title("<p>none</p>"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_404_reports_its_title_not_its_head() {
        let page = "<html><head><meta charset=\"utf-8\"><title>ページが見つかりません</title></head><body><h1>Not Found</h1></body></html>";
        let url = serve_body("HTTP/1.1 404 Not Found\r\ncontent-type: text/html\r\n\r\n", page).await;
        let args = serde_json::json!({"url": url});
        let out = super::fetch_url(&ctx(), &args).await;
        assert!(out.is_error && out.text.contains("ページが見つかりません") && out.text.contains("do not guess"), "{}", out.text);
        assert!(!out.text.contains("meta charset"), "{}", out.text);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unresolvable_host_says_not_to_guess_spellings() {
        let args = serde_json::json!({"url": "https://killi-test.invalid/"});
        let out = tokio::time::timeout(std::time::Duration::from_secs(20), super::fetch_url(&ctx(), &args)).await.expect("dns should fail fast");
        assert!(out.is_error && out.text.contains("Do not retry spelling variants"), "{}", out.text);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_pdf_is_read_as_text_or_says_why_not() {
        // Smallest valid one-page PDF with the word Hello.
        let pdf = "%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 24 Tf 100 700 Td (Hello) Tj ET\nendstream\nendobj\n5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
        let url = serve_body("HTTP/1.1 200 OK\r\ncontent-type: application/pdf\r\n\r\n", pdf).await;
        let args = serde_json::json!({"url": url});
        let out = super::fetch_url(&ctx(), &args).await;
        if std::process::Command::new("pdftotext").arg("-v").output().is_ok() {
            assert!(!out.is_error && out.text.contains("Hello"), "{}", out.text);
        } else {
            assert!(out.is_error && out.text.contains("pdftotext"), "{}", out.text);
        }
    }

    #[test]
    fn extracts_the_article() {
        let html = r#"<html><head><title>Notes</title></head><body>
            <nav><a href="/">Home</a> <a href="/about">About</a></nav>
            <article><h1>Ring buffers</h1><p>A ring buffer keeps the newest items and drops the oldest
            once it is full. It is useful for logs, metrics and audio, where only recent data matters
            and memory must stay bounded. This paragraph is long enough to count as content.</p>
            <p>Writes wrap around to the start of the storage.</p></article>
            <footer>Copyright</footer></body></html>"#;
        let text = super::readable(html, "https://example.test/notes").unwrap();
        assert!(text.starts_with("# Notes"), "{text}");
        assert!(text.contains("A ring buffer keeps the newest items"));
        assert!(!text.contains("Copyright"));
    }

    #[test]
    fn github_blob_urls_become_raw_urls() {
        assert_eq!(
            super::raw_github("https://github.com/tokio-rs/tokio/blob/master/tokio/src/lib.rs"),
            "https://raw.githubusercontent.com/tokio-rs/tokio/master/tokio/src/lib.rs"
        );
        assert_eq!(super::raw_github("https://github.com/tokio-rs/tokio"), "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn reads_results_from_a_brave_page() {
        let html = r#"<div class="snippet svelte-x" data-pos="0" data-type="web" data-keynav="true"><div class="result-content">
            <a href="https://docs.rs/tokio/fn.spawn_blocking.html" class="l1"><div class="site-name">Docs.rs</div>
            <div class="title search-snippet-title svelte" title="spawn_blocking in tokio::task">spawn_blocking in tokio::task</div></a>
            <div class="generic-snippet"><div class="content desktop"><!----><strong>Runs a closure</strong> on a thread &amp; more&nbsp;...<!----></div></div></div></div>"#;
        let found = super::brave_page(html, 5);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://docs.rs/tokio/fn.spawn_blocking.html");
        assert_eq!(found[0].title, "spawn_blocking in tokio::task");
        assert_eq!(found[0].snippet, "Runs a closure on a thread & more ...");
    }

    #[test]
    fn reads_results_from_api_json() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"results":[{"title":"A","url":"https://a.test","content":"about a"},{"title":"no url"}]}"#).unwrap();
        let found = super::hits(&json["results"], "title", "url", "content", 5);
        assert_eq!((found.len(), found[0].snippet.as_str()), (1, "about a"));
    }

    fn searxng(url: Option<String>) -> super::SearchSettings {
        super::SearchSettings { provider: "searxng".into(), url, ..super::SearchSettings::default() }
    }

    /// Answers 429 to the first request and results to the rest, counting hits.
    async fn serve_flaky(hits: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = socket.read(&mut [0u8; 4096]).await;
                let reply = if n == 0 {
                    "HTTP/1.1 429 Too Many Requests\r\ncontent-type: text/plain\r\n\r\nslow down"
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"results\":[{\"title\":\"A\",\"url\":\"https://a.test\",\"content\":\"about a\"}]}"
                };
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        url
    }

    #[tokio::test(flavor = "current_thread")]
    async fn searxng_reads_the_instance_results() {
        let body = r#"{"results":[{"title":"A","url":"https://a.test","content":"about a"},{"title":"no url"}]}"#;
        let url = serve_body("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n", body).await;
        let found = super::search(&searxng(Some(url)), "a", 5).await.unwrap();
        assert_eq!((found.len(), found[0].title.as_str(), found[0].url.as_str()), (1, "A", "https://a.test"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn searxng_without_an_instance_is_an_error() {
        let err = super::search(&searxng(None), "a", 5).await.unwrap_err();
        assert!(err.contains("`url`"), "{err}");
    }

    #[test]
    fn the_chain_is_the_provider_then_what_else_is_usable() {
        let keyed = super::SearchSettings { provider: "tavily".into(), api_key: Some("k".into()), ..super::SearchSettings::default() };
        assert_eq!(super::sources(&keyed).unwrap(), vec![super::Source::Tavily("k".into()), super::Source::BravePage]);
        let local = super::SearchSettings { provider: "brave".into(), api_key: Some("k".into()), url: Some("http://localhost:8888".into()) };
        assert_eq!(
            super::sources(&local).unwrap(),
            vec![
                super::Source::BraveApi("k".into()),
                super::Source::Searxng { base: "http://localhost:8888".into(), key: None },
                super::Source::BravePage
            ]
        );
        assert_eq!(super::sources(&super::SearchSettings::default()).unwrap(), vec![super::Source::BravePage]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn every_source_failing_names_them_all() {
        // Nothing listens on ports 1 and 9, so both instances fail at once without a
        // network; the public page isn't in the chain because it answers for real when online.
        let chain = [super::Source::Searxng { base: "http://127.0.0.1:1".into(), key: None }, super::Source::Searxng { base: "http://127.0.0.1:9".into(), key: None }];
        let err = super::search_chain(&chain, "a", 5).await.unwrap_err();
        assert!(err.contains("every source") && err.contains("SearXNG"), "{err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_rate_limited_source_is_retried_once() {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let url = serve_flaky(hits.clone()).await;
        let chain = vec![super::Source::Searxng { base: url, key: None }];
        let found = super::search_chain(&chain, "a", 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transport_errors_name_their_cause() {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap();
        let error = client.get("http://127.0.0.1:1").send().await.unwrap_err();
        let text = super::fetch_error(&error);
        assert!(text.contains(": "), "the cause follows: {text}");
        assert!(text.to_ascii_lowercase().contains("refused") || text.to_ascii_lowercase().contains("failed"), "{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_dead_source_falls_back_to_the_next() {
        let body = r#"{"results":[{"title":"A","url":"https://a.test","content":"about a"}]}"#;
        let url = serve_body("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n", body).await;
        let chain = vec![
            super::Source::Searxng { base: "http://127.0.0.1:1".into(), key: None },
            super::Source::Searxng { base: url, key: None },
        ];
        let found = super::search_chain(&chain, "a", 5).await.unwrap();
        assert_eq!((found.len(), found[0].url.as_str()), (1, "https://a.test"));
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        assert!(super::retryable("Brave search answered HTTP 429 Too Many Requests: x"));
        assert!(super::retryable("SearXNG search failed: error sending request"));
        assert!(!super::retryable("Tavily search answered HTTP 401 Unauthorized: x"));
        assert!(!super::retryable("turned the search away (HTTP 403)"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn searxng_points_at_the_format_setting_when_refused() {
        let url = serve_body("HTTP/1.1 403 Forbidden\r\ncontent-type: text/plain\r\n\r\n", "not allowed").await;
        let err = super::search_chain(&[super::Source::Searxng { base: url, key: None }], "a", 5).await.unwrap_err();
        assert!(err.contains("json format"), "{err}");
    }
}
