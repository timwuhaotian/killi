//! A custom endpoint: `/provider custom` asks its name, protocol, endpoint and key, reads
//! the models the endpoint lists and writes the ones the user ticks.

use std::time::Duration;

use super::*;
use anyhow::{Context, Result, bail};

/// What `/provider custom` writes, said before its questions.
pub const ABOUT: &str = "Asks a name, the endpoint's protocol (openai or anthropic), its base URL and its API key, reads the models the endpoint lists and writes a provider of that name with the ones you tick. The key is kept in config.toml, readable only by you; a wrong one shows on the first request.";

/// The model ids the endpoint lists: `/models` on the openai protocol (the shape OpenAI,
/// vLLM and LM Studio serve), `/v1/models` on anthropic's. Sorted, without repeats.
pub async fn served(protocol: &str, base_url: &str, api_key: &str) -> Result<Vec<String>> {
    let root = base_url.trim_end_matches('/');
    let url = if protocol == "anthropic" {
        // Messages' list pages at 20 by default; the tick list should offer them all.
        format!("{root}/v1/models?limit=1000")
    } else {
        format!("{root}/models")
    };
    let http = reqwest::Client::builder()
        .user_agent(crate::llm::USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;
    let request = if protocol == "anthropic" {
        http.get(&url)
            .header("x-api-key", api_key)
            // The version Messages requests carry, as `llm/anthropic.rs` sends it.
            .header("anthropic-version", "2023-06-01")
    } else {
        http.get(&url).bearer_auth(api_key)
    };
    let response = request
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{url} answered HTTP {status}");
    }
    let body: serde_json::Value = serde_json::from_slice(
        &response
            .bytes()
            .await
            .with_context(|| format!("could not read {url}'s answer"))?,
    )
    .with_context(|| format!("{url}'s answer isn't JSON"))?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str())
        .map(String::from)
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        bail!("{url} listed no models");
    }
    Ok(ids)
}

/// A provider name the endpoint's URL suggests for its question's prefill: the host's
/// first label without a leading api. (`api.example.com` → `example`, `localhost:8000` →
/// `localhost`), cleaned the way [`name`] cleans.
pub fn suggested_name(url: &str) -> String {
    let host = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', ':', '?'])
        .next()
        .unwrap_or_default();
    let host = host.strip_prefix("api.").unwrap_or(host);
    name(host.split('.').next().unwrap_or_default())
}

/// A provider name as the config can carry it: letters, digits, dashes and underscores,
/// anything else folded into one dash. `custom` when nothing is left.
pub fn name(wanted: &str) -> String {
    let mut cleaned = String::new();
    let mut dash = false;
    for c in wanted.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            cleaned.push(c);
            dash = false;
        } else if !cleaned.is_empty() && !dash {
            cleaned.push('-');
            dash = true;
        }
    }
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        "custom".to_string()
    } else {
        cleaned.to_string()
    }
}

/// `/provider custom`'s write: a provider of the given name on the protocol with the
/// endpoint and key, and a model per ticked id.
pub fn add(
    name: &str,
    protocol: &str,
    base_url: &str,
    api_key: &str,
    ids: &[String],
) -> Result<Added> {
    add_models(&additions(name, protocol, base_url, api_key, ids))
}

/// The provider and models the flow writes. A repeat under the same name writes another
/// provider (`example-2`, …) instead of repairing: only the endpoint's own list says which
/// models it serves, so the ticked ones decide what changes.
fn additions(
    wanted: &str,
    protocol: &str,
    base_url: &str,
    api_key: &str,
    ids: &[String],
) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), protocol.into());
    provider.insert("base_url".into(), base_url.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: name(wanted),
        ours: |_| false,
        provider,
        models: ids
            .iter()
            .map(|id| NewModel {
                id: id.clone(),
                tools: true,
                ..NewModel::default()
            })
            .collect(),
        comment: "A custom endpoint, added with /provider custom.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serves one JSON answer, returning its root URL and the request as received once the
    /// caller's client has run.
    async fn serve(
        body: &'static str,
        status: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(answer.as_bytes()).await;
            request
        });
        (url, asked)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_openai_protocol_lists_models_with_a_bearer_key() {
        let (url, asked) = serve(r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#, "200 OK").await;
        assert_eq!(served("openai", &url, "sk").await.unwrap(), ["a", "b"]);
        let request = asked.await.unwrap();
        assert!(request.starts_with("GET /models "), "{request}");
        assert!(
            request.to_lowercase().contains("authorization: bearer sk"),
            "{request}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_anthropic_protocol_lists_models_with_its_headers() {
        let (url, asked) = serve(r#"{"data":[{"id":"claude-x"}]}"#, "200 OK").await;
        assert_eq!(served("anthropic", &url, "sk").await.unwrap(), ["claude-x"]);
        let request = asked.await.unwrap();
        assert!(
            request.starts_with("GET /v1/models?limit=1000 "),
            "{request}"
        );
        assert!(request.contains("x-api-key: sk"), "{request}");
        assert!(
            request.contains("anthropic-version: 2023-06-01"),
            "{request}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_endpoint_that_refuses_the_key_lists_nothing() {
        let (url, _) = serve("{}", "401 Unauthorized").await;
        let error = served("openai", &url, "sk").await.unwrap_err().to_string();
        assert!(error.contains("HTTP 401"), "{error}");
    }

    #[test]
    fn writes_the_provider_and_the_ticked_models() {
        let ids = vec!["glm-5.3".to_string(), "gpt-4o".to_string()];
        let (out, name, _, added) = models::with_models(
            "",
            &additions(
                "example",
                "openai",
                "https://api.example.com/v1",
                "sk",
                &ids,
            ),
        )
        .unwrap();
        assert_eq!((name.as_str(), added.len()), ("example", 2));
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.example.com/v1\""),
            "{out}"
        );
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("example/gpt-4o")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, "https://api.example.com/v1");
        assert_eq!(model.api_key, "sk");
    }

    #[test]
    fn the_anthropic_protocol_resolves_messages() {
        let ids = vec!["claude-x".to_string()];
        let (out, _, _, added) = models::with_models(
            "",
            &additions("mine", "anthropic", "https://api.example.com", "sk", &ids),
        )
        .unwrap();
        assert_eq!(added, ["mine/claude-x"]);
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("mine/claude-x")).unwrap();
        assert_eq!(model.protocol, Protocol::Anthropic);
        assert_eq!(model.base_url, "https://api.example.com");
    }

    #[test]
    fn the_models_take_tool_use_only() {
        let ids = vec!["gpt-4o".to_string()];
        let (out, _, _, _) = models::with_models(
            "",
            &additions(
                "example",
                "openai",
                "https://api.example.com/v1",
                "sk",
                &ids,
            ),
        )
        .unwrap();
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.models["example/gpt-4o"].capabilities, ["tool_use"]);
    }

    #[test]
    fn a_taken_name_writes_the_next_one() {
        let mine = "[providers.example]\ntype = \"openai\"\nbase_url = \"https://a.example/v1\"\napi_key = \"sk\"\n";
        let ids = vec!["gpt-4o".to_string()];
        let (out, name, _, added) = models::with_models(
            mine,
            &additions("example", "openai", "https://b.example/v1", "sk", &ids),
        )
        .unwrap();
        assert_eq!(name, "example-2");
        assert_eq!(added, ["example-2/gpt-4o"]);
        assert!(out.contains(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            config.providers["example-2"].base_url.as_deref(),
            Some("https://b.example/v1")
        );
    }

    #[test]
    fn a_name_is_cleaned_to_what_the_config_can_carry() {
        for (wanted, name) in [
            ("example", "example"),
            ("My Provider!", "My-Provider"),
            ("a/b", "a-b"),
            ("..", "custom"),
        ] {
            assert_eq!(super::name(wanted), name, "{wanted}");
        }
    }

    #[test]
    fn a_name_is_suggested_from_the_endpoint_host() {
        for (url, name) in [
            ("https://api.example.com/v1", "example"),
            ("https://openrouter.ai/v1", "openrouter"),
            ("http://localhost:8000", "localhost"),
            ("https://api.z.ai/api/paas/v4", "z"),
        ] {
            assert_eq!(suggested_name(url), name, "{url}");
        }
    }
}
