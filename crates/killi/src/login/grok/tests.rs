use super::*;
use crate::login::chatgpt::tests::token_server;

fn scratch(name: &str) -> crate::test_temp::TempDir {
    crate::test_temp::TempDir::new(&format!("killi-grok-{name}"))
}

fn signed_in_until(expires_at: u64) -> Credentials {
    Credentials {
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        expires_at,
    }
}

fn device() -> Device {
    Device {
        device_code: "dev-1".into(),
        user_code: "ABCD-1234".into(),
        verification_uri: "https://accounts.x.ai/oauth2/device".into(),
        verification_uri_complete: None,
        expires_in: Some(600),
        interval: Some(1),
    }
}

#[test]
fn a_token_lasts_as_the_response_says_else_as_the_jwt_says() {
    let mut credentials = Credentials::default();
    credentials
        .take_tokens(
            &serde_json::json!({"access_token": "a", "refresh_token": "r", "expires_in": 900}),
            1000,
        )
        .unwrap();
    assert_eq!(
        (credentials.expires_at, credentials.refresh_token.as_str()),
        (1900, "r")
    );
    let claims = URL_SAFE_NO_PAD.encode(br#"{"exp":5000}"#);
    let jwt = format!("h.{claims}.s");
    credentials
        .take_tokens(&serde_json::json!({"access_token": jwt}), 1000)
        .unwrap();
    assert_eq!(credentials.expires_at, 5000);
    assert_eq!(
        credentials.refresh_token, "r",
        "a refresh that sends none keeps the old one"
    );
    credentials
        .take_tokens(&serde_json::json!({"access_token": "opaque"}), 1000)
        .unwrap();
    assert_eq!(credentials.expires_at, 1000 + ASSUMED_LIFETIME);
    assert!(
        credentials
            .take_tokens(&serde_json::json!({}), 1000)
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_device_flow_waits_out_pending_and_slow_down_then_takes_the_tokens() {
    let (url, bodies) = token_server(vec![
        (200, r#"{"device_code":"dev-1","user_code":"ABCD-1234","verification_uri":"https://accounts.x.ai/oauth2/device","verification_uri_complete":"https://accounts.x.ai/oauth2/device?user_code=ABCD-1234","expires_in":600,"interval":1}"#),
        (400, r#"{"error":"authorization_pending"}"#),
        (400, r#"{"error":"slow_down"}"#),
        (200, r#"{"access_token":"a1","refresh_token":"r1","expires_in":900}"#),
    ])
    .await;
    let http = reqwest::Client::new();
    let endpoints = Endpoints {
        device: &url,
        token: &url,
    };
    let device = request_device(&http, &endpoints).await.unwrap();
    assert_eq!(device.user_code, "ABCD-1234");
    let tokens = poll(&http, &endpoints, &device, Duration::from_millis(1))
        .await
        .unwrap();
    assert_eq!(tokens["access_token"], "a1");
    let bodies = bodies.lock().unwrap();
    assert!(
        bodies[0].contains("client_id=b1a00492") && bodies[0].contains("grok-cli%3Aaccess"),
        "{}",
        bodies[0]
    );
    for poll in &bodies[1..] {
        assert!(
            poll.contains("device_code=dev-1") && poll.contains("grant_type=urn%3Aietf"),
            "{poll}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_declined_or_expired_code_ends_the_sign_in() {
    let http = reqwest::Client::new();
    let (url, _) = token_server(vec![(400, r#"{"error":"access_denied"}"#)]).await;
    let endpoints = Endpoints {
        device: &url,
        token: &url,
    };
    let error = poll(&http, &endpoints, &device(), Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("declined"), "{error}");

    let (url, _) = token_server(vec![(400, r#"{"error":"expired_token"}"#)]).await;
    let endpoints = Endpoints {
        device: &url,
        token: &url,
    };
    let error = poll(&http, &endpoints, &device(), Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expired"), "{error}");

    let mut short = device();
    short.expires_in = Some(1);
    short.interval = Some(2);
    let error = poll(&http, &endpoints, &short, Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("expired"),
        "a code past its lifetime isn't polled: {error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_fresh_token_is_sent_and_an_expiring_one_is_rotated() {
    let tmp = scratch("rotate");
    let path = tmp.join("grok.json");
    let http = reqwest::Client::new();
    signed_in_until(now() + 3600).save(&path).unwrap();
    assert_eq!(
        access_token_at(&http, &path, "http://127.0.0.1:9/unreachable", None)
            .await
            .unwrap(),
        "old-access"
    );
    let (url, bodies) = token_server(vec![
        (
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":900}"#,
        ),
        (
            200,
            r#"{"access_token":"newer-access","refresh_token":"newer-refresh","expires_in":900}"#,
        ),
    ])
    .await;
    assert_eq!(
        access_token_at(&http, &path, &url, Some("old-access"))
            .await
            .unwrap(),
        "new-access",
        "a rejected token is refreshed though it hasn't expired"
    );
    let saved = Credentials::load(&path).unwrap().unwrap();
    assert_eq!(saved.refresh_token, "new-refresh");
    assert!(bodies.lock().unwrap()[0].contains("refresh_token=old-refresh"));
    signed_in_until(now() + 60).save(&path).unwrap();
    assert_eq!(
        access_token_at(&http, &path, &url, None).await.unwrap(),
        "newer-access"
    );
    assert!(
        std::fs::metadata(&path)
            .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777)
            .unwrap()
            == 0o600
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_dead_refresh_token_signs_out_but_a_tier_refusal_or_outage_does_not() {
    let tmp = scratch("refusals");
    let path = tmp.join("grok.json");
    let http = reqwest::Client::new();
    let (url, _) = token_server(vec![
        (503, r#"{}"#),
        (403, r#"{"error":"forbidden"}"#),
        (400, r#"{"error":"invalid_request"}"#),
        (400, r#"{"error":"invalid_grant"}"#),
    ])
    .await;
    signed_in_until(0).save(&path).unwrap();
    assert!(matches!(
        access_token_at(&http, &path, &url, None).await,
        Err(TokenError::Transient(_))
    ));
    let Err(TokenError::SignIn(message)) = access_token_at(&http, &path, &url, None).await else {
        panic!("a 403 is the plan's answer");
    };
    assert!(message.contains("/provider grok"), "{message}");
    assert!(
        Credentials::load(&path).unwrap().unwrap().signed_in(),
        "a 403 keeps the tokens"
    );
    assert!(matches!(
        access_token_at(&http, &path, &url, None).await,
        Err(TokenError::SignIn(_))
    ));
    assert!(
        Credentials::load(&path).unwrap().unwrap().signed_in(),
        "a refusal that doesn't call the token dead keeps it"
    );
    let Err(TokenError::SignIn(message)) = access_token_at(&http, &path, &url, None).await else {
        panic!("invalid_grant ends the sign-in");
    };
    assert!(message.contains("killi login grok"), "{message}");
    assert!(!Credentials::load(&path).unwrap().unwrap().signed_in());
    assert!(matches!(
        access_token_at(&http, &path, &url, None).await,
        Err(TokenError::SignIn(_))
    ));
}

#[test]
fn advice_names_the_way_out() {
    assert!(advice("HTTP 401 Unauthorized: {}").contains("killi login grok"));
    assert!(advice("HTTP 403 Forbidden: {}").contains("/provider grok"));
    assert!(advice("HTTP 429 Too Many Requests: {}").contains(PLAN_URL));
    assert_eq!(advice("HTTP 400 Bad Request: {}"), "");
}

#[test]
fn the_earlier_of_expires_in_and_the_jwt_wins() {
    let jwt = |exp: u64| {
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
        )
    };
    let mut credentials = Credentials::default();
    credentials
        .take_tokens(
            &serde_json::json!({"access_token": jwt(1300), "expires_in": 3600}),
            1000,
        )
        .unwrap();
    assert_eq!(credentials.expires_at, 1300);
    credentials
        .take_tokens(
            &serde_json::json!({"access_token": jwt(9000), "expires_in": 600}),
            1000,
        )
        .unwrap();
    assert_eq!(credentials.expires_at, 1600);
}

#[tokio::test(flavor = "current_thread")]
async fn a_first_sign_in_locks_before_killis_folder_exists() {
    let tmp = scratch("fresh-home");
    let path = tmp.join(".killi").join("grok.json");
    let _lock = Lock::acquire(&path, "Grok").await.unwrap();
    assert!(tmp.join(".killi").join("grok.lock").exists());
}
