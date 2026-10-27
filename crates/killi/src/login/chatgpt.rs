//! Sign in with ChatGPT: killi registers as an OAuth client of the user's ChatGPT account and
//! sends Responses requests on their plan, the flow OpenAI documents for open-source tools
//! (developers.openai.com/siwc/token-sharing-open-source). `killi login chatgpt` runs the
//! browser part once; sessions read the saved credentials and refresh them before they expire.

mod models;
mod oauth;
mod signin;
mod token;

use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::killi_home;

use models::plan_models;
use oauth::{authorize_url, identity, issued_client, wait_for_callback};

pub use signin::{login, logout, offer};
pub use token::{access_token, advice, signed_in};

#[cfg(test)]
use models::models_from;
#[cfg(test)]
use oauth::parse_callback;
#[cfg(test)]
use token::access_token_at;

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// The API the tokens are issued for, where requests on the plan go.
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope that lets killi use the plan; a sign-in without it names the account and no more.
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The first-time registration entry point: never saved, never used for a token exchange.
const REGISTER: &str = "dynamic_agent_client";
const CALLBACK_PATH: &str = "/auth/callback";
/// The callback port tried first; any free port works after that.
const PORT: u16 = 1455;
/// Where users see and limit what killi uses of their plan.
const USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// An access token is refreshed once it has less than this many seconds left.
const REFRESH_EARLY: u64 = 5 * 60;
/// How long `killi login chatgpt` waits for the browser to come back.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
const NOT_SIGNED_IN: &str = "killi isn't signed in to ChatGPT: run `killi login chatgpt`";
/// Refresh errors that mean the refresh token can't be used again.
const UNUSABLE: [&str; 6] =
    ["invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalidated", "refresh_token_reused"];

/// What `killi login chatgpt` keeps in `~/.killi/chatgpt.json` (owner-only): the client
/// registration, this machine's host ID, the account, and the current tokens. Signing out
/// clears the tokens and scopes and keeps the rest, so the next sign-in reuses the registration.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Credentials {
    email: String,
    subject: String,
    client_id: String,
    ext_agent_host_id: String,
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    scopes: Vec<String>,
    /// Unix seconds when the tokens arrived; with `expires_in`, when the access token expires.
    saved_at: u64,
}

impl Credentials {
    fn load(path: &Path) -> Result<Option<Self>> {
        match fs::read(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).with_context(|| format!("invalid {}", path.display())),
        }
    }

    /// Writes the file whole, owner-only, through a temporary file renamed into place.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let temporary = path.with_extension(format!("json.{}", std::process::id()));
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path).with_context(|| format!("could not write {}", path.display()))
    }

    fn plan(&self) -> bool {
        self.scopes.iter().any(|scope| scope == PLAN_SCOPE)
    }

    fn signed_in(&self) -> bool {
        !self.access_token.is_empty() && self.plan()
    }

    /// Whether the access token has more than `REFRESH_EARLY` seconds left at `now`.
    fn fresh(&self, now: u64) -> bool {
        self.saved_at.saturating_add(self.expires_in) > now.saturating_add(REFRESH_EARLY)
    }

    /// Takes the tokens from a token-endpoint response received at `now`. A refresh may leave
    /// out the refresh token or the scope, which then stay as they were.
    fn take_tokens(&mut self, response: &Value, now: u64) -> Result<()> {
        let access = response["access_token"].as_str().filter(|token| !token.is_empty());
        self.access_token = access.ok_or_else(|| anyhow!("the token response has no access_token"))?.to_string();
        if let Some(refresh) = response["refresh_token"].as_str().filter(|token| !token.is_empty()) {
            self.refresh_token = refresh.to_string();
        }
        if let Some(scope) = response["scope"].as_str() {
            self.scopes = scope.split_whitespace().map(String::from).collect();
        }
        self.expires_in = response["expires_in"].as_u64().unwrap_or(3600);
        self.saved_at = now;
        Ok(())
    }

    fn sign_out(&mut self) {
        self.access_token.clear();
        self.refresh_token.clear();
        self.scopes.clear();
        self.expires_in = 0;
        self.saved_at = 0;
    }
}

/// Why a request on the plan has no token: signing in again fixes `SignIn`, waiting may fix
/// `Transient`.
#[derive(Debug)]
pub enum TokenError {
    SignIn(String),
    Transient(String),
}

fn credentials_path() -> PathBuf {
    killi_home().join("chatgpt.json")
}

/// The machine-readable code of an OAuth error body: `{"error": "code"}` or
/// `{"error": {"code": "code"}}`.
fn oauth_error(body: &Value) -> &str {
    body["error"].as_str().or_else(|| body["error"]["code"].as_str()).unwrap_or_default()
}

/// An exclusive `flock` on a file beside the credentials, held while one killi changes them.
struct Lock {
    _file: fs::File,
}

impl Lock {
    async fn acquire(credentials: &Path) -> std::io::Result<Self> {
        let file = fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(credentials.with_extension("lock"))?;
        for _ in 0..600 {
            // SAFETY: the descriptor belongs to `file`, which stays open for the call.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { _file: file });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(std::io::Error::other("another killi kept the ChatGPT sign-in locked for 30 s"))
    }
}

/// The fresh secrets of one sign-in attempt.
struct Attempt {
    state: String,
    nonce: String,
    verifier: String,
}

impl Attempt {
    fn new() -> Result<Self> {
        Ok(Self { state: random_token()?, nonce: random_token()?, verifier: random_token()? })
    }
}

/// What the browser brings back from a sign-in that went through.
#[derive(Debug, PartialEq)]
struct Callback {
    code: String,
    client_id: Option<String>,
    scope: Option<String>,
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow!("the system's random source failed"))?;
    Ok(bytes)
}

/// 32 random bytes in base64url: 43 characters, as a PKCE verifier needs.
fn random_token() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?))
}

/// The PKCE S256 challenge for `verifier`.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()))
}

/// A random (version 4) UUID.
fn uuid_v4() -> Result<String> {
    let mut bytes = random_bytes::<16>()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests;
