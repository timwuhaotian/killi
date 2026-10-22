use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;
use serde_json::Value;
use tokio::process::Command;

use super::{Context, MAX_BYTES, Output, SearchSettings, files};
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
            Ok(dom) => page(dom, &url, raw).await,
            Err(e) => Output::error(e),
        };
    }
    let client = match reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build() {
        Ok(client) => client,
        Err(e) => return Output::error(format!("Could not create an HTTP client: {e}")),
    };
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => return Output::error(format!("Could not fetch {url}: {e}")),
    };
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let final_url = response.url().to_string();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {url}: {e}")),
    };
    if !status.is_success() {
        return Output::error(format!("{url} answered HTTP {status}: {}", truncate(&String::from_utf8_lossy(&bytes), 500)));
    }
    let bytes = &bytes[..bytes.len().min(MAX_DOWNLOAD)];
    let text = String::from_utf8_lossy(bytes).into_owned();
    if kind.contains("html") || (kind.is_empty() && looks_like_html(bytes)) {
        if raw {
            return shown(&text);
        }
        if let Some(article) = extract(text, final_url.clone()).await.filter(|a| a.len() >= MIN_READABLE) {
            return shown(&article);
        }
        // Little or nothing in the static HTML: the page probably builds itself with
        // JavaScript, so let a real browser run it, if there is one.
        return match render(&final_url).await {
            Ok(dom) => page(dom, &final_url, false).await,
            Err(_) => Output::error(format!("Could not find readable text in {url}. It may need JavaScript; try render.")),
        };
    }
    if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") || kind.is_empty() {
        return shown(&text);
    }
    Output::error(format!("{url} is `{kind}`, not text."))
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
async fn page(dom: String, url: &str, raw: bool) -> Output {
    if raw {
        return shown(&dom);
    }
    match extract(dom, url.to_string()).await {
        Some(article) => shown(&article),
        None => Output::error(format!("Could not find readable text in {url}.")),
    }
}

async fn extract(html: String, url: String) -> Option<String> {
    tokio::task::spawn_blocking(move || readable(&html, &url)).await.ok().flatten()
}

fn shown(text: &str) -> Output {
    let shown = truncate(text, MAX_BYTES);
    let more = text.len() - shown.len();
    let note = if more > 0 { format!("\n\n[Truncated: {more} more characters.]") } else { String::new() };
    Output::ok(format!("{shown}{note}")).brief(format!("{} KB", text.len() / 1024))
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
/// in its own process group so the helpers it starts can be killed with it.
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
        .process_group(0)
        .kill_on_drop(true);
    Ok((command, profile))
}

/// Kills the browser and the helpers it started, then reaps it.
async fn stop(child: &mut tokio::process::Child, profile: &std::path::Path) {
    if let Some(pid) = child.id() {
        // SAFETY: the child leads its own process group, so this reaches only the browser.
        unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    }
    let _ = child.wait().await;
    let _ = std::fs::remove_dir_all(profile);
}

/// The page's DOM after its scripts have run. Chrome prints the DOM but often doesn't exit
/// afterwards, so this stops reading at the closing `</html>`.
async fn render(url: &str) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let (mut command, profile) = browser(url, &["--dump-dom".to_string()])?;
    let mut child = command.stdout(Stdio::piped()).spawn().map_err(|e| format!("Could not run the browser: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
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
    stop(&mut child, &profile).await;
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
    let mut child = command.spawn().map_err(|e| format!("Could not run the browser: {e}"))?;
    let started = std::time::Instant::now();
    let mut last = 0;
    let outcome = loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let size = std::fs::metadata(file).map_or(0, |m| m.len());
        if size > 0 && size == last {
            break Ok(());
        }
        last = size;
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            break if file.exists() { Ok(()) } else { Err(format!("The browser made no screenshot of {url}.")) };
        }
        if started.elapsed() > BROWSER_TIMEOUT {
            break Err(format!("The browser took more than {} s on {url}.", BROWSER_TIMEOUT.as_secs()));
        }
    };
    stop(&mut child, &profile).await;
    outcome
}

async fn screenshot(ctx: &Context, url: &str) -> Output {
    if !ctx.images {
        return Output::error("This model can't see images, so a screenshot is no use.");
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let file = std::env::temp_dir().join(format!("killi-shot-{}-{nanos}.png", std::process::id()));
    let output = match capture(url, &file).await {
        Ok(()) => {
            let mut output = files::read_image(ctx, url, &file, "image/png");
            if !output.is_error {
                output.text = format!("Screenshot of {url} (1280x900, top of the page); it is attached below.");
            }
            output
        }
        Err(e) => Output::error(e),
    };
    let _ = std::fs::remove_file(&file);
    output
}

pub async fn web_search(ctx: &Context, args: &Value) -> Output {
    let Some(query) = args.get("query").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()) else {
        return Output::error("web_search needs `query`.");
    };
    let limit = args.get("limit").and_then(Value::as_u64).map_or(DEFAULT_RESULTS, |l| (l as usize).clamp(1, MAX_RESULTS));
    let found = match search(&ctx.search, query, limit).await {
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

struct Hit {
    title: String,
    url: String,
    snippet: String,
}

async fn search(settings: &SearchSettings, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Could not create an HTTP client: {e}"))?;
    let key = settings.api_key.as_deref();
    let provider = if settings.provider.is_empty() { "brave" } else { settings.provider.as_str() };
    match (provider, key) {
        ("tavily", Some(key)) => {
            let body = serde_json::json!({"query": query, "max_results": limit});
            let reply = client.post("https://api.tavily.com/search").bearer_auth(key).header("content-type", "application/json").body(body.to_string()).send().await;
            let json = json_reply(reply, "Tavily").await?;
            Ok(hits(&json["results"], "title", "url", "content", limit))
        }
        ("tavily", None) => Err("web_search with Tavily needs an API key: set `api_key` or `api_key_env` under [search].".into()),
        ("brave", Some(key)) => {
            let reply = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("X-Subscription-Token", key)
                .query(&[("q", query), ("count", &limit.to_string())])
                .send()
                .await;
            let json = json_reply(reply, "Brave").await?;
            Ok(hits(&json["web"]["results"], "title", "url", "description", limit))
        }
        ("brave", None) => {
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
                return Err(format!(
                    "Brave turned the search away (HTTP {status}). Set an API key under [search] in the config to search reliably."
                ));
            }
            Ok(found)
        }
        (other, _) => Err(format!("Unknown search provider `{other}`; use \"brave\" or \"tavily\".")),
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

/// The page's main content as Markdown, with its title on top.
fn readable(html: &str, url: &str) -> Option<String> {
    let config = Config { text_mode: TextMode::Markdown, ..Default::default() };
    let article = Readability::new(html, Some(url), Some(config)).ok()?.parse().ok()?;
    let body = article.text_content.trim().to_string();
    if body.is_empty() {
        return None;
    }
    Some(if article.title.is_empty() { body } else { format!("# {}\n\n{body}", article.title.trim()) })
}

#[cfg(test)]
mod tests {
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
}
