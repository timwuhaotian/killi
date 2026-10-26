//! Sign in with ChatGPT: killi registers as an OAuth client of the user's ChatGPT account and
//! sends Responses requests on their plan, the flow OpenAI documents for open-source tools
//! (developers.openai.com/siwc/token-sharing-open-source). `killi login` runs the browser part
//! once; sessions read the saved credentials and refresh them before they expire.

use std::fs;
use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{Config, PlanModel, killi_home};

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
/// How long `killi login` waits for the browser to come back.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
const NOT_SIGNED_IN: &str = "killi isn't signed in to ChatGPT: run `killi login`";
/// Refresh errors that mean the refresh token can't be used again.
const UNUSABLE: [&str; 6] =
    ["invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalidated", "refresh_token_reused"];

/// What `killi login` keeps in `~/.killi/chatgpt.json` (owner-only): the client registration,
/// this machine's host ID, the account, and the current tokens. Signing out clears the tokens
/// and scopes and keeps the rest, so the next sign-in reuses the registration.
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

/// Whether `killi login` has left credentials that may use the ChatGPT plan.
pub fn signed_in() -> bool {
    Credentials::load(&credentials_path()).ok().flatten().is_some_and(|credentials| credentials.signed_in())
}

/// The access token to send: the saved one while it has time left, else a refreshed one.
/// `rejected` is a token the API just turned away, refreshed even though it hasn't expired.
pub async fn access_token(http: &reqwest::Client, rejected: Option<&str>) -> Result<String, TokenError> {
    access_token_at(http, &credentials_path(), TOKEN_URL, rejected).await
}

async fn access_token_at(http: &reqwest::Client, path: &Path, token_url: &str, rejected: Option<&str>) -> Result<String, TokenError> {
    let usable = |credentials: &Credentials| credentials.fresh(now()) && rejected != Some(credentials.access_token.as_str());
    let credentials = signed_in_at(path)?;
    if usable(&credentials) {
        return Ok(credentials.access_token);
    }
    // Refresh tokens rotate: one killi refreshes at a time, and the others take its result.
    let _lock = Lock::acquire(path).await.map_err(|error| TokenError::Transient(error.to_string()))?;
    let mut credentials = signed_in_at(path)?;
    if usable(&credentials) {
        return Ok(credentials.access_token);
    }
    refresh(http, token_url, path, &mut credentials).await?;
    credentials.save(path).map_err(|error| TokenError::Transient(format!("{error:#}")))?;
    Ok(credentials.access_token)
}

fn signed_in_at(path: &Path) -> Result<Credentials, TokenError> {
    let credentials = Credentials::load(path).map_err(|error| TokenError::SignIn(format!("{error:#}")))?.unwrap_or_default();
    if credentials.signed_in() { Ok(credentials) } else { Err(TokenError::SignIn(NOT_SIGNED_IN.into())) }
}

/// Trades the refresh token for new tokens. A refresh token the server calls unusable is
/// cleared from the file, so killi asks for a new sign-in instead of trying it again.
async fn refresh(http: &reqwest::Client, token_url: &str, path: &Path, credentials: &mut Credentials) -> Result<(), TokenError> {
    if credentials.refresh_token.is_empty() {
        return Err(TokenError::SignIn("the ChatGPT sign-in has expired: run `killi login`".into()));
    }
    let transient = |reason: String| TokenError::Transient(format!("could not refresh the ChatGPT sign-in: {reason}"));
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", credentials.client_id.as_str()),
        ("refresh_token", credentials.refresh_token.as_str()),
        ("resource", RESOURCE),
    ];
    let response = http.post(token_url).form(&form).send().await.map_err(|error| transient(error.to_string()))?;
    let status = response.status();
    let body = response.bytes().await.map_err(|error| transient(error.to_string()))?;
    let body: Value = serde_json::from_slice(&body).unwrap_or_default();
    if status.is_success() {
        return credentials.take_tokens(&body, now()).map_err(|error| transient(error.to_string()));
    }
    let code = oauth_error(&body);
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(transient(format!("HTTP {status} {code}")));
    }
    if UNUSABLE.contains(&code) {
        credentials.sign_out();
        let _ = credentials.save(path);
    }
    Err(TokenError::SignIn(format!("the ChatGPT sign-in has ended (HTTP {status} {code}): run `killi login`")))
}

/// The machine-readable code of an OAuth error body: `{"error": "code"}` or
/// `{"error": {"code": "code"}}`.
fn oauth_error(body: &Value) -> &str {
    body["error"].as_str().or_else(|| body["error"]["code"].as_str()).unwrap_or_default()
}

/// What to add to the server's message when a request on the plan fails.
pub fn advice(message: &str) -> String {
    if message.contains("usage_limit_exceeded") || message.starts_with("HTTP 429") {
        format!(" Manage usage: {USAGE_URL}")
    } else if message.starts_with("HTTP 401") {
        " Run `killi login` to sign in again.".to_string()
    } else {
        String::new()
    }
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

/// Before an interactive start on a ChatGPT model, offers to sign in when killi isn't.
pub async fn offer(model: Option<&str>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() || signed_in() {
        return Ok(());
    }
    let Ok(config) = Config::load() else { return Ok(()) };
    let Some(alias) = config.chatgpt_model(model) else { return Ok(()) };
    if crate::config::confirm(&format!("`{alias}` runs on your ChatGPT plan and killi isn't signed in. Continue with ChatGPT in the browser?"))? {
        login().await?;
    }
    Ok(())
}

/// `killi login`: signs in with ChatGPT in the browser, keeps the credentials, and adds the
/// plan's models to the config.
pub async fn login() -> Result<()> {
    let path = credentials_path();
    let mut saved = Credentials::load(&path)?.unwrap_or_default();
    if saved.ext_agent_host_id.is_empty() {
        saved.ext_agent_host_id = format!("urn:uuid:{}", uuid_v4()?);
        saved.save(&path)?;
    }
    let listener = match TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(listener) => listener,
        Err(_) => TcpListener::bind(("127.0.0.1", 0)).await.context("could not listen for the browser's answer")?,
    };
    let redirect_uri = format!("http://127.0.0.1:{}{CALLBACK_PATH}", listener.local_addr()?.port());
    let attempt = Attempt::new()?;
    let url = authorize_url(&saved, &attempt, &redirect_uri)?;
    eprintln!("Continue with ChatGPT in your browser. If it didn't open, go to:\n\n  {url}\n");
    open_browser(url.as_str());
    let callback = tokio::time::timeout(SIGN_IN_WAIT, wait_for_callback(&listener, &attempt.state))
        .await
        .map_err(|_| anyhow!("the browser didn't come back within 5 minutes; run `killi login` again"))??;
    drop(listener);
    saved.client_id = issued_client(&saved.client_id, callback.client_id.as_deref())?;
    saved.save(&path)?;

    let http = http_client()?;
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", saved.client_id.as_str()),
        ("code", callback.code.as_str()),
        ("code_verifier", attempt.verifier.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("resource", RESOURCE),
    ];
    let response = http.post(TOKEN_URL).form(&form).send().await.context("could not reach OpenAI's token endpoint")?;
    let status = response.status();
    let body: Value = serde_json::from_slice(&response.bytes().await?).unwrap_or_default();
    if !status.is_success() {
        bail!("OpenAI turned the sign-in away (HTTP {status} {}); run `killi login` again", oauth_error(&body));
    }
    let id_token = body["id_token"].as_str().ok_or_else(|| anyhow!("the token response has no id_token"))?;
    let (subject, email) = identity(id_token, &saved.client_id, &attempt.nonce, now())?;
    if !saved.subject.is_empty() && saved.subject != subject {
        bail!(
            "that is a different ChatGPT account than killi's registration ({}); to switch accounts, delete {} and run `killi login` again",
            saved.email,
            path.display()
        );
    }
    let _lock = Lock::acquire(&path).await?;
    saved.subject = subject;
    saved.email = email;
    saved.scopes = callback.scope.as_deref().unwrap_or_default().split_whitespace().map(String::from).collect();
    saved.take_tokens(&body, now())?;
    if !saved.plan() {
        saved.access_token.clear();
        saved.refresh_token.clear();
        saved.save(&path)?;
        bail!("signed in as {}, but ChatGPT plan use wasn't allowed for killi; run `killi login` again to allow it", saved.email);
    }
    saved.save(&path)?;
    eprintln!("Signed in to ChatGPT as {}.", saved.email);
    eprintln!("You're using your ChatGPT plan: requests in killi count toward its usage. Manage usage: {USAGE_URL}");
    match plan_models(&http, &saved.access_token).await {
        Ok(models) if !models.is_empty() => crate::config::add_plan_models(&models)?,
        Ok(_) => eprintln!("killi: your plan listed no models; add them to ~/.killi/config.toml under a `type = \"chatgpt\"` provider"),
        Err(error) => eprintln!("killi: could not list your plan's models: {error:#}"),
    }
    Ok(())
}

/// `killi logout`: ends the renewable session with OpenAI and clears the tokens. The
/// registration and host ID stay, so the next `killi login` signs in to the same client.
pub async fn logout() -> Result<()> {
    let path = credentials_path();
    let mut credentials = Credentials::load(&path)?.unwrap_or_default();
    if credentials.access_token.is_empty() && credentials.refresh_token.is_empty() {
        println!("killi isn't signed in to ChatGPT.");
        return Ok(());
    }
    let revoked = if credentials.refresh_token.is_empty() { Ok(()) } else { revoke(&http_client()?, &credentials).await };
    let _lock = Lock::acquire(&path).await?;
    credentials.sign_out();
    credentials.save(&path)?;
    match revoked {
        Ok(()) => println!("Signed out of ChatGPT."),
        Err(error) => println!("Signed out here, but OpenAI didn't confirm the session ended ({error:#}). You can disconnect killi in ChatGPT's settings."),
    }
    Ok(())
}

/// Revokes the refresh token at the endpoint OpenAI's OpenID configuration names.
async fn revoke(http: &reqwest::Client, credentials: &Credentials) -> Result<()> {
    let discovery = http.get(format!("{ISSUER}/.well-known/openid-configuration")).send().await?.error_for_status()?;
    let discovery: Value = serde_json::from_slice(&discovery.bytes().await?)?;
    let endpoint = discovery["revocation_endpoint"].as_str().ok_or_else(|| anyhow!("OpenAI names no revocation endpoint"))?;
    let form = [
        ("token", credentials.refresh_token.as_str()),
        ("token_type_hint", "refresh_token"),
        ("client_id", credentials.client_id.as_str()),
    ];
    http.post(endpoint).form(&form).send().await?.error_for_status()?;
    Ok(())
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).timeout(Duration::from_secs(60)).build()?)
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

fn authorize_url(saved: &Credentials, attempt: &Attempt, redirect_uri: &str) -> Result<reqwest::Url> {
    let challenge = challenge(&attempt.verifier);
    let registered = !saved.client_id.is_empty();
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", if registered { &saved.client_id } else { REGISTER }),
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

/// What the browser brings back from a sign-in that went through.
#[derive(Debug, PartialEq)]
struct Callback {
    code: String,
    client_id: Option<String>,
    scope: Option<String>,
}

/// Answers the browser until a request carries this attempt's callback.
async fn wait_for_callback(listener: &TcpListener, state: &str) -> Result<Callback> {
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
            Ok(Some(callback)) => ("200 OK", "Signed in to killi. You can close this tab.".to_string(), Some(Ok(callback))),
            Err(message) => ("200 OK", format!("Sign-in didn't finish: {}. You can close this tab.", escape(&message)), Some(Err(anyhow!(message)))),
        };
        let body = format!("<!doctype html><meta charset=utf-8><title>killi</title><p>{page}</p>");
        let head = format!("HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
        let _ = stream.write_all(format!("{head}{body}").as_bytes()).await;
        let _ = stream.shutdown().await;
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
}

/// Reads a callback request's `target` (path and query). `Ok(None)` is a request to ignore —
/// another path, or a state from an older attempt — and an `Err` ends the attempt.
fn parse_callback(target: &str, state: &str) -> Result<Option<Callback>, String> {
    let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else { return Ok(None) };
    if url.path() != CALLBACK_PATH {
        return Ok(None);
    }
    let param = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
    if param("state").as_deref() != Some(state) {
        return Ok(None);
    }
    if let Some(error) = param("error") {
        return Err(match error.as_str() {
            "access_denied" => "ChatGPT plan use wasn't allowed for killi".to_string(),
            _ => format!("{error} {}", param("error_description").unwrap_or_default()).trim().to_string(),
        });
    }
    let code = param("code").filter(|code| !code.is_empty()).ok_or("the browser came back without a code")?;
    Ok(Some(Callback { code, client_id: param("client_id"), scope: param("scope") }))
}

/// The client ID after a callback: the one this sign-in registered, or the saved one, which a
/// callback must not replace.
fn issued_client(saved: &str, returned: Option<&str>) -> Result<String> {
    match (saved.is_empty(), returned) {
        (true, Some(issued)) if !issued.is_empty() && issued != REGISTER => Ok(issued.to_string()),
        (true, _) => bail!("ChatGPT didn't return a client ID for killi, so its registration didn't finish; run `killi login` again"),
        (false, Some(other)) if other != saved => bail!("ChatGPT returned a client ID ({other}) other than killi's registration ({saved}); sign-in stopped"),
        (false, _) => Ok(saved.to_string()),
    }
}

/// The ID token's subject and email, after checking its issuer, audience, expiry and nonce.
/// Its signature isn't checked: the token came straight from the token endpoint over TLS,
/// which OpenID Connect accepts instead (Core 1.0, 3.1.3.7), and killi uses it only to name
/// the account.
fn identity(id_token: &str, client_id: &str, nonce: &str, now: u64) -> Result<(String, String)> {
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
    let subject = claims["sub"].as_str().filter(|subject| !subject.is_empty()).ok_or_else(|| anyhow!("the ID token names no account"))?;
    Ok((subject.to_string(), claims["email"].as_str().unwrap_or_default().to_string()))
}

/// The models the plan offers, in the server's order.
async fn plan_models(http: &reqwest::Client, token: &str) -> Result<Vec<PlanModel>> {
    let response = http.get(format!("{RESOURCE}/models")).header("authorization", format!("Bearer {token}")).send().await?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", crate::llm::truncate(&String::from_utf8_lossy(&body), 300));
    }
    Ok(models_from(&serde_json::from_slice(&body)?))
}

/// Reads the plan's catalog: `models` entries with `slug` and `visibility`, and when given,
/// `context_window`, `supported_reasoning_levels`, `default_reasoning_level` and
/// `input_modalities`. A plain OpenAI list (`data` with `id`s) is read too.
fn models_from(catalog: &Value) -> Vec<PlanModel> {
    let listed = catalog["models"].as_array().into_iter().flatten().filter(|model| model["visibility"] == "list");
    let mut models: Vec<PlanModel> = listed
        .filter_map(|model| {
            Some(PlanModel {
                slug: model["slug"].as_str().filter(|slug| !slug.is_empty())?.to_string(),
                context: model["context_window"].as_u64(),
                efforts: model["supported_reasoning_levels"].as_array().into_iter().flatten().filter_map(|level| level["effort"].as_str().map(String::from)).collect(),
                default_effort: model["default_reasoning_level"].as_str().map(String::from),
                images: model["input_modalities"].as_array().is_some_and(|inputs| inputs.iter().any(|input| input == "image")),
            })
        })
        .collect();
    if models.is_empty() {
        let ids = catalog["data"].as_array().into_iter().flatten().filter_map(|model| model["id"].as_str());
        models = ids.map(|id| PlanModel { slug: id.to_string(), ..PlanModel::default() }).collect();
    }
    models
}

/// Opens `url` in the system browser; the printed link is there when it can't.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn jwt(claims: Value) -> String {
        format!("{}.{}.sig", URL_SAFE_NO_PAD.encode(b"{\"alg\":\"RS256\"}"), URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("killi-login-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("chatgpt.json")
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
        let url = format!("http://127.0.0.1:{}/token", listener.local_addr().unwrap().port());
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
                        let length = text.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            seen.lock().unwrap().push(text[end + 4..].to_string());
                            break;
                        }
                    }
                }
                let response = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len());
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, bodies)
    }

    #[test]
    fn the_pkce_challenge_matches_rfc_7636() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(random_token().unwrap().len(), 43);
        let uuid = uuid_v4().unwrap();
        assert_eq!((uuid.len(), &uuid[14..15]), (36, "4"));
        assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"), "{uuid}");
    }

    #[test]
    fn only_this_attempts_callback_ends_the_wait() {
        let done = parse_callback("/auth/callback?code=c1&state=s1&client_id=oaiapp_9&scope=openid+chatgpt.tokens.use.direct", "s1");
        assert_eq!(done, Ok(Some(Callback { code: "c1".into(), client_id: Some("oaiapp_9".into()), scope: Some("openid chatgpt.tokens.use.direct".into()) })));
        assert_eq!(parse_callback("/auth/callback?code=c1&state=old", "s1"), Ok(None), "a stale tab is ignored");
        assert_eq!(parse_callback("/favicon.ico", "s1"), Ok(None));
        assert_eq!(parse_callback("/callback?code=c1&state=s1", "s1"), Ok(None), "the path must match exactly");
        assert!(parse_callback("/auth/callback?error=access_denied&state=s1", "s1").unwrap_err().contains("wasn't allowed"));
        assert!(parse_callback("/auth/callback?state=s1", "s1").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_callback_server_answers_the_browser_until_the_callback_comes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            let mut pages = Vec::new();
            for target in ["/favicon.ico", "/auth/callback?code=c&state=old", "/auth/callback?code=c&state=s&client_id=oaiapp_1"] {
                let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                stream.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).await.unwrap();
                let mut page = String::new();
                stream.read_to_string(&mut page).await.unwrap();
                pages.push(page);
            }
            pages
        });
        let callback = wait_for_callback(&listener, "s").await.unwrap();
        assert_eq!((callback.code.as_str(), callback.client_id.as_deref()), ("c", Some("oaiapp_1")));
        let pages = browser.await.unwrap();
        assert!(pages[0].starts_with("HTTP/1.1 404") && pages[1].starts_with("HTTP/1.1 404"), "{pages:?}");
        assert!(pages[2].starts_with("HTTP/1.1 200") && pages[2].contains("Signed in to killi"), "{}", pages[2]);
    }

    #[test]
    fn a_registration_keeps_its_client_id() {
        assert_eq!(issued_client("", Some("oaiapp_1")).unwrap(), "oaiapp_1");
        assert!(issued_client("", None).is_err(), "a new registration must return its client");
        assert!(issued_client("", Some(REGISTER)).is_err());
        assert_eq!(issued_client("oaiapp_1", None).unwrap(), "oaiapp_1");
        assert_eq!(issued_client("oaiapp_1", Some("oaiapp_1")).unwrap(), "oaiapp_1");
        assert!(issued_client("oaiapp_1", Some("oaiapp_2")).is_err());
    }

    #[test]
    fn the_id_token_must_be_for_this_sign_in() {
        let claims = |aud: Value, nonce: &str, exp: u64| jwt(serde_json::json!({"iss": ISSUER, "aud": aud, "nonce": nonce, "exp": exp, "sub": "user-1", "email": "a@b.c"}));
        let identified = identity(&claims("oaiapp_1".into(), "n", 2_000), "oaiapp_1", "n", 1_000).unwrap();
        assert_eq!(identified, ("user-1".to_string(), "a@b.c".to_string()));
        assert!(identity(&claims(serde_json::json!(["x", "oaiapp_1"]), "n", 2_000), "oaiapp_1", "n", 1_000).is_ok());
        assert!(identity(&claims("oaiapp_2".into(), "n", 2_000), "oaiapp_1", "n", 1_000).is_err());
        assert!(identity(&claims("oaiapp_1".into(), "other", 2_000), "oaiapp_1", "n", 1_000).is_err());
        assert!(identity(&claims("oaiapp_1".into(), "n", 1_000), "oaiapp_1", "n", 1_000).is_err());
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
        credentials.take_tokens(&serde_json::json!({"access_token": "new", "expires_in": 60}), 5).unwrap();
        assert_eq!((credentials.access_token.as_str(), credentials.refresh_token.as_str()), ("new", "old-refresh"));
        assert_eq!((credentials.expires_in, credentials.saved_at), (60, 5));
        assert!(credentials.plan(), "the scopes stay when the response leaves them out");
        assert!(credentials.take_tokens(&serde_json::json!({}), 5).is_err());
    }

    #[test]
    fn the_first_sign_in_registers_and_later_ones_reuse_the_client() {
        let attempt = Attempt { state: "s".into(), nonce: "n".into(), verifier: "v".into() };
        let params = |saved: &Credentials| -> Vec<(String, String)> {
            authorize_url(saved, &attempt, "http://127.0.0.1:1455/auth/callback").unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
        };
        let get = |params: &[(String, String)], key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let fresh = Credentials { ext_agent_host_id: "urn:uuid:h".into(), ..Credentials::default() };
        let first = params(&fresh);
        assert_eq!(get(&first, "client_id").as_deref(), Some(REGISTER));
        assert_eq!(get(&first, "agent_name_hint").as_deref(), Some("killi"));
        assert_eq!(get(&first, "ext_agent_host_id").as_deref(), Some("urn:uuid:h"));
        assert_eq!(get(&first, "resource").as_deref(), Some(RESOURCE));
        assert_eq!(get(&first, "code_challenge").as_deref(), Some(challenge("v").as_str()));
        assert_eq!(get(&first, "scope").as_deref(), Some(SCOPES));

        let registered = Credentials { client_id: "oaiapp_1".into(), email: "a@b.c".into(), ..fresh.clone() };
        let again = params(&registered);
        assert_eq!(get(&again, "client_id").as_deref(), Some("oaiapp_1"));
        assert_eq!(get(&again, "agent_name_hint"), None);
        assert_eq!(get(&again, "login_hint").as_deref(), Some("a@b.c"));
        assert_eq!(get(&again, "prompt"), None, "an ordinary sign-in doesn't force consent");

        let declined = Credentials { scopes: vec!["openid".into()], ..registered };
        assert_eq!(get(&params(&declined), "prompt").as_deref(), Some("consent"));
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
        assert_eq!(models.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(), ["gpt-a", "gpt-b"]);
        assert_eq!((models[0].context, models[0].images, models[0].default_effort.as_deref()), (Some(272000), true, Some("medium")));
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert!(!models[1].images && models[1].efforts.is_empty());
        assert_eq!(models_from(&serde_json::json!({"data": [{"id": "gpt-c"}]}))[0].slug, "gpt-c");
    }

    #[test]
    fn advice_points_at_usage_or_sign_in() {
        assert!(advice("HTTP 429 Too Many Requests: {\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}").contains(USAGE_URL));
        assert!(advice("HTTP 401 Unauthorized: {}").contains("killi login"));
        assert_eq!(advice("HTTP 400 Bad Request: {}"), "");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_fresh_token_is_sent_without_asking_the_server() {
        let path = scratch("fresh");
        signed_in_at_time(now()).save(&path).unwrap();
        let http = reqwest::Client::new();
        let token = access_token_at(&http, &path, "http://127.0.0.1:9/unreachable", None).await.unwrap();
        assert_eq!(token, "old-access");
        let none = scratch("none");
        assert!(matches!(access_token_at(&http, &none, "http://127.0.0.1:9/", None).await, Err(TokenError::SignIn(_))));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        fs::remove_dir_all(none.parent().unwrap()).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_expiring_or_rejected_token_is_refreshed_and_saved() {
        let path = scratch("refresh");
        signed_in_at_time(now() - 3500).save(&path).unwrap();
        let (url, bodies) = token_server(vec![
            (200, r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#),
            (200, r#"{"access_token":"newer-access","refresh_token":"newer-refresh","expires_in":3600}"#),
        ])
        .await;
        let http = reqwest::Client::new();
        assert_eq!(access_token_at(&http, &path, &url, None).await.unwrap(), "new-access");
        let saved = Credentials::load(&path).unwrap().unwrap();
        assert_eq!((saved.refresh_token.as_str(), saved.client_id.as_str()), ("new-refresh", "oaiapp_1"));
        let form = bodies.lock().unwrap()[0].clone();
        for field in ["grant_type=refresh_token", "client_id=oaiapp_1", "refresh_token=old-refresh", "resource=https%3A%2F%2Fapi.openai.com%2Fv1"] {
            assert!(form.contains(field), "{form}");
        }
        assert_eq!(access_token_at(&http, &path, &url, None).await.unwrap(), "new-access", "a fresh token is reused");
        assert_eq!(access_token_at(&http, &path, &url, Some("new-access")).await.unwrap(), "newer-access");
        assert!(bodies.lock().unwrap()[1].contains("refresh_token=new-refresh"), "the rotated token is the one spent");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_waiting_on_one_refresh_take_its_token() {
        let path = scratch("race");
        signed_in_at_time(now() - 3500).save(&path).unwrap();
        let (url, bodies) = token_server(vec![(200, r#"{"access_token":"shared","refresh_token":"r2","expires_in":3600}"#)]).await;
        let http = reqwest::Client::new();
        let (first, second) = tokio::join!(access_token_at(&http, &path, &url, None), access_token_at(&http, &path, &url, None));
        assert_eq!((first.unwrap(), second.unwrap()), ("shared".to_string(), "shared".to_string()));
        assert_eq!(bodies.lock().unwrap().len(), 1, "the refresh token is spent once");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unusable_refresh_token_signs_out_and_a_server_error_does_not() {
        let path = scratch("unusable");
        signed_in_at_time(now() - 3500).save(&path).unwrap();
        let (url, _) = token_server(vec![(503, r#"{"error":"temporarily_unavailable"}"#), (400, r#"{"error":"refresh_token_reused"}"#)]).await;
        let http = reqwest::Client::new();
        assert!(matches!(access_token_at(&http, &path, &url, None).await, Err(TokenError::Transient(_))));
        assert!(Credentials::load(&path).unwrap().unwrap().signed_in(), "a server error keeps the sign-in");
        let Err(TokenError::SignIn(message)) = access_token_at(&http, &path, &url, None).await else { panic!("expected a sign-in error") };
        assert!(message.contains("killi login") && message.contains("refresh_token_reused"), "{message}");
        let saved = Credentials::load(&path).unwrap().unwrap();
        assert!(!saved.signed_in() && saved.refresh_token.is_empty());
        assert_eq!((saved.client_id.as_str(), saved.ext_agent_host_id.as_str()), ("oaiapp_1", "urn:uuid:x"), "the registration stays");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
