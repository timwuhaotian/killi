use super::*;

const DEFAULT_RESULTS: usize = 8;
const MAX_RESULTS: usize = 20;

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
pub(super) struct Hit {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) snippet: String,
}

/// One way of searching, tried in `search`'s order until one returns results.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Source {
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
pub(super) fn sources(settings: &SearchSettings) -> Result<Vec<Source>, String> {
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

pub(super) async fn search(settings: &SearchSettings, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    search_chain(&sources(settings)?, query, limit).await
}

/// Tries each source in order, retrying a transient failure once, until one returns
/// results; when none does, every failure is named so the model can say what it tried.
pub(super) async fn search_chain(chain: &[Source], query: &str, limit: usize) -> Result<Vec<Hit>, String> {
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
pub(super) fn retryable(error: &str) -> bool {
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

pub(super) fn hits(results: &Value, title: &str, url: &str, snippet: &str, limit: usize) -> Vec<Hit> {
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
pub(super) fn brave_page(html: &str, limit: usize) -> Vec<Hit> {
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
