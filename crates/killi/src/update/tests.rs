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
    let bytes = super::download(&mut status, &http, &format!("{base}/{}", super::BINARY), "9.9.9")
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

#[tokio::test(flavor = "current_thread")]
async fn a_newer_release_comes_with_the_changelog_above_the_releases_folder() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/main/releases", listener.local_addr().unwrap());
    let changelog = "# Changelog\n\n## 0.2.0 — 2026-11-09\n\n- New.\n\n## 0.1.0 — 2026-11-08\n\n- Old.\n";
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let n = socket.read(&mut request).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..n]).to_string();
            let response = if request.starts_with("GET /main/releases/latest.toml ") {
                respond("200 OK", "version = \"0.2.0\"\nsha256 = \"abc\"\n")
            } else if request.starts_with("GET /main/CHANGELOG.md ") {
                respond("200 OK", changelog)
            } else {
                respond("404 Not Found", "")
            };
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let http = super::http_client().unwrap();
    let release = super::check_at(&http, &base, "0.1.0").await.unwrap().unwrap();
    assert_eq!(release.version, "0.2.0");
    assert_eq!(
        release.notes,
        vec![super::notes::Section {
            heading: "0.2.0 — 2026-11-09".into(),
            body: "- New.".into(),
        }]
    );
    assert!(super::check_at(&http, &base, "0.2.0").await.unwrap().is_none());
}

#[test]
fn versions_compare_numerically() {
    assert!(super::parse("0.10.0") > super::parse("0.9.3"));
    assert!(super::parse("0.1.0") == super::parse("0.1.0"));
}

#[test]
fn the_binary_name_matches_the_host() {
    #[cfg(target_os = "macos")]
    {
        assert_eq!(super::BINARY, "killi-arm64");
        assert_eq!(super::SHA_KEY, "sha256-arm64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        assert_eq!(super::BINARY, "killi-linux-x64");
        assert_eq!(super::SHA_KEY, "sha256-linux-x64");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        assert_eq!(super::BINARY, "killi-linux-arm64");
        assert_eq!(super::SHA_KEY, "sha256-linux-arm64");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn latest_prefers_the_per_binary_checksum() {
    let body = format!(
        "version = \"9.9.9\"\n{} = \"per-binary\"\nsha256 = \"legacy\"\n",
        super::SHA_KEY
    );
    let base = serve(vec![respond("200 OK", &body)]).await;
    let http = super::http_client().unwrap();
    assert_eq!(
        super::latest(&http, &base).await.unwrap(),
        ("9.9.9".to_string(), "per-binary".to_string())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn latest_falls_back_to_the_legacy_checksum() {
    let base = serve(vec![respond("200 OK", "version = \"9.9.9\"\nsha256 = \"legacy\"\n")]).await;
    let http = super::http_client().unwrap();
    assert_eq!(
        super::latest(&http, &base).await.unwrap(),
        ("9.9.9".to_string(), "legacy".to_string())
    );
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
        line.contains('🧊'),
        "the ice-cube spinner: {line:?}"
    );
}
