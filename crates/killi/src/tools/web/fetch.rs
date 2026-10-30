use super::*;

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
    let client = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
    {
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
    let is_pdf = kind.contains("application/pdf")
        || url
            .split(['?', '#'])
            .next()
            .is_some_and(|u| u.to_ascii_lowercase().ends_with(".pdf"));
    // A body that can't be shown is not downloaded at all.
    if status.is_success() && !kind.is_empty() && !is_text(&kind) && !is_pdf {
        return Output::error(format!(
            "{url} is `{kind}`, not text. PDFs aside, fetch_url only shows text, including HTML."
        ));
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
        let hint = if status.as_u16() == 404 {
            " The address is likely wrong; do not guess variants — find the real URL with web_search."
        } else {
            ""
        };
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
        if let Some(article) = extract(text, final_url.clone())
            .await
            .filter(|a| a.len() >= MIN_READABLE)
        {
            return shown(ctx, &article);
        }
        // Little or nothing in the static HTML: the page probably builds itself with
        // JavaScript, so let a real browser run it, if there is one.
        return match render(&final_url).await {
            Ok(dom) => page(ctx, dom, &final_url, false).await,
            Err(_) => Output::error(format!(
                "Could not find readable text in {url}. It may need JavaScript; try render."
            )),
        };
    }
    if kind.is_empty() || is_text(&kind) {
        return shown(ctx, &text);
    }
    Output::error(format!("{url} is `{kind}`, not text."))
}

/// Why a request failed, with the whole cause chain and what to do about it: a name that
/// does not resolve means the address itself is invented, so guessing spellings is wasted.
pub(super) fn fetch_error(error: &reqwest::Error) -> String {
    let detail = crate::llm::error_chain(error);
    let hint = if error.is_timeout() {
        " The site took too long; retry once or try another source."
    } else if detail.contains("dns error")
        || detail.contains("failed to lookup address")
        || detail.contains("Name or service not known")
        || detail.contains("nodename nor servname")
    {
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

/// How long `pdftotext` may take over a PDF before the read gives up and kills it.
const PDF_TIMEOUT: Duration = Duration::from_secs(60);

/// Extracts a downloaded PDF's text with `pdftotext` (poppler) when it is installed.
/// The run has its own session and a timeout, and is tracked so quitting ends it too.
async fn pdf_text(ctx: &Context, bytes: &[u8], url: &str) -> Output {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let file =
        Temp(std::env::temp_dir().join(format!("killi-pdf-{}-{nanos}.pdf", std::process::id())));
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
                Output::error(format!(
                    "{url} is a PDF with no extractable text (scanned images or encrypted); fetch_url cannot read it."
                ))
            } else {
                shown(ctx, text.trim())
            }
        }
        Ok(Ok(out)) => Output::error(format!(
            "{url} is a PDF pdftotext could not read: {}",
            truncate(&String::from_utf8_lossy(&out.stderr), 300)
        )),
        Ok(Err(e)) => Output::error(format!("Could not run pdftotext on {url}: {e}")),
        Err(_) => Output::error(format!(
            "{url} is a PDF pdftotext took more than {} s to read.",
            PDF_TIMEOUT.as_secs()
        )),
    }
}

/// Whether a content type is one `fetch_url` can show.
fn is_text(kind: &str) -> bool {
    kind.contains("html")
        || kind.starts_with("text/")
        || kind.contains("json")
        || kind.contains("xml")
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
pub(super) fn raw_github(url: &str) -> String {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return url.to_string();
    };
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

/// The text within the display budget; longer text is cut and spilled to a file, like other
/// tools' truncated output, so the model pages through it with `read`.
pub(super) fn shown(ctx: &Context, text: &str) -> Output {
    let shown = truncate(text, MAX_BYTES);
    let more = text.len() - shown.len();
    if more == 0 {
        return Output::ok(text.to_string()).brief(format!("{} KB", text.len() / 1024));
    }
    let saved = match super::spill(ctx, "fetch", text) {
        Some(path) => format!(
            " The full text is saved at {}; read it with offset and limit.",
            path.display()
        ),
        None => String::new(),
    };
    Output::ok(format!(
        "{shown}\n\n[Showing the first {} of {} bytes.{saved}]",
        shown.len(),
        text.len()
    ))
    .brief(format!("{} KB", text.len() / 1024))
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    start.contains("<!doctype html") || start.contains("<html")
}
