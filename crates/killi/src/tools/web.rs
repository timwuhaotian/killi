use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use serde_json::Value;

use super::{MAX_BYTES, Output};
use crate::llm::truncate;

const MAX_DOWNLOAD: usize = 10 * 1024 * 1024;
/// Some sites turn away clients that don't look like a browser.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";

pub async fn fetch_url(args: &Value) -> Output {
    let Some(url) = args.get("url").and_then(Value::as_str) else {
        return Output::error("fetch_url needs `url`.");
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Output::error("fetch_url only fetches http:// and https:// URLs.");
    }
    let client = match reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build() {
        Ok(client) => client,
        Err(e) => return Output::error(format!("Could not create an HTTP client: {e}")),
    };
    let response = match client.get(url).send().await {
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
    let text = if kind.contains("html") || (kind.is_empty() && looks_like_html(bytes)) {
        let html = String::from_utf8_lossy(bytes).into_owned();
        match tokio::task::spawn_blocking(move || readable(&html, &final_url)).await {
            Ok(Some(text)) => text,
            _ => return Output::error(format!("Could not find readable text in {url}.")),
        }
    } else if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") || kind.is_empty() {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        return Output::error(format!("{url} is `{kind}`, not text."));
    };
    let shown = truncate(&text, MAX_BYTES);
    let more = text.len() - shown.len();
    let note = if more > 0 { format!("\n\n[Truncated: {more} more characters.]") } else { String::new() };
    Output::ok(format!("{shown}{note}")).brief(format!("{} KB", text.len() / 1024))
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
}
