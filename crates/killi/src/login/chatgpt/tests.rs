use super::*;
use std::sync::{Arc, Mutex};

fn jwt(claims: Value) -> String {
    format!(
        "{}.{}.sig",
        URL_SAFE_NO_PAD.encode(b"{\"alg\":\"RS256\"}"),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn scratch(name: &str) -> crate::test_temp::TempDir {
    crate::test_temp::TempDir::new(&format!("killi-login-{name}"))
}

fn signed_in_at_time(saved_at: u64) -> Credentials {
    Credentials {
        client_id: "oaiapp_1".into(),
        ext_agent_host_id: "urn:uuid:x".into(),
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        expires_in: 3600,
        scopes: vec![PLAN_SCOPE.into()],
        saved_at,
        ..Credentials::default()
    }
}

/// A token endpoint on a free local port that answers each POST with the next reply and
/// records the form bodies it got.
async fn token_server(replies: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/token",
        listener.local_addr().unwrap().port()
    );
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    tokio::spawn(async move {
        for (status, reply) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        seen.lock().unwrap().push(text[end + 4..].to_string());
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, bodies)
}

#[test]
fn the_pkce_challenge_matches_rfc_7636() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(random_token().unwrap().len(), 43);
    let uuid = uuid_v4().unwrap();
    assert_eq!((uuid.len(), &uuid[14..15]), (36, "4"));
    assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"), "{uuid}");
}

#[test]
fn only_this_attempts_callback_ends_the_wait() {
    let done = parse_callback(
        "/auth/callback?code=c1&state=s1&client_id=oaiapp_9&scope=openid+chatgpt.tokens.use.direct",
        "s1",
    );
    assert_eq!(
        done,
        Ok(Some(Callback {
            code: "c1".into(),
            client_id: Some("oaiapp_9".into()),
            scope: Some("openid chatgpt.tokens.use.direct".into())
        }))
    );
    assert_eq!(
        parse_callback("/auth/callback?code=c1&state=old", "s1"),
        Ok(None),
        "a stale tab is ignored"
    );
    assert_eq!(parse_callback("/favicon.ico", "s1"), Ok(None));
    assert_eq!(
        parse_callback("/callback?code=c1&state=s1", "s1"),
        Ok(None),
        "the path must match exactly"
    );
    assert!(
        parse_callback("/auth/callback?error=access_denied&state=s1", "s1")
            .unwrap_err()
            .contains("wasn't allowed")
    );
    assert!(parse_callback("/auth/callback?state=s1", "s1").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn the_callback_server_answers_the_browser_until_the_callback_comes() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let browser = tokio::spawn(async move {
        let mut pages = Vec::new();
        for target in [
            "/favicon.ico",
            "/auth/callback?code=c&state=old",
            "/auth/callback?code=c&state=s&client_id=oaiapp_1",
        ] {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut page = String::new();
            stream.read_to_string(&mut page).await.unwrap();
            pages.push(page);
        }
        pages
    });
    let callback = wait_for_callback(&listener, "s").await.unwrap();
    assert_eq!(
        (callback.code.as_str(), callback.client_id.as_deref()),
        ("c", Some("oaiapp_1"))
    );
    let pages = browser.await.unwrap();
    assert!(
        pages[0].starts_with("HTTP/1.1 404") && pages[1].starts_with("HTTP/1.1 404"),
        "{pages:?}"
    );
    assert!(
        pages[2].starts_with("HTTP/1.1 200") && pages[2].contains("Signed in to killi"),
        "{}",
        pages[2]
    );
}

#[test]
fn a_registration_keeps_its_client_id() {
    assert_eq!(issued_client("", Some("oaiapp_1")).unwrap(), "oaiapp_1");
    assert!(
        issued_client("", None).is_err(),
        "a new registration must return its client"
    );
    assert!(issued_client("", Some(REGISTER)).is_err());
    assert_eq!(issued_client("oaiapp_1", None).unwrap(), "oaiapp_1");
    assert_eq!(
        issued_client("oaiapp_1", Some("oaiapp_1")).unwrap(),
        "oaiapp_1"
    );
    assert!(issued_client("oaiapp_1", Some("oaiapp_2")).is_err());
}

#[test]
fn the_id_token_must_be_for_this_sign_in() {
    let claims = |aud: Value, nonce: &str, exp: u64| {
        jwt(
            serde_json::json!({"iss": ISSUER, "aud": aud, "nonce": nonce, "exp": exp, "sub": "user-1", "email": "a@b.c"}),
        )
    };
    let identified = identity(
        &claims("oaiapp_1".into(), "n", 2_000),
        "oaiapp_1",
        "n",
        1_000,
    )
    .unwrap();
    assert_eq!(identified, ("user-1".to_string(), "a@b.c".to_string()));
    assert!(
        identity(
            &claims(serde_json::json!(["x", "oaiapp_1"]), "n", 2_000),
            "oaiapp_1",
            "n",
            1_000
        )
        .is_ok()
    );
    assert!(
        identity(
            &claims("oaiapp_2".into(), "n", 2_000),
            "oaiapp_1",
            "n",
            1_000
        )
        .is_err()
    );
    assert!(
        identity(
            &claims("oaiapp_1".into(), "other", 2_000),
            "oaiapp_1",
            "n",
            1_000
        )
        .is_err()
    );
    assert!(
        identity(
            &claims("oaiapp_1".into(), "n", 1_000),
            "oaiapp_1",
            "n",
            1_000
        )
        .is_err()
    );
    assert!(identity("not-a-jwt", "oaiapp_1", "n", 1_000).is_err());
}

#[test]
fn a_token_is_refreshed_five_minutes_before_it_expires() {
    let credentials = signed_in_at_time(10_000);
    assert!(credentials.fresh(10_000 + 3600 - REFRESH_EARLY - 1));
    assert!(!credentials.fresh(10_000 + 3600 - REFRESH_EARLY));
}

#[test]
fn a_refresh_without_a_new_refresh_token_keeps_the_old_one() {
    let mut credentials = signed_in_at_time(0);
    credentials
        .take_tokens(
            &serde_json::json!({"access_token": "new", "expires_in": 60}),
            5,
        )
        .unwrap();
    assert_eq!(
        (
            credentials.access_token.as_str(),
            credentials.refresh_token.as_str()
        ),
        ("new", "old-refresh")
    );
    assert_eq!((credentials.expires_in, credentials.saved_at), (60, 5));
    assert!(
        credentials.plan(),
        "the scopes stay when the response leaves them out"
    );
    assert!(credentials.take_tokens(&serde_json::json!({}), 5).is_err());
}

#[test]
fn the_first_sign_in_registers_and_later_ones_reuse_the_client() {
    let attempt = Attempt {
        state: "s".into(),
        nonce: "n".into(),
        verifier: "v".into(),
    };
    let params = |saved: &Credentials| -> Vec<(String, String)> {
        authorize_url(saved, &attempt, "http://127.0.0.1:1455/auth/callback")
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    };
    let get = |params: &[(String, String)], key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let fresh = Credentials {
        ext_agent_host_id: "urn:uuid:h".into(),
        ..Credentials::default()
    };
    let first = params(&fresh);
    assert_eq!(get(&first, "client_id").as_deref(), Some(REGISTER));
    assert_eq!(get(&first, "agent_name_hint").as_deref(), Some("killi"));
    assert_eq!(
        get(&first, "ext_agent_host_id").as_deref(),
        Some("urn:uuid:h")
    );
    assert_eq!(get(&first, "resource").as_deref(), Some(RESOURCE));
    assert_eq!(
        get(&first, "code_challenge").as_deref(),
        Some(challenge("v").as_str())
    );
    assert_eq!(get(&first, "scope").as_deref(), Some(SCOPES));

    let registered = Credentials {
        client_id: "oaiapp_1".into(),
        email: "a@b.c".into(),
        ..fresh.clone()
    };
    let again = params(&registered);
    assert_eq!(get(&again, "client_id").as_deref(), Some("oaiapp_1"));
    assert_eq!(get(&again, "agent_name_hint"), None);
    assert_eq!(get(&again, "login_hint").as_deref(), Some("a@b.c"));
    assert_eq!(
        get(&again, "prompt"),
        None,
        "an ordinary sign-in doesn't force consent"
    );

    let declined = Credentials {
        scopes: vec!["openid".into()],
        ..registered
    };
    assert_eq!(
        get(&params(&declined), "prompt").as_deref(),
        Some("consent")
    );
}

#[test]
fn the_catalog_lists_visible_models_with_what_they_take() {
    let catalog = serde_json::json!({"models": [
        {"slug": "gpt-a", "visibility": "list", "context_window": 272000, "default_reasoning_level": "medium",
         "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}], "input_modalities": ["text", "image"]},
        {"slug": "hidden", "visibility": "hide"},
        {"slug": "gpt-b", "visibility": "list"}
    ]});
    let models = models_from(&catalog);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gpt-a", "gpt-b"]
    );
    assert_eq!(
        (
            models[0].context,
            models[0].images,
            models[0].default_effort.as_deref()
        ),
        (Some(272000), true, Some("medium"))
    );
    assert_eq!(models[0].efforts, ["low", "high"]);
    assert!(!models[1].images && models[1].efforts.is_empty());
    assert_eq!(
        models_from(&serde_json::json!({"data": [{"id": "gpt-c"}]}))[0].id,
        "gpt-c"
    );
}

#[test]
fn advice_points_at_usage_or_sign_in() {
    assert!(advice("HTTP 429 Too Many Requests: {\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}").contains(USAGE_URL));
    assert!(advice("HTTP 401 Unauthorized: {}").contains("killi login"));
    assert_eq!(advice("HTTP 400 Bad Request: {}"), "");
}

#[tokio::test(flavor = "current_thread")]
async fn a_fresh_token_is_sent_without_asking_the_server() {
    let tmp = scratch("fresh");
    let path = tmp.join("chatgpt.json");
    signed_in_at_time(now()).save(&path).unwrap();
    let http = reqwest::Client::new();
    let token = access_token_at(&http, &path, "http://127.0.0.1:9/unreachable", None)
        .await
        .unwrap();
    assert_eq!(token, "old-access");
    let none_tmp = scratch("none");
    let none = none_tmp.join("chatgpt.json");
    assert!(matches!(
        access_token_at(&http, &none, "http://127.0.0.1:9/", None).await,
        Err(TokenError::SignIn(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn an_expiring_or_rejected_token_is_refreshed_and_saved() {
    let tmp = scratch("refresh");
    let path = tmp.join("chatgpt.json");
    signed_in_at_time(now() - 3500).save(&path).unwrap();
    let (url, bodies) = token_server(vec![
        (
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
        ),
        (
            200,
            r#"{"access_token":"newer-access","refresh_token":"newer-refresh","expires_in":3600}"#,
        ),
    ])
    .await;
    let http = reqwest::Client::new();
    assert_eq!(
        access_token_at(&http, &path, &url, None).await.unwrap(),
        "new-access"
    );
    let saved = Credentials::load(&path).unwrap().unwrap();
    assert_eq!(
        (saved.refresh_token.as_str(), saved.client_id.as_str()),
        ("new-refresh", "oaiapp_1")
    );
    let form = bodies.lock().unwrap()[0].clone();
    for field in [
        "grant_type=refresh_token",
        "client_id=oaiapp_1",
        "refresh_token=old-refresh",
        "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
    ] {
        assert!(form.contains(field), "{form}");
    }
    assert_eq!(
        access_token_at(&http, &path, &url, None).await.unwrap(),
        "new-access",
        "a fresh token is reused"
    );
    assert_eq!(
        access_token_at(&http, &path, &url, Some("new-access"))
            .await
            .unwrap(),
        "newer-access"
    );
    assert!(
        bodies.lock().unwrap()[1].contains("refresh_token=new-refresh"),
        "the rotated token is the one spent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn requests_waiting_on_one_refresh_take_its_token() {
    let tmp = scratch("race");
    let path = tmp.join("chatgpt.json");
    signed_in_at_time(now() - 3500).save(&path).unwrap();
    let (url, bodies) = token_server(vec![(
        200,
        r#"{"access_token":"shared","refresh_token":"r2","expires_in":3600}"#,
    )])
    .await;
    let http = reqwest::Client::new();
    let (first, second) = tokio::join!(
        access_token_at(&http, &path, &url, None),
        access_token_at(&http, &path, &url, None)
    );
    assert_eq!(
        (first.unwrap(), second.unwrap()),
        ("shared".to_string(), "shared".to_string())
    );
    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "the refresh token is spent once"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_unusable_refresh_token_signs_out_and_a_server_error_does_not() {
    let tmp = scratch("unusable");
    let path = tmp.join("chatgpt.json");
    signed_in_at_time(now() - 3500).save(&path).unwrap();
    let (url, _) = token_server(vec![
        (503, r#"{"error":"temporarily_unavailable"}"#),
        (400, r#"{"error":"refresh_token_reused"}"#),
    ])
    .await;
    let http = reqwest::Client::new();
    assert!(matches!(
        access_token_at(&http, &path, &url, None).await,
        Err(TokenError::Transient(_))
    ));
    assert!(
        Credentials::load(&path).unwrap().unwrap().signed_in(),
        "a server error keeps the sign-in"
    );
    let Err(TokenError::SignIn(message)) = access_token_at(&http, &path, &url, None).await else {
        panic!("expected a sign-in error")
    };
    assert!(
        message.contains("killi login") && message.contains("refresh_token_reused"),
        "{message}"
    );
    let saved = Credentials::load(&path).unwrap().unwrap();
    assert!(!saved.signed_in() && saved.refresh_token.is_empty());
    assert_eq!(
        (saved.client_id.as_str(), saved.ext_agent_host_id.as_str()),
        ("oaiapp_1", "urn:uuid:x"),
        "the registration stays"
    );
}
