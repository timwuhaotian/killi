use super::*;
use anyhow::{anyhow, bail};

pub(super) fn authorize_url(
    saved: &Credentials,
    attempt: &Attempt,
    redirect_uri: &str,
) -> Result<reqwest::Url> {
    let challenge = challenge(&attempt.verifier);
    let registered = !saved.client_id.is_empty();
    let mut params = vec![
        ("response_type", "code"),
        (
            "client_id",
            if registered {
                &saved.client_id
            } else {
                REGISTER
            },
        ),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("resource", RESOURCE),
        ("state", &attempt.state),
        ("nonce", &attempt.nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
        ("ext_agent_host_id", &saved.ext_agent_host_id),
    ];
    if !registered {
        params.push(("agent_name_hint", "killi"));
    } else if !saved.email.is_empty() {
        params.push(("login_hint", &saved.email));
    }
    // Plan use was declined last time: show the consent screen again instead of skipping it.
    if registered && !saved.scopes.is_empty() && !saved.plan() {
        params.push(("prompt", "consent"));
    }
    Ok(reqwest::Url::parse_with_params(AUTHORIZE_URL, &params)?)
}

/// Answers the browser until a request carries this attempt's callback.
pub(super) async fn wait_for_callback(listener: &TcpListener, state: &str) -> Result<Callback> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request.windows(4).any(|end| end == b"\r\n\r\n") && request.len() < 16_384 {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => request.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        let request = String::from_utf8_lossy(&request);
        let target = request.split_whitespace().nth(1).unwrap_or("/");
        let (status, page, outcome) = match parse_callback(target, state) {
            Ok(None) => ("404 Not Found", "Nothing here.".to_string(), None),
            Ok(Some(callback)) => (
                "200 OK",
                "Signed in to killi. You can close this tab.".to_string(),
                Some(Ok(callback)),
            ),
            Err(message) => (
                "200 OK",
                format!(
                    "Sign-in didn't finish: {}. You can close this tab.",
                    escape(&message)
                ),
                Some(Err(anyhow!(message))),
            ),
        };
        let body = format!("<!doctype html><meta charset=utf-8><title>killi</title><p>{page}</p>");
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(format!("{head}{body}").as_bytes()).await;
        let _ = stream.shutdown().await;
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
}

/// Reads a callback request's `target` (path and query). `Ok(None)` is a request to ignore —
/// another path, or a state from an older attempt — and an `Err` ends the attempt.
pub(super) fn parse_callback(target: &str, state: &str) -> Result<Option<Callback>, String> {
    let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Ok(None);
    };
    if url.path() != CALLBACK_PATH {
        return Ok(None);
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Ok(None);
    }
    if let Some(error) = param("error") {
        return Err(match error.as_str() {
            "access_denied" => "ChatGPT plan use wasn't allowed for killi".to_string(),
            _ => format!("{error} {}", param("error_description").unwrap_or_default())
                .trim()
                .to_string(),
        });
    }
    let code = param("code")
        .filter(|code| !code.is_empty())
        .ok_or("the browser came back without a code")?;
    Ok(Some(Callback {
        code,
        client_id: param("client_id"),
        scope: param("scope"),
    }))
}

/// The client ID after a callback: the one this sign-in registered, or the saved one, which a
/// callback must not replace.
pub(super) fn issued_client(saved: &str, returned: Option<&str>) -> Result<String> {
    match (saved.is_empty(), returned) {
        (true, Some(issued)) if !issued.is_empty() && issued != REGISTER => Ok(issued.to_string()),
        (true, _) => bail!(
            "ChatGPT didn't return a client ID for killi, so its registration didn't finish; run `killi login chatgpt` again"
        ),
        (false, Some(other)) if other != saved => bail!(
            "ChatGPT returned a client ID ({other}) other than killi's registration ({saved}); sign-in stopped"
        ),
        (false, _) => Ok(saved.to_string()),
    }
}

/// The ID token's subject and email, after checking its issuer, audience, expiry and nonce.
/// Its signature isn't checked: the token came straight from the token endpoint over TLS,
/// which OpenID Connect accepts instead (Core 1.0, 3.1.3.7), and killi uses it only to name
/// the account.
pub(super) fn identity(
    id_token: &str,
    client_id: &str,
    nonce: &str,
    now: u64,
) -> Result<(String, String)> {
    let claims = id_token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok())
        .and_then(|json| serde_json::from_slice::<Value>(&json).ok())
        .ok_or_else(|| anyhow!("the ID token can't be read"))?;
    let for_killi = match &claims["aud"] {
        Value::Array(audiences) => audiences.iter().any(|audience| audience == client_id),
        audience => audience == client_id,
    };
    if claims["iss"] != ISSUER {
        bail!("the ID token is from {}, not {ISSUER}", claims["iss"]);
    }
    if !for_killi {
        bail!("the ID token is for another client");
    }
    if claims["exp"].as_u64().is_none_or(|expiry| expiry <= now) {
        bail!("the ID token has expired");
    }
    if claims["nonce"] != nonce {
        bail!("the ID token's nonce doesn't match this sign-in");
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|subject| !subject.is_empty())
        .ok_or_else(|| anyhow!("the ID token names no account"))?;
    Ok((
        subject.to_string(),
        claims["email"].as_str().unwrap_or_default().to_string(),
    ))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
