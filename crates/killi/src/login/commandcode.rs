//! Command Code: the subscription behind the commandcode CLI. `killi login command-code`
//! signs in the way the CLI does: it opens the studio's CLI login page in the browser, the
//! page posts the fresh API key back to a loopback callback, and killi checks the key and
//! writes the provider with the subscription's models. A paste fallback covers a browser
//! that can't open or come back, and `/provider` uses it too.

use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SecureRandom;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{Additions, Config, NewModel, Provider, killi_home};

/// Where the CLI's gateway answers.
pub const BASE_URL: &str = "https://api.commandcode.ai";
/// Where the account behind the subscription lives.
const STUDIO: &str = "https://commandcode.ai";
const WHOAMI_PATH: &str = "/alpha/whoami";
/// The subscription's model catalog, readable without a key.
const CATALOG_PATH: &str = "/provider/v1/models?view=client";
const CALLBACK_PATH: &str = "/callback";
const COMPLETE_PATH: &str = "/callback/complete";
/// How long the login waits for the browser to come back.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// The largest callback body, mirroring the CLI's cap.
const MAX_CALLBACK_BODY: usize = 10_000;

/// The configured provider that is Command Code: the one on its gateway.
fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with(BASE_URL))
}

/// Whether the config has Command Code with a key.
pub(crate) fn signed_in() -> bool {
    Config::load().is_ok_and(|config| config.find_provider(ours).is_some_and(|(_, key)| key))
}

/// What adding Command Code wrote: the models now in the config, and notes besides them.
pub(crate) struct Outcome {
    pub added: crate::config::Added,
    pub notes: Vec<String>,
}

/// Adds Command Code's provider and the models the subscription serves, after checking the
/// key names an account.
pub(crate) async fn add(api_key: &str) -> Result<Outcome> {
    let http = super::http_client()?;
    whoami(&http, api_key).await?;
    let entries = catalog(&http)
        .await
        .context("could not list Command Code's models")?;
    let models = cmd_models(&entries);
    if models.is_empty() {
        bail!("Command Code listed no models");
    }
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "commandcode".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    let added = crate::config::add_models(&Additions {
        name: "commandcode",
        ours,
        provider,
        models,
        comment: "Command Code, signed in with `killi login command-code`.",
    })?;
    Ok(Outcome {
        added,
        notes: Vec::new(),
    })
}

/// Before an interactive start on a Command Code model, offers to sign in when killi isn't.
pub(crate) async fn offer(model: Option<&str>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() || signed_in() {
        return Ok(());
    }
    let Ok(config) = Config::load() else {
        return Ok(());
    };
    let Some(alias) = config.commandcode_model(model) else {
        return Ok(());
    };
    if crate::config::confirm(&format!(
        "`{alias}` runs on your Command Code subscription and killi isn't signed in. Continue with commandcode.ai in the browser?"
    ))? {
        login().await?;
    }
    Ok(())
}

/// `killi login command-code`: signs in with the account in the browser, checks the key the
/// studio posts back, and adds the subscription's models to the config.
pub(super) async fn login() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("`killi login command-code` needs a terminal: it opens the browser to sign in");
    }
    eprintln!("Command Code signs in with your commandcode.ai account, in the browser.");
    let state = random_token()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("could not listen for the browser's answer")?;
    let port = listener.local_addr()?.port();
    let url = auth_url(port, &state)?;
    eprintln!("Continue with Command Code in your browser. If it didn't open, go to:\n\n  {url}\n");
    super::open_browser(url.as_str());
    let outcome = tokio::time::timeout(SIGN_IN_WAIT, wait_for_callback(&listener, &state)).await;
    drop(listener);
    let callback = match outcome {
        Ok(Ok(callback)) => Some(callback),
        Ok(Err(error)) => {
            eprintln!("killi: browser sign-in failed: {error:#}");
            None
        }
        Err(_) => {
            eprintln!("killi: the browser didn't come back within 5 minutes.");
            None
        }
    };
    let (key, name) = match callback {
        Some(callback) => (callback.api_key, callback.user_name),
        None => {
            let pasted = crate::config::read_secret(
                "Paste your Command Code API key instead (empty changes nothing): ",
            )?;
            if pasted.is_empty() {
                bail!("no key given, so nothing changed");
            }
            (pasted, String::new())
        }
    };
    let http = super::http_client()?;
    let who = whoami(&http, &key).await?;
    let name = match name.is_empty() {
        false => name,
        true => who["user"]["userName"]
            .as_str()
            .or_else(|| who["user"]["name"].as_str())
            .unwrap_or_default()
            .to_string(),
    };
    if name.is_empty() {
        eprintln!("Signed in to Command Code.");
    } else {
        eprintln!("Signed in to Command Code as {name}.");
    }
    eprintln!(
        "You're using your Command Code subscription: requests in killi count toward its usage, which the commandcode CLI's /usage shows."
    );
    let outcome = add(&key).await?;
    for note in &outcome.notes {
        println!("killi: {note}");
    }
    println!("{}", outcome.added.report());
    Ok(())
}

/// `killi logout command-code`: takes the key out of the config. The key itself works until
/// it is revoked in the account; the commandcode CLI's own sign-in is untouched.
pub(super) fn logout() -> Result<()> {
    let path = killi_home().join("config.toml");
    let name = Config::load()
        .ok()
        .and_then(|config| config.find_provider(ours).map(|(name, _)| name.to_string()));
    match name {
        Some(name) if crate::config::remove_provider_key(&path, &name)? => {
            println!(
                "Took the Command Code key out of {}. It still works until you revoke it in your account.",
                path.display()
            );
        }
        _ => println!("killi has no Command Code key."),
    }
    Ok(())
}

/// The sign-in page: the callback and state the studio posts the key back with.
fn auth_url(port: u16, state: &str) -> Result<reqwest::Url> {
    let callback = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    Ok(reqwest::Url::parse_with_params(
        &format!("{STUDIO}/studio/auth/cli"),
        [
            ("callback", callback.as_str()),
            ("state", state),
            ("mode", "redirect"),
        ],
    )?)
}

/// 32 random bytes in base64url, as the CLI's state token.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("the system's random source failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// What the browser brings back from a sign-in that went through.
#[derive(Debug, PartialEq)]
struct Callback {
    api_key: String,
    user_name: String,
}

/// Answers the loopback server until a request carries this attempt's callback: the key as
/// JSON, or as a form that lands on the completion page.
async fn wait_for_callback(listener: &TcpListener, state: &str) -> Result<Callback> {
    let mut pending: Option<Callback> = None;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !has_head(&request) && request.len() < 16_384 {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => request.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        let head_end = request
            .windows(4)
            .position(|end| end == b"\r\n\r\n")
            .map(|at| at + 4)
            .unwrap_or(request.len());
        let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
        let mut body = request[head_end..].to_vec();
        let mut lines = head.lines();
        let mut request_line = lines.next().unwrap_or_default().split_whitespace();
        let (method, target) = (
            request_line.next().unwrap_or_default(),
            request_line.next().unwrap_or("/"),
        );
        let header = |name: &str| {
            lines.clone().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().to_string())
            })
        };
        let origin = header("origin").unwrap_or_default();
        let private =
            header("access-control-request-private-network").is_some_and(|value| value == "true");
        let over_cap = method == "POST"
            && header("content-length")
                .and_then(|len| len.parse::<usize>().ok())
                .is_some_and(|len| len > MAX_CALLBACK_BODY);
        while !over_cap
            && method == "POST"
            && body.len()
                < header("content-length")
                    .and_then(|len| len.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(MAX_CALLBACK_BODY + 1)
        {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => body.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        let (reply, outcome) = match over_cap || body.len() > MAX_CALLBACK_BODY {
            true => (
                Reply::json(413, r#"{"success":false,"error":"Payload too large"}"#),
                None,
            ),
            false => route(
                method,
                target,
                &header("content-type").unwrap_or_default(),
                &body,
                state,
                &mut pending,
            ),
        };
        let head = reply.head(&origin, private);
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(reply.body.as_bytes()).await;
        let _ = stream.shutdown().await;
        if let Some(outcome) = outcome {
            return outcome.map_err(anyhow::Error::msg);
        }
    }
}

/// Whether the bytes hold a complete head: headers closed by a blank line.
fn has_head(request: &[u8]) -> bool {
    request.windows(4).any(|end| end == b"\r\n\r\n")
}

/// One callback answer: status, body, and where a form lands next.
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
    location: Option<String>,
}

impl Reply {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.to_string(),
            location: None,
        }
    }

    fn page(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body,
            location: None,
        }
    }

    /// The head with the CORS headers the studio page needs to post here.
    fn head(&self, origin: &str, private: bool) -> String {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            303 => "See Other",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Content Too Large",
            415 => "Unsupported Media Type",
            _ => "Error",
        };
        let allowed = ["https://commandcode.ai", "https://staging.commandcode.ai"];
        let origin = match allowed.contains(&origin) {
            true => origin,
            false => "http://localhost:3000",
        };
        let location = self
            .location
            .as_deref()
            .map_or(String::new(), |to| format!("location: {to}\r\n"));
        let private = match private {
            true => "access-control-allow-private-network: true\r\n",
            false => "",
        };
        format!(
            "HTTP/1.1 {} {reason}\r\n\
             access-control-allow-origin: {origin}\r\n\
             access-control-allow-methods: GET, POST, OPTIONS\r\n\
             access-control-allow-headers: Content-Type\r\n\
             {location}\
             {private}\
             content-type: {}\r\n\
             content-length: {}\r\n\
             connection: close\r\n\r\n",
            self.status,
            self.content_type,
            self.body.len(),
        )
    }
}

/// Routes one callback request. `Ok(None)` keeps listening; otherwise the attempt ends.
#[allow(clippy::too_many_arguments)]
fn route(
    method: &str,
    target: &str,
    content_type: &str,
    body: &[u8],
    state: &str,
    pending: &mut Option<Callback>,
) -> (Reply, Option<Result<Callback, String>>) {
    if method == "OPTIONS" {
        return (Reply::json(204, ""), None);
    }
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"));
    let path = url.as_ref().map(|url| url.path()).unwrap_or_default();
    if method == "GET" && path == COMPLETE_PATH {
        let here = url
            .as_ref()
            .ok()
            .and_then(|url| pair(&url_query_pairs(url), "state"));
        if here.as_deref() != Some(state) {
            return (
                Reply::page(
                    403,
                    error_page("Invalid state token", "Restart login from your terminal."),
                ),
                None,
            );
        }
        return match pending.take() {
            Some(callback) => {
                let page = format!(
                    "<!doctype html><meta charset=utf-8><title>killi</title><p>Signed in to killi as {}. You can close this tab.</p>",
                    escape(&callback.user_name)
                );
                (Reply::page(200, page), Some(Ok(callback)))
            }
            None => (
                Reply::page(
                    404,
                    error_page(
                        "Return to your terminal",
                        "Restart login from your terminal if you reached it directly.",
                    ),
                ),
                None,
            ),
        };
    }
    if path != CALLBACK_PATH {
        return (
            Reply::json(404, r#"{"success":false,"error":"Not found"}"#),
            None,
        );
    }
    if method == "GET" {
        return (
            Reply::page(
                405,
                error_page(
                    "Return to your terminal",
                    "This page completes login automatically during sign-in.",
                ),
            ),
            None,
        );
    }
    if method != "POST" {
        return (
            Reply::json(
                405,
                r#"{"success":false,"error":"Method not allowed. Use POST."}"#,
            ),
            None,
        );
    }
    let kind = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if kind == "application/json" {
        return json_callback(body, state);
    }
    if kind == "application/x-www-form-urlencoded" {
        return form_callback(&String::from_utf8_lossy(body), state, pending);
    }
    (
        Reply::json(
            415,
            r#"{"success":false,"error":"Unsupported content type"}"#,
        ),
        None,
    )
}

/// A JSON post: the key, or the denial.
fn json_callback(body: &[u8], state: &str) -> (Reply, Option<Result<Callback, String>>) {
    let posted: Value = serde_json::from_slice(body).unwrap_or_default();
    if posted.get("error").is_some() {
        return denied(
            posted["state"].as_str().unwrap_or_default(),
            posted["error"].as_str().unwrap_or_default(),
            posted["error_description"].as_str().unwrap_or_default(),
            state,
        );
    }
    let fields = ["apiKey", "state", "userId", "userName", "keyName"]
        .into_iter()
        .map(|name| posted[name].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    let [api_key, here, _, user_name, _] = fields[..] else {
        return (
            Reply::json(
                400,
                r#"{"success":false,"error":"Missing required fields"}"#,
            ),
            None,
        );
    };
    if api_key.is_empty() {
        return (
            Reply::json(
                400,
                r#"{"success":false,"error":"Missing required fields"}"#,
            ),
            None,
        );
    }
    if here != state {
        return (
            Reply::json(403, r#"{"success":false,"error":"Invalid state token"}"#),
            None,
        );
    }
    (
        Reply::json(200, r#"{"success":true}"#),
        Some(Ok(Callback {
            api_key: api_key.to_string(),
            user_name: user_name.to_string(),
        })),
    )
}

/// A form post: the key lands on the completion page, which ends the attempt.
fn form_callback(
    body: &str,
    state: &str,
    pending: &mut Option<Callback>,
) -> (Reply, Option<Result<Callback, String>>) {
    let pairs = url_query_pairs_raw(body);
    if let Some(error) = pair(&pairs, "error") {
        let (reply, outcome) = denied(
            pair(&pairs, "state").as_deref().unwrap_or_default(),
            &error,
            pair(&pairs, "error_description")
                .as_deref()
                .unwrap_or_default(),
            state,
        );
        let reply = match outcome {
            Some(Err(_)) => Reply::page(
                200,
                error_page(
                    "Authorization denied",
                    "Return to your terminal and run login again, or paste a key.",
                ),
            ),
            _ => Reply::page(
                reply.status,
                error_page("Invalid state token", "Restart login from your terminal."),
            ),
        };
        return (reply, outcome);
    }
    let field = |name: &str| pair(&pairs, name).unwrap_or_default();
    let (api_key, here, user_name) = (field("apiKey"), field("state"), field("userName"));
    if api_key.is_empty()
        || field("userId").is_empty()
        || user_name.is_empty()
        || field("keyName").is_empty()
    {
        return (
            Reply::page(
                400,
                error_page(
                    "Invalid request",
                    "Required parameters are missing. Restart login, or paste a key.",
                ),
            ),
            None,
        );
    }
    if here != state {
        return (
            Reply::page(
                403,
                error_page("Invalid state token", "Restart login from your terminal."),
            ),
            None,
        );
    }
    *pending = Some(Callback { api_key, user_name });
    (
        Reply {
            status: 303,
            content_type: "text/html; charset=utf-8",
            body: String::new(),
            location: Some(format!("{COMPLETE_PATH}?state={state}")),
        },
        None,
    )
}

/// A denial post: 403 on a foreign state, else the attempt ends with its reason.
fn denied(
    here: &str,
    error: &str,
    description: &str,
    state: &str,
) -> (Reply, Option<Result<Callback, String>>) {
    if here != state {
        return (
            Reply::json(403, r#"{"success":false,"error":"Invalid state token"}"#),
            None,
        );
    }
    let message = match error {
        "access_denied" => "Command Code authorization was denied".to_string(),
        _ => format!("{error} {description}").trim().to_string(),
    };
    (Reply::json(200, r#"{"success":true}"#), Some(Err(message)))
}

/// The account `key` names, or why it names none.
async fn whoami(http: &reqwest::Client, key: &str) -> Result<Value> {
    let response = http
        .get(format!("{BASE_URL}{WHOAMI_PATH}"))
        .header("user-agent", "cli")
        .header("x-command-code-version", crate::llm::CLI_VERSION)
        .header("x-cli-environment", "production")
        .header("authorization", format!("Bearer {key}"))
        .send()
        .await
        .context("could not reach Command Code")?;
    let status = response.status();
    let body: Value = serde_json::from_slice(&response.bytes().await?).unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("Command Code turned the key away (HTTP 401); run `killi login command-code` again");
    }
    if !status.is_success() {
        bail!(
            "Command Code turned the sign-in away (HTTP {status}); run `killi login command-code` again"
        );
    }
    Ok(body)
}

/// The subscription's model catalog entries, in the server's order.
async fn catalog(http: &reqwest::Client) -> Result<Vec<Value>> {
    let body = http
        .get(format!("{BASE_URL}{CATALOG_PATH}"))
        .header("user-agent", "cli")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let list: Value = serde_json::from_slice(&body)?;
    list["data"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("Command Code's catalog has no models"))
}

/// The catalog entries as models to add: every entry answers on the subscription API.
fn cmd_models(entries: &[Value]) -> Vec<NewModel> {
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str().filter(|id| !id.is_empty())?;
            let efforts: Vec<String> = entry["reasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            Some(NewModel {
                id: id.to_string(),
                protocol: None,
                base_url: None,
                context: entry["context_length"].as_u64(),
                output: None,
                images: entry["inputModalities"]
                    .as_array()
                    .is_some_and(|inputs| inputs.iter().any(|input| input == "image")),
                thinking: !efforts.is_empty(),
                tools: true,
                default_effort: None,
                efforts,
                more_capabilities: Vec::new(),
                display: None,
            })
        })
        .collect()
}

/// A query string's decoded pairs: the form posts are encoded the same way.
fn url_query_pairs_raw(query: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://127.0.0.1/x?{query}"))
        .map(|url| {
            url.query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn url_query_pairs(url: &reqwest::Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn pair(pairs: &[(String, String)], name: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn error_page(heading: &str, message: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>killi</title><h1>{}</h1><p>{}</p>",
        escape(heading),
        escape(message)
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn routed(
        method: &str,
        target: &str,
        content_type: &str,
        body: &str,
        pending: &mut Option<Callback>,
    ) -> (Reply, Option<Result<Callback, String>>) {
        route(
            method,
            target,
            content_type,
            body.as_bytes(),
            "state-1",
            pending,
        )
    }

    #[test]
    fn a_json_post_with_the_key_ends_the_attempt() {
        let mut pending = None;
        let (reply, outcome) = routed(
            "POST",
            "/callback",
            "application/json",
            r#"{"apiKey":"user_k","state":"state-1","userId":"u","userName":"tim","keyName":"cli"}"#,
            &mut pending,
        );
        assert_eq!(
            (reply.status, reply.body.as_str()),
            (200, r#"{"success":true}"#)
        );
        assert_eq!(
            outcome.unwrap().unwrap(),
            Callback {
                api_key: "user_k".to_string(),
                user_name: "tim".to_string(),
            }
        );
    }

    #[test]
    fn a_form_post_lands_on_the_completion_page_which_ends_it() {
        let mut pending = None;
        let (reply, outcome) = routed(
            "POST",
            "/callback",
            "application/x-www-form-urlencoded",
            "apiKey=user_k&state=state-1&userId=u&userName=tim&keyName=cli",
            &mut pending,
        );
        assert_eq!(reply.status, 303);
        assert_eq!(
            reply.location.as_deref(),
            Some("/callback/complete?state=state-1")
        );
        assert!(outcome.is_none() && pending.is_some());

        let (reply, outcome) = routed(
            "GET",
            "/callback/complete?state=state-1",
            "",
            "",
            &mut pending,
        );
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("tim"), "{}", reply.body);
        assert_eq!(outcome.unwrap().unwrap().api_key, "user_k");
    }

    #[test]
    fn a_foreign_state_and_a_denial_end_nothing_and_everything() {
        let mut pending = None;
        let (reply, outcome) = routed(
            "POST",
            "/callback",
            "application/json",
            r#"{"apiKey":"user_k","state":"older","userId":"u","userName":"tim","keyName":"cli"}"#,
            &mut pending,
        );
        assert_eq!(reply.status, 403);
        assert!(outcome.is_none());

        let (reply, outcome) = routed(
            "POST",
            "/callback",
            "application/json",
            r#"{"error":"access_denied","error_description":"","state":"state-1"}"#,
            &mut pending,
        );
        assert_eq!(reply.status, 200);
        assert_eq!(
            outcome.unwrap().unwrap_err(),
            "Command Code authorization was denied"
        );
    }

    #[test]
    fn noise_gets_an_answer_and_the_attempt_goes_on() {
        let mut pending = None;
        for (method, target, content_type, status) in [
            ("OPTIONS", "/callback", "", 204),
            ("GET", "/favicon.ico", "", 404),
            ("GET", "/callback", "", 405),
            ("POST", "/callback", "text/plain", 415),
            ("POST", "/callback", "application/json", 400),
        ] {
            let (reply, outcome) = routed(method, target, content_type, "{}", &mut pending);
            assert_eq!(reply.status, status, "{method} {target}");
            assert!(outcome.is_none());
        }
        assert!(pending.is_none());
    }

    #[test]
    fn the_callback_answers_carry_cors_and_a_length() {
        let head = Reply::json(200, r#"{"success":true}"#).head("https://commandcode.ai", true);
        assert!(
            head.contains("access-control-allow-origin: https://commandcode.ai"),
            "{head}"
        );
        assert!(
            head.contains("access-control-allow-private-network: true"),
            "{head}"
        );
        assert!(head.contains("content-length: 16"), "{head}");
        let head = Reply::json(200, "{}").head("https://rogue.example", false);
        assert!(
            head.contains("access-control-allow-origin: http://localhost:3000"),
            "{head}"
        );
    }

    #[test]
    fn the_auth_url_names_the_callback_and_state() {
        let url = auth_url(43210, "state-1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://commandcode.ai/studio/auth/cli?callback=http%3A%2F%2F127.0.0.1%3A43210%2Fcallback&state=state-1&mode=redirect"
        );
    }

    #[test]
    fn the_catalog_becomes_models_with_their_efforts() {
        let entries = vec![
            json!({"id": "claude-sonnet-5-5", "context_length": 1000000,
                "display": {"label": "Claude Sonnet 5.5"}, "inputModalities": ["text", "image"],
                "reasoningEfforts": ["low", "medium", "high", "xhigh", "max"]}),
            json!({"id": "muse-spark", "context_length": 1000000,
                "display": {"label": "Spark"}, "inputModalities": ["text"], "reasoningEfforts": []}),
            json!({"context_length": 1}),
        ];
        let models = cmd_models(&entries);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-sonnet-5-5");
        assert!(models[0].images && models[0].thinking && models[0].tools);
        assert_eq!(models[0].efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(models[0].context, Some(1000000));
        assert!(!models[1].images && !models[1].thinking && models[1].tools);
    }

    #[test]
    fn the_provider_is_found_by_its_url() {
        let config: Config = toml::from_str(
            "[providers.mine]\ntype = \"commandcode\"\nbase_url = \"https://api.commandcode.ai\"\napi_key = \"k\"\n\n[providers.other]\ntype = \"openai\"\napi_key = \"x\"\n",
        )
        .unwrap();
        assert_eq!(config.find_provider(ours), Some(("mine", true)));
    }
}
