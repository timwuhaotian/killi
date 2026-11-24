//! Sign in with Grok: xAI lets SuperGrok and X Premium subscribers use their plan in the
//! third-party coding agents it names (x.ai/news/grok-opencode). killi isn't one of them; like
//! them, `killi login grok` runs OAuth's device flow (RFC 8628) with the Grok CLI's public
//! client, so it works over SSH too, and xAI may turn a plan away (HTTP 403) that it doesn't
//! open to such use. Sessions send Responses requests to xAI's API with the saved token and
//! refresh it before it expires.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Lock, TokenError};
use crate::config::{Additions, killi_home};

const DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const REVOKE_URL: &str = "https://auth.x.ai/oauth2/revoke";
/// The Grok CLI's public client, which xAI's sign-in accepts from the agents it supports.
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Where subscribers see their plan and its limits.
const PLAN_URL: &str = "https://grok.com/settings";
/// An access token is refreshed once it has less than this many seconds left.
const REFRESH_EARLY: u64 = 120;
/// How long a token is taken to last when neither the response nor the token says.
const ASSUMED_LIFETIME: u64 = 15 * 60;
const NOT_SIGNED_IN: &str = "killi isn't signed in to Grok: run `killi login grok`";

/// What `killi login grok` keeps in `~/.killi/grok.json` (owner-only).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Credentials {
    access_token: String,
    refresh_token: String,
    /// Unix seconds when the access token expires.
    expires_at: u64,
}

impl Credentials {
    fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("invalid {}", path.display())),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        crate::fs_atomic::write(path, &serde_json::to_vec_pretty(self)?, Some(0o600))
    }

    fn signed_in(&self) -> bool {
        !self.access_token.is_empty() && !self.refresh_token.is_empty()
    }

    /// Takes the tokens from a token-endpoint response received at `now`. Refresh tokens
    /// rotate, but one a response leaves out stays as it was.
    fn take_tokens(&mut self, response: &Value, now: u64) -> Result<()> {
        let access = response["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow!("the token response has no access_token"))?;
        self.access_token = access.to_string();
        if let Some(refresh) = response["refresh_token"].as_str().filter(|t| !t.is_empty()) {
            self.refresh_token = refresh.to_string();
        }
        // xAI doesn't always send `expires_in`, nor always truly: the earlier of it and the
        // token's own `exp` wins.
        let said = response["expires_in"]
            .as_u64()
            .filter(|seconds| *seconds > 0);
        let claimed = jwt_expiry(access).map(|exp| exp.saturating_sub(now));
        let lifetime = match (said, claimed) {
            (Some(said), Some(claimed)) => said.min(claimed),
            (said, claimed) => said.or(claimed).unwrap_or(ASSUMED_LIFETIME),
        };
        self.expires_at = now.saturating_add(lifetime);
        Ok(())
    }

    fn fresh(&self, now: u64) -> bool {
        self.expires_at > now.saturating_add(REFRESH_EARLY)
    }

    fn sign_out(&mut self) {
        *self = Credentials::default();
    }
}

/// The `exp` claim of a JWT access token, when it is one.
fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims["exp"].as_u64()
}

fn credentials_path() -> PathBuf {
    killi_home().join("grok.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Whether `killi login grok` has left credentials to use.
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

async fn access_token_at(
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
    // A refresh token works once: one killi refreshes at a time, and the others take its result.
    let _lock = Lock::acquire(path, "Grok")
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

/// Trades the refresh token for new tokens. A refresh token xAI turns away is cleared from
/// the file, so killi asks for a new sign-in instead of trying it again; a 403 is the plan
/// not allowing API use, which signing in again doesn't change, so the tokens stay.
async fn refresh(
    http: &reqwest::Client,
    token_url: &str,
    path: &Path,
    credentials: &mut Credentials,
) -> Result<(), TokenError> {
    let transient = |reason: String| {
        TokenError::Transient(format!("could not refresh the Grok sign-in: {reason}"))
    };
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", credentials.refresh_token.as_str()),
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
    let code = body["error"].as_str().unwrap_or_default();
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(transient(format!("HTTP {status} {code}")));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(TokenError::SignIn(format!(
            "xAI turned the Grok plan away (HTTP {status} {code}): the subscription may not include use outside Grok's apps; check it at {PLAN_URL}, or add an API key with /provider grok"
        )));
    }
    // Only a refresh token xAI calls dead is cleared; another refusal may pass.
    if status == StatusCode::UNAUTHORIZED || matches!(code, "invalid_grant" | "invalid_token") {
        credentials.sign_out();
        let _ = credentials.save(path);
        return Err(TokenError::SignIn(format!(
            "the Grok sign-in has ended (HTTP {status} {code}): run `killi login grok`"
        )));
    }
    Err(TokenError::SignIn(format!(
        "xAI turned the Grok token refresh away (HTTP {status} {code}); if it persists, run `killi login grok`"
    )))
}

/// What to add to the server's message when a request on the plan fails.
pub fn advice(message: &str) -> String {
    if message.starts_with("HTTP 429") {
        format!(" Your plan's limits: {PLAN_URL}")
    } else if message.starts_with("HTTP 401") {
        " Run `killi login grok` to sign in again.".to_string()
    } else if message.starts_with("HTTP 403") {
        format!(
            " xAI may not allow API use on this subscription; check it at {PLAN_URL}, or add an API key with /provider grok."
        )
    } else {
        String::new()
    }
}

/// The device flow's endpoints, so tests can stand in for xAI's.
struct Endpoints<'a> {
    device: &'a str,
    token: &'a str,
}

/// What the device endpoint hands out: the code to poll with and what the user confirms.
#[derive(Debug, Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

/// `killi login grok`: confirms a code in the browser, keeps the credentials and adds the
/// Grok models to the config.
pub async fn login() -> Result<()> {
    let http = super::http_client()?;
    let endpoints = Endpoints {
        device: DEVICE_URL,
        token: TOKEN_URL,
    };
    let device = request_device(&http, &endpoints).await?;
    let url = device
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| device.verification_uri.clone());
    eprintln!(
        "Sign in to Grok in your browser and confirm the code {}. If it didn't open, go to:\n\n  {url}\n",
        device.user_code
    );
    super::open_browser(&url);
    let tokens = poll(&http, &endpoints, &device, Duration::from_secs(1)).await?;
    let path = credentials_path();
    let access = {
        let _lock = Lock::acquire(&path, "Grok").await?;
        let mut credentials = Credentials::default();
        credentials.take_tokens(&tokens, now())?;
        if credentials.refresh_token.is_empty() {
            bail!("xAI's sign-in came back without a refresh token; run `killi login grok` again");
        }
        credentials.save(&path)?;
        credentials.access_token
    };
    eprintln!("Signed in to Grok. Requests in killi count toward your plan's limits: {PLAN_URL}");
    let models = crate::config::grok_models();
    // xAI's list leaves out some models the plan takes, so it only says what else there is.
    if let Ok(listed) =
        crate::config::custom_served("openai", crate::config::GROK_BASE_URL, &access).await
    {
        let more: Vec<&str> = listed
            .iter()
            .map(String::as_str)
            .filter(|id| id.starts_with("grok-") && !models.iter().any(|model| model.id == *id))
            .collect();
        if !more.is_empty() {
            eprintln!(
                "Your plan also lists {}; add one under the `grok` provider in ~/.killi/config.toml to use it.",
                more.join(", ")
            );
        }
    }
    println!(
        "{}",
        crate::config::add_models(&Additions {
            name: "grok".into(),
            ours: |provider| provider.kind == "grok",
            provider: toml::Table::from_iter([("type".to_string(), toml::Value::from("grok"))]),
            models,
            comment: "Your Grok subscription, signed in with `killi login grok`.",
        })?
        .report()
    );
    Ok(())
}

async fn request_device(http: &reqwest::Client, endpoints: &Endpoints<'_>) -> Result<Device> {
    let response = http
        .post(endpoints.device)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPES)])
        .send()
        .await
        .context("could not reach xAI's sign-in")?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        bail!(
            "xAI's sign-in answered HTTP {status}: {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(300)
                .collect::<String>()
        );
    }
    serde_json::from_slice(&body).context("xAI's sign-in sent no device code")
}

/// Polls the token endpoint until the user confirms the code, says no, or the code expires.
/// `second` is how long one second of the server's interval lasts (shorter in tests).
async fn poll(
    http: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    device: &Device,
    second: Duration,
) -> Result<Value> {
    let mut interval = device.interval.filter(|n| *n > 0).unwrap_or(5);
    let lifetime = device.expires_in.filter(|n| *n > 0).unwrap_or(300);
    let deadline = tokio::time::Instant::now() + second * u32::try_from(lifetime).unwrap_or(300);
    let form = [
        ("grant_type", DEVICE_GRANT),
        ("client_id", CLIENT_ID),
        ("device_code", device.device_code.as_str()),
    ];
    loop {
        tokio::time::sleep(second * u32::try_from(interval).unwrap_or(5)).await;
        if tokio::time::Instant::now() >= deadline {
            bail!("the code expired before it was confirmed; run `killi login grok` again");
        }
        let response = http
            .post(endpoints.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await
            .context("could not reach xAI's sign-in")?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&response.bytes().await?).unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        match body["error"].as_str().unwrap_or_default() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" | "authorization_denied" => bail!("the sign-in was declined"),
            "expired_token" => {
                bail!("the code expired before it was confirmed; run `killi login grok` again")
            }
            code if status.is_server_error() => {
                bail!("xAI's sign-in answered HTTP {status} {code}; run `killi login grok` again")
            }
            code => bail!("xAI turned the sign-in away (HTTP {status} {code})"),
        }
    }
}

/// `killi logout grok`: revokes the refresh token with xAI and clears the saved tokens.
pub async fn logout() -> Result<()> {
    let path = credentials_path();
    let mut credentials = Credentials::load(&path)?.unwrap_or_default();
    if !credentials.signed_in() {
        println!("killi isn't signed in to Grok.");
        return Ok(());
    }
    let revoked = async {
        super::http_client()?
            .post(REVOKE_URL)
            .form(&[
                ("token", credentials.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await?
            .error_for_status()?;
        anyhow::Ok(())
    }
    .await;
    let _lock = Lock::acquire(&path, "Grok").await?;
    credentials.sign_out();
    credentials.save(&path)?;
    match revoked {
        Ok(()) => println!("Signed out of Grok."),
        Err(error) => println!(
            "Signed out here, but xAI didn't confirm the session ended ({error:#}). You can remove killi's access in your xAI account."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
