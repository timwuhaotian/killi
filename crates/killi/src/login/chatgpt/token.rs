use super::*;
use reqwest::StatusCode;

/// Whether `killi login chatgpt` has left credentials that may use the ChatGPT plan.
pub fn signed_in() -> bool {
    Credentials::load(&credentials_path())
        .ok()
        .flatten()
        .is_some_and(|credentials| credentials.signed_in())
}

/// The access token to send: the saved one while it has time left, else a refreshed one.
/// `rejected` is a token the API just turned away, refreshed even though it hasn't expired.
pub async fn access_token(
    http: &reqwest::Client,
    rejected: Option<&str>,
) -> Result<String, TokenError> {
    access_token_at(http, &credentials_path(), TOKEN_URL, rejected).await
}

pub(super) async fn access_token_at(
    http: &reqwest::Client,
    path: &Path,
    token_url: &str,
    rejected: Option<&str>,
) -> Result<String, TokenError> {
    let usable = |credentials: &Credentials| {
        credentials.fresh(now()) && rejected != Some(credentials.access_token.as_str())
    };
    let credentials = signed_in_at(path)?;
    if usable(&credentials) {
        return Ok(credentials.access_token);
    }
    // Refresh tokens rotate: one killi refreshes at a time, and the others take its result.
    let _lock = Lock::acquire(path)
        .await
        .map_err(|error| TokenError::Transient(error.to_string()))?;
    let mut credentials = signed_in_at(path)?;
    if usable(&credentials) {
        return Ok(credentials.access_token);
    }
    refresh(http, token_url, path, &mut credentials).await?;
    credentials
        .save(path)
        .map_err(|error| TokenError::Transient(format!("{error:#}")))?;
    Ok(credentials.access_token)
}

fn signed_in_at(path: &Path) -> Result<Credentials, TokenError> {
    let credentials = Credentials::load(path)
        .map_err(|error| TokenError::SignIn(format!("{error:#}")))?
        .unwrap_or_default();
    if credentials.signed_in() {
        Ok(credentials)
    } else {
        Err(TokenError::SignIn(NOT_SIGNED_IN.into()))
    }
}

/// Trades the refresh token for new tokens. A refresh token the server calls unusable is
/// cleared from the file, so killi asks for a new sign-in instead of trying it again.
async fn refresh(
    http: &reqwest::Client,
    token_url: &str,
    path: &Path,
    credentials: &mut Credentials,
) -> Result<(), TokenError> {
    if credentials.refresh_token.is_empty() {
        return Err(TokenError::SignIn(
            "the ChatGPT sign-in has expired: run `killi login chatgpt`".into(),
        ));
    }
    let transient = |reason: String| {
        TokenError::Transient(format!("could not refresh the ChatGPT sign-in: {reason}"))
    };
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", credentials.client_id.as_str()),
        ("refresh_token", credentials.refresh_token.as_str()),
        ("resource", RESOURCE),
    ];
    let response = http
        .post(token_url)
        .form(&form)
        .send()
        .await
        .map_err(|error| transient(error.to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| transient(error.to_string()))?;
    let body: Value = serde_json::from_slice(&body).unwrap_or_default();
    if status.is_success() {
        return credentials
            .take_tokens(&body, now())
            .map_err(|error| transient(error.to_string()));
    }
    let code = oauth_error(&body);
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(transient(format!("HTTP {status} {code}")));
    }
    if UNUSABLE.contains(&code) {
        credentials.sign_out();
        let _ = credentials.save(path);
    }
    Err(TokenError::SignIn(format!(
        "the ChatGPT sign-in has ended (HTTP {status} {code}): run `killi login chatgpt`"
    )))
}

/// What to add to the server's message when a request on the plan fails.
pub fn advice(message: &str) -> String {
    if message.contains("usage_limit_exceeded") || message.starts_with("HTTP 429") {
        format!(" Manage usage: {USAGE_URL}")
    } else if message.starts_with("HTTP 401") {
        " Run `killi login chatgpt` to sign in again.".to_string()
    } else {
        String::new()
    }
}
