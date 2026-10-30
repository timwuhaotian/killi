use super::*;

/// The `<title>` of an HTML page, tags and extra space stripped.
pub(super) fn html_title(html: &str) -> Option<String> {
    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
    title
        .captures(html)
        .map(|c| plain(&c[1]))
        .filter(|t| !t.is_empty())
}

pub(super) async fn extract(html: String, url: String) -> Option<String> {
    tokio::task::spawn_blocking(move || readable(&html, &url))
        .await
        .ok()
        .flatten()
}

/// Text of an HTML fragment: comments and tags removed, entities decoded, spaces collapsed.
pub(super) fn plain(fragment: &str) -> String {
    let comments = Regex::new(r"(?s)<!--.*?-->")
        .unwrap()
        .replace_all(fragment, "");
    let tags = Regex::new(r"(?s)<[^>]*>")
        .unwrap()
        .replace_all(&comments, "");
    decode(&tags)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The page's main content as Markdown, with its title on top. The Markdown keeps links;
/// on a short page (a home or index page) the page's own links are appended, so the next
/// address comes from the page and is never guessed.
pub(super) fn readable(html: &str, url: &str) -> Option<String> {
    let config = Config {
        text_mode: TextMode::Markdown,
        ..Default::default()
    };
    let article = Readability::new(html, Some(url), Some(config))
        .ok()?
        .parse()
        .ok()?;
    // With `TextMode::Markdown`, `text_content` is the Markdown (links kept as `[text](url)`).
    let body = article.text_content.trim().to_string();
    if body.is_empty() {
        return None;
    }
    let mut text = if article.title.is_empty() {
        body
    } else {
        format!("# {}\n\n{body}", article.title.trim())
    };
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
        if href.is_empty()
            || href.starts_with('#')
            || href.starts_with("javascript:")
            || href.starts_with("mailto:")
            || href.starts_with("tel:")
        {
            continue;
        }
        let Some(base) = &base else { break };
        let Ok(absolute) = base.join(href) else {
            continue;
        };
        let absolute = absolute.to_string();
        if !seen.insert(absolute.clone()) {
            continue;
        }
        let label = plain(&capture[2]);
        let label = if label.is_empty() {
            absolute.clone()
        } else {
            truncate(&label, 80).to_string()
        };
        links.push(format!("- [{label}]({absolute})"));
        if links.len() >= 60 {
            break;
        }
    }
    links
}
