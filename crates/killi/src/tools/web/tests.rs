use std::path::Path;
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

#[test]
fn searxng_settings_enable_the_json_format() {
    let yaml = super::setup::settings_yaml("abc123");
    assert!(yaml.contains("use_default_settings: true"), "{yaml}");
    assert!(yaml.contains("limiter: false"), "the limiter would turn scripted requests away: {yaml}");
    assert!(yaml.contains("formats:\n    - html\n    - json"), "{yaml}");
    assert!(yaml.contains("secret_key: \"abc123\""), "{yaml}");
    assert!(yaml.contains(&super::setup::url()), "the instance knows its own URL: {yaml}");
}

#[test]
fn the_docker_run_mounts_killis_settings_on_localhost_only() {
    let args = super::setup::run_args(Path::new("/home/u/.killi/searxng/settings.yml"));
    let joined = args.join(" ");
    assert!(joined.contains("--name killi-searxng"), "{joined}");
    assert!(joined.contains("--restart unless-stopped"), "it should come back with Docker: {joined}");
    assert!(joined.contains("-p 127.0.0.1:8888:8080"), "the port binds localhost, not every interface: {joined}");
    assert!(joined.contains("-v /home/u/.killi/searxng/settings.yml:/etc/searxng/settings.yml:ro"), "{joined}");
    assert!(joined.ends_with("searxng/searxng"), "{joined}");
}

/// Serves `n` probes with 503 and the rest with a json results body, counting requests.
async fn serve_warming(up_after: usize) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let n = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = socket.read(&mut [0u8; 4096]).await;
            let reply = if n < up_after {
                "HTTP/1.1 503 Service Unavailable\r\ncontent-type: text/plain\r\n\r\nstarting"
            } else {
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"results\":[{\"title\":\"A\",\"url\":\"https://a.test\"}]}"
            };
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    (url, hits)
}

#[tokio::test(flavor = "current_thread")]
async fn ready_once_the_instance_answers_json() {
    let (url, hits) = serve_warming(2).await;
    super::setup::wait_ready(&url, std::time::Duration::from_millis(10), std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3, "two refusals, then the answer");
}

#[tokio::test(flavor = "current_thread")]
async fn an_instance_that_never_answers_times_out() {
    let (url, _) = serve_warming(usize::MAX).await;
    let err = super::setup::wait_ready(&url, std::time::Duration::from_millis(10), std::time::Duration::from_millis(100)).await.unwrap_err();
    assert!(err.contains("never answered"), "{err}");
}
