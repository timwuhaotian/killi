use super::*;
use std::io::IsTerminal;
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use crate::config::{Additions, Config};

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

/// `killi login chatgpt`: signs in with ChatGPT in the browser, keeps the credentials, and
/// adds the plan's models to the config.
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
        .map_err(|_| anyhow!("the browser didn't come back within 5 minutes; run `killi login chatgpt` again"))??;
    drop(listener);
    saved.client_id = issued_client(&saved.client_id, callback.client_id.as_deref())?;
    saved.save(&path)?;

    let http = super::super::http_client()?;
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
        bail!("OpenAI turned the sign-in away (HTTP {status} {}); run `killi login chatgpt` again", oauth_error(&body));
    }
    let id_token = body["id_token"].as_str().ok_or_else(|| anyhow!("the token response has no id_token"))?;
    let (subject, email) = identity(id_token, &saved.client_id, &attempt.nonce, now())?;
    if !saved.subject.is_empty() && saved.subject != subject {
        bail!(
            "that is a different ChatGPT account than killi's registration ({}); to switch accounts, delete {} and run `killi login chatgpt` again",
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
        bail!("signed in as {}, but ChatGPT plan use wasn't allowed for killi; run `killi login chatgpt` again to allow it", saved.email);
    }
    saved.save(&path)?;
    eprintln!("Signed in to ChatGPT as {}.", saved.email);
    eprintln!("You're using your ChatGPT plan: requests in killi count toward its usage. Manage usage: {USAGE_URL}");
    match plan_models(&http, &saved.access_token).await {
        Ok(models) if !models.is_empty() => println!("{}", crate::config::add_models(&Additions {
            name: "chatgpt",
            ours: |provider| provider.kind == "chatgpt",
            provider: toml::Table::from_iter([("type".to_string(), toml::Value::from("chatgpt"))]),
            models,
            comment: "Your ChatGPT plan, signed in with `killi login chatgpt`.",
        })?.report()),
        Ok(_) => eprintln!("killi: your plan listed no models; add them to ~/.killi/config.toml under a `type = \"chatgpt\"` provider"),
        Err(error) => eprintln!("killi: could not list your plan's models: {error:#}"),
    }
    Ok(())
}

/// `killi logout chatgpt`: ends the renewable session with OpenAI and clears the tokens. The
/// registration and host ID stay, so the next sign-in reuses the same client.
pub async fn logout() -> Result<()> {
    let path = credentials_path();
    let mut credentials = Credentials::load(&path)?.unwrap_or_default();
    if credentials.access_token.is_empty() && credentials.refresh_token.is_empty() {
        println!("killi isn't signed in to ChatGPT.");
        return Ok(());
    }
    let revoked = if credentials.refresh_token.is_empty() { Ok(()) } else { revoke(&super::super::http_client()?, &credentials).await };
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

/// Opens `url` in the system browser; the printed link is there when it can't.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}
