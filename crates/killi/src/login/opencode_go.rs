//! OpenCode Go: a subscription to open coding models behind one API key from the OpenCode
//! Console (opencode.ai/docs/go). Its models answer on three APIs killi speaks (Chat
//! Completions, Responses and Anthropic Messages), so `killi login opencode-go` writes each one
//! with the API and URL that models.dev, OpenCode's own catalog, gives for it.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{Additions, Config, NewModel, Provider, killi_home};

const CONSOLE: &str = "https://opencode.ai/console";
/// Where Chat Completions and Responses answer: the provider's `base_url`.
const OPENAI_URL: &str = "https://opencode.ai/inference/go/openai/v1";
/// The models Go serves now, as ids.
const SERVED_URL: &str = "https://opencode.ai/inference/go/v1/models";
/// models.dev's catalog: every provider's models with their API, limits and inputs.
const CATALOG_URL: &str = "https://models.dev/api.json";
const KEY_ENV: &str = "OPENCODE_API_KEY";
/// The header OpenCode routes and caches a conversation by.
const SESSION_HEADER: &str = "x-opencode-session";

/// Every model Go serves answers under this prefix; the key goes nowhere else.
const GO_PREFIX: &str = "https://opencode.ai/inference/go/";

/// The configured provider that is OpenCode Go: the one on its URL.
fn ours(provider: &Provider) -> bool {
    provider.base_url.as_deref().is_some_and(|url| url.starts_with(GO_PREFIX))
}

/// Whether the config has OpenCode Go with a key.
pub(super) fn signed_in() -> bool {
    Config::load().is_ok_and(|config| config.find_provider(ours).is_some_and(|(_, key)| key))
}

/// `killi login opencode-go`: takes the key, pasted or from `$OPENCODE_API_KEY`, and adds
/// Go's models to the config.
pub(super) async fn login() -> Result<()> {
    eprintln!("OpenCode Go uses the API key from {CONSOLE}, where you subscribe.");
    let from_env = std::env::var(KEY_ENV).is_ok_and(|key| !key.is_empty()) && crate::config::confirm(&format!("Use the key in ${KEY_ENV}?"))?;
    let key = match from_env {
        true => None,
        false => {
            let key = crate::config::read_secret("Paste your OpenCode Go API key: ")?;
            if key.is_empty() {
                bail!("no key given, so nothing changed");
            }
            Some(key)
        }
    };
    let http = super::http_client()?;
    let served = match served(&http).await {
        Ok(served) => Some(served),
        Err(error) => {
            eprintln!("killi: could not list the models Go serves ({error:#}); taking models.dev's list");
            None
        }
    };
    let catalog = catalog(&http).await.context("could not read OpenCode Go's models from models.dev")?;
    let (models, skipped, dropped) = go_models(&catalog, served.as_deref());
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), OPENAI_URL.into());
    provider.insert("session_header".into(), SESSION_HEADER.into());
    match key {
        Some(key) => provider.insert("api_key".into(), key.into()),
        None => provider.insert("api_key_env".into(), KEY_ENV.into()),
    };
    crate::config::add_models(&Additions {
        name: "opencode-go",
        ours,
        provider,
        models,
        comment: "OpenCode Go, signed in with `killi login opencode-go`.",
    })?;
    if !skipped.is_empty() {
        println!("Left out, as they take Gemini's API, which killi doesn't speak: {}.", skipped.join(", "));
    }
    if !dropped.is_empty() {
        println!("Left out, as their URLs are outside OpenCode Go: {}.", dropped.join(", "));
    }
    println!("The key isn't checked here; a wrong one shows as HTTP 401 on the first request. Usage: {CONSOLE}");
    Ok(())
}

/// `killi logout opencode-go`: takes the key out of the config. The key itself works until it
/// is revoked in the console.
pub(super) fn logout() -> Result<()> {
    let path = killi_home().join("config.toml");
    let name = Config::load().ok().and_then(|config| config.find_provider(ours).map(|(name, _)| name.to_string()));
    match name {
        Some(name) if crate::config::remove_provider_key(&path, &name)? => {
            println!("Took the OpenCode Go key out of {}. It still works until you revoke it at {CONSOLE}.", path.display());
        }
        _ => println!("killi has no OpenCode Go key."),
    }
    Ok(())
}

/// The ids of the models Go serves now, in its order.
async fn served(http: &reqwest::Client) -> Result<Vec<String>> {
    let body = http.get(SERVED_URL).send().await?.error_for_status()?.bytes().await?;
    let list: Value = serde_json::from_slice(&body)?;
    Ok(list["data"].as_array().into_iter().flatten().filter_map(|model| model["id"].as_str().map(String::from)).collect())
}

/// models.dev's entries for OpenCode Go; the rest of the catalog is skipped as it is read.
async fn catalog(http: &reqwest::Client) -> Result<BTreeMap<String, Value>> {
    #[derive(Deserialize)]
    struct Catalog {
        #[serde(rename = "opencode-go")]
        go: Option<Go>,
    }
    #[derive(Deserialize)]
    struct Go {
        models: BTreeMap<String, Value>,
    }
    let body = http.get(CATALOG_URL).send().await?.error_for_status()?.bytes().await?;
    let catalog: Catalog = serde_json::from_slice(&body)?;
    catalog.go.map(|go| go.models).ok_or_else(|| anyhow!("models.dev has no opencode-go entry"))
}

/// The models to add, in the order Go serves them (models.dev's when that list is missing),
/// each on the API and URL models.dev names, and the ones left out: Gemini's API, and URLs
/// outside OpenCode Go, which the key must not be sent to. A model Go serves that
/// models.dev doesn't know is left out as well: its API can't be told.
fn go_models(catalog: &BTreeMap<String, Value>, served: Option<&[String]>) -> (Vec<NewModel>, Vec<String>, Vec<String>) {
    let ids: Vec<&String> = match served {
        Some(served) => served.iter().filter(|id| catalog.contains_key(*id)).collect(),
        None => catalog.keys().collect(),
    };
    let mut models = Vec::new();
    let mut skipped = Vec::new();
    let mut dropped = Vec::new();
    for id in ids {
        let entry = &catalog[id];
        let api = entry["provider"]["api"].as_str().unwrap_or(OPENAI_URL).trim_end_matches('/');
        let elsewhere = (api != OPENAI_URL).then(|| api.to_string());
        let (protocol, base_url) = match entry["provider"]["npm"].as_str() {
            Some("@ai-sdk/google") => {
                skipped.push(id.clone());
                continue;
            }
            // killi adds `/v1/messages` to an Anthropic base URL.
            Some("@ai-sdk/anthropic") => (Some("anthropic"), Some(api.trim_end_matches("/v1").to_string())),
            Some("@ai-sdk/openai") => (Some("openai_responses"), elsewhere),
            _ => (None, elsewhere),
        };
        if !base_url.as_deref().unwrap_or(OPENAI_URL).starts_with(GO_PREFIX) {
            dropped.push(id.clone());
            continue;
        }
        let options = entry["reasoning_options"].as_array().into_iter().flatten().filter(|option| option["type"] == "effort");
        let efforts = options.flat_map(|option| option["values"].as_array().into_iter().flatten()).filter_map(Value::as_str);
        models.push(NewModel {
            id: id.clone(),
            protocol,
            base_url,
            context: entry["limit"]["context"].as_u64(),
            output: entry["limit"]["output"].as_u64(),
            images: entry["modalities"]["input"].as_array().is_some_and(|inputs| inputs.iter().any(|input| input == "image")),
            thinking: entry["reasoning"].as_bool().unwrap_or(false),
            tools: entry["tool_call"].as_bool().unwrap_or(false),
            default_effort: None,
            // killi's own "off" stands for "none".
            efforts: efforts.filter(|effort| *effort != "none").map(String::from).collect(),
        });
    }
    (models, skipped, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("gpt-6-luna".to_string(), json!({"provider": {"npm": "@ai-sdk/openai", "api": "https://opencode.ai/inference/go/openai/v1"},
                "reasoning": true, "tool_call": true, "limit": {"context": 1050000, "output": 128000},
                "modalities": {"input": ["text", "image"]}, "reasoning_options": [{"type": "effort", "values": ["none", "low", "high"]}]})),
            ("claude-haiku-5-5".to_string(), json!({"provider": {"npm": "@ai-sdk/anthropic", "api": "https://opencode.ai/inference/go/anthropic/v1"},
                "reasoning": true, "tool_call": true, "limit": {"context": 1000000, "output": 128000}})),
            ("kimi-k3".to_string(), json!({"provider": {"api": "https://opencode.ai/inference/go/openai/v1"}, "tool_call": true,
                "limit": {"context": 1048576, "output": 131072}, "modalities": {"input": ["text"]}})),
            ("gemini-4-argon".to_string(), json!({"provider": {"npm": "@ai-sdk/google", "api": "https://opencode.ai/inference/go/google/v1beta"}})),
            ("space-bunny".to_string(), json!({"provider": {"api": "https://opencode.ai/inference/go/openai/v1"}})),
        ])
    }

    #[test]
    fn each_model_gets_the_api_and_url_it_answers_on() {
        let served: Vec<String> = ["kimi-k3", "gpt-6-luna", "claude-haiku-5-5", "gemini-4-argon", "glm-5.1"].map(String::from).to_vec();
        let (models, skipped, dropped) = go_models(&catalog(), Some(&served));
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["kimi-k3", "gpt-6-luna", "claude-haiku-5-5"], "Go's order; unknown and codename models left out");
        assert_eq!(skipped, ["gemini-4-argon"]);
        assert!(dropped.is_empty(), "the fixture stays on {GO_PREFIX}");

        assert_eq!((models[0].protocol, models[0].base_url.as_deref()), (None, None), "Chat Completions on the provider's URL");
        assert_eq!((models[1].protocol, models[1].base_url.as_deref()), (Some("openai_responses"), None));
        assert_eq!((models[2].protocol, models[2].base_url.as_deref()), (Some("anthropic"), Some("https://opencode.ai/inference/go/anthropic")));

        let luna = &models[1];
        assert_eq!((luna.context, luna.output, luna.images, luna.thinking, luna.tools), (Some(1050000), Some(128000), true, true, true));
        assert_eq!(luna.efforts, ["low", "high"]);
        assert!(!models[0].thinking && models[0].tools && !models[0].images);
    }

    #[test]
    fn a_model_on_an_outside_url_is_dropped() {
        let mut entries = catalog();
        entries.insert("rogue-1".to_string(), json!({"provider": {"npm": "@ai-sdk/openai", "api": "https://rogue.example/v1"}, "tool_call": true}));
        let served: Vec<String> = ["rogue-1", "kimi-k3"].map(String::from).to_vec();
        let (models, _, dropped) = go_models(&entries, Some(&served));
        assert_eq!(dropped, ["rogue-1"]);
        assert!(models.iter().all(|model| model.id != "rogue-1"), "the key would be sent to {}", models.iter().find(|model| model.id == "rogue-1").and_then(|model| model.base_url.as_deref()).unwrap_or("(provider url)"));
        assert!(models.iter().any(|model| model.id == "kimi-k3"));
    }

    #[test]
    fn without_gos_list_models_dev_names_the_models() {
        let (models, skipped, dropped) = go_models(&catalog(), None);
        assert_eq!(models.len(), 4);
        assert_eq!(skipped, ["gemini-4-argon"]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_provider_is_found_by_its_url() {
        let config: Config = toml::from_str(
            "[providers.mine]\ntype = \"openai\"\nbase_url = \"https://opencode.ai/inference/go/openai/v1\"\napi_key = \"k\"\n\n[providers.other]\ntype = \"openai\"\napi_key = \"x\"\n",
        )
        .unwrap();
        assert_eq!(config.find_provider(ours), Some(("mine", true)));
    }
}
